//! 檢查 GitHub Release 是否有新版，並就地替換目前執行檔。
//!
//! 不依賴 `gh`：呼叫 `curl`（沒有 shell、沒有 `.sh` 檔，跟專案呼叫 `git` 同一套
//! 機制），也不碰 GitHub API——版本與 asset 檔名都從 release 附的
//! `checksum.txt` 讀，免 token、無 rate limit。
//!
//! 純函式（`platform_label`／`asset_filename`／`pick_asset`／`newer_than_current`）
//! 與碰外界的部分（curl、檔案系統）分開，前者可以直接測。

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, SystemTime},
};

use semver::Version;

use crate::event::{AppEvent, EventController};

const REPO_URL: &str = env!("CARGO_PKG_REPOSITORY");
const CHECKSUMS_URL: &str = concat!(
    env!("CARGO_PKG_REPOSITORY"),
    "/releases/latest/download/checksum.txt"
);
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const MARKER_FILE_NAME: &str = "update_check";

/// 檢查並視需要開啟更新提示。啟動時的自動檢查與 `U` 鍵／`-U` 共用這個入口，
/// 差異只在 `manual`：
/// - 自動（`manual = false`）：沒開 config 旋鈕給這件事，節流未到期就整個
///   不檢查；已是最新或出錯（沒裝 curl、沒網路）一律靜默，不能在啟動時噴錯。
/// - 手動：繞過節流，任何結果都要吭聲。
///
/// 兩種情況都會標記「已檢查」，手動觸發後下次啟動不會馬上又問一次。
pub fn spawn_check(ec: &EventController, manual: bool) {
    if !manual && !should_check_on_startup() {
        return;
    }
    let tx = ec.sender();
    std::thread::spawn(move || {
        let result = check_for_update();
        mark_checked();
        match result {
            Ok(Some(tag)) => tx.send(AppEvent::OpenUpdatePrompt { tag }),
            Ok(None) if manual => tx.send(AppEvent::NotifyInfo(format!(
                "Already up to date (v{})",
                env!("CARGO_PKG_VERSION")
            ))),
            Err(e) if manual => tx.send(AppEvent::NotifyError(e)),
            // 自動檢查：已是最新或出錯一律靜默。
            _ => {}
        }
    });
}

/// 查詢最新 release，比目前版本新才回傳 tag（`v2.4.1`）。
pub fn check_for_update() -> Result<Option<String>, String> {
    let checksums = fetch_checksums()?;
    let (latest, _asset) =
        pick_asset(&checksums, env::consts::OS, env::consts::ARCH).ok_or_else(no_asset_error)?;
    Ok(newer_than_current(&latest, env!("CARGO_PKG_VERSION")).map(|v| format!("v{v}")))
}

/// 下載 `tag` 對應這台機器的 asset 並就地替換目前執行檔。
pub fn download_and_replace(tag: &str) -> Result<(), String> {
    if cfg!(windows) {
        return Err("Windows 請至 GitHub Releases 頁面手動下載更新".into());
    }
    if cfg!(debug_assertions) {
        return Err("開發版本（debug build）不支援自我更新".into());
    }
    if UPDATE_RUNNING.swap(true, Ordering::SeqCst) {
        return Err("已經有一個更新在進行中".into());
    }
    let _guard = UpdateGuard;

    let version = tag.trim_start_matches('v');
    let asset =
        asset_filename(version, env::consts::OS, env::consts::ARCH).ok_or_else(no_asset_error)?;

    let target = current_exe_checked()?;
    let tmp_name = format!(
        ".{}.new.{}",
        target
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("ysgit"),
        std::process::id()
    );
    let tmp = target.with_file_name(tmp_name);

    // 可寫性檢查兼暫存檔——同目錄，之後 `rename` 才不會跨檔案系統失敗。
    fs::File::create(&tmp)
        .map_err(|e| format!("無法在 {} 寫入（權限不足？）: {e}", tmp.display()))?;

    let staged = download_asset(tag, &asset, &tmp)
        .and_then(|()| copy_permissions(&target, &tmp))
        .and_then(|()| verify_binary(&tmp, version));
    if let Err(e) = staged {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }

    // 原子替換；舊 inode 仍被執行中的 process 持有所以不會 `ETXTBSY`，產生的是
    // 全新 inode，天然避開 macOS 用 `cp` 覆蓋會繼承舊檔 security metadata 的坑
    // （deploy-ysgit skill 記下的那個坑：舊檔被 Gatekeeper 標記過，新 binary
    // 繼承標記，啟動時 SIGKILL）。絕對不要用 `cp`。
    fs::rename(&tmp, &target).map_err(|e| format!("替換執行檔失敗: {e}"))
}

fn no_asset_error() -> String {
    format!(
        "沒有 {}-{} 平台的發布版本",
        env::consts::OS,
        env::consts::ARCH
    )
}

// ── 重入保護 ──
//
// 下載中按 Esc 藏掉 pending overlay，背景 thread 會繼續跑；藏完 `U` 又能按了。
// 用 AtomicBool 擋第二次呼叫，而不是拿一個 `create_new` 的暫存檔當鎖——
// 那種鎖在 process 被強制終止時會變成永久殘留檔案，往後每次更新都撞
// 「File exists」，只能手動 `rm` 才能解。

static UPDATE_RUNNING: AtomicBool = AtomicBool::new(false);

struct UpdateGuard;

impl Drop for UpdateGuard {
    fn drop(&mut self) {
        UPDATE_RUNNING.store(false, Ordering::SeqCst);
    }
}

// ── 節流：一個 0-byte 檔，只看 mtime ──

fn marker_path() -> Option<PathBuf> {
    crate::config::config_dir().map(|dir| dir.join(MARKER_FILE_NAME))
}

fn should_check_on_startup() -> bool {
    if env::var_os("YSGIT_NO_UPDATE_CHECK").is_some() {
        return false;
    }
    let Some(path) = marker_path() else {
        return true;
    };
    let Ok(modified) = fs::metadata(&path).and_then(|m| m.modified()) else {
        return true;
    };
    SystemTime::now()
        .duration_since(modified)
        .is_ok_and(|elapsed| elapsed >= CHECK_INTERVAL)
}

fn mark_checked() {
    let Some(path) = marker_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::File::create(&path);
}

// ── 純函式 ──

/// release.yml 產出的四個平台各自的檔名後綴。linux 沒有 arm64 target，
/// windows 帶 `.exe`。組合外的平台（例如 linux-arm64）回 None。
fn platform_label(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Some("macos-arm64"),
        ("macos", "x86_64") => Some("macos-x64"),
        ("linux", "x86_64") => Some("linux"),
        ("windows", "x86_64") => Some("windows.exe"),
        _ => None,
    }
}

fn asset_filename(version: &str, os: &str, arch: &str) -> Option<String> {
    platform_label(os, arch).map(|label| format!("ysgit_{version}-{label}"))
}

/// 從 `checksum.txt`（`<sha256>␠␠<filename>` 每行一筆）挑出這個平台那一行，
/// 回傳 (版本, 檔名)。找不到對應平台、或內容被 captive portal 換成垃圾 HTML
/// 都回 None——不強行解出一個假版本。
fn pick_asset(checksums: &str, os: &str, arch: &str) -> Option<(String, String)> {
    let suffix = format!("-{}", platform_label(os, arch)?);
    checksums.lines().find_map(|line| {
        let filename = line.split_whitespace().nth(1)?;
        let version = filename.strip_prefix("ysgit_")?.strip_suffix(&suffix)?;
        Some((version.to_string(), filename.to_string()))
    })
}

/// `latest` 比 `current` 新才回 Some。用 semver 比較，不比字串——
/// `"2.10.0" > "2.9.0"` 字串比較會給錯答案。
fn newer_than_current(latest: &str, current: &str) -> Option<Version> {
    let latest = Version::parse(latest).ok()?;
    let current = Version::parse(current).ok()?;
    (latest > current).then_some(latest)
}

// ── 碰外界的部分 ──

/// 兩個呼叫點共用的旗標，`--proto-redir =https` 是安全性設定（擋 redirect
/// 被降級到 http）——散在兩處的話，日後改一邊漏一邊不會有任何提示。
fn curl(max_time: &str) -> Command {
    let mut cmd = Command::new("curl");
    cmd.args([
        "-fsS",
        "-L",
        "--proto-redir",
        "=https",
        "--max-time",
        max_time,
    ])
    .stdin(Stdio::null());
    cmd
}

fn fetch_checksums() -> Result<String, String> {
    let output = curl("15")
        .arg(CHECKSUMS_URL)
        .output()
        .map_err(|e| curl_spawn_error(&e))?;

    if !output.status.success() {
        return Err(format!(
            "抓取 release 資訊失敗（{}）",
            exit_status_message(&output.status)
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| format!("回應不是合法 UTF-8: {e}"))
}

fn download_asset(tag: &str, asset: &str, dest: &Path) -> Result<(), String> {
    let url = format!("{REPO_URL}/releases/download/{tag}/{asset}");
    let output = curl("300")
        .arg("-o")
        .arg(dest)
        .arg(&url)
        .output()
        .map_err(|e| curl_spawn_error(&e))?;

    if !output.status.success() {
        return Err(format!(
            "下載失敗（{}）",
            exit_status_message(&output.status)
        ));
    }
    Ok(())
}

fn copy_permissions(from: &Path, to: &Path) -> Result<(), String> {
    let perms = fs::metadata(from)
        .map_err(|e| format!("讀取原執行檔權限失敗: {e}"))?
        .permissions();
    fs::set_permissions(to, perms).map_err(|e| format!("設定執行權限失敗: {e}"))
}

/// 跑 `<path> --version`，輸出要含 `expected_version` 才算過。驗的是「這個
/// 檔案在這台機器上真的跑得起來」——正好涵蓋 binary 被 Gatekeeper 標記、
/// 啟動時 SIGKILL 的情況，而那是 checksum 驗不出來的（bytes 完全正確，
/// 照樣被殺）。順帶擋掉抓錯架構、下載不完整。
fn verify_binary(path: &Path, expected_version: &str) -> Result<(), String> {
    let output = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("無法執行下載的檔案: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "下載的檔案無法正常執行（{}）——可能被系統標記為不安全，或下載不完整",
            exit_status_message(&output.status)
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.contains(expected_version) {
        return Err(format!(
            "下載的檔案版本不符（預期含 {expected_version}，實際輸出：{}）",
            stdout.trim()
        ));
    }
    Ok(())
}

/// 自我更新特有的坑：rename 替換會把舊 inode unlink 掉，Linux 的
/// `/proc/self/exe` 對已 unlink 的執行檔回傳 `".../ysgit (deleted)"`。
/// 更新完不重開又更新一次，第二次會產生一個叫這個名字的檔案、真正的
/// ysgit 原封不動——所以要在 `canonicalize` 之前就擋下來，晚了
/// `canonicalize` 只會回「找不到檔案」，看不出真正原因。
///
/// `canonicalize` 本身是為了 symlink：`current_exe()` 不保證解析 symlink，
/// PATH 上放 symlink 時少了這步，`fs::rename` 會把 symlink 換成普通檔，
/// 默默拆掉部署佈局。
fn current_exe_checked() -> Result<PathBuf, String> {
    let exe = env::current_exe().map_err(|e| format!("找不到目前執行檔路徑: {e}"))?;
    let raw_name = exe.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if raw_name.contains("(deleted)") {
        return Err("目前執行檔已被替換過，請先重新啟動 ysgit 再更新".into());
    }
    let exe = fs::canonicalize(&exe).map_err(|e| format!("無法解析執行檔路徑: {e}"))?;
    if !exe.is_file() {
        return Err(format!("{} 不是一般檔案", exe.display()));
    }
    Ok(exe)
}

fn curl_spawn_error(e: &std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::NotFound {
        "找不到 curl，請先安裝".to_string()
    } else {
        format!("執行 curl 失敗: {e}")
    }
}

/// 非零 exit 的可讀說明。被 signal 殺掉時 `ExitStatus::code()` 回 `None`——
/// 用 `code()` 顯示的話，最需要辨識的那個 case（例如 Gatekeeper 用
/// SIGKILL 擋執行）反而看不出原因，所以 unix 上優先看 `signal()`。
fn exit_status_message(status: &std::process::ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return format!("被 signal {sig} 中止");
        }
    }
    match status.code() {
        Some(code) => format!("exit code {code}"),
        None => "異常終止".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHECKSUMS: &str = "\
e13cde2f17cad17bba7fd2a57c01a4fb7753186fcb4d1e6362c508dbb33c359e  ysgit_2.4.1-linux
580615d6189439f72d74b2a62274f396f90047316b74d16001da21a8f5891edc  ysgit_2.4.1-macos-arm64
ca0a1ac34d2e1138b9308820e3a53b6822552268907e87a11a1350b98f1a6ada  ysgit_2.4.1-macos-x64
28ec2342b0f1aca73bf076e8f1c6e7fc45759d4e0a1d6753ecbb5043a2f1a7c9  ysgit_2.4.1-windows.exe
";

    #[test]
    fn pick_asset_matches_each_platform() {
        for (os, arch, expected_asset) in [
            ("linux", "x86_64", "ysgit_2.4.1-linux"),
            ("macos", "aarch64", "ysgit_2.4.1-macos-arm64"),
            ("macos", "x86_64", "ysgit_2.4.1-macos-x64"),
            ("windows", "x86_64", "ysgit_2.4.1-windows.exe"),
        ] {
            let (version, asset) =
                pick_asset(CHECKSUMS, os, arch).unwrap_or_else(|| panic!("{os}-{arch} 沒挑到"));
            assert_eq!(version, "2.4.1", "{os}-{arch}");
            assert_eq!(asset, expected_asset, "{os}-{arch}");
        }
    }

    #[test]
    fn pick_asset_unsupported_platform_is_none() {
        // release.yml 沒有 linux-arm64 target
        assert!(pick_asset(CHECKSUMS, "linux", "aarch64").is_none());
    }

    #[test]
    fn pick_asset_rejects_garbage_input() {
        // captive portal 把回應換成 HTML 登入頁
        let html = "<!DOCTYPE html><html><body>login</body></html>";
        assert!(pick_asset(html, "linux", "x86_64").is_none());
    }

    #[test]
    fn asset_filename_matches_pick_asset_naming() {
        for (os, arch) in [
            ("linux", "x86_64"),
            ("macos", "aarch64"),
            ("macos", "x86_64"),
            ("windows", "x86_64"),
        ] {
            let (version, expected) = pick_asset(CHECKSUMS, os, arch).unwrap();
            assert_eq!(asset_filename(&version, os, arch).unwrap(), expected);
        }
    }

    #[test]
    fn newer_than_current_compares_semver_not_strings() {
        // 字串比較會把 "2.9.0" 排在 "2.10.0" 之後
        assert!(newer_than_current("2.10.0", "2.9.0").is_some());
    }

    #[test]
    fn newer_than_current_same_version_is_none() {
        assert!(newer_than_current("2.4.1", "2.4.1").is_none());
    }

    #[test]
    fn newer_than_current_older_version_is_none() {
        assert!(newer_than_current("2.3.0", "2.4.1").is_none());
    }
}

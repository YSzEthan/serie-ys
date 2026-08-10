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
    sync::{
        atomic::{AtomicBool, Ordering},
        OnceLock,
    },
    time::{Duration, SystemTime},
};

use clap::ValueEnum;
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::event::{AppEvent, EventController};

const REPO_URL: &str = env!("CARGO_PKG_REPOSITORY");
const CHECKSUMS_URL: &str = concat!(
    env!("CARGO_PKG_REPOSITORY"),
    "/releases/latest/download/checksum.txt"
);
const MARKER_FILE_NAME: &str = ".ysgit.update_check";

/// 自動更新檢查模式。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateMode {
    // 完全不檢查——連手動的 `U`／`-U` 都不受它影響，那是使用者當下的明確
    // 意圖，跟「程式自作主張背景檢查」是兩回事。
    //
    // 用一般註解而非 `///`：clap 的 `ValueEnum` derive 會把 doc comment
    // 當成每個變體的說明文字塞進 `--help`（長格式），但 `-h`（短格式）
    // 不會展開它——一旦某個 enum 的變體有 doc comment，兩者就不再逐字
    // 相同，會撞上 `tests/help_flag.rs` 釘住的
    // `short_and_long_help_are_byte_identical_and_exit_zero`。
    Off,
    #[default]
    Check,
    Auto,
}

/// 更新完成後是否自動重啟（TUI）／開啟新版（CLI），不再詢問。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AutoRestart {
    #[default]
    Off,
    On,
}

/// 檢查間隔的合法範圍（小時）；CLI／設定檔／精靈三處都要用同一組數字。
pub const MIN_INTERVAL_HOURS: u64 = 1;
pub const MAX_INTERVAL_HOURS: u64 = 48;
pub const DEFAULT_INTERVAL_HOURS: u64 = 6;

/// CLI 與設定檔合併後的自動更新設定，`run()`／`run_self_update` 只算一次、
/// 之後所有地方（`AppContext`、`-U` 的兩個 confirm、背景檢查 thread）都吃
/// 這個值，不各自再 `.or()` 一遍。
#[derive(Debug, Clone, Copy)]
pub struct UpdateSettings {
    pub mode: UpdateMode,
    pub interval: Duration,
    pub auto_restart: bool,
}

impl Default for UpdateSettings {
    /// 給測試 fixture 用（真正的合併入口是 `resolve()`，`run()` 一律走它）。
    /// 手寫而非 derive：`Duration` 的 derive 預設是 0 秒，套進
    /// `should_check_on_startup` 會讓每次呼叫都判定「早就該查了」，測試
    /// 用到這個預設值時看起來會像巧合通過，不是邏輯真的對。
    fn default() -> Self {
        Self {
            mode: UpdateMode::default(),
            interval: Duration::from_secs(DEFAULT_INTERVAL_HOURS * 3600),
            auto_restart: false,
        }
    }
}

/// 唯一的合併入口：CLI > 設定檔 > 內建預設。`YSGIT_NO_UPDATE_CHECK` 在這裡
/// 壓成 `mode = Off`——原本散在 `should_check_on_startup` 裡的另一個閘門，
/// 併過來後全程只剩 `mode` 一個判斷點。
///
/// 收純值而非整個 `&Args`／`&CoreConfig`：不必為了讀三個欄位就讓這個模組
/// 認得 `Args` 的私有欄位，呼叫端（`lib.rs::run()`）在自己的模組裡解構
/// 一次即可，這個函式本身也因此是純函式，好測。
pub fn resolve(
    cli_mode: Option<UpdateMode>,
    cli_interval_hours: Option<u64>,
    cli_auto_restart: Option<AutoRestart>,
    config_mode: Option<UpdateMode>,
    config_interval_hours: Option<u64>,
    config_auto_restart: Option<AutoRestart>,
) -> UpdateSettings {
    let mode = if env::var_os("YSGIT_NO_UPDATE_CHECK").is_some() {
        UpdateMode::Off
    } else {
        cli_mode.or(config_mode).unwrap_or_default()
    };
    let interval_hours = cli_interval_hours
        .or(config_interval_hours)
        .unwrap_or(DEFAULT_INTERVAL_HOURS);
    let auto_restart =
        cli_auto_restart.or(config_auto_restart).unwrap_or_default() == AutoRestart::On;
    UpdateSettings {
        mode,
        interval: Duration::from_secs(interval_hours * 3600),
        auto_restart,
    }
}

/// 檢查並視需要開啟更新提示。啟動時的自動檢查與 `U` 鍵共用這個入口，
/// 差異只在 `manual`：
/// - 自動（`manual = false`）：`mode = Off` 或節流未到期就整個不檢查；
///   已是最新或出錯（沒裝 curl、沒網路）一律靜默，不能在啟動時噴錯。
/// - 手動：繞過節流與 `mode = Off`（使用者當下的明確意圖，跟「程式自作
///   主張背景檢查」是兩回事），任何結果都要吭聲。
///
/// `mode = Auto` 時（不論手動或自動觸發）查到新版直接下載替換，不彈 y/n
/// 提示——跟 `-U` 在 `mode = Auto` 下跳過 confirm 是同一個承諾，不因為
/// 觸發來源是背景 thread 還是使用者按鍵而有兩套行為。
///
/// 兩種情況都會標記「已檢查」，手動觸發後下次啟動不會馬上又問一次。
pub fn spawn_check(ec: &EventController, manual: bool, settings: UpdateSettings) {
    if !manual && (settings.mode == UpdateMode::Off || !should_check_on_startup(settings.interval))
    {
        return;
    }
    let tx = ec.sender();
    let auto_download = settings.mode == UpdateMode::Auto;
    std::thread::spawn(move || {
        let result = check_for_update();
        mark_checked();
        match result {
            Ok(Some(tag)) if auto_download => match download_and_replace(&tag) {
                Ok(exe) => tx.send(AppEvent::UpdateInstalled { tag, exe }),
                Err(e) => tx.send(AppEvent::NotifyError(e)),
            },
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

/// 下載 `tag` 對應這台機器的 asset 並就地替換目前執行檔，回傳替換後的執行檔路徑。
///
/// 呼叫端不能事後自己再叫一次 `current_exe()`：`fs::rename` 已經把舊 inode
/// unlink 掉，Linux 上 `/proc/self/exe` 對已 unlink 的執行檔會回傳
/// `".../ysgit (deleted)"`（見 `current_exe_checked` 的註解）。這裡回傳的是
/// `current_exe_checked()` 在 rename 前就算好的路徑，避開這個坑。
pub fn download_and_replace(tag: &str) -> Result<PathBuf, String> {
    if cfg!(windows) {
        return Err("Windows 請至 GitHub Releases 頁面手動下載更新".into());
    }
    if cfg!(debug_assertions) {
        return Err("開發版本（debug build）不支援自我更新".into());
    }
    if UPDATE_INSTALLED.load(Ordering::SeqCst) {
        return Err("目前執行檔已更新過，請先重新啟動 ysgit 再更新".into());
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
    fs::rename(&tmp, &target).map_err(|e| format!("替換執行檔失敗: {e}"))?;
    UPDATE_INSTALLED.store(true, Ordering::SeqCst);
    Ok(target)
}

/// 用替換後的新執行檔取代目前 process；成功時不返回。
///
/// unix 專用：`CommandExt::exec` 直接置換 process image，保留 pid／stdio／
/// 終端機連線，比另外 spawn 一個子行程再退出乾淨——不會有「父行程先死、子行程
/// 變成 orphan 被 shell job control 用不同方式對待」的落差。`download_and_replace`
/// 開頭已經擋掉 Windows，這裡的 `#[cfg(not(unix))]` 分支純粹是讓其他平台編得動，
/// 實際執行不到。
///
/// 進入時先把 stdout flush 掉：`println!` 是 LineWriter，重導向到檔案時是
/// block-buffered，`exec` 換掉 process image 的瞬間緩衝區內容會直接消失。
///
/// `argv[0]` 預期是程式名（`Args::to_argv` 的輸出就長這樣），這裡跳過
/// 它——`exec` 本身就會帶入新 process 的 argv[0]，不需要呼叫端另外去頭。
pub fn exec_replacing_self(exe: &Path, argv: &[String]) -> Result<(), String> {
    use std::io::Write;
    let _ = std::io::stdout().flush();

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // 成功時 exec() 不返回；回傳值本身就是失敗原因，不是 Result。
        let err = Command::new(exe).args(argv.iter().skip(1)).exec();
        Err(format!("自動重新啟動失敗: {err}"))
    }
    #[cfg(not(unix))]
    {
        let _ = (exe, argv);
        Err("此平台不支援自動重新啟動".to_string())
    }
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

/// 本 process 這輩子有沒有成功替換過一次執行檔——`current_exe_checked` 的
/// `"(deleted)"` 字串偵測只在 Linux 上有效（`/proc/self/exe` 的行為），
/// macOS 的 `current_exe()`（`_NSGetExecutablePath`）不會標記已被替換，靠它
/// 擋不住「更新完不重啟、繼續用同一個 process 再按一次更新」——沒有這個旗標，
/// macOS 上會整包重新下載一次（無害但白費頻寬與 300 秒 timeout）。這個旗標
/// 是跨平台的事實：不管哪個平台，`fs::rename` 一旦成功，目前這個 process
/// 手上的執行檔內容就已經跟磁碟上的不是同一份了。
static UPDATE_INSTALLED: AtomicBool = AtomicBool::new(false);

struct UpdateGuard;

impl Drop for UpdateGuard {
    fn drop(&mut self) {
        UPDATE_RUNNING.store(false, Ordering::SeqCst);
    }
}

// ── 節流：一個 0-byte 檔，只看 mtime ──
//
// 兩層路徑：優先跟著執行檔走（`exe_dir()`），寫不進去（例如裝在
// `/usr/local/bin` 這種唯讀目錄）就退到系統暫存目錄。只保護「寫不寫得
// 進去」，不處理多使用者共用暫存目錄可能撞名——marker 內容只有 mtime，
// 撞名的後果最多是誤判節流時機，不是安全問題，犯不著為它另外做隔離。

fn primary_marker_path() -> Option<PathBuf> {
    exe_dir().map(|dir| dir.join(MARKER_FILE_NAME))
}

fn fallback_marker_path() -> PathBuf {
    env::temp_dir().join(MARKER_FILE_NAME)
}

/// 讀取用：兩個位置誰有檔就用誰，優先層先看。都沒有就當作「從沒檢查過」。
fn existing_marker_path() -> Option<PathBuf> {
    primary_marker_path()
        .filter(|p| p.exists())
        .or_else(|| Some(fallback_marker_path()).filter(|p| p.exists()))
}

/// 距上次檢查是否已經超過 `interval`——純函式，供啟動檢查與週期檢查共用
/// 判斷。`YSGIT_NO_UPDATE_CHECK` 不在這裡判斷：那個閘門已經併進 `resolve()`
/// 壓成 `mode = Off`，`spawn_check` 用 `mode` 一個判斷點就夠。
fn check_due(last_checked: SystemTime, now: SystemTime, interval: Duration) -> bool {
    now.duration_since(last_checked)
        .is_ok_and(|elapsed| elapsed >= interval)
}

fn should_check_on_startup(interval: Duration) -> bool {
    let Some(path) = existing_marker_path() else {
        return true;
    };
    let Ok(modified) = fs::metadata(&path).and_then(|m| m.modified()) else {
        return true;
    };
    check_due(modified, SystemTime::now(), interval)
}

/// 寫入用：先試優先層，寫失敗（唯讀目錄）才退到暫存目錄。
pub fn mark_checked() {
    if let Some(primary) = primary_marker_path() {
        if touch(&primary) {
            return;
        }
    }
    touch(&fallback_marker_path());
}

fn touch(path: &Path) -> bool {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    fs::File::create(path).is_ok()
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

/// 執行檔所在目錄，啟動早期算一次後永久快取。自我更新的 `fs::rename` 會讓
/// Linux 上 `current_exe()` 在同一個 process 裡回傳帶 `(deleted)` 的路徑
/// （見 `current_exe_checked` 的註解）——不快取的話，下載完成後同一個
/// process 任何再次呼叫（marker、設定檔讀寫）都會突然失敗。設定檔的位置
/// （`config::default_config_file_path`）也共用這個函式，不在那邊另外算
/// 一次 `current_exe()`。
pub(crate) fn exe_dir() -> Option<&'static Path> {
    static EXE_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    EXE_DIR
        .get_or_init(|| {
            current_exe_checked()
                .ok()
                .and_then(|p| p.parent().map(Path::to_path_buf))
        })
        .as_deref()
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

    // ── resolve()：CLI > 設定檔 > 內建預設 ──
    //
    // 不測 YSGIT_NO_UPDATE_CHECK 那條分支：`cargo test` 預設多執行緒跑同一個
    // process，`env::set_var` 會是跨測試共用的全域可變狀態，這個專案沒有
    // `serial_test` 之類的依賴能安全隔離它。那條分支是單行 if，用人工驗證
    // （README／docs 的驗證清單）覆蓋，不值得為了測它引入新依賴或測試順序
    // 依賴。

    #[test]
    fn resolve_cli_wins_over_config_and_default() {
        let settings = resolve(
            Some(UpdateMode::Auto),
            Some(2),
            Some(AutoRestart::On),
            Some(UpdateMode::Off),
            Some(20),
            Some(AutoRestart::Off),
        );
        assert_eq!(settings.mode, UpdateMode::Auto);
        assert_eq!(settings.interval, Duration::from_secs(2 * 3600));
        assert!(settings.auto_restart);
    }

    #[test]
    fn resolve_falls_back_to_config_when_cli_unset() {
        let settings = resolve(
            None,
            None,
            None,
            Some(UpdateMode::Off),
            Some(20),
            Some(AutoRestart::On),
        );
        assert_eq!(settings.mode, UpdateMode::Off);
        assert_eq!(settings.interval, Duration::from_secs(20 * 3600));
        assert!(settings.auto_restart);
    }

    #[test]
    fn resolve_falls_back_to_builtin_default_when_nothing_set() {
        let settings = resolve(None, None, None, None, None, None);
        assert_eq!(settings.mode, UpdateMode::Check);
        assert_eq!(
            settings.interval,
            Duration::from_secs(DEFAULT_INTERVAL_HOURS * 3600)
        );
        assert!(!settings.auto_restart);
    }

    // ── check_due()：邊界 ──

    #[test]
    fn check_due_exactly_at_interval_is_due() {
        let last = SystemTime::UNIX_EPOCH;
        let now = last + Duration::from_secs(3600);
        assert!(check_due(last, now, Duration::from_secs(3600)));
    }

    #[test]
    fn check_due_just_under_interval_is_not_due() {
        let last = SystemTime::UNIX_EPOCH;
        let now = last + Duration::from_secs(3599);
        assert!(!check_due(last, now, Duration::from_secs(3600)));
    }

    #[test]
    fn check_due_well_past_interval_is_due() {
        let last = SystemTime::UNIX_EPOCH;
        let now = last + Duration::from_secs(3600 * 100);
        assert!(check_due(last, now, Duration::from_secs(3600)));
    }

    #[test]
    fn check_due_clock_went_backwards_is_not_due() {
        // duration_since 在 now < last 時回 Err——不是「早就該查了」，是
        // 系統時鐘被調過去，寧可保守不查。
        let last = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let now = SystemTime::UNIX_EPOCH;
        assert!(!check_due(last, now, Duration::from_secs(3600)));
    }
}

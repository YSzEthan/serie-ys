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
use serde::Deserialize;

use crate::event::{AppEvent, EventController};

const REPO_URL: &str = env!("CARGO_PKG_REPOSITORY");
const CHECKSUMS_URL: &str = concat!(
    env!("CARGO_PKG_REPOSITORY"),
    "/releases/latest/download/checksum.txt"
);
const UPDATE_CHECK_MARKER_FILE_NAME: &str = ".ysgit.update_check";
const SEEN_VERSION_MARKER_FILE_NAME: &str = ".ysgit.seen_version";

/// release.yml 的 upload job 把 `CHANGELOG.md` 裡當次版本的區塊原封不動當
/// GitHub Release 的 body（見 `prepare_release.py::extract_changelog_section`）。
/// `prepare` job 先 commit CHANGELOG＋打 tag，`build` job 才 checkout 該 tag
/// 編譯，所以編譯進這個 binary 的 CHANGELOG.md 必然已經含當前版本的區塊——
/// 不必打 GitHub API 就能在本機重現同一份內容，離線也能顯示。
const CHANGELOG: &str = include_str!("../CHANGELOG.md");

/// 自動更新檢查模式。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Deserialize)]
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AutoRestart {
    #[default]
    Off,
    On,
}

/// 版本變了、第一次啟動時是否自動跳出該版的 release notes。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReleaseNotes {
    Off,
    #[default]
    On,
}

/// 檢查間隔的合法範圍（小時）；CLI／設定檔／精靈三處都要用同一組數字。
pub const MIN_INTERVAL_HOURS: u64 = 1;
pub const MAX_INTERVAL_HOURS: u64 = 48;
pub const DEFAULT_INTERVAL_HOURS: u64 = 6;

/// CLI 與設定檔合併後的自動更新設定，`run()`／`run_self_update` 只算一次、
/// 之後所有地方（`AppContext`、`-U` 的兩個 confirm、背景檢查 thread）都吃
/// 這個值，不各自再 `.or()` 一遍。
///
/// `release_notes` 嚴格說不是「更新檢查」的一部分（它甚至不連網），但跟
/// `auto_restart` 一樣是「更新之後的行為」，歸在同一組設定裡自洽。代價是
/// 這個欄位會跟著整個 `UpdateSettings` 被塞進 `AppContext.update` 給所有
/// view 看得到，也會傳進從不讀它的 `spawn_check`——只有 `run()` 開頭
/// `update::pending_release_notes()` 那一次讀它。
#[derive(Debug, Clone, Copy)]
pub struct UpdateSettings {
    pub mode: UpdateMode,
    pub interval: Duration,
    pub auto_restart: bool,
    pub release_notes: bool,
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
            release_notes: true,
        }
    }
}

/// `resolve()` 的一組來源（CLI 或設定檔）。四個欄位型別兩兩相同
/// （`Option<UpdateMode>`、`Option<u64>`、`Option<AutoRestart>`、
/// `Option<ReleaseNotes>`），拆開寫成位置參數時 CLI 側跟設定檔側寫反編譯器
/// 抓不到——包成同一個具名型別、呼叫端用欄位名字建構，才擋得住這種手滑。
#[derive(Debug, Clone, Copy, Default)]
pub struct UpdateOverrides {
    pub mode: Option<UpdateMode>,
    pub interval_hours: Option<u64>,
    pub auto_restart: Option<AutoRestart>,
    pub release_notes: Option<ReleaseNotes>,
}

/// 唯一的合併入口：CLI > 設定檔 > 內建預設。`YSGIT_NO_UPDATE_CHECK` 在這裡
/// 壓成 `mode = Off`——原本散在 `should_check_on_startup` 裡的另一個閘門，
/// 併過來後全程只剩 `mode` 一個判斷點。這個 env var **不影響**
/// `release_notes`：它管的是「不要背景連網」，release notes 完全讀本機
/// 內嵌的 `CHANGELOG.md`、不連網，兩者是不同的閘門——要關 release notes
/// 請用 `--release-notes off` 或設定檔。
///
/// 收純值而非整個 `&Args`／`&CoreConfig`：不必為了讀四個欄位就讓這個模組
/// 認得 `Args` 的私有欄位，呼叫端（`lib.rs::run()`）在自己的模組裡解構
/// 一次即可，這個函式本身也因此是純函式，好測。
pub fn resolve(cli: UpdateOverrides, config: UpdateOverrides) -> UpdateSettings {
    let mode = if env::var_os("YSGIT_NO_UPDATE_CHECK").is_some() {
        UpdateMode::Off
    } else {
        cli.mode.or(config.mode).unwrap_or_default()
    };
    let interval_hours = cli
        .interval_hours
        .or(config.interval_hours)
        .unwrap_or(DEFAULT_INTERVAL_HOURS);
    let auto_restart =
        cli.auto_restart.or(config.auto_restart).unwrap_or_default() == AutoRestart::On;
    let release_notes = cli
        .release_notes
        .or(config.release_notes)
        .unwrap_or_default()
        == ReleaseNotes::On;
    UpdateSettings {
        mode,
        interval: Duration::from_secs(interval_hours * 3600),
        auto_restart,
        release_notes,
    }
}

/// 檢查並視需要開啟更新提示。啟動時的自動檢查與 `U` 鍵共用這個入口，
/// 差異只在 `manual`：
/// - 自動（`manual = false`）：執行檔已被替換、`mode = Off`、或節流未到期
///   就整個不檢查；已是最新或出錯（沒裝 curl、沒網路）一律靜默，不能在
///   啟動時噴錯。
/// - 手動：繞過節流與 `mode = Off`（使用者當下的明確意圖，跟「程式自作
///   主張背景檢查」是兩回事），任何結果都要吭聲——包括執行檔已被替換。
///
/// `mode = Auto` 時（不論手動或自動觸發）查到新版直接下載替換，不彈 y/n
/// 提示——跟 `-U` 在 `mode = Auto` 下跳過 confirm 是同一個承諾，不因為
/// 觸發來源是背景 thread 還是使用者按鍵而有兩套行為。
///
/// 兩種情況都會標記「已檢查」，手動觸發後下次啟動不會馬上又問一次。
pub fn spawn_check(ec: &EventController, manual: bool, settings: UpdateSettings) {
    // 執行檔已被替換時，`mode` 與節流都不再有意義——不管 `Off` 還是還沒
    // 到期，這次檢查都問不出正確答案。手動觸發一樣要吭聲，跟下面
    // `mode = Off` 的早退是同一個哲學：使用者當下的明確意圖不能被吞掉。
    if exe_is_stale() {
        if manual {
            ec.send(AppEvent::NotifyInfo(EXE_REPLACED_MSG.to_string()));
        }
        return;
    }
    if !manual && (settings.mode == UpdateMode::Off || !should_check_now(settings.interval)) {
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
    if exe_is_stale() {
        return Err(EXE_REPLACED_MSG.into());
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

// ── 「磁碟上的執行檔已經不是我在跑的那一份」 ──
//
// 這件事有兩個來源，但它們是同一個事實，所以只有一個 predicate
// （`exe_is_stale`）：
// - 本 process 自己更新過（`UPDATE_INSTALLED`）
// - 別的 ysgit 實例更新過，或使用者手動部署過（inode 變了）
//
// 拆成兩個判斷的話，三個呼叫點就得各自記得該問哪一個。實際發生過的
// 後果：週期重新武裝只問了前者，於是另一個實例更新完之後，這個 process
// 每個 interval 打一次網路、彈一次提示、被 `download_and_replace` 擋下，
// 永遠迴圈。

/// 本 process 自己成功替換過執行檔。是 `exe_is_stale()` 的其中一個來源：
/// 非 unix（沒有 inode 可比）時是唯一來源，unix 上則是省掉一次 stat 的
/// 短路——`fs::rename` 一旦成功，這個 process 手上的執行檔內容就已經跟
/// 磁碟上的不是同一份，不必再去問檔案系統。
static UPDATE_INSTALLED: AtomicBool = AtomicBool::new(false);

/// 啟動時的執行檔身分：canonicalize 過的路徑，加上它當下的 inode。
///
/// `None` = 沒有可用快照（非 unix，或啟動時就解析不出執行檔路徑），此時
/// 守衛失效、一律視同未被替換。用 `Option<(PathBuf, u64)>` 而不是
/// `(PathBuf, Option<u64>)`：兩個獨立的失敗軸會有四種狀態而只有兩種有
/// 意義，而且路徑解析失敗時根本生不出那個 `PathBuf`。
static STARTUP_EXE: OnceLock<Option<(PathBuf, u64)>> = OnceLock::new();

/// 三個守衛共用同一句話——`download_and_replace` 的重入檢查、
/// `current_exe_checked` 的 `(deleted)` 偵測、`spawn_check` 的早退，講的
/// 都是同一個事實。
///
/// 措辭中性、不指名兇手：deploy 走的是 `rm` + `cp`（macOS security
/// metadata 那個坑），那也會換 inode，手動部署測試版時說「已由其他實例
/// 更新」是在說謊。
const EXE_REPLACED_MSG: &str = "磁碟上的 ysgit 已被替換（自我更新或手動部署），請重新啟動後再更新";

/// 啟動早期呼叫一次，把執行檔身分釘住。
///
/// 必須排在 `-U` 那條早退路徑之前（`lib.rs::run()` 裡跟
/// `config::ensure_config_file()` 相鄰），否則 `ysgit -U` 全程沒有快照，
/// 守衛靜默失效。
///
/// 需要這個明確的初始化點，是因為 `OnceLock` 是惰性的：沒有它，第一次
/// `get_or_init` 會發生在比對的當下，快照永遠等於現值，守衛一輩子不會
/// 觸發。也不能指望 `config::ensure_config_file()` 順手 force 出
/// `exe_dir()`——`$SERIE_CONFIG_FILE` 有設時它直接 return，根本沒碰。
///
/// 失敗完全靜默：這是背景事實不是使用者要求的操作，而且 `-h`／`--help`
/// 的輸出被 `tests/help_flag.rs` 釘成逐位元組相同，多印一行都會踩到。
pub fn snapshot_exe() {
    STARTUP_EXE.get_or_init(|| {
        let path = current_exe_checked().ok()?;
        let ino = exe_fingerprint(&path)?;
        Some((path, ino))
    });
}

/// 這個路徑當下的 inode。非 unix 沒有等價概念，回 `None`。
///
/// **收路徑而不自己找**：`fs::rename` 換掉執行檔後，Linux 的
/// `/proc/self/exe` 會回 `".../ysgit (deleted)"`，`current_exe_checked()`
/// 因此回 `Err`（見該函式註解）。在這裡改成現算路徑的話，正好會在「別的
/// 實例剛更新完」這個唯一需要偵測的場景拿到 `None`，`stale()` 判定未被
/// 替換，週期鏈繼續無限迴圈。而 macOS 的 `current_exe()` 不會標記已被
/// 替換，所以這個坑只在 Linux 發作，開發機上測不出來。
///
/// 傳進來的是 `snapshot_exe()` 啟動時算好的乾淨路徑，rename 之後對它
/// stat 拿到的是新檔的 inode，兩個平台行為一致。
///
/// 不快取：這個函式的全部價值就是反映「現在」磁碟上的狀態，快取等於把它
/// 變成第二個 `UPDATE_INSTALLED`。一次 `stat(2)` 在 dentry cache 裡是微秒
/// 級，而呼叫頻率是每個 interval 一次——`should_check_now()` 對 marker 檔
/// 本來就在做同一件事。
fn exe_fingerprint(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(path).ok().map(|m| m.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// 純函式，三個輸入攤開才測得到（`exe_is_stale()` 讀全域 static）。
///
/// 快照或現值任一為 `None` 就視同未被替換（fail open）：非 unix、啟動時
/// 解析失敗、執行檔被整個刪掉——這幾種都寧可讓更新走既有路徑，也不要拿
/// 一個問不出答案的判準去擋使用者。
fn stale(installed: bool, startup: Option<u64>, current: Option<u64>) -> bool {
    installed || startup.zip(current).is_some_and(|(a, b)| a != b)
}

/// 磁碟上的執行檔是不是已經不是我在跑的那一份。
///
/// `fs::rename` 換上新 binary 必然產生新 inode，所以 inode 是精確的判準。
/// inode 重用在這裡不會發生：呼叫者正在跑舊 binary，kernel 的 vnode 釘住
/// 舊 inode，unlink 之後也不會被釋放重用。
///
/// 已知的理論風險：某些 FUSE／網路掛載對同一個檔案回不穩定的 inode，那種
/// 環境會永久判定被替換、再也自我更新不了。訊息（`EXE_REPLACED_MSG`）是
/// 中性的，使用者看得懂發生什麼事。
pub fn exe_is_stale() -> bool {
    let (startup, current) = match STARTUP_EXE.get().and_then(Option::as_ref) {
        Some((path, ino)) => (Some(*ino), exe_fingerprint(path)),
        None => (None, None),
    };
    stale(UPDATE_INSTALLED.load(Ordering::SeqCst), startup, current)
}

struct UpdateGuard;

impl Drop for UpdateGuard {
    fn drop(&mut self) {
        UPDATE_RUNNING.store(false, Ordering::SeqCst);
    }
}

// ── marker 檔共用路徑邏輯 ──
//
// 兩層路徑：優先跟著執行檔走（`exe_dir()`），寫不進去（例如裝在
// `/usr/local/bin` 這種唯讀目錄）就退到系統暫存目錄。只保護「寫不寫得
// 進去」，不處理多使用者共用暫存目錄可能撞名——`.ysgit.update_check` 撞名
// 的後果最多是誤判節流時機，`.ysgit.seen_version` 撞名最多是誤判有沒有
// 看過某一版，都不是安全問題，犯不著為它另外做隔離。
//
// 兩個 marker（節流用的 `.ysgit.update_check`、記版本用的
// `.ysgit.seen_version`）共用同一套「試優先層、失敗退暫存層」的路徑邏輯，
// 差別只在檔名跟內容，所以收成一組帶 `name` 參數的函式，不重複寫兩份。

fn primary_marker_path(name: &str) -> Option<PathBuf> {
    exe_dir().map(|dir| dir.join(name))
}

fn fallback_marker_path(name: &str) -> PathBuf {
    env::temp_dir().join(name)
}

/// 讀取用：兩個位置誰有檔就用誰，優先層先看。都沒有就當作「從沒寫過」。
fn existing_marker_path(name: &str) -> Option<PathBuf> {
    primary_marker_path(name)
        .filter(|p| p.exists())
        .or_else(|| Some(fallback_marker_path(name)).filter(|p| p.exists()))
}

/// 寫入用：先試優先層，寫失敗（唯讀目錄）才退到暫存目錄。`.ysgit.update_check`
/// 只在意 mtime，內容留空；`.ysgit.seen_version` 內容是版本字串。
fn write_marker(name: &str, content: &str) {
    if let Some(primary) = primary_marker_path(name) {
        if write_marker_file(&primary, content) {
            return;
        }
    }
    write_marker_file(&fallback_marker_path(name), content);
}

fn write_marker_file(path: &Path, content: &str) -> bool {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    fs::write(path, content).is_ok()
}

// ── 節流：一個 0-byte 檔，只看 mtime ──

/// 距上次檢查是否已經超過 `interval`——純函式，供啟動檢查與週期檢查共用
/// 判斷。`YSGIT_NO_UPDATE_CHECK` 不在這裡判斷：那個閘門已經併進 `resolve()`
/// 壓成 `mode = Off`，`spawn_check` 用 `mode` 一個判斷點就夠。
fn check_due(last_checked: SystemTime, now: SystemTime, interval: Duration) -> bool {
    now.duration_since(last_checked)
        .is_ok_and(|elapsed| elapsed >= interval)
}

/// 啟動檢查與週期檢查共用同一個節流判斷——名字不叫 `on_startup` 是因為
/// `spawn_check(manual = false)` 兩種觸發時機都會走到這裡。
fn should_check_now(interval: Duration) -> bool {
    let Some(path) = existing_marker_path(UPDATE_CHECK_MARKER_FILE_NAME) else {
        return true;
    };
    let Ok(modified) = fs::metadata(&path).and_then(|m| m.modified()) else {
        return true;
    };
    check_due(modified, SystemTime::now(), interval)
}

pub fn mark_checked() {
    write_marker(UPDATE_CHECK_MARKER_FILE_NAME, "");
}

// ── Release notes：版本變了、第一次啟動時跳出當版 CHANGELOG 區塊 ──
//
// marker 兩層路徑的既有毛病在這裡症狀較重：`existing_marker_path` 讀取時
// 只要優先層存在就用它。若優先層存在但事後變成不可寫，寫入會退到暫存層、
// 讀取卻仍讀到優先層的舊版號，導致每次啟動都誤判成「沒看過新版」而重跳。
// `.ysgit.update_check` 有同一個毛病，但後果只是多查幾次更新；這裡的後果
// 是使用者每次啟動都被擋一下，嚴重度差一個量級——場景仍然罕見（需要優先
// 層先寫成功、之後才變唯讀），不特別處理，但不能跟 `.ysgit.update_check`
// 混為一談。

/// 抽出 `## [version]` 那一節（含標題行），起點與終點都 anchor 到行首。
/// 跟 `.github/scripts/prepare_release.py::extract_changelog_section` 是
/// 同一條規則的 Rust 版——這裡比 python 版更嚴謹（那邊起點沒 anchor）：
/// 不 anchor 起點的話，commit 訊息內文剛好出現 `## [1.0.0]` 這種字串會被
/// 誤切，TUI 顯示的內容就會跟 GitHub Release 實際的 body 不一致。
fn extract_release_notes(changelog: &'static str, version: &str) -> Option<&'static str> {
    let marker = format!("\n## [{version}]");
    let marker_pos = changelog.find(&marker)?;
    let start = marker_pos + 1; // 跳過 anchor 用的換行，區塊本身從 `##` 開始
    let rest = &changelog[start..];
    let end = rest.find("\n## [").unwrap_or(rest.len());
    let section = rest[..end].trim();
    (!section.is_empty()).then_some(section)
}

/// marker 檔可能被 `echo` 或編輯器補上結尾換行，不 trim 會讓已看過的版本
/// 每次啟動都被誤判成「沒看過」而重跳。
fn should_show(seen: Option<&str>, current: &str) -> bool {
    seen.map(str::trim) != Some(current)
}

fn read_seen_version() -> Option<String> {
    let path = existing_marker_path(SEEN_VERSION_MARKER_FILE_NAME)?;
    fs::read_to_string(path).ok()
}

/// `app.rs::open_release_notes()` 專用：release notes view 真的建出來、
/// 下一幀就會畫出來的當下才算「看過」。
pub fn mark_version_seen() {
    write_marker(SEEN_VERSION_MARKER_FILE_NAME, env!("CARGO_PKG_VERSION"));
}

/// 這次啟動該顯示的 release notes，沒有就 `None`。**不寫 marker**——寫入
/// 時機在 `mark_version_seen()`，故意跟這個函式分開：`lib.rs::run()` 呼叫
/// 這裡之後還有 `git::Repository::load(...)?` 這類會早退的路徑，若在這裡
/// 就寫 marker，非 git 目錄啟動會讓 marker 寫下去但畫面從沒出現過，這一版
/// 的 notes 就永遠看不到了。
///
/// debug build 不自動跳：`cargo clean` 後每次 `cargo run` 都會被擋在
/// commit list 前面，跟 `download_and_replace` 的 `cfg!(debug_assertions)`
/// 早退同一個先例。`--whats-new` 不受這個限制，隨時能手動看。
pub fn pending_release_notes(settings: UpdateSettings) -> Option<&'static str> {
    if cfg!(debug_assertions) || !settings.release_notes {
        return None;
    }
    if !should_show(read_seen_version().as_deref(), env!("CARGO_PKG_VERSION")) {
        return None;
    }
    current_release_notes()
}

/// 目前這一版的 release notes，跟版本設定、marker 都無關——`--whats-new`
/// 用它隨時手動查看，不受 `pending_release_notes()` 那些節流／開關限制。
pub fn current_release_notes() -> Option<&'static str> {
    extract_release_notes(CHANGELOG, env!("CARGO_PKG_VERSION"))
}

// ── 殘檔清掃：process 被砍掉（SIGKILL、斷電）時來不及跑到
// `download_and_replace` 結尾的 `fs::remove_file`，留下 `.{exe}.new.{pid}`。
//
// 判準只看 mtime 夠不夠舊，不比對 pid 是不是自己的：pid 會被 OS 重用，
// 「pid 跟我不同就刪」會誤殺另一個真的正在下載的 ysgit 實例手上的暫存檔
// （它才剛建立，遠比一次下載的逾時新），害它之後 `fs::rename` 撞
// ENOENT。下載逾時（`curl --max-time`）是幾百秒等級，用遠大於它的門檻
// 就足夠安全地把「早就沒有 process 在管」的殘檔篩出來。

const STALE_TMP_FILE_AGE: Duration = Duration::from_secs(24 * 3600);

/// 啟動時掃一次執行檔目錄，清掉早就沒人管的自我更新暫存檔。跟更新設定
/// （`UpdateMode`）無關——`mode = Off` 的這次啟動一樣可能有上次啟動
/// （那時還沒關）留下的殘檔要清，不能因為這次沒在檢查更新就跳過。
pub fn cleanup_stale_temp_files() {
    let Some(dir) = exe_dir() else {
        return;
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !is_stale_tmp_name(&name) {
            continue;
        }
        let is_old = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|modified| {
                now.duration_since(modified)
                    .is_ok_and(|age| age >= STALE_TMP_FILE_AGE)
            });
        if is_old {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// `download_and_replace` 產生的暫存檔名固定是 `.{原檔名}.new.{pid}`——
/// 認 `.new.` 這個中段加上結尾是純數字（pid），不比對確切的執行檔名稱：
/// 不同平台的原檔名不一樣（Windows 帶 `.exe`），沒必要在這裡重算一次。
fn is_stale_tmp_name(name: &str) -> bool {
    if !name.starts_with('.') {
        return false;
    }
    let Some((_, pid)) = name.rsplit_once(".new.") else {
        return false;
    };
    !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit())
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
        return Err(EXE_REPLACED_MSG.into());
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

    // ── stale()：磁碟上的執行檔是不是已經不是我在跑的那一份 ──

    #[test]
    fn stale_same_inode_is_not_stale() {
        assert!(!stale(false, Some(42), Some(42)));
    }

    #[test]
    fn stale_different_inode_is_stale() {
        assert!(stale(false, Some(42), Some(99)));
    }

    #[test]
    fn stale_missing_startup_is_not_stale() {
        // 沒有可用快照（非 unix／啟動時解析失敗）：問不出答案，fail open。
        assert!(!stale(false, None, Some(42)));
    }

    #[test]
    fn stale_missing_current_is_not_stale() {
        // 現值拿不到（例如執行檔被整個刪掉）：同樣 fail open。
        assert!(!stale(false, Some(42), None));
    }

    #[test]
    fn stale_installed_short_circuits_regardless_of_inode() {
        // 本 process 自己裝過，不必管 inode 比對結果。
        assert!(stale(true, Some(42), Some(42)));
        assert!(stale(true, None, None));
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
            UpdateOverrides {
                mode: Some(UpdateMode::Auto),
                interval_hours: Some(2),
                auto_restart: Some(AutoRestart::On),
                release_notes: Some(ReleaseNotes::Off),
            },
            UpdateOverrides {
                mode: Some(UpdateMode::Off),
                interval_hours: Some(20),
                auto_restart: Some(AutoRestart::Off),
                release_notes: Some(ReleaseNotes::On),
            },
        );
        assert_eq!(settings.mode, UpdateMode::Auto);
        assert_eq!(settings.interval, Duration::from_secs(2 * 3600));
        assert!(settings.auto_restart);
        assert!(!settings.release_notes);
    }

    #[test]
    fn resolve_falls_back_to_config_when_cli_unset() {
        let settings = resolve(
            UpdateOverrides::default(),
            UpdateOverrides {
                mode: Some(UpdateMode::Off),
                interval_hours: Some(20),
                auto_restart: Some(AutoRestart::On),
                release_notes: Some(ReleaseNotes::Off),
            },
        );
        assert_eq!(settings.mode, UpdateMode::Off);
        assert_eq!(settings.interval, Duration::from_secs(20 * 3600));
        assert!(settings.auto_restart);
        assert!(!settings.release_notes);
    }

    #[test]
    fn resolve_falls_back_to_builtin_default_when_nothing_set() {
        let settings = resolve(UpdateOverrides::default(), UpdateOverrides::default());
        assert_eq!(settings.mode, UpdateMode::Check);
        assert_eq!(
            settings.interval,
            Duration::from_secs(DEFAULT_INTERVAL_HOURS * 3600)
        );
        assert!(!settings.auto_restart);
        assert!(settings.release_notes);
    }

    // ── extract_release_notes()：CHANGELOG 區塊切割 ──

    const TEST_CHANGELOG: &str = "\
# Changelog

## [3.0.0](url) (2026-08-13)

### Features

* first (abc)

## [2.7.3](url) (2026-08-12)

### Refactors

* second (def)

## [2.7.2](url) (2026-08-11)

### Bug Fixes

* third (ghi)
";

    #[test]
    fn extract_release_notes_first_section() {
        let section = extract_release_notes(TEST_CHANGELOG, "3.0.0").unwrap();
        assert!(section.starts_with("## [3.0.0]"));
        assert!(section.contains("first (abc)"));
    }

    #[test]
    fn extract_release_notes_middle_section() {
        let section = extract_release_notes(TEST_CHANGELOG, "2.7.3").unwrap();
        assert!(section.starts_with("## [2.7.3]"));
        assert!(section.contains("second (def)"));
    }

    #[test]
    fn extract_release_notes_last_section() {
        let section = extract_release_notes(TEST_CHANGELOG, "2.7.2").unwrap();
        assert!(section.starts_with("## [2.7.2]"));
        assert!(section.contains("third (ghi)"));
    }

    #[test]
    fn extract_release_notes_missing_version_is_none() {
        assert!(extract_release_notes(TEST_CHANGELOG, "9.9.9").is_none());
    }

    #[test]
    fn extract_release_notes_does_not_bleed_into_next_section() {
        let section = extract_release_notes(TEST_CHANGELOG, "3.0.0").unwrap();
        assert!(!section.contains("second (def)"));
        assert!(!section.contains("## [2.7.3]"));
    }

    #[test]
    fn current_version_has_a_changelog_section() {
        // 唯一一條會抓到真問題的測試：Cargo.toml 版號跟 CHANGELOG.md 脫節
        // （忘了在 release 流程外手動改版號、或 CHANGELOG 沒同步）時直接紅燈。
        assert!(current_release_notes().is_some());
    }

    // ── should_show()：marker 記錄的版本 vs 目前版本 ──

    #[test]
    fn should_show_no_marker_is_true() {
        assert!(should_show(None, "3.0.0"));
    }

    #[test]
    fn should_show_same_version_is_false() {
        assert!(!should_show(Some("3.0.0"), "3.0.0"));
    }

    #[test]
    fn should_show_trims_trailing_newline() {
        // marker 檔可能被 `echo`（而非 `printf`）或編輯器補上結尾換行。
        assert!(!should_show(Some("3.0.0\n"), "3.0.0"));
    }

    #[test]
    fn should_show_upgraded_version_is_true() {
        assert!(should_show(Some("2.0.0"), "3.0.0"));
    }

    #[test]
    fn should_show_downgraded_version_is_true() {
        // 刻意不用 semver 比大小：rollback 後也該看得到自己實際在跑的版本
        // 說明，不是「只有升版才顯示」。
        assert!(should_show(Some("3.0.0"), "2.0.0"));
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

    // ── is_stale_tmp_name() ──

    #[test]
    fn is_stale_tmp_name_matches_unix_style_name() {
        assert!(is_stale_tmp_name(".ysgit.new.12345"));
    }

    #[test]
    fn is_stale_tmp_name_matches_windows_style_name() {
        assert!(is_stale_tmp_name(".ysgit.exe.new.12345"));
    }

    #[test]
    fn is_stale_tmp_name_rejects_missing_leading_dot() {
        assert!(!is_stale_tmp_name("ysgit.new.12345"));
    }

    #[test]
    fn is_stale_tmp_name_rejects_non_numeric_suffix() {
        assert!(!is_stale_tmp_name(".ysgit.new.abc"));
    }

    #[test]
    fn is_stale_tmp_name_rejects_empty_suffix() {
        assert!(!is_stale_tmp_name(".ysgit.new."));
    }

    #[test]
    fn is_stale_tmp_name_rejects_unrelated_dotfile() {
        assert!(!is_stale_tmp_name(".ysgit.toml"));
        assert!(!is_stale_tmp_name(".ysgit.update_check"));
    }
}

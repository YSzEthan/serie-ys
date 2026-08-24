//! 背景輪詢 git remote 是否有新內容，偵測到就 `git fetch`。**只更新
//! remote-tracking refs，本地 branch 一律不動**——不 merge、不 rebase、
//! 不 ff，使用者自己決定要不要跟上。
//!
//! 用 `AppEvent::AutoFetchPoll { last_fingerprint }` + `send_after` 自我
//! 重新武裝，不是 detached thread + `loop { sleep }`——理由跟
//! `AppEvent::PeriodicUpdateCheck` 相同，見該處註解。`last_fingerprint`
//! 是這條鏈的累加器，不是共享狀態，靠 event payload 一路往前傳。
//!
//! 偵測方式是 `git ls-remote --heads --tags <remote>` 逐一比對輸出，不打
//! GitHub API：不需要 token、無 rate limit、`upstream` 這類非 GitHub
//! remote 也一起顧到。

use std::{path::Path, process::Command, time::Duration};

use clap::ValueEnum;
use serde::Deserialize;

use crate::{
    event::{AppEvent, EventController, PendingRefreshFlag, Sender},
    git::background_command,
    process::run_with_timeout,
};

/// 是否開啟自動 fetch。預設關閉：背景連網是有感知的行為，開不開由使用者
/// 明確決定，不是裝完就自動生效。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AutoFetch {
    #[default]
    Off,
    On,
}

/// 輪詢間隔的合法範圍（秒）；CLI／設定檔／精靈三處都要用同一組數字。
/// 下限 30 秒：`LS_REMOTE_TIMEOUT` 是 10 秒，重新武裝又是等 worker 跑完
/// 才排下一次（見模組文件），兩者天生序列化，下限不必抓得比逾時預算更
/// 寬，只要不至於變成熱迴圈就好。
pub const MIN_INTERVAL_SECS: u64 = 30;
pub const MAX_INTERVAL_SECS: u64 = 3600;
pub const DEFAULT_INTERVAL_SECS: u64 = 600;

const LS_REMOTE_TIMEOUT: Duration = Duration::from_secs(10);
/// `fetch --all --prune` 的逾時預算，跟 `app.rs::GIT_FETCH_TIMEOUT` 同一個
/// 數字、但分開宣告——兩者觸發來源不同（使用者按 `f` vs 背景輪詢），沒有
/// 理由綁死成同一個常數，各自表達各自呼叫端的取捨。
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

/// CLI 與設定檔合併後的自動 fetch 設定。
#[derive(Debug, Clone, Copy)]
pub struct AutoFetchSettings {
    pub mode: AutoFetch,
    pub interval: Duration,
}

impl Default for AutoFetchSettings {
    fn default() -> Self {
        Self {
            mode: AutoFetch::default(),
            interval: Duration::from_secs(DEFAULT_INTERVAL_SECS),
        }
    }
}

/// `resolve()` 的一組來源（CLI 或設定檔），形狀照抄
/// `update::UpdateOverrides`——兩個欄位型別都是 `Option`，拆開寫成位置
/// 參數時 CLI 側跟設定檔側寫反編譯器抓不到。
#[derive(Debug, Clone, Copy, Default)]
pub struct AutoFetchOverrides {
    pub mode: Option<AutoFetch>,
    pub interval_secs: Option<u64>,
}

/// 唯一的合併入口：CLI > 設定檔 > 內建預設。刻意不看
/// `YSGIT_NO_UPDATE_CHECK`——那個變數的名字與語意都是「不要檢查更新」，
/// auto-fetch 是完全不同的功能，且本來就預設關閉、需要使用者明確開啟，
/// 讓一個叫 `NO_UPDATE_CHECK` 的變數偷偷否決使用者更晚、更明確的
/// `auto_fetch = on` 才是 break userspace。
pub fn resolve(cli: AutoFetchOverrides, config: AutoFetchOverrides) -> AutoFetchSettings {
    let mode = cli.mode.or(config.mode).unwrap_or_default();
    let interval_secs = cli
        .interval_secs
        .or(config.interval_secs)
        .unwrap_or(DEFAULT_INTERVAL_SECS);
    AutoFetchSettings {
        mode,
        interval: Duration::from_secs(interval_secs),
    }
}

/// 收到 `AppEvent::AutoFetchPoll` 時呼叫。背景比對指紋、視情況 fetch，
/// 最後才重新武裝下一輪——重新武裝必須在 worker 尾端，不能在收到事件的
/// 當下就做：`interval` 最小值是 30 秒，`LS_REMOTE_TIMEOUT` 是 10 秒，
/// 提早重新武裝的話，多個 remote 逾時疊加起來可能讓兩輪 poll 疊在一起、
/// 變成兩個 `git fetch` 同時打同一個 repo。移到尾端，這條鏈天生序列化。
///
/// `pending_message.is_some()`（有 blocking overlay）期間要不要真的跑
/// 這一輪，是 `App` 才看得到的狀態，由呼叫端（`app.rs`）決定要不要呼叫
/// 這個函式；跳過的話呼叫端要自己把同一顆指紋原封不動 `send_after`
/// 傳下去，不能連重新武裝一起跳過，否則這條鏈永久死掉。
pub fn spawn_poll(ec: &EventController, repo: &Path, last_fingerprint: String, interval: Duration) {
    let tx = ec.sender();
    let pending_refresh = ec.pending_refresh_flag();
    let repo = repo.to_path_buf();
    std::thread::spawn(move || {
        let next_fingerprint = poll_once(&tx, &pending_refresh, &repo, last_fingerprint);
        tx.send_after(
            AppEvent::AutoFetchPoll {
                last_fingerprint: next_fingerprint,
            },
            interval,
        );
    });
}

/// 回傳值是「下一輪該拿去比對的指紋」——不是每次都等於新算出來的那個：
/// ls-remote 失敗、或 fetch 本身失敗，都要保留舊指紋，讓下一輪重新判定
/// 同一批差異，而不是默默放棄這次同步。
fn poll_once(
    tx: &Sender,
    pending_refresh: &PendingRefreshFlag,
    repo: &Path,
    last_fingerprint: String,
) -> String {
    let Some(current) = fingerprint(repo) else {
        // 任一 remote 的 ls-remote 失敗，整輪作廢：指紋是全有全無的東西，
        // 拼裝的話網路抖一次會變成兩次假 fetch（先看起來變了，恢復後又
        // 變回去，各觸發一次判定）。
        return last_fingerprint;
    };
    if current == last_fingerprint {
        return last_fingerprint;
    }

    // 有變化：跟 `spawn_git_task` 一樣先標記 token，讓 watcher 在 debounce
    // 視窗內偵測到的 fs 事件被吞掉，不會跟這裡自己送的
    // `AutoFetchCompleted` 疊加成兩次 refresh。bare repo 沒有 worktree、
    // watcher 根本沒啟動，所以下面成功時必須自己觸發 refresh，不能指望
    // watcher 順便觸發。
    pending_refresh.mark();

    let cmd = background_command(repo, ["fetch", "--all", "--prune"]);
    let output = run_with_timeout(cmd, None, FETCH_TIMEOUT);
    match output {
        Ok(o) if o.status.success() => {
            tx.send(AppEvent::AutoFetchCompleted);
            current
        }
        // fetch 失敗（含逾時）：本地 remote-tracking refs 沒有真的更新，
        // 保留舊指紋讓下一輪重試同一批差異；跟 ls-remote 失敗一樣靜默，
        // 背景功能不該每個 interval 就彈一次錯誤。
        _ => last_fingerprint,
    }
}

/// 對每個 remote 跑 `git ls-remote --heads --tags`，把「remote 名字 +
/// 輸出」逐一串成一個字串當指紋（帶名字才不會讓兩個 remote 的輸出互換
/// 位置時看起來一樣）。任一 remote 失敗就回傳 `None`，語意見
/// `poll_once`。
///
/// `--heads --tags`：GitHub 的 `refs/pull/*/merge` 是每次 base branch
/// 前進就整批重算的——任何人 push 一次 main，所有開著的 PR 的 merge ref
/// SHA 全變。不濾掉的話，活躍 repo 上等於「別人每 push 一次就多打一次
/// 沒必要的 fetch，還彈一個沒東西變的通知」。
///
/// 這條規則只求「盡量」對齊 `git fetch` 實際更新的集合，不是「必須」：
/// `--all` 會跳過設了 `remote.<name>.skipDefaultUpdate` 的 remote，但
/// `git remote` 會列它；預設 `fetch` 只跟隨可從抓下來的歷史到達的 tag，
/// 不是全部 remote tag，`--tags` 涵蓋過頭；remote 有自訂 refspec（例如
/// 只抓 `refs/heads/main`）時 `--heads` 同樣涵蓋過頭。三者都只造成偶發
/// 的多餘 fetch 與一個「沒東西變」的通知，修掉的成本遠高於症狀。
///
/// 沒有 remote（`git remote` 空輸出）時回傳空字串：跟種子指紋
/// （`lib.rs::run()` 排的第一顆也是空字串）相等，天然是靜默 no-op，不需要
/// 額外的「有沒有 remote」閘門，也讓使用者在 session 中途用內嵌命令列
/// `git remote add` 之後自動開始運作。
fn fingerprint(repo: &Path) -> Option<String> {
    let remotes = list_remotes(repo)?;
    let mut entries = Vec::with_capacity(remotes.len());
    for remote in remotes {
        let cmd = background_command(repo, ["ls-remote", "--heads", "--tags", &remote]);
        let output = run_with_timeout(cmd, None, LS_REMOTE_TIMEOUT)
            .ok()
            .filter(|o| o.status.success())?;
        entries.push((remote, String::from_utf8_lossy(&output.stdout).into_owned()));
    }
    Some(build_fingerprint(&entries))
}

/// 序列化格式（含「為何要帶 remote 名字」的理由）見 `fingerprint`；抽成
/// 純函式方便獨立測試，不必真的連網路。
fn build_fingerprint(entries: &[(String, String)]) -> String {
    entries
        .iter()
        .map(|(remote, output)| format!("{remote}\n{output}\n"))
        .collect()
}

/// `git remote` 是純本地讀取（讀 `.git/config`），不打網路，不需要
/// `background_command` 的憑證／逾時硬化，直接跑就好。
fn list_remotes(repo: &Path) -> Option<Vec<String>> {
    let output = Command::new("git")
        .arg("remote")
        .current_dir(repo)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Some(text.lines().map(String::from).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_prefers_cli_over_config_over_default() {
        let settings = resolve(
            AutoFetchOverrides {
                mode: Some(AutoFetch::On),
                interval_secs: Some(45),
            },
            AutoFetchOverrides {
                mode: Some(AutoFetch::Off),
                interval_secs: Some(120),
            },
        );
        assert_eq!(settings.mode, AutoFetch::On);
        assert_eq!(settings.interval, Duration::from_secs(45));
    }

    #[test]
    fn resolve_falls_back_to_config_when_cli_is_unset() {
        let settings = resolve(
            AutoFetchOverrides::default(),
            AutoFetchOverrides {
                mode: Some(AutoFetch::On),
                interval_secs: Some(120),
            },
        );
        assert_eq!(settings.mode, AutoFetch::On);
        assert_eq!(settings.interval, Duration::from_secs(120));
    }

    #[test]
    fn resolve_defaults_to_off_and_default_interval() {
        let settings = resolve(AutoFetchOverrides::default(), AutoFetchOverrides::default());
        assert_eq!(settings.mode, AutoFetch::Off);
        assert_eq!(
            settings.interval,
            Duration::from_secs(DEFAULT_INTERVAL_SECS)
        );
    }

    /// 沒有 remote：`git remote` 空輸出 → 指紋是空字串，跟種子指紋
    /// （`String::new()`）相等，`poll_once` 判定成「沒有變化」而不是
    /// 誤判成失敗或誤觸發 fetch。
    #[test]
    fn no_remotes_yields_empty_fingerprint_matching_the_seed() {
        let dir = tempfile::tempdir().unwrap();
        Command::new("git")
            .arg("init")
            .arg("-q")
            .current_dir(dir.path())
            .status()
            .unwrap();

        assert_eq!(fingerprint(dir.path()), Some(String::new()));
    }

    /// 指紋帶 remote 名字：同一份 ls-remote 輸出掛在不同 remote 名字下，
    /// 指紋必須不同——不能只看內容，名字本身也是指紋的一部分。用純函式測，
    /// 不必真的連網路。
    #[test]
    fn build_fingerprint_distinguishes_same_content_under_different_remote_names() {
        let content = "abc123\trefs/heads/main\n".to_string();
        let as_origin = build_fingerprint(&[("origin".to_string(), content.clone())]);
        let as_upstream = build_fingerprint(&[("upstream".to_string(), content)]);
        assert_ne!(as_origin, as_upstream);
    }

    /// 兩個 remote 的內容互換位置，指紋要跟著變：`("a", X), ("b", Y)` 不能
    /// 跟 `("a", Y), ("b", X)` 算成同一個指紋。
    #[test]
    fn build_fingerprint_distinguishes_swapped_content_between_remotes() {
        let x = "SHA-X\trefs/heads/main\n".to_string();
        let y = "SHA-Y\trefs/heads/main\n".to_string();
        let before =
            build_fingerprint(&[("a".to_string(), x.clone()), ("b".to_string(), y.clone())]);
        let after = build_fingerprint(&[("a".to_string(), y), ("b".to_string(), x)]);
        assert_ne!(before, after);
    }
}

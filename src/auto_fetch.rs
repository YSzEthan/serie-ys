//! 背景輪詢 git remote 是否有新內容，偵測到就 `git fetch`；判定要真的
//! fetch 時比照手動 fetch 蓋 pending overlay。**只更新 remote-tracking
//! refs，本地 branch 一律不動**——不 merge、不 rebase、不 ff，使用者自己
//! 決定要不要跟上。
//!
//! 用 `AppEvent::AutoFetchPoll` + `send_after` 自我重新武裝，不是
//! detached thread + `loop { sleep }`——理由跟 `AppEvent::PeriodicUpdateCheck`
//! 相同，見該處註解。比對用的基準指紋活在 `AutoFetchClock`（共享狀態，
//! `arm`/`begin_resync`/`baseline`/`set_baseline` 是僅有的寫入／讀取點），
//! 不透過 event payload 傳遞：手動 fetch 成功後會觸發重算
//! （`spawn_resync`），一旦有兩個寫入者，它就不再是「單一鏈的累加器」。
//!
//! 分兩階段：`spawn_poll`（背景 thread）只做 `ls-remote`，把結果（`None`
//! = 失敗）回報給主執行緒；比對「跟現在的基準有沒有差異」與真的要不要蓋
//! overlay 跑 `git fetch`（`spawn_due_fetch`）都在主執行緒用
//! `AutoFetchClock::baseline()`／`can_interrupt()` 決定——這兩個判斷只有
//! 在主執行緒才問得準，背景 thread 拿不到。
//!
//! 偵測方式是 `git ls-remote --heads --tags <remote>` 逐一比對輸出，不打
//! GitHub API：不需要 token、無 rate limit、`upstream` 這類非 GitHub
//! remote 也一起顧到。

use std::{
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

use clap::ValueEnum;
use serde::Deserialize;

use crate::{
    event::{AppEvent, AutoFetchClock, EventController, Sender},
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

/// 收到 `AppEvent::AutoFetchPoll`、且 deadline 已到、且沒有其他 pending
/// overlay 時呼叫。背景只做 `ls-remote`（`None` = 失敗），**不在這裡比對
/// 基準、不碰 `git fetch`**——比對留給主執行緒用「現在」的基準做，那才是
/// 唯一的判準（見 `AppEvent::AutoFetchPolled` 的 doc comment）。
pub fn spawn_poll(ec: &EventController, repo: &Path) {
    let tx = ec.sender();
    let repo = repo.to_path_buf();
    std::thread::spawn(move || {
        let fingerprint = fingerprint(&repo);
        tx.send(AppEvent::AutoFetchPolled { fingerprint });
    });
}

/// 排下一輪 poll，只推 deadline——基準指紋不動，只有 `spawn_resync`／
/// `spawn_due_fetch` 成功時才改它，見 `AutoFetchClock::arm` 為什麼刻意
/// 不在這裡碰 baseline。
///
/// 排程與倒數必須成對——漏掉 `arm` 的話倒數會停在 `00:00` 直到下一輪真的
/// 跑完。凡是用 `send_after` 排下一輪的都走這裡。
///
/// 收兩個把手而不是 `&EventController`：後者不是 `Send`，進不了 worker
/// thread 的 `'static` closure。
pub fn rearm(tx: Sender, clock: AutoFetchClock, interval: Duration) {
    clock.arm(Instant::now() + interval);
    tx.send_after(AppEvent::AutoFetchPoll, interval);
}

/// `AppEvent::AutoFetchPolled` 判定有差異、且 `can_interrupt()` 為真時
/// 呼叫。跟手動 `fetch_all()` 一樣蓋 pending overlay、跑
/// `git fetch --all --prune`；成功才把 `candidate` 寫回基準，失敗保留
/// 原基準讓下一輪重新判定同一批差異。
///
/// `mark_pending_refresh()` 在這裡呼叫（不是像舊版那樣在背景 thread 裡）
/// ——這是新架構下第一次真正確定「這輪要跑 `git fetch`」的時間點，且已
/// 經在主執行緒，直接用 `&EventController` 即可，不需要可攜把手。
///
/// 已知角落案例：使用者可以在這個 overlay 顯示時按 Cancel 把它藏起來
/// （背景操作繼續跑，見 `app.rs::handle_key`），這期間若又手動按 `f`，
/// 會有兩個 `git fetch --all` 同時打同一個 repo。這不是這次改動引入的
/// 新問題——`checkout`／merge PR 等既有功能共用同一個「Cancel 不中止
/// 背景工作」設計，git 對併發 fetch 通常安全，最壞是其中一個因鎖檔失敗
/// 靜默 `NotifyError`，不特別處理。
pub fn spawn_due_fetch(ec: &EventController, repo: &Path, candidate: String, interval: Duration) {
    let tx = ec.sender();
    let clock = ec.auto_fetch_clock();
    let repo = repo.to_path_buf();

    ec.mark_pending_refresh();
    tx.send(AppEvent::ShowPendingOverlay {
        message: "Auto-fetching...".into(),
    });

    std::thread::spawn(move || {
        let cmd = background_command(&repo, ["fetch", "--all", "--prune"]);
        let succeeded =
            run_with_timeout(cmd, None, FETCH_TIMEOUT).is_ok_and(|o| o.status.success());
        tx.send(AppEvent::HidePendingOverlay);
        if succeeded {
            clock.set_baseline(Some(candidate));
            tx.send(AppEvent::AutoFetchCompleted);
        }
        // 失敗（含逾時）：基準維持原值不動，下一輪重新判定同一批差異；
        // 跟 `ls-remote` 失敗一樣靜默，背景功能不該每個 interval 就彈一次
        // 錯誤。
        rearm(tx, clock, interval);
    });
}

/// 手動 `fetch_all()` 成功後呼叫（見 `app.rs::spawn_git_task` 的
/// `on_success` 掛鉤），也是啟動種子（`lib.rs::run()`）用來建立初始基準的
/// 唯一入口。同步（在呼叫當下，不等網路回來）把倒數重置成滿一個
/// interval、基準清成 `None`——`AutoFetchClock::begin_resync` 在同一個
/// 臨界區內完成這兩件事，任何在這之後才被處理的舊 `AutoFetchPolled` 都會
/// 看到 `None` 或重算後的新值，不會拿過期基準判斷出錯誤的「有差異」而蓋出
/// 重複的 overlay。
///
/// 背景重算指紋失敗（`ls-remote` 打不通）就把舊基準原封不動放回去——
/// 靜默放棄，讓原本的排程（deadline 已經被這次呼叫重置過）繼續運作，不
/// 重試。
pub fn spawn_resync(ec: &EventController, repo: &Path, interval: Duration) {
    let clock = ec.auto_fetch_clock();
    let previous_baseline = clock.begin_resync(Instant::now() + interval);
    let repo = repo.to_path_buf();
    std::thread::spawn(move || {
        clock.set_baseline(fingerprint(&repo).or(previous_baseline));
    });
}

/// 對每個 remote 跑 `git ls-remote --heads --tags`，把「remote 名字 +
/// 輸出」逐一串成一個字串當指紋（帶名字才不會讓兩個 remote 的輸出互換
/// 位置時看起來一樣）。任一 remote 失敗就回傳 `None`——呼叫端一律把
/// `None` 當「這輪問不出答案」處理，不當作真的「沒有變化」。
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
/// 沒有 remote（`git remote` 空輸出）時回傳空字串，不是 `None`——這是一個
/// 真實、穩定的指紋值（只要沒有 remote，永遠算出同一個空字串），不需要
/// 額外的「有沒有 remote」閘門，也讓使用者在 session 中途用內嵌命令列
/// `git remote add` 之後自動開始運作：下一輪算出非空指紋，自然跟基準
/// 「有差異」。
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

    /// 沒有 remote：`git remote` 空輸出 → 指紋是穩定的空字串，不是
    /// `None`——不會被誤判成 `ls-remote` 失敗，也不會每一輪都判定「有
    /// 差異」。
    #[test]
    fn no_remotes_yields_a_stable_empty_fingerprint() {
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

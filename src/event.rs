use std::{
    ffi::OsStr,
    fmt::{self, Debug, Formatter},
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use ratatui::crossterm::event::KeyEvent;
use rustc_hash::FxHashSet;
use serde::{
    de::{self, Deserializer, Visitor},
    Deserialize,
};

use crate::view::RefreshViewContext;

/// 驅動 UI 動畫（跑馬燈等）的 tick 事件間隔。
pub const TICK_INTERVAL: Duration = Duration::from_millis(100);

/// watchdog 的輪詢間隔。要夠短，SIGTERM 才不會等太久才生效。
const WATCHDOG_INTERVAL: Duration = Duration::from_millis(200);

/// 心跳停多久就判定 event thread 卡死。event thread 正常每 `TICK_INTERVAL`
/// （100ms）至少跳一次，這裡留了 20 倍餘裕。
const WATCHDOG_STALL_TIMEOUT: Duration = Duration::from_secs(2);

/// 送出 Quit 之後留給正常退出路徑的寬限時間，逾時就自己收尾。
const WATCHDOG_GRACE: Duration = Duration::from_millis(300);

#[derive(Debug)]
pub enum AppEvent {
    Key(KeyEvent),
    Resize(usize, usize),
    Tick,
    Quit,
    OpenDetail,
    CloseDetail,
    OpenUserCommand(usize),
    CloseUserCommand,
    OpenRefs,
    CloseRefs,
    OpenCreateTag,
    CloseCreateTag,
    OpenDeleteTag,
    CloseDeleteTag,
    OpenDeleteRef {
        ref_name: String,
        ref_type: crate::git::RefType,
    },
    CloseDeleteRef,
    OpenHelp,
    CloseHelp,
    OpenGitHub,
    CloseGitHub,
    OpenShell,
    CloseShell,
    /// 背景 thread 跑完 shell 指令了——不帶 payload，純喚醒；結果本身走
    /// `ShellView` 自己持有的 `mpsc::Receiver`。見 `view/shell.rs` 頂端註解。
    ShellOutputReady,
    /// 更新後（或全新安裝）第一次啟動時該顯示的 release notes——見
    /// `update::pending_release_notes`。`body` 是編譯期常數
    /// （`include_str!` 讀 `CHANGELOG.md`），不必轉成 `String`。
    OpenReleaseNotes {
        body: &'static str,
    },
    CloseReleaseNotes,
    RefreshGitHub {
        state: crate::github::StateFilter,
    },
    GitHubDataLoaded {
        data: crate::github::GitHubData,
        warnings: Vec<String>,
    },
    LoadMoreGitHub {
        kind: crate::github::GhItemKind,
        generation: u64,
    },
    GitHubMoreIssuesLoaded {
        items: Vec<crate::github::GhIssue>,
        next_cursor: Option<String>,
        generation: u64,
    },
    GitHubMorePrsLoaded {
        items: Vec<crate::github::GhPullRequest>,
        next_cursor: Option<String>,
        generation: u64,
    },
    LoadGitHubTimeline {
        number: u64,
        kind: crate::github::GhItemKind,
        after: Option<String>,
    },
    GitHubTimelineLoaded {
        number: u64,
        kind: crate::github::GhItemKind,
        /// 原樣帶回請求時的 `after`——回應自己知道是不是第一頁，不用靠
        /// entry 上的旗標猜，避免刷新跟「載入更多」交錯時猜錯。
        after: Option<String>,
        page: crate::github::GhTimelinePage,
    },
    GitHubTimelineFailed {
        number: u64,
        kind: crate::github::GhItemKind,
        error: String,
    },
    GitHubFlash {
        message: String,
        is_error: bool,
    },
    GitHubLoadFailed {
        error: String,
    },
    BatchToggleCheckboxes {
        number: u64,
        kind: crate::github::GhItemKind,
        checkbox_indices: Vec<usize>,
    },
    CheckboxToggled {
        number: u64,
        kind: crate::github::GhItemKind,
        new_body: String,
    },
    SelectNewerCommit,
    SelectOlderCommit,
    SelectParentCommit,
    SelectChildCommit,
    CopyToClipboard {
        name: String,
        value: String,
    },
    OpenUrl(String),
    Refresh(RefreshViewContext),
    ClearStatusLine,
    UpdateStatusInput(String, Option<u16>, Option<String>),
    NotifyInfo(String),
    NotifySuccess(String),
    NotifyWarn(String),
    NotifyError(String),
    ShowPendingOverlay {
        message: String,
    },
    HidePendingOverlay,
    FetchAll,
    CheckoutCommit {
        target: String,
    },
    AutoRefresh,
    OpenRefPicker {
        options: Vec<String>,
        kind: RefCopyKind,
    },
    OpenCheckoutPicker {
        options: Vec<String>,
        kind: CheckoutPickKind,
    },
    OpenRelatedPicker {
        items: Vec<RelatedItem>,
    },
    GitHubJumpToIssue {
        number: u64,
    },
    OpenDeleteBranch {
        names: Vec<String>,
    },
    OpenDeleteBranchPicker {
        options: Vec<String>,
        total: usize,
    },
    OpenDeleteBranchConfirm {
        name: String,
    },
    OpenMergePrMethodPicker {
        number: u64,
        head_ref: String,
        state: crate::github::StateFilter,
    },
    OpenToggleStatePrompt {
        number: u64,
        kind: crate::github::GhItemKind,
        action: crate::github::StateAction,
        filter_state: crate::github::StateFilter,
    },
    OpenTogglePrDraftPrompt {
        number: u64,
        action: crate::github::PrDraftAction,
        filter_state: crate::github::StateFilter,
    },
    /// 觸發一次更新檢查——啟動時的自動檢查直接 spawn thread，不經過這裡；
    /// 這個只給 `U` 鍵／`UserEvent::CheckUpdate` 用，走 `global_app_event`
    /// 才能受 `is_browsing_view`／`is_input_mode` 守衛，不會在文字輸入框裡
    /// 把打的 `U` 吃掉。
    CheckUpdate,
    /// 持續運作期間每隔一個 `interval` 再檢查一次——`send_after` 自我重新
    /// 武裝，不是 detached thread + `loop { sleep }`（那會有 wall clock vs
    /// 單調時鐘的落差、多實例流量放大、且要真的等一個 interval 才驗得到）。
    /// 沒有鍵盤／`UserEvent` 對應：只有 `lib.rs::run()` 排第一次、
    /// 處理它的地方排下一次，使用者無法直接觸發。
    PeriodicUpdateCheck,
    /// 有新版才送——見 `update::check_for_update`。
    OpenUpdatePrompt {
        tag: String,
    },
    /// 開始下載＋替換執行檔。兩個觸發來源：使用者在更新提示按下確認，或
    /// `mode = Auto` 查到新版直接送（跳過詢問）。理由與守衛見
    /// `update::spawn_check` 和處理端 `app.rs::spawn_update_download`。
    UpdateRequested {
        tag: String,
    },
    /// 下載＋替換執行檔完成——帶替換後的路徑（供離開前 exec 用）與版本號
    /// （供重啟提示顯示 `Updated to {tag}. Restart now?`）。
    UpdateInstalled {
        tag: String,
        exe: PathBuf,
    },
    /// 使用者在重啟提示按下確認。
    RestartRequested {
        exe: PathBuf,
    },
    /// draft 切換成功後就地更新列表，補上 RefreshGitHub 完成前的空窗。
    PrDraftToggled {
        number: u64,
        is_draft: bool,
    },
    /// 使用者在狀態列 prompt 裡按下確認、請求執行——跟上面 `PrDraftToggled`
    /// 這類「已完成」的結果事件不同時態，命名故意用 `Requested` 而非
    /// `Confirmed` 拉開差異。
    DeleteBranchRequested {
        name: String,
        force: bool,
    },
    MergePrRequested {
        number: u64,
        state: crate::github::StateFilter,
        method: crate::github::MergeMethod,
        delete_branch: bool,
    },
    ToggleItemStateRequested {
        number: u64,
        kind: crate::github::GhItemKind,
        action: crate::github::StateAction,
        filter_state: crate::github::StateFilter,
    },
    TogglePrDraftRequested {
        number: u64,
        action: crate::github::PrDraftAction,
        filter_state: crate::github::StateFilter,
    },
    /// 持續運作期間每隔一個 interval 再檢查一次遠端有沒有變化——跟
    /// `PeriodicUpdateCheck` 同一種鏈：`send_after` 自我重新武裝，不是
    /// detached thread + `loop { sleep }`。
    ///
    /// `last_fingerprint` 是這條鏈的累加器，不是共享狀態：種子是空字串
    /// （`lib.rs::run()` 只排這一次），之後每一輪都由
    /// `auto_fetch::spawn_poll` 的背景 thread 算出下一顆、在自己收尾時
    /// `send_after` 傳下去——重新武裝必須在 worker 尾端而不是這個事件剛
    /// 收到時，否則 `interval` 太短、單一 remote 逾時預算又不小時，會讓
    /// 兩輪 poll 疊在一起。
    AutoFetchPoll {
        last_fingerprint: String,
    },
    /// `auto_fetch` 背景 thread 偵測到遠端有變化、`git fetch` 成功了——
    /// 沒有變化或任何一步失敗都不送事件（見 `auto_fetch` 模組文件），
    /// 所以這個事件本身就代表「該讓使用者知道」，不必帶 payload：顯示的
    /// 文案固定，見 `app.rs` 處理端。顯示與否仍要過守衛：使用者正在
    /// picker／輸入框裡的時候不搶 status line。
    AutoFetchCompleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelatedGroup {
    Parent,
    Sub,
    Linked,
}

impl RelatedGroup {
    pub fn label(self) -> &'static str {
        match self {
            RelatedGroup::Parent => "Parent",
            RelatedGroup::Sub => "Sub",
            RelatedGroup::Linked => "Linked",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RelatedItem {
    pub number: u64,
    pub state: String,
    pub group: RelatedGroup,
}

#[derive(Debug, Clone, Copy)]
pub enum RefCopyKind {
    Local,
    Remote,
    Tag,
}

impl RefCopyKind {
    pub fn copy_label(self) -> &'static str {
        match self {
            RefCopyKind::Local => "Branch Name",
            RefCopyKind::Remote => "Remote Branch Name",
            RefCopyKind::Tag => "Tag Name",
        }
    }

    pub fn picker_prompt(self) -> &'static str {
        match self {
            RefCopyKind::Local => "Pick branch: ",
            RefCopyKind::Remote => "Pick remote branch: ",
            RefCopyKind::Tag => "Pick tag: ",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckoutPickKind {
    Branch,
    Tag,
}

impl CheckoutPickKind {
    pub fn picker_prompt(self) -> &'static str {
        match self {
            CheckoutPickKind::Branch => "Checkout branch: ",
            CheckoutPickKind::Tag => "Checkout tag: ",
        }
    }
}

#[derive(Clone)]
pub struct Sender {
    tx: mpsc::Sender<AppEvent>,
}

impl Sender {
    pub fn send(&self, event: AppEvent) {
        let _ = self.tx.send(event);
    }

    /// 在背景 thread 上，延遲一段時間後送出事件。
    pub fn send_after(&self, event: AppEvent, delay: std::time::Duration) {
        let tx = self.clone();
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            tx.send(event);
        });
    }
}

impl Debug for Sender {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "Sender")
    }
}

/// `EventController::mark_pending_refresh` 的可攜版——`EventController` 不是
/// `Clone`（`handle`／`term_signal` 這些欄位綁著整個 process 的生命週期），
/// 塞不進 `'static` thread closure。跟 `Sender` 是同一種角色：只暴露
/// `EventController` 內部狀態的一個輕量把手，讓背景 thread（目前只有
/// `auto_fetch` 的 poll worker）能在真的要 fetch 之前標記 token，不需要
/// 整個 `EventController`。
#[derive(Clone)]
pub struct PendingRefreshFlag(Arc<AtomicBool>);

impl PendingRefreshFlag {
    /// 語意與呼叫時機見 `EventController::mark_pending_refresh`。
    pub fn mark(&self) {
        self.0.store(true, Ordering::Release);
    }
}

/// 下一輪 auto-fetch 的預定時間，狀態列的倒數讀它。
///
/// 跟 `PendingRefreshFlag` 同一種角色（`EventController` 內部狀態的輕量
/// 把手），但存在這裡還有第二個、更硬的理由：`App` 每次 `AppEvent::Refresh`
/// 都會被 `lib.rs::run()` 整個重建，watcher 一有動靜就發生。deadline 若存在
/// `App` 欄位，每次重建就歸零，倒數會消失最長達一整個 interval。
/// `EventController` 建在那個迴圈外面，跨重建存活。
/// `Default` = 沒 arm 過，`remaining()` 恆為 `None`，語意等同「沒開
/// auto-fetch」——不是危險狀態，所以對正式程式碼開放也無妨。
#[derive(Debug, Clone, Default)]
pub struct AutoFetchClock(Arc<Mutex<Option<Instant>>>);

impl AutoFetchClock {
    /// 排下一輪 poll 的同時呼叫——兩件事必須成對，見 `auto_fetch::rearm`。
    pub fn arm(&self, at: Instant) {
        *self.0.lock().unwrap() = Some(at);
    }

    /// `None` = 沒開 auto-fetch，或還沒排過第一輪。
    ///
    /// 已過期回 `Some(ZERO)`——`saturating_duration_since` 本來就是這個
    /// 語意，不必手寫 `if at > now` 去製造特殊情況。過期期間正是 worker
    /// 在跑 ls-remote／fetch 的時候，`00:00` 就是「正在抓」。
    pub fn remaining(&self) -> Option<Duration> {
        // 臨界區只做一次讀取，持鎖期間不呼叫任何東西——理由同
        // `EventController` 對 mutex 中毒的註解。
        let at = *self.0.lock().unwrap();
        at.map(|at| at.saturating_duration_since(Instant::now()))
    }
}

#[cfg(test)]
impl Sender {
    pub(crate) fn channel_for_test() -> (Self, mpsc::Receiver<AppEvent>) {
        let (tx, rx) = mpsc::channel();
        (Sender { tx }, rx)
    }
}

pub struct Receiver {
    rx: mpsc::Receiver<AppEvent>,
}

impl Receiver {
    fn recv(&self) -> AppEvent {
        self.rx.recv().unwrap_or(AppEvent::Quit)
    }
}

impl Debug for Receiver {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "Receiver")
    }
}

#[derive(Debug)]
pub struct EventController {
    tx: Sender,
    rx: Receiver,
    stop: Arc<AtomicBool>,
    handle: Arc<Mutex<Option<thread::JoinHandle<()>>>>,
    /// 「已有 refresh 在路上」的一次性 token——`mark_pending_refresh` 設它、
    /// `start_git_watcher` 的背景 thread 用 `swap(false, ...)` 消費，見該處
    /// 註解。無條件建好（不是 `Option`）：沒有 watcher 時這個 flag 只是沒人
    /// 讀，不是需要特判的錯誤狀態。
    pending_refresh: Arc<AtomicBool>,
    /// 下一輪 auto-fetch 的預定時間，狀態列倒數用。無條件建好：沒開
    /// auto-fetch 時只是沒人 `arm`，`remaining()` 恆為 `None`，不是需要
    /// 特判的狀態。見 `AutoFetchClock`。
    ///
    /// 直接存 newtype 而不是裸 `Arc`（`pending_refresh` 那樣）——
    /// `PendingRefreshFlag` 不是 `Clone`，只能存裸的再包；`AutoFetchClock`
    /// 是，沒有同一個限制。
    auto_fetch_clock: AutoFetchClock,
    term_signal: Arc<AtomicBool>,
    heartbeat: Arc<AtomicU64>,
}

/// 請主 thread 正常收工，`WATCHDOG_GRACE` 內沒收工就結束程序。
///
/// event thread 還活著時它自己也會送 Quit，重複送無妨；重點是卡死時也有人送。
///
/// `restore_terminal` 必須是「終端現在歸 serie 所有」。終端已經消失時還原會失敗，
/// 無所謂；SIGTERM 那條路徑則非還原不可，否則留下一個 raw mode 的爛攤子。**但
/// suspend 期間不行** —— 那時 `suspend()` 已經把終端還原並交給外部程式（編輯器
/// 等），再送一次 `LeaveAlternateScreen` / `disable_raw_mode` 是拆掉還在跑的子行程
/// 的終端狀態，然後 `exit(0)` 把它變成孤兒。
fn force_quit(tx: &Sender, restore_terminal: bool) -> ! {
    tx.send(AppEvent::Quit);
    thread::sleep(WATCHDOG_GRACE);
    if restore_terminal {
        let _ = ratatui::crossterm::execute!(
            std::io::stdout(),
            ratatui::crossterm::event::DisableMouseCapture,
            ratatui::crossterm::terminal::LeaveAlternateScreen,
        );
        let _ = ratatui::crossterm::terminal::disable_raw_mode();
    }
    std::process::exit(0);
}

impl EventController {
    pub fn init() -> Self {
        let (tx, rx) = mpsc::channel();
        let tx = Sender { tx };
        let rx = Receiver { rx };

        let term_signal = Arc::new(AtomicBool::new(false));
        #[cfg(unix)]
        {
            let _ =
                signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&term_signal));
            let _ =
                signal_hook::flag::register(signal_hook::consts::SIGHUP, Arc::clone(&term_signal));
        }

        let controller = EventController {
            tx: tx.clone(),
            rx,
            stop: Arc::new(AtomicBool::new(false)),
            handle: Arc::new(Mutex::new(None)),
            pending_refresh: Arc::new(AtomicBool::new(false)),
            auto_fetch_clock: AutoFetchClock::default(),
            term_signal,
            heartbeat: Arc::new(AtomicU64::new(0)),
        };
        controller.start();
        controller.start_watchdog();

        controller
    }

    /// event thread 卡死時的唯一出路。
    ///
    /// crossterm 的 mio event source 在 stdin EOF 時會卡在內層 read 迴圈裡
    /// （`read` 回 `Ok(0)` 既不 break 也不檢查 timeout，見 crossterm 0.29 的
    /// `event/source/unix/mio.rs`），`poll()` 永遠不返回。這表示 event thread
    /// 沒有任何自救機會：`stop` / `term_signal` 都在 loop 頂端檢查，而控制流
    /// 根本回不到那裡。terminal 被關掉後程式就這樣吃滿一顆核心，SIGTERM 也
    /// 只是設了個沒人讀的 flag。
    ///
    /// 所以改由一個獨立 thread 從外面看心跳。它同時涵蓋 event thread panic
    /// （panic 後心跳一樣停住，主 thread 原本會永久 park 在 `recv` 上）。
    ///
    /// 監看範圍只涵蓋 event thread 的讀取迴圈。主 thread 的 tty **寫入**一律不在內
    /// —— 寫入可以在終端還活著時阻塞（Ctrl-S/XOFF、pty buffer 滿），正常運行期間也
    /// 沒有人在看，所以 `suspend()` 的終端還原同樣不受監看。那是刻意的一致性，
    /// 不是漏網。
    fn start_watchdog(&self) {
        let heartbeat = self.heartbeat.clone();
        let stop = self.stop.clone();
        let term_signal = self.term_signal.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let mut last = heartbeat.load(Ordering::Relaxed);
            let mut last_change = Instant::now();
            loop {
                thread::sleep(WATCHDOG_INTERVAL);

                if term_signal.load(Ordering::Acquire) {
                    // 這道檢查刻意在 stop 閘門之前，所以 suspend 期間也會走到 ——
                    // 而那時終端歸外部程式所有，不能還原。
                    force_quit(&tx, !stop.load(Ordering::Acquire));
                }

                // suspend 期間 event thread 本來就該停著，不是卡死 —— 心跳來自
                // event thread，分不出「卡死」與「使用者 suspend 去編輯了半小時」。
                // 盲區有限：`term_signal` 檢查刻意放在這道閘門之前，而終端被關掉時
                // 前景 process group 收到的正是 SIGHUP，所以 suspend 期間終端死掉
                // 照樣一個 `WATCHDOG_INTERVAL` 內收工。
                let now = heartbeat.load(Ordering::Relaxed);
                if now != last || stop.load(Ordering::Acquire) {
                    (last, last_change) = (now, Instant::now());
                    continue;
                }

                if last_change.elapsed() >= WATCHDOG_STALL_TIMEOUT {
                    // 上面的閘門已經濾掉 stop 為 true 的情況，終端確定歸 serie。
                    force_quit(&tx, true);
                }
            }
        });
    }

    pub fn start(&self) {
        self.stop.store(false, Ordering::Release);
        let stop = self.stop.clone();
        let tx = self.tx.clone();
        let term_signal = self.term_signal.clone();
        let heartbeat = self.heartbeat.clone();
        let handle = thread::spawn(move || {
            let tick_interval = TICK_INTERVAL;
            let mut last_tick = Instant::now();
            loop {
                // 心跳：外部的 watchdog 靠它判斷這個 thread 是否還活著。
                heartbeat.fetch_add(1, Ordering::Relaxed);
                if stop.load(Ordering::Acquire) {
                    break;
                }
                if term_signal.load(Ordering::Acquire) {
                    tx.send(AppEvent::Quit);
                    break;
                }
                match ratatui::crossterm::event::poll(tick_interval) {
                    Ok(true) => match ratatui::crossterm::event::read() {
                        Ok(e) => match e {
                            ratatui::crossterm::event::Event::Key(key) => {
                                tx.send(AppEvent::Key(key));
                            }
                            ratatui::crossterm::event::Event::Resize(w, h) => {
                                tx.send(AppEvent::Resize(w as usize, h as usize));
                            }
                            _ => {}
                        },
                        Err(e) => {
                            panic!("Failed to read event: {e}");
                        }
                    },
                    Ok(false) => {}
                    Err(e) => {
                        panic!("Failed to poll event: {e}");
                    }
                }
                if last_tick.elapsed() >= tick_interval {
                    tx.send(AppEvent::Tick);
                    last_tick = Instant::now();
                }
            }
        });
        *self.handle.lock().unwrap() = Some(handle);
    }

    pub fn resume(&self) {
        ratatui::crossterm::execute!(
            std::io::stdout(),
            ratatui::crossterm::terminal::EnterAlternateScreen
        )
        .unwrap();
        ratatui::crossterm::terminal::enable_raw_mode().unwrap();

        // 提前清掉 stop，讓 watchdog 在 drain 期間就恢復監看：drain 走的是主 thread
        // 上的 crossterm poll/read，會踩到同一個 EOF 自旋。此刻沒有 event thread 在
        // 跳心跳，但 watchdog 在 stop 為 true 時每輪都刷新計時，離
        // `WATCHDOG_STALL_TIMEOUT` 還有近一整個週期，而 drain + start 是微秒等級。
        // 位置必須在上面兩個終端還原之後：主 thread 的 tty 寫入不納入監看
        // （見 `start_watchdog`）。
        self.stop.store(false, Ordering::Release);
        self.drain_crossterm_event();
        self.start();
    }

    pub fn suspend(&self) {
        self.stop();

        ratatui::crossterm::terminal::disable_raw_mode().unwrap();
        ratatui::crossterm::execute!(
            std::io::stdout(),
            ratatui::crossterm::terminal::LeaveAlternateScreen
        )
        .unwrap();
    }

    fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.lock().unwrap().take() {
            // 不 unwrap：event thread 在 poll/read 出錯時會 panic，unwrap 會把它轉成
            // 主 thread panic，整個程式就因為 event thread 打了個嗝而死。更糟的是
            // 這裡還握著 `self.handle` 的 mutex guard —— 在 guard 存活期間 panic 會
            // 毒化那把鎖，之後每次 `lock().unwrap()` 都跟著炸。忽略之後 suspend 照樣
            // 走完，`resume()` 的 `start()` 會生一條乾淨的新 thread。
            let _ = handle.join();
        }
    }

    fn drain_crossterm_event(&self) {
        while let Ok(true) = ratatui::crossterm::event::poll(std::time::Duration::from_millis(0)) {
            let _ = ratatui::crossterm::event::read();
        }
    }

    pub fn sender(&self) -> Sender {
        self.tx.clone()
    }

    pub fn send(&self, event: AppEvent) {
        self.tx.send(event);
    }

    pub fn recv(&self) -> AppEvent {
        self.rx.recv()
    }

    pub fn start_git_watcher(&self, repo_root: &Path) {
        start_git_watcher(self.tx.clone(), self.pending_refresh.clone(), repo_root);
    }

    /// 標記「已有 refresh 在路上」，讓 watcher 短期內偵測到的後續 fs 事件
    /// 被 debounce 吃掉，避免主動 refresh 後 watcher 重複觸發 slow-path。
    ///
    /// 一次性 token：watcher 端消費掉就清掉（見 `claim_send_slot`），不需要任何
    /// 呼叫端負責清除。就算標記後的操作本身失敗（只送 `NotifyError`、不送
    /// `AutoRefresh`），watcher 也不會因此永久卡住——最壞情況是多吞一次
    /// 無關的 fs 事件。
    pub fn mark_pending_refresh(&self) {
        self.pending_refresh_flag().mark();
    }

    /// `mark_pending_refresh` 的可攜版，供背景 thread 使用——見
    /// `PendingRefreshFlag` 文件。
    pub fn pending_refresh_flag(&self) -> PendingRefreshFlag {
        PendingRefreshFlag(self.pending_refresh.clone())
    }

    /// 狀態列倒數與 auto-fetch worker 共用的 deadline 把手，見
    /// `AutoFetchClock` 文件。
    pub fn auto_fetch_clock(&self) -> AutoFetchClock {
        self.auto_fetch_clock.clone()
    }
}

/// 節流視窗內、或 `pending` token 被設過，都不送 `AutoRefresh`——後者是
/// 背景 git 操作（`spawn_git_task`）主動觸發的 refresh 順便產生的 fs 事件，
/// 不必疊加一次。抽出來獨立測：watcher thread 本身只做管線接線，這個判斷
/// 才是真正需要單元測試覆蓋的邏輯。
///
/// 注意這不是 predicate——呼叫一次就會消費 `pending` token、推進
/// `last_sent`，語意跟著改變，不能被安全地重複呼叫來「先問再做」。
///
/// `pending` 用 `swap(false, ...)` 消費，讀了就清掉——不像單向閂鎖那樣需要
/// 第三方負責清除，沒有人清得掉的話，一次背景操作失敗就會把 watcher 永久
/// 卡住。
fn claim_send_slot(
    pending: &AtomicBool,
    now: Instant,
    last_sent: &mut Instant,
    throttle: Duration,
) -> bool {
    if now.duration_since(*last_sent) < throttle {
        return false;
    }
    // 過了節流視窗就重設時鐘；吞掉的這批也算「已送」，讓視窗蓋住背景操作
    // 觸發的 fs 事件尾巴，不會緊接著又被下一批事件重新判定成「該送」。
    *last_sent = now;
    !pending.swap(false, Ordering::AcqRel)
}

fn start_git_watcher(tx: Sender, pending: Arc<AtomicBool>, repo_root: &Path) {
    use notify_debouncer_mini::new_debouncer;

    let repo_root = repo_root.to_path_buf();
    let git_dir = repo_root
        .join(".git")
        .canonicalize()
        .unwrap_or_else(|_| repo_root.join(".git"));

    let mut ignored = read_gitignore_name_hints(&repo_root.join(".gitignore"));
    // .gitignore 已涵蓋使用者專案噪音；這裡只兜底 macOS 系統檔案。
    for name in [".DS_Store", ".AppleDouble", ".Spotlight-V100", ".Trashes"] {
        ignored.insert(name.to_string());
    }

    thread::spawn(move || {
        let (debounce_tx, debounce_rx) = std::sync::mpsc::channel();

        let mut debouncer = match new_debouncer(Duration::from_millis(500), debounce_tx) {
            Ok(d) => d,
            Err(_) => return,
        };

        if debouncer
            .watcher()
            .watch(&repo_root, notify::RecursiveMode::Recursive)
            .is_err()
        {
            return;
        }

        // 節流間隔：避免大量 fs 事件觸發 Repository::load 重跑（本身可能 200-500ms）。
        let throttle = Duration::from_secs(1);
        let mut last_sent = Instant::now()
            .checked_sub(throttle)
            .unwrap_or_else(Instant::now);
        loop {
            match debounce_rx.recv() {
                Ok(Ok(events)) => {
                    let has_relevant = events
                        .iter()
                        .any(|e| is_relevant_event(e, &git_dir, &repo_root, &ignored));
                    if !has_relevant {
                        continue;
                    }
                    let now = Instant::now();
                    if claim_send_slot(&pending, now, &mut last_sent, throttle) {
                        tx.send(AppEvent::AutoRefresh);
                    }
                }
                Ok(Err(_)) => {}
                Err(_) => break,
            }
        }
    });
}

/// 先走快速路徑：在任何 syscall 之前，先對原始 event path 做便宜的字串檢查。
/// 只有在需要跟 `git_dir` 比較時才 canonicalize
/// （在 macOS 上，worktree／submodule 的 `git_dir` 可能是 symlink）。
fn is_relevant_event(
    e: &notify_debouncer_mini::DebouncedEvent,
    git_dir: &Path,
    repo_root: &Path,
    ignored: &FxHashSet<String>,
) -> bool {
    use notify_debouncer_mini::DebouncedEventKind;

    if e.kind != DebouncedEventKind::Any {
        return false;
    }
    if e.path.extension() == Some(OsStr::new("lock")) {
        return false;
    }
    // macOS 在 HFS+ 上用 tar/cp 產生的 AppleDouble 檔案（._foo）。
    if e.path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("._"))
    {
        return false;
    }
    if path_has_ignored_component(&e.path, repo_root, ignored) {
        return false;
    }
    // 只對留下來的路徑做 canonicalize，讓跟 git_dir 的比較在 macOS 的
    // symlink（例如 /tmp → /private/tmp）之下也能穩定。
    let path = e.path.canonicalize().unwrap_or_else(|_| e.path.clone());
    if path.starts_with(git_dir) {
        return true;
    }
    // canonicalize 之後路徑可能改變，要重新檢查是否含被忽略的路徑片段。
    !path_has_ignored_component(&path, repo_root, ignored)
}

/// 從 .gitignore 收集純目錄／檔案名稱。Glob 模式、路徑、
/// 否定條目一律故意跳過 —— 這只是雜訊過濾的提示，
/// 不是完整的 gitignore 解析器。最終正確性交給 `git status` 判斷。
fn read_gitignore_name_hints(path: &Path) -> FxHashSet<String> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return FxHashSet::default();
    };
    content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            if line.starts_with('!')
                || line.contains(['*', '?', '['])
                || line.trim_end_matches('/').contains('/')
            {
                return None;
            }
            Some(line.trim_end_matches('/').to_string())
        })
        .collect()
}

fn path_has_ignored_component(path: &Path, repo_root: &Path, ignored: &FxHashSet<String>) -> bool {
    let rel = path.strip_prefix(repo_root).unwrap_or(path);
    rel.components().any(|c| match c {
        Component::Normal(name) => name.to_str().is_some_and(|n| ignored.contains(n)),
        _ => false,
    })
}

// 由使用者按鍵輸入觸發的事件
//
// `PartialOrd`／`Ord` 是給 wizard 的 keybind 編輯器排序用（`BTreeMap<UserEvent, _>`
// 記錄本次 session 改過哪些 action）——derive 出來的順序就是這裡的宣告順序，
// 跟 `assets/default-keybind.toml` 的檔案順序一致，不需要另外定義排序。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UserEvent {
    ForceQuit,
    Quit,
    HelpToggle,
    Cancel,
    Close,
    NavigateUp,
    NavigateDown,
    NavigateRight,
    NavigateLeft,
    SelectUp,
    SelectDown,
    GoToTop,
    GoToBottom,
    GoToParent,
    GoToChild,
    GoToHead,
    ScrollUp,
    ScrollDown,
    PageUp,
    PageDown,
    HalfPageUp,
    HalfPageDown,
    GoToNext,
    GoToPrevious,
    Confirm,
    RefList,
    Search,
    Filter,
    UserCommand(usize),
    IgnoreCaseToggle,
    FuzzyToggle,
    Refresh,
    ShortCopy,
    FullCopy,
    BranchCopy,
    FullBranchCopy,
    TagCopy,
    CreateTag,
    DeleteTag,
    DeleteRef,
    RemoteRefsToggle,
    GitHubToggle,
    ShellToggle,
    TaskListToggle,
    DetailPaneToggle,
    Fetch,
    Checkout,
    MergePr,
    ToggleIssueState,
    TogglePrDraft,
    ToggleCommitLog,
    CheckUpdate,
    Unknown,
}

impl UserEvent {
    /// 這個事件在 config 檔中的名稱，與 `Deserialize` 接受的字串互為反向。
    /// 窮盡 match：新增事件時這裡不編譯，就不會漏掉。
    /// `Unknown` 沒有對應名稱（它是無綁定按鍵的內部訊號）。
    pub fn config_name(self) -> Option<String> {
        let name = match self {
            UserEvent::ForceQuit => "force_quit",
            UserEvent::Quit => "quit",
            UserEvent::HelpToggle => "help_toggle",
            UserEvent::Cancel => "cancel",
            UserEvent::Close => "close",
            UserEvent::NavigateUp => "navigate_up",
            UserEvent::NavigateDown => "navigate_down",
            UserEvent::NavigateRight => "navigate_right",
            UserEvent::NavigateLeft => "navigate_left",
            UserEvent::SelectUp => "select_up",
            UserEvent::SelectDown => "select_down",
            UserEvent::GoToTop => "go_to_top",
            UserEvent::GoToBottom => "go_to_bottom",
            UserEvent::GoToParent => "go_to_parent",
            UserEvent::GoToChild => "go_to_child",
            UserEvent::GoToHead => "go_to_head",
            UserEvent::ScrollUp => "scroll_up",
            UserEvent::ScrollDown => "scroll_down",
            UserEvent::PageUp => "page_up",
            UserEvent::PageDown => "page_down",
            UserEvent::HalfPageUp => "half_page_up",
            UserEvent::HalfPageDown => "half_page_down",
            UserEvent::GoToNext => "go_to_next",
            UserEvent::GoToPrevious => "go_to_previous",
            UserEvent::Confirm => "confirm",
            UserEvent::RefList => "ref_list",
            UserEvent::Search => "search",
            UserEvent::Filter => "filter",
            UserEvent::UserCommand(n) => return Some(format!("user_command_{n}")),
            UserEvent::IgnoreCaseToggle => "ignore_case_toggle",
            UserEvent::FuzzyToggle => "fuzzy_toggle",
            UserEvent::Refresh => "refresh",
            UserEvent::ShortCopy => "short_copy",
            UserEvent::FullCopy => "full_copy",
            UserEvent::BranchCopy => "branch_copy",
            UserEvent::FullBranchCopy => "full_branch_copy",
            UserEvent::TagCopy => "tag_copy",
            UserEvent::CreateTag => "create_tag",
            UserEvent::DeleteTag => "delete_tag",
            UserEvent::DeleteRef => "delete_ref",
            UserEvent::RemoteRefsToggle => "remote_refs_toggle",
            UserEvent::GitHubToggle => "github_toggle",
            UserEvent::ShellToggle => "shell_toggle",
            UserEvent::TaskListToggle => "task_list_toggle",
            UserEvent::DetailPaneToggle => "detail_pane_toggle",
            UserEvent::Fetch => "fetch",
            UserEvent::Checkout => "checkout",
            UserEvent::MergePr => "merge_pr",
            UserEvent::ToggleIssueState => "toggle_issue_state",
            UserEvent::TogglePrDraft => "toggle_pr_draft",
            UserEvent::ToggleCommitLog => "toggle_commit_log",
            UserEvent::CheckUpdate => "check_update",
            UserEvent::Unknown => return None,
        };
        Some(name.to_string())
    }

    /// wizard 的 keybind 編輯器用的中文說明——跟 `src/view/help.rs` 的
    /// `BindingSpec` 不是同一份：後者的粒度是 (view, event)，同一個 event
    /// 在不同畫面說明不同（`NavigateLeft` 在 help 是「關閉說明」、在 list
    /// 是「向左移動」），這裡要的是跟畫面無關、獨立成立的單一描述。窮盡
    /// match：新增事件時這裡不編譯，就不會漏掉。`UserCommand(n)` 沒有固定
    /// 文字可寫（名稱要查設定檔），交給呼叫端另外組。
    pub fn description(self) -> Option<&'static str> {
        let s: &'static str = match self {
            UserEvent::ForceQuit => "強制離開",
            UserEvent::Quit => "離開（按兩下）",
            UserEvent::HelpToggle => "開啟／關閉說明",
            UserEvent::Cancel => "取消",
            UserEvent::Close => "關閉",
            UserEvent::NavigateUp => "向上移動",
            UserEvent::NavigateDown => "向下移動",
            UserEvent::NavigateRight => "向右移動／顯示詳情",
            UserEvent::NavigateLeft => "向左移動／關閉詳情",
            UserEvent::SelectUp => "選取範圍向上擴展",
            UserEvent::SelectDown => "選取範圍向下擴展",
            UserEvent::GoToTop => "跳到頂端",
            UserEvent::GoToBottom => "跳到底端",
            UserEvent::GoToParent => "選擇 parent commit",
            UserEvent::GoToChild => "選擇 child commit",
            UserEvent::GoToHead => "回到 HEAD",
            UserEvent::ScrollUp => "向上捲動",
            UserEvent::ScrollDown => "向下捲動",
            UserEvent::PageUp => "上一頁",
            UserEvent::PageDown => "下一頁",
            UserEvent::HalfPageUp => "上半頁",
            UserEvent::HalfPageDown => "下半頁",
            UserEvent::GoToNext => "下一個符合項",
            UserEvent::GoToPrevious => "上一個符合項",
            UserEvent::Confirm => "確認",
            UserEvent::RefList => "開啟 refs 清單",
            UserEvent::Search => "開始搜尋",
            UserEvent::Filter => "開始過濾",
            UserEvent::UserCommand(_) => return None,
            UserEvent::IgnoreCaseToggle => "切換大小寫忽略",
            UserEvent::FuzzyToggle => "切換模糊比對",
            UserEvent::Refresh => "重新整理",
            UserEvent::ShortCopy => "複製 commit short hash",
            UserEvent::FullCopy => "複製 commit subject",
            UserEvent::BranchCopy => "複製 branch 名稱（優先 local）",
            UserEvent::FullBranchCopy => "複製 remote branch 名稱",
            UserEvent::TagCopy => "複製 tag 名稱",
            UserEvent::CreateTag => "在 commit 上建立 tag",
            UserEvent::DeleteTag => "刪除 commit 上的 tag",
            UserEvent::DeleteRef => "刪除 commit 上的 local branch",
            UserEvent::RemoteRefsToggle => "切換 remote refs",
            UserEvent::GitHubToggle => "開啟 GitHub issues/PRs",
            UserEvent::ShellToggle => "開啟命令列",
            UserEvent::TaskListToggle => "切換 task 清單",
            UserEvent::DetailPaneToggle => "切換詳情區塊",
            UserEvent::Fetch => "fetch 所有 remote",
            UserEvent::Checkout => "checkout 選取的 commit/ref",
            UserEvent::MergePr => "合併 PR",
            UserEvent::ToggleIssueState => "切換 issue 開關狀態",
            UserEvent::TogglePrDraft => "切換 PR draft 狀態",
            UserEvent::ToggleCommitLog => "切換 commit log 顯示",
            UserEvent::CheckUpdate => "檢查更新",
            UserEvent::Unknown => return None,
        };
        Some(s)
    }
}

impl<'de> Deserialize<'de> for UserEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct UserEventVisitor;

        impl<'de> Visitor<'de> for UserEventVisitor {
            type Value = UserEvent;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a string representing a user event")
            }

            fn visit_str<E>(self, value: &str) -> Result<UserEvent, E>
            where
                E: de::Error,
            {
                if value.starts_with("user_command_") {
                    if let Some(num) = parse_user_command_number(value) {
                        Ok(UserEvent::UserCommand(num))
                    } else {
                        let msg = format!("Invalid user_command_n format: {value}",);
                        Err(de::Error::custom(msg))
                    }
                } else {
                    match value {
                        "force_quit" => Ok(UserEvent::ForceQuit),
                        "quit" => Ok(UserEvent::Quit),
                        "help_toggle" => Ok(UserEvent::HelpToggle),
                        "cancel" => Ok(UserEvent::Cancel),
                        "close" => Ok(UserEvent::Close),
                        "navigate_up" => Ok(UserEvent::NavigateUp),
                        "navigate_down" => Ok(UserEvent::NavigateDown),
                        "navigate_right" => Ok(UserEvent::NavigateRight),
                        "navigate_left" => Ok(UserEvent::NavigateLeft),
                        "select_up" => Ok(UserEvent::SelectUp),
                        "select_down" => Ok(UserEvent::SelectDown),
                        "go_to_top" => Ok(UserEvent::GoToTop),
                        "go_to_bottom" => Ok(UserEvent::GoToBottom),
                        "go_to_parent" => Ok(UserEvent::GoToParent),
                        "go_to_child" => Ok(UserEvent::GoToChild),
                        "go_to_head" => Ok(UserEvent::GoToHead),
                        "scroll_up" => Ok(UserEvent::ScrollUp),
                        "scroll_down" => Ok(UserEvent::ScrollDown),
                        "page_up" => Ok(UserEvent::PageUp),
                        "page_down" => Ok(UserEvent::PageDown),
                        "half_page_up" => Ok(UserEvent::HalfPageUp),
                        "half_page_down" => Ok(UserEvent::HalfPageDown),
                        "go_to_next" => Ok(UserEvent::GoToNext),
                        "go_to_previous" => Ok(UserEvent::GoToPrevious),
                        "confirm" => Ok(UserEvent::Confirm),
                        "ref_list" | "ref_list_toggle" => Ok(UserEvent::RefList),
                        "search" => Ok(UserEvent::Search),
                        "filter" => Ok(UserEvent::Filter),
                        "ignore_case_toggle" => Ok(UserEvent::IgnoreCaseToggle),
                        "fuzzy_toggle" => Ok(UserEvent::FuzzyToggle),
                        "refresh" => Ok(UserEvent::Refresh),
                        "short_copy" => Ok(UserEvent::ShortCopy),
                        "full_copy" => Ok(UserEvent::FullCopy),
                        "branch_copy" => Ok(UserEvent::BranchCopy),
                        "full_branch_copy" => Ok(UserEvent::FullBranchCopy),
                        "tag_copy" => Ok(UserEvent::TagCopy),
                        "create_tag" => Ok(UserEvent::CreateTag),
                        "delete_tag" => Ok(UserEvent::DeleteTag),
                        "delete_ref" => Ok(UserEvent::DeleteRef),
                        "remote_refs_toggle" => Ok(UserEvent::RemoteRefsToggle),
                        "github_toggle" => Ok(UserEvent::GitHubToggle),
                        "shell_toggle" => Ok(UserEvent::ShellToggle),
                        "task_list_toggle" => Ok(UserEvent::TaskListToggle),
                        "detail_pane_toggle" => Ok(UserEvent::DetailPaneToggle),
                        "fetch" => Ok(UserEvent::Fetch),
                        "checkout" => Ok(UserEvent::Checkout),
                        "merge_pr" => Ok(UserEvent::MergePr),
                        "toggle_issue_state" => Ok(UserEvent::ToggleIssueState),
                        "toggle_pr_draft" => Ok(UserEvent::TogglePrDraft),
                        "toggle_commit_log" => Ok(UserEvent::ToggleCommitLog),
                        "check_update" => Ok(UserEvent::CheckUpdate),
                        _ => {
                            let msg = format!("Unknown user event: {value}");
                            Err(de::Error::custom(msg))
                        }
                    }
                }
            }
        }

        deserializer.deserialize_str(UserEventVisitor)
    }
}

fn parse_user_command_number(s: &str) -> Option<usize> {
    if let Some(num_str) = s.strip_prefix("user_command_") {
        if let Ok(n) = num_str.parse::<usize>() {
            return Some(n);
        }
    }
    if let Some(num_str) = s.strip_prefix("user_command_view_toggle_") {
        if let Ok(n) = num_str.parse::<usize>() {
            return Some(n);
        }
    }
    None
}

impl UserEvent {
    pub fn is_countable(&self) -> bool {
        matches!(
            self,
            UserEvent::NavigateUp
                | UserEvent::NavigateDown
                | UserEvent::ScrollUp
                | UserEvent::ScrollDown
                | UserEvent::GoToParent
                | UserEvent::GoToChild
                | UserEvent::PageUp
                | UserEvent::PageDown
                | UserEvent::HalfPageUp
                | UserEvent::HalfPageDown
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserEventWithCount {
    pub event: UserEvent,
    pub count: usize,
}

impl UserEventWithCount {
    pub fn new(event: UserEvent, count: usize) -> Self {
        Self {
            event,
            count: if count == 0 { 1 } else { count },
        }
    }

    pub fn from_event(event: UserEvent) -> Self {
        Self::new(event, 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// watchdog 的容忍值不能掉到心跳週期附近，否則正常運行會被誤判成卡死 ——
    /// 而且是在沒有 panic 訊息的情況下被自己 `process::exit(0)`。`TICK_INTERVAL`
    /// 是 `pub const`，調它的人不會經過 `start_watchdog`。
    #[test]
    fn watchdog_stall_timeout_leaves_headroom_over_tick_interval() {
        assert!(
            WATCHDOG_STALL_TIMEOUT >= TICK_INTERVAL * 4,
            "心跳週期 {TICK_INTERVAL:?} 對 stall 門檻 {WATCHDOG_STALL_TIMEOUT:?} 來說太長"
        );
        assert!(WATCHDOG_INTERVAL < WATCHDOG_STALL_TIMEOUT);
    }

    // ── claim_send_slot() ──

    const THROTTLE: Duration = Duration::from_secs(1);

    /// 節流視窗過了、沒有 pending token：正常送出，且更新 `last_sent`。
    #[test]
    fn claim_send_slot_sends_when_not_throttled_and_no_pending_token() {
        let pending = AtomicBool::new(false);
        let mut last_sent = Instant::now() - THROTTLE * 2;
        let now = Instant::now();

        assert!(claim_send_slot(&pending, now, &mut last_sent, THROTTLE));
        assert_eq!(last_sent, now);
    }

    /// 節流視窗內：不送，且不觸碰 `pending`（沒有消費掉任何人設的 token）。
    #[test]
    fn claim_send_slot_swallows_within_throttle_window_without_consuming_token() {
        let pending = AtomicBool::new(true);
        let mut last_sent = Instant::now();
        let now = last_sent + THROTTLE / 2;

        assert!(!claim_send_slot(&pending, now, &mut last_sent, THROTTLE));
        assert!(
            pending.load(Ordering::Acquire),
            "節流視窗內不該消費 token，留給視窗外的下一次判斷"
        );
    }

    /// `pending` 是一次性 token：第一次呼叫吞掉（不送）並清成 `false`，且更新
    /// `last_sent`；緊接著第二次呼叫（視窗外）沒有 token 可吞，正常送出。
    /// 這是把單向閂鎖換成消費式 token 的核心不變式：token 讀了就清，不需要
    /// 任何第三方負責清除。
    #[test]
    fn claim_send_slot_consumes_pending_token_exactly_once() {
        let pending = AtomicBool::new(true);
        let mut last_sent = Instant::now() - THROTTLE * 2;
        let first = Instant::now();

        assert!(!claim_send_slot(&pending, first, &mut last_sent, THROTTLE));
        assert!(!pending.load(Ordering::Acquire), "token 應該被消費掉");
        assert_eq!(last_sent, first, "吞掉的這批也算已送，更新 last_sent");

        let second = first + THROTTLE * 2;
        assert!(
            claim_send_slot(&pending, second, &mut last_sent, THROTTLE),
            "token 已經被上一次呼叫消費掉，這次沒有東西可吞，該正常送出"
        );
    }

    #[test]
    fn test_user_event_with_count_new() {
        let event = UserEventWithCount::new(UserEvent::NavigateUp, 5);
        assert_eq!(event.event, UserEvent::NavigateUp);
        assert_eq!(event.count, 5);
    }

    #[test]
    fn test_user_event_with_count_new_zero_count() {
        let event = UserEventWithCount::new(UserEvent::NavigateDown, 0);
        assert_eq!(event.event, UserEvent::NavigateDown);
        assert_eq!(event.count, 1); // 0 應該被轉成 1
    }

    #[test]
    fn test_user_event_with_count_from_event() {
        let event = UserEventWithCount::from_event(UserEvent::NavigateLeft);
        assert_eq!(event.event, UserEvent::NavigateLeft);
        assert_eq!(event.count, 1);
    }

    #[test]
    fn test_user_event_with_count_equality() {
        let event1 = UserEventWithCount::new(UserEvent::ScrollUp, 3);
        let event2 = UserEventWithCount::new(UserEvent::ScrollUp, 3);
        let event3 = UserEventWithCount::new(UserEvent::ScrollDown, 3);

        assert_eq!(event1, event2);
        assert_ne!(event1, event3);
    }

    /// `config_name()` 與 `Deserialize` 必須互為反向。少了這個測試，兩份對照
    /// 表就會各自漂移 —— 而它們正是「config 檔怎麼寫」的唯一說明。
    #[test]
    fn config_name_round_trips_through_deserialize() {
        // 沒有 Iterator 可列舉，所以逐一列出。漏掉任何一個，
        // `config_name` 的窮盡 match 也不會提醒 —— 但 all_events 至少
        // 讓遺漏集中在一處。
        let all_events = [
            UserEvent::ForceQuit,
            UserEvent::Quit,
            UserEvent::HelpToggle,
            UserEvent::Cancel,
            UserEvent::Close,
            UserEvent::NavigateUp,
            UserEvent::NavigateDown,
            UserEvent::NavigateRight,
            UserEvent::NavigateLeft,
            UserEvent::SelectUp,
            UserEvent::SelectDown,
            UserEvent::GoToTop,
            UserEvent::GoToBottom,
            UserEvent::GoToParent,
            UserEvent::GoToChild,
            UserEvent::GoToHead,
            UserEvent::ScrollUp,
            UserEvent::ScrollDown,
            UserEvent::PageUp,
            UserEvent::PageDown,
            UserEvent::HalfPageUp,
            UserEvent::HalfPageDown,
            UserEvent::GoToNext,
            UserEvent::GoToPrevious,
            UserEvent::Confirm,
            UserEvent::RefList,
            UserEvent::Search,
            UserEvent::Filter,
            UserEvent::UserCommand(1),
            UserEvent::UserCommand(42),
            UserEvent::IgnoreCaseToggle,
            UserEvent::FuzzyToggle,
            UserEvent::Refresh,
            UserEvent::ShortCopy,
            UserEvent::FullCopy,
            UserEvent::BranchCopy,
            UserEvent::FullBranchCopy,
            UserEvent::TagCopy,
            UserEvent::CreateTag,
            UserEvent::DeleteTag,
            UserEvent::DeleteRef,
            UserEvent::RemoteRefsToggle,
            UserEvent::GitHubToggle,
            UserEvent::TaskListToggle,
            UserEvent::DetailPaneToggle,
            UserEvent::Fetch,
            UserEvent::Checkout,
            UserEvent::MergePr,
            UserEvent::ToggleIssueState,
            UserEvent::TogglePrDraft,
            UserEvent::ToggleCommitLog,
            UserEvent::CheckUpdate,
        ];

        for event in all_events {
            let name = event
                .config_name()
                .unwrap_or_else(|| panic!("{event:?} 沒有 config 名稱"));
            let line = format!("{name} = [\"a\"]");
            let parsed: std::collections::HashMap<UserEvent, Vec<String>> = toml::from_str(&line)
                .unwrap_or_else(|e| panic!("{event:?} 的名稱 {name:?} 無法反向解析: {e}"));
            assert!(
                parsed.contains_key(&event),
                "{name:?} 解析回了不同的事件: {parsed:?}"
            );
        }

        assert_eq!(UserEvent::Unknown.config_name(), None);
    }
}

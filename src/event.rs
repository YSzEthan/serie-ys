use std::{
    ffi::OsStr,
    fmt::{self, Debug, Formatter},
    path::{Component, Path},
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
    RefreshGitHub {
        state: String,
    },
    GitHubDataLoaded {
        issues: Vec<crate::github::GhIssue>,
        pull_requests: Vec<crate::github::GhPullRequest>,
        warnings: Vec<String>,
        issues_cursor: Option<String>,
        prs_cursor: Option<String>,
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
        state: String,
    },
    OpenToggleStatePrompt {
        number: u64,
        kind: crate::github::GhItemKind,
        action: crate::github::StateAction,
        filter_state: String,
    },
    OpenTogglePrDraftPrompt {
        number: u64,
        action: crate::github::PrDraftAction,
        filter_state: String,
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
        state: String,
        method: crate::github::MergeMethod,
        delete_branch: bool,
    },
    ToggleItemStateRequested {
        number: u64,
        kind: crate::github::GhItemKind,
        action: crate::github::StateAction,
        filter_state: String,
    },
    TogglePrDraftRequested {
        number: u64,
        action: crate::github::PrDraftAction,
        filter_state: String,
    },
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
    pending_refresh: Option<Arc<AtomicBool>>,
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
            pending_refresh: None,
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

    pub fn start_git_watcher(&mut self, repo_root: &Path) {
        let flag = start_git_watcher(self.tx.clone(), repo_root);
        self.pending_refresh = Some(flag);
    }

    pub fn clear_pending_refresh(&self) {
        if let Some(ref flag) = self.pending_refresh {
            flag.store(false, Ordering::Release);
        }
    }

    /// 標記「已有 refresh 在路上」，讓 watcher 短期內偵測到的後續 fs 事件
    /// 被 debounce 吃掉，避免主動 refresh 後 watcher 重複觸發 slow-path。
    pub fn mark_pending_refresh(&self) {
        if let Some(ref flag) = self.pending_refresh {
            flag.store(true, Ordering::Release);
        }
    }
}

pub fn start_git_watcher(tx: Sender, repo_root: &Path) -> Arc<AtomicBool> {
    use notify_debouncer_mini::new_debouncer;

    let pending_refresh = Arc::new(AtomicBool::new(false));
    let pending = pending_refresh.clone();

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
                    if now.duration_since(last_sent) < throttle {
                        continue;
                    }
                    if !pending.swap(true, Ordering::AcqRel) {
                        tx.send(AppEvent::AutoRefresh);
                        last_sent = now;
                    }
                }
                Ok(Err(_)) => {}
                Err(_) => break,
            }
        }
    });

    pending_refresh
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    TaskListToggle,
    DetailPaneToggle,
    Fetch,
    Checkout,
    MergePr,
    ToggleIssueState,
    TogglePrDraft,
    ToggleCommitLog,
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
            UserEvent::TaskListToggle => "task_list_toggle",
            UserEvent::DetailPaneToggle => "detail_pane_toggle",
            UserEvent::Fetch => "fetch",
            UserEvent::Checkout => "checkout",
            UserEvent::MergePr => "merge_pr",
            UserEvent::ToggleIssueState => "toggle_issue_state",
            UserEvent::TogglePrDraft => "toggle_pr_draft",
            UserEvent::ToggleCommitLog => "toggle_commit_log",
            UserEvent::Unknown => return None,
        };
        Some(name.to_string())
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
                        "task_list_toggle" => Ok(UserEvent::TaskListToggle),
                        "detail_pane_toggle" => Ok(UserEvent::DetailPaneToggle),
                        "fetch" => Ok(UserEvent::Fetch),
                        "checkout" => Ok(UserEvent::Checkout),
                        "merge_pr" => Ok(UserEvent::MergePr),
                        "toggle_issue_state" => Ok(UserEvent::ToggleIssueState),
                        "toggle_pr_draft" => Ok(UserEvent::TogglePrDraft),
                        "toggle_commit_log" => Ok(UserEvent::ToggleCommitLog),
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

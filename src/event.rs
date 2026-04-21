use std::{
    ffi::OsStr,
    fmt::{self, Debug, Formatter},
    path::{Component, Path},
    sync::{
        atomic::{AtomicBool, Ordering},
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

/// Tick event interval driving UI animations (marquee, etc.).
pub const TICK_INTERVAL: Duration = Duration::from_millis(100);

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

    /// Send an event after a delay, on a background thread.
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
        };
        controller.start();

        controller
    }

    pub fn start(&self) {
        self.stop.store(false, Ordering::Release);
        let stop = self.stop.clone();
        let tx = self.tx.clone();
        let term_signal = self.term_signal.clone();
        let handle = thread::spawn(move || {
            let tick_interval = TICK_INTERVAL;
            let mut last_tick = Instant::now();
            loop {
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
            handle.join().unwrap();
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

/// Fast-path first: cheap string checks on the raw event path before any
/// syscalls. Only canonicalize when we need to compare against `git_dir`
/// (which may be a symlink on macOS for worktrees/submodules).
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
    // macOS AppleDouble (._foo) produced by tar/cp on HFS+.
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
    // Canonicalize only the survivors to stabilize comparison against git_dir
    // across macOS symlinks (e.g. /tmp → /private/tmp).
    let path = e.path.canonicalize().unwrap_or_else(|_| e.path.clone());
    if path.starts_with(git_dir) {
        return true;
    }
    // After canonicalization the path may differ; re-check ignored components.
    !path_has_ignored_component(&path, repo_root, ignored)
}

/// Collect plain directory/file names from a .gitignore. Glob patterns, paths,
/// and negation entries are intentionally skipped — this is a noise-filter hint,
/// not a gitignore parser. Final correctness is delegated to `git status`.
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

// The event triggered by user's key input
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
    ScrollUp,
    ScrollDown,
    PageUp,
    PageDown,
    HalfPageUp,
    HalfPageDown,
    SelectTop,
    SelectMiddle,
    SelectBottom,
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
    RemoteRefsToggle,
    GitHubToggle,
    TaskListToggle,
    DetailPaneToggle,
    Fetch,
    Checkout,
    Unknown,
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
                        "scroll_up" => Ok(UserEvent::ScrollUp),
                        "scroll_down" => Ok(UserEvent::ScrollDown),
                        "page_up" => Ok(UserEvent::PageUp),
                        "page_down" => Ok(UserEvent::PageDown),
                        "half_page_up" => Ok(UserEvent::HalfPageUp),
                        "half_page_down" => Ok(UserEvent::HalfPageDown),
                        "select_top" => Ok(UserEvent::SelectTop),
                        "select_middle" => Ok(UserEvent::SelectMiddle),
                        "select_bottom" => Ok(UserEvent::SelectBottom),
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
                        "remote_refs_toggle" => Ok(UserEvent::RemoteRefsToggle),
                        "github_toggle" => Ok(UserEvent::GitHubToggle),
                        "task_list_toggle" => Ok(UserEvent::TaskListToggle),
                        "detail_pane_toggle" => Ok(UserEvent::DetailPaneToggle),
                        "fetch" => Ok(UserEvent::Fetch),
                        "checkout" => Ok(UserEvent::Checkout),
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
        assert_eq!(event.count, 1); // zero should be converted to 1
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
}

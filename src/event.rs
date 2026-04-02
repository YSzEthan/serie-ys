use std::{
    fmt::{self, Debug, Formatter},
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::Duration,
};

use ratatui::crossterm::event::KeyEvent;
use serde::{
    de::{self, Deserializer, Visitor},
    Deserialize,
};

use crate::view::RefreshViewContext;

#[derive(Debug)]
pub enum AppEvent {
    Key(KeyEvent),
    Resize(usize, usize),
    Quit,
    Clear,
    OpenDetail,
    CloseDetail,
    ClearDetail,
    OpenUserCommand(usize),
    CloseUserCommand,
    ClearUserCommand,
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
    ClearHelp,
    OpenGitHub,
    CloseGitHub,
    ClearGitHub,
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
    GitHubDetailsLoaded {
        issue_details: Vec<(u64, String)>,
        pr_details: Vec<(u64, String)>,
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
    LoadDetail {
        number: u64,
        kind: crate::github::GhItemKind,
    },
    DetailFetched {
        number: u64,
        kind: crate::github::GhItemKind,
        rendered: String,
    },
    LoadTaskPanel {
        number: u64,
        kind: crate::github::GhItemKind,
    },
    TaskPanelLoaded {
        number: u64,
        kind: crate::github::GhItemKind,
        items: Vec<crate::github::CheckboxItem>,
    },
    SelectNewerCommit,
    SelectOlderCommit,
    SelectParentCommit,
    CopyToClipboard {
        name: String,
        value: String,
    },
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
    AutoRefresh,
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
}

impl EventController {
    pub fn init() -> Self {
        let (tx, rx) = mpsc::channel();
        let tx = Sender { tx };
        let rx = Receiver { rx };

        let controller = EventController {
            tx: tx.clone(),
            rx,
            stop: Arc::new(AtomicBool::new(false)),
            handle: Arc::new(Mutex::new(None)),
            pending_refresh: None,
        };
        controller.start();

        controller
    }

    pub fn start(&self) {
        self.stop.store(false, Ordering::Release);
        let stop = self.stop.clone();
        let tx = self.tx.clone();
        let handle = thread::spawn(move || loop {
            if stop.load(Ordering::Acquire) {
                break;
            }
            match ratatui::crossterm::event::poll(std::time::Duration::from_millis(100)) {
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
                Ok(false) => {
                    continue;
                }
                Err(e) => {
                    panic!("Failed to poll event: {e}");
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

    pub fn start_git_watcher(&mut self, git_dir: &Path) {
        let flag = start_git_watcher(self.tx.clone(), git_dir);
        self.pending_refresh = Some(flag);
    }

    pub fn clear_pending_refresh(&self) {
        if let Some(ref flag) = self.pending_refresh {
            flag.store(false, Ordering::Release);
        }
    }
}

pub fn start_git_watcher(tx: Sender, git_dir: &Path) -> Arc<AtomicBool> {
    use notify_debouncer_mini::{new_debouncer, DebouncedEventKind};

    let pending_refresh = Arc::new(AtomicBool::new(false));
    let pending = pending_refresh.clone();

    let git_dir = git_dir.to_path_buf();
    thread::spawn(move || {
        let (debounce_tx, debounce_rx) = std::sync::mpsc::channel();

        let mut debouncer = match new_debouncer(Duration::from_millis(500), debounce_tx) {
            Ok(d) => d,
            Err(_) => return,
        };

        if debouncer
            .watcher()
            .watch(&git_dir, notify::RecursiveMode::Recursive)
            .is_err()
        {
            return;
        }

        loop {
            match debounce_rx.recv() {
                Ok(Ok(events)) => {
                    let has_relevant = events.iter().any(|e| {
                        e.kind == DebouncedEventKind::Any
                            && e.path.extension() != Some(std::ffi::OsStr::new("lock"))
                    });
                    // Only send if no pending refresh is already queued
                    if has_relevant && !pending.swap(true, Ordering::AcqRel) {
                        tx.send(AppEvent::AutoRefresh);
                    }
                }
                Ok(Err(_)) => {}
                Err(_) => break,
            }
        }
    });

    pending_refresh
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
    CreateTag,
    DeleteTag,
    RemoteRefsToggle,
    GitHubToggle,
    TaskListToggle,
    DetailPaneToggle,
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
                        "create_tag" => Ok(UserEvent::CreateTag),
                        "delete_tag" => Ok(UserEvent::DeleteTag),
                        "remote_refs_toggle" => Ok(UserEvent::RemoteRefsToggle),
                        "github_toggle" => Ok(UserEvent::GitHubToggle),
                        "task_list_toggle" => Ok(UserEvent::TaskListToggle),
                        "detail_pane_toggle" => Ok(UserEvent::DetailPaneToggle),
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

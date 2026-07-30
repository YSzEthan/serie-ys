use std::cell::Cell;
use std::rc::Rc;

use ratatui::{
    crossterm::event::{Event, KeyEvent},
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Padding, Paragraph, Wrap},
    Frame,
};
use rustc_hash::FxHashMap;
use tui_input::{backend::crossterm::EventHandler, Input};

use crate::{
    app::AppContext,
    event::{AppEvent, RelatedGroup, RelatedItem, Sender, UserEvent, UserEventWithCount},
    fuzzy::SearchMatcher,
    github::{
        self, CheckboxItem, GhIssue, GhItemKind, GhPullRequest, GhTimelineItem, PrDraftAction,
        StateAction,
    },
    view::View,
};

const PREFETCH_THRESHOLD: usize = 5;
const TIMELINE_LOAD_MORE_THRESHOLD: usize = 5;

/// Which region of the timeline a divider closes off. Colour is decided by
/// what came *before* the divider, not what follows — reading top to bottom,
/// that's the piece of context the eye needs while scrolling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Body,
    Comment,
    Commit,
}

impl Section {
    /// Indexed rather than Rgb so these survive terminals without truecolor.
    fn color(self) -> Color {
        match self {
            Section::Body => Color::Indexed(146), // pastel blue-grey (#afafd7)
            Section::Comment => Color::Indexed(151), // pastel green-grey (#afd7af)
            Section::Commit => Color::Indexed(186), // pastel yellow-grey (#d7d787)
        }
    }

    fn divider(self, width: usize) -> Line<'static> {
        super::markdown::rule_line_colored(width, self.color())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StateFilter {
    Open,
    Closed,
    All,
}

impl StateFilter {
    fn next(self) -> Self {
        match self {
            StateFilter::Open => StateFilter::Closed,
            StateFilter::Closed => StateFilter::All,
            StateFilter::All => StateFilter::Open,
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            StateFilter::Open => "open",
            StateFilter::Closed => "closed",
            StateFilter::All => "all",
        }
    }

    fn from_arg(s: &str) -> Self {
        match s {
            "closed" => Self::Closed,
            "all" => Self::All,
            _ => Self::Open,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitHubTab {
    Issues,
    PullRequests,
}

impl GitHubTab {
    fn kind(self) -> GhItemKind {
        match self {
            GitHubTab::Issues => GhItemKind::Issue,
            GitHubTab::PullRequests => GhItemKind::PullRequest,
        }
    }
}

#[derive(Debug)]
struct TaskListPanel {
    number: u64,
    kind: GhItemKind,
    items: Vec<CheckboxItem>,
    original_checked: Vec<bool>,
    selected: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitHubFocus {
    List,
    Preview,
    Prompt,
    CheckboxEdit,
}

#[derive(Debug, Default)]
enum LoadState {
    #[default]
    Idle,
    Loading,
    Error(String),
}

#[derive(Debug, Default, PartialEq, Eq)]
enum TimelineLoad {
    #[default]
    NotRequested,
    Loading,
    Loaded,
    Error(String),
}

#[derive(Debug, Default)]
struct TimelineEntry {
    state: TimelineLoad,
    items: Vec<GhTimelineItem>,
    next_cursor: Option<String>,
    loading_more: bool,
}

#[derive(Debug)]
pub struct GitHubView<'a> {
    before: View<'a>,

    focus: GitHubFocus,

    active_tab: GitHubTab,
    issues: Vec<GhIssue>,
    pull_requests: Vec<GhPullRequest>,
    selected_index: usize,
    offset: usize,
    height: usize,

    preview_offset: usize,

    search_input: Input,
    filtered_issue_indices: Vec<usize>,
    filtered_pr_indices: Vec<usize>,

    state_filter: StateFilter,

    task_panel: Option<TaskListPanel>,

    load_state: LoadState,

    flash_message: Option<(String, bool)>,

    /// Render-time overflow flag (selected row's title+author wider than
    /// available). App reads this to decide whether to tick `marquee_frame`.
    selected_row_overflows: Cell<bool>,

    issues_next_cursor: Option<String>,
    prs_next_cursor: Option<String>,
    loading_more: bool,
    request_generation: u64,

    pending_jump: Option<u64>,

    timeline: FxHashMap<(GhItemKind, u64), TimelineEntry>,
    last_preview_len: usize,

    /// Bumped on any in-place edit the preview can show — currently body
    /// replacement and bulk reload. `set_pr_draft_flag` deliberately does not,
    /// since `is_draft` never reaches `build_preview_content`; that changes the
    /// day the preview starts showing draft status.
    body_rev: u64,
    preview_cache: Option<PreviewCache>,
    /// Preview content height, recorded by `render_preview`. The scroll
    /// handlers use it instead of re-deriving it from `height`.
    preview_height: usize,

    ctx: Rc<AppContext>,
    tx: Sender,
}

impl<'a> GitHubView<'a> {
    pub fn new(
        before: View<'a>,
        issues: Vec<GhIssue>,
        pull_requests: Vec<GhPullRequest>,
        issues_next_cursor: Option<String>,
        prs_next_cursor: Option<String>,
        state_filter: &str,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> GitHubView<'a> {
        let load_state = if issues.is_empty() && pull_requests.is_empty() {
            LoadState::Loading
        } else {
            LoadState::Idle
        };
        let state_filter = StateFilter::from_arg(state_filter);
        GitHubView {
            before,
            focus: GitHubFocus::List,
            active_tab: GitHubTab::Issues,
            issues,
            pull_requests,
            selected_index: 0,
            offset: 0,
            height: 0,
            preview_offset: 0,
            search_input: Input::default(),
            filtered_issue_indices: Vec::new(),
            filtered_pr_indices: Vec::new(),
            state_filter,
            task_panel: None,
            load_state,
            flash_message: None,
            selected_row_overflows: Cell::new(false),
            issues_next_cursor,
            prs_next_cursor,
            loading_more: false,
            request_generation: 0,
            pending_jump: None,
            timeline: FxHashMap::default(),
            last_preview_len: 0,
            body_rev: 0,
            preview_cache: None,
            preview_height: 0,
            ctx,
            tx,
        }
    }

    pub fn marquee_id(&self) -> Option<std::sync::Arc<str>> {
        let tab = match self.active_tab {
            GitHubTab::Issues => "issues",
            GitHubTab::PullRequests => "prs",
        };
        let idx = self.actual_index(self.selected_index);
        let num = match self.active_tab {
            GitHubTab::Issues => self.issues.get(idx).map(|i| i.number)?,
            GitHubTab::PullRequests => self.pull_requests.get(idx).map(|p| p.number)?,
        };
        Some(std::sync::Arc::from(format!("gh:{tab}:{num}")))
    }

    pub fn marquee_needed(&self) -> bool {
        self.selected_row_overflows.get()
    }

    pub fn take_before_view(&mut self) -> View<'a> {
        std::mem::take(&mut self.before)
    }

    pub fn set_flash(&mut self, msg: String, is_error: bool) {
        self.flash_message = Some((msg, is_error));
    }

    pub fn set_error(&mut self, msg: String) {
        if matches!(self.load_state, LoadState::Loading) {
            self.load_state = LoadState::Error(msg);
        }
    }

    pub fn update_data(
        &mut self,
        issues: Vec<GhIssue>,
        pull_requests: Vec<GhPullRequest>,
        issues_next_cursor: Option<String>,
        prs_next_cursor: Option<String>,
    ) {
        self.load_state = LoadState::Idle;
        self.issues = issues;
        self.pull_requests = pull_requests;
        self.issues_next_cursor = issues_next_cursor;
        self.prs_next_cursor = prs_next_cursor;
        self.timeline.clear();
        self.body_rev = self.body_rev.wrapping_add(1);
        self.bump_generation();
        // 修正選取索引避免越界
        let max = self.current_list_len().saturating_sub(1);
        if self.selected_index > max {
            self.selected_index = max;
        }
        self.preview_offset = 0;
        self.request_timeline_for_selected();
    }

    /// Refresh 完成但資料無變更時（`update_data` 不會被呼叫）用來清掉 Loading 指示器。
    pub fn finish_loading(&mut self) {
        if matches!(self.load_state, LoadState::Loading) {
            self.load_state = LoadState::Idle;
        }
    }

    /// Returns `true` when the page was accepted (generation matched and
    /// state updated). Caller uses this to decide whether to also sync the
    /// cache, keeping view ↔ cache in lockstep.
    pub fn append_issues(
        &mut self,
        items: Vec<GhIssue>,
        next_cursor: Option<String>,
        generation: u64,
    ) -> bool {
        if generation != self.request_generation {
            return false;
        }
        self.issues.extend(items);
        self.issues_next_cursor = next_cursor;
        self.loading_more = false;
        if self.pending_jump.is_some() {
            self.try_resolve_jump();
        }
        true
    }

    pub fn append_pull_requests(
        &mut self,
        items: Vec<GhPullRequest>,
        next_cursor: Option<String>,
        generation: u64,
    ) -> bool {
        if generation != self.request_generation {
            return false;
        }
        self.pull_requests.extend(items);
        self.prs_next_cursor = next_cursor;
        self.loading_more = false;
        if self.pending_jump.is_some() {
            self.try_resolve_jump();
        }
        true
    }

    fn bump_generation(&mut self) {
        self.request_generation = self.request_generation.wrapping_add(1);
        self.loading_more = false;
        self.pending_jump = None;
    }

    fn start_jump(&mut self, number: u64) {
        self.pending_jump = Some(number);
        self.try_resolve_jump();
    }

    fn try_resolve_jump(&mut self) {
        let Some(number) = self.pending_jump else {
            return;
        };

        let found = match self.active_tab {
            GitHubTab::Issues => self.issues.iter().position(|i| i.number == number),
            GitHubTab::PullRequests => self.pull_requests.iter().position(|p| p.number == number),
        };
        if let Some(idx) = found {
            self.selected_index = idx;
            self.preview_offset = 0;
            self.adjust_scroll();
            self.pending_jump = None;
            self.request_timeline_for_selected();
            return;
        }

        if self.current_has_next_cursor() {
            if self.loading_more {
                return;
            }
            self.dispatch_load_more();
            return;
        }

        let tab = match self.active_tab {
            GitHubTab::Issues => "issues",
            GitHubTab::PullRequests => "PRs",
        };
        self.set_flash(
            format!(
                "No #{number} in {tab} (filter: {})",
                self.state_filter.as_str()
            ),
            true,
        );
        self.pending_jump = None;
    }

    fn request_timeline_for_selected(&mut self) {
        let Some((number, kind)) = self.selected_number_and_kind() else {
            return;
        };
        let entry = self.timeline.entry((kind, number)).or_default();
        if matches!(entry.state, TimelineLoad::NotRequested) {
            entry.state = TimelineLoad::Loading;
            self.tx.send(AppEvent::LoadGitHubTimeline {
                number,
                kind,
                after: None,
            });
        }
    }

    fn maybe_load_more_timeline(&mut self) {
        let visible = self.preview_height;
        let near_bottom = self
            .preview_offset
            .saturating_add(visible)
            .saturating_add(TIMELINE_LOAD_MORE_THRESHOLD)
            >= self.last_preview_len;
        if !near_bottom {
            return;
        }
        let Some((number, kind)) = self.selected_number_and_kind() else {
            return;
        };
        let Some(entry) = self.timeline.get_mut(&(kind, number)) else {
            return;
        };
        if entry.state != TimelineLoad::Loaded || entry.loading_more || entry.next_cursor.is_none()
        {
            return;
        }
        let cursor = entry.next_cursor.clone();
        entry.loading_more = true;
        self.tx.send(AppEvent::LoadGitHubTimeline {
            number,
            kind,
            after: cursor,
        });
    }

    pub fn append_timeline_items(
        &mut self,
        number: u64,
        kind: GhItemKind,
        items: Vec<GhTimelineItem>,
        next_cursor: Option<String>,
    ) {
        let entry = self.timeline.entry((kind, number)).or_default();
        entry.items.extend(items);
        entry.next_cursor = next_cursor;
        entry.state = TimelineLoad::Loaded;
        entry.loading_more = false;
    }

    pub fn set_timeline_error(&mut self, number: u64, kind: GhItemKind, error: String) {
        let entry = self.timeline.entry((kind, number)).or_default();
        entry.state = TimelineLoad::Error(error);
    }

    fn current_has_next_cursor(&self) -> bool {
        match self.active_tab {
            GitHubTab::Issues => self.issues_next_cursor.is_some(),
            GitHubTab::PullRequests => self.prs_next_cursor.is_some(),
        }
    }

    fn dispatch_load_more(&mut self) {
        self.loading_more = true;
        let kind = self.active_tab.kind();
        self.tx.send(AppEvent::LoadMoreGitHub {
            kind,
            generation: self.request_generation,
        });
    }

    fn maybe_load_more(&mut self) {
        if self.loading_more || self.has_active_filter() {
            return;
        }
        if !self.current_has_next_cursor() {
            return;
        }
        let threshold = self.current_list_len().saturating_sub(PREFETCH_THRESHOLD);
        if self.selected_index < threshold {
            return;
        }
        self.dispatch_load_more();
    }

    pub fn update_body_for_item(&mut self, number: u64, kind: GhItemKind, new_body: String) {
        match kind {
            GhItemKind::Issue => {
                if let Some(issue) = self.issues.iter_mut().find(|i| i.number == number) {
                    issue.body = new_body;
                }
            }
            GhItemKind::PullRequest => {
                if let Some(pr) = self.pull_requests.iter_mut().find(|p| p.number == number) {
                    pr.body = new_body;
                }
            }
        }
        self.body_rev = self.body_rev.wrapping_add(1);
        self.preview_offset = 0;
    }

    pub fn status_hints(&self) -> Vec<(UserEvent, &'static str)> {
        match self.focus {
            GitHubFocus::CheckboxEdit => {
                vec![
                    (UserEvent::NavigateLeft, "toggle"),
                    (UserEvent::Confirm, "submit"),
                    (UserEvent::Cancel, "cancel"),
                ]
            }
            GitHubFocus::Prompt => {
                vec![
                    (UserEvent::Confirm, "done"),
                    (UserEvent::Cancel, "clear/close"),
                ]
            }
            GitHubFocus::Preview => {
                let mut hints = vec![(UserEvent::Cancel, "back")];
                hints.extend(self.action_hints());
                if self.selected_has_related() {
                    hints.push((UserEvent::DetailPaneToggle, "related"));
                }
                hints
            }
            GitHubFocus::List => {
                if self.current_list_len() == 0 {
                    return match &self.load_state {
                        LoadState::Loading => vec![(UserEvent::Cancel, "close")],
                        LoadState::Error(_) => {
                            vec![(UserEvent::Refresh, "retry"), (UserEvent::Cancel, "close")]
                        }
                        LoadState::Idle => vec![
                            (UserEvent::Refresh, "refresh"),
                            (UserEvent::Cancel, "close"),
                        ],
                    };
                }
                // contextual action 隨選取項目變動、使用者猜不到，排在靜態提示之前，
                // 讓被終端寬度切掉的是 help 裡查得到的那些。
                let mut hints = vec![(UserEvent::RefList, "switch tab")];
                hints.extend(self.action_hints());
                hints.extend([
                    (UserEvent::Search, "search"),
                    (UserEvent::Confirm, "preview"),
                    (UserEvent::Refresh, "refresh"),
                    (UserEvent::Filter, "filter"),
                    (UserEvent::ShortCopy, "copy url / C open / v #num"),
                ]);
                if self.selected_has_related() {
                    hints.push((UserEvent::DetailPaneToggle, "related"));
                }
                hints.push((UserEvent::GitHubToggle, "close"));
                hints
            }
        }
    }

    pub fn handle_event(&mut self, event_with_count: UserEventWithCount, key: KeyEvent) {
        let count = event_with_count.count;
        // In modal-ish focus (List/Preview), Right/Left double as Confirm/Cancel.
        // Prompt takes raw key input; CheckboxEdit uses Left/Right to toggle.
        let event = match self.focus {
            GitHubFocus::List | GitHubFocus::Preview => modal_yesno_aliases(event_with_count.event),
            GitHubFocus::Prompt | GitHubFocus::CheckboxEdit => event_with_count.event,
        };

        self.flash_message = None;

        let before = (self.active_tab, self.selected_index);
        match self.focus {
            GitHubFocus::CheckboxEdit => self.handle_checkbox_edit_event(event, count),
            GitHubFocus::Preview => self.handle_preview_event(event, count),
            GitHubFocus::Prompt => self.handle_prompt_event(event, count, key),
            GitHubFocus::List => self.handle_list_event(event, count),
        }
        if (self.active_tab, self.selected_index) != before {
            self.request_timeline_for_selected();
        }
    }

    fn handle_checkbox_edit_event(&mut self, event: UserEvent, count: usize) {
        let Some(ref mut panel) = self.task_panel else {
            self.focus = GitHubFocus::Preview;
            return;
        };
        match event {
            UserEvent::Cancel => {
                self.task_panel = None;
                self.focus = GitHubFocus::Preview;
            }
            UserEvent::NavigateDown | UserEvent::SelectDown => {
                let max = panel.items.len().saturating_sub(1);
                for _ in 0..count {
                    if panel.selected < max {
                        panel.selected += 1;
                    }
                }
            }
            UserEvent::NavigateUp | UserEvent::SelectUp => {
                for _ in 0..count {
                    panel.selected = panel.selected.saturating_sub(1);
                }
            }
            UserEvent::NavigateLeft | UserEvent::NavigateRight => {
                if let Some(item) = panel.items.get_mut(panel.selected) {
                    item.checked = !item.checked;
                }
            }
            UserEvent::Confirm => {
                let changed: Vec<usize> = panel
                    .items
                    .iter()
                    .enumerate()
                    .filter(|(i, item)| item.checked != panel.original_checked[*i])
                    .map(|(_, item)| item.index)
                    .collect();
                if !changed.is_empty() {
                    self.tx.send(AppEvent::BatchToggleCheckboxes {
                        number: panel.number,
                        kind: panel.kind,
                        checkbox_indices: changed,
                    });
                }
                self.task_panel = None;
                self.focus = GitHubFocus::Preview;
            }
            _ => {}
        }
    }

    fn handle_preview_event(&mut self, event: UserEvent, count: usize) {
        match event {
            UserEvent::Cancel | UserEvent::Close => {
                self.focus = GitHubFocus::List;
            }
            UserEvent::NavigateDown | UserEvent::SelectDown => {
                for _ in 0..count {
                    self.preview_offset = self.preview_offset.saturating_add(1);
                }
                self.maybe_load_more_timeline();
            }
            UserEvent::NavigateUp | UserEvent::SelectUp => {
                for _ in 0..count {
                    self.preview_offset = self.preview_offset.saturating_sub(1);
                }
            }
            UserEvent::PageDown => {
                let page = self.preview_height.max(1);
                self.preview_offset = self.preview_offset.saturating_add(page);
                self.maybe_load_more_timeline();
            }
            UserEvent::PageUp => {
                let page = self.preview_height.max(1);
                self.preview_offset = self.preview_offset.saturating_sub(page);
            }
            UserEvent::HalfPageDown => {
                let half = self.preview_height.max(1) / 2;
                self.preview_offset = self.preview_offset.saturating_add(half);
                self.maybe_load_more_timeline();
            }
            UserEvent::HalfPageUp => {
                let half = self.preview_height.max(1) / 2;
                self.preview_offset = self.preview_offset.saturating_sub(half);
            }
            UserEvent::GoToTop => {
                self.preview_offset = 0;
            }
            UserEvent::Confirm => {
                self.try_enter_checkbox_edit();
            }
            UserEvent::DetailPaneToggle => {
                self.open_related_picker();
            }
            UserEvent::ToggleIssueState => {
                self.trigger_toggle_state();
            }
            UserEvent::MergePr if matches!(self.active_tab, GitHubTab::PullRequests) => {
                self.try_merge_selected_pr();
            }
            UserEvent::TogglePrDraft if matches!(self.active_tab, GitHubTab::PullRequests) => {
                self.try_toggle_pr_draft();
            }
            _ => {}
        }
    }

    fn handle_list_event(&mut self, event: UserEvent, count: usize) {
        match event {
            UserEvent::GitHubToggle | UserEvent::Cancel | UserEvent::Close => {
                self.tx.send(AppEvent::CloseGitHub);
            }
            UserEvent::RefList => {
                self.active_tab = match self.active_tab {
                    GitHubTab::Issues => GitHubTab::PullRequests,
                    GitHubTab::PullRequests => GitHubTab::Issues,
                };
                self.selected_index = 0;
                self.offset = 0;
                self.preview_offset = 0;
                self.bump_generation();
            }
            UserEvent::NavigateDown | UserEvent::SelectDown => {
                let max = self.current_list_len().saturating_sub(1);
                for _ in 0..count {
                    if self.selected_index < max {
                        self.selected_index += 1;
                    }
                }
                self.preview_offset = 0;
                self.adjust_scroll();
                self.maybe_load_more();
            }
            UserEvent::NavigateUp | UserEvent::SelectUp => {
                for _ in 0..count {
                    self.selected_index = self.selected_index.saturating_sub(1);
                }
                self.preview_offset = 0;
                self.adjust_scroll();
            }
            UserEvent::GoToTop => {
                self.selected_index = 0;
                self.offset = 0;
                self.preview_offset = 0;
            }
            UserEvent::GoToBottom => {
                self.selected_index = self.current_list_len().saturating_sub(1);
                self.preview_offset = 0;
                self.adjust_scroll();
                self.maybe_load_more();
            }
            UserEvent::Confirm if self.current_list_len() > 0 => {
                self.focus = GitHubFocus::Preview;
            }
            UserEvent::PageDown => {
                let page = self.height.saturating_sub(3).max(1);
                let max = self.current_list_len().saturating_sub(1);
                self.selected_index = (self.selected_index + page).min(max);
                self.preview_offset = 0;
                self.adjust_scroll();
                self.maybe_load_more();
            }
            UserEvent::PageUp => {
                let page = self.height.saturating_sub(3).max(1);
                self.selected_index = self.selected_index.saturating_sub(page);
                self.preview_offset = 0;
                self.adjust_scroll();
            }
            UserEvent::HalfPageDown => {
                let half = self.height.saturating_sub(3).max(1) / 2;
                let max = self.current_list_len().saturating_sub(1);
                self.selected_index = (self.selected_index + half).min(max);
                self.preview_offset = 0;
                self.adjust_scroll();
                self.maybe_load_more();
            }
            UserEvent::HalfPageUp => {
                let half = self.height.saturating_sub(3).max(1) / 2;
                self.selected_index = self.selected_index.saturating_sub(half);
                self.preview_offset = 0;
                self.adjust_scroll();
            }
            UserEvent::Search => {
                self.focus = GitHubFocus::Prompt;
            }
            UserEvent::Filter => {
                self.state_filter = self.state_filter.next();
                self.selected_index = 0;
                self.offset = 0;
                self.preview_offset = 0;
                self.load_state = LoadState::Loading;
                self.bump_generation();
                self.tx.send(AppEvent::RefreshGitHub {
                    state: self.state_filter.as_str().to_string(),
                });
            }
            UserEvent::Refresh => {
                self.load_state = LoadState::Loading;
                self.tx.send(AppEvent::RefreshGitHub {
                    state: self.state_filter.as_str().to_string(),
                });
            }
            UserEvent::ShortCopy => {
                let kind = self.active_tab.kind();
                self.with_selected_url(|url| AppEvent::CopyToClipboard {
                    name: format!("{} URL", kind.display_name()),
                    value: url,
                });
            }
            UserEvent::FullCopy => {
                self.with_selected_url(AppEvent::OpenUrl);
            }
            UserEvent::TagCopy => {
                if let Some((number, kind)) = self.selected_number_and_kind() {
                    self.tx.send(AppEvent::CopyToClipboard {
                        name: format!("{} Number", kind.display_name()),
                        value: format!("#{number}"),
                    });
                }
            }
            UserEvent::DetailPaneToggle => {
                self.open_related_picker();
            }
            UserEvent::MergePr if matches!(self.active_tab, GitHubTab::PullRequests) => {
                self.try_merge_selected_pr();
            }
            UserEvent::ToggleIssueState => {
                self.trigger_toggle_state();
            }
            UserEvent::TogglePrDraft if matches!(self.active_tab, GitHubTab::PullRequests) => {
                self.try_toggle_pr_draft();
            }
            _ => {}
        }
    }

    fn try_merge_selected_pr(&mut self) {
        let idx = self.actual_index(self.selected_index);
        let Some(pr) = self.pull_requests.get(idx) else {
            return;
        };
        if pr.is_draft {
            self.set_flash(format!("PR #{} is draft", pr.number), true);
            return;
        }
        if pr.state != "OPEN" {
            self.set_flash(
                format!("PR #{} is {}", pr.number, pr.state.to_lowercase()),
                true,
            );
            return;
        }
        self.tx.send(AppEvent::OpenMergePrMethodPicker {
            number: pr.number,
            head_ref: pr.head_ref_name.clone(),
            state: self.state_filter.as_str().to_string(),
        });
    }

    fn try_toggle_pr_draft(&mut self) {
        let idx = self.actual_index(self.selected_index);
        let Some(pr) = self.pull_requests.get(idx) else {
            return;
        };
        if pr.state != "OPEN" {
            self.set_flash(
                format!("PR #{} is {}", pr.number, pr.state.to_lowercase()),
                true,
            );
            return;
        }
        let action = PrDraftAction::for_pr(pr.is_draft);
        self.tx.send(AppEvent::OpenTogglePrDraftPrompt {
            number: pr.number,
            action,
            filter_state: self.state_filter.as_str().to_string(),
        });
    }

    /// 就地更新 draft 狀態。`RefreshGitHub` 是非同步的，成功通知到列表刷新之間
    /// 若讀到過期的 `is_draft`，反向操作會挑錯方向，而 `gh pr ready` 對非 draft
    /// PR 是 idempotent 成功 — 使用者不會收到任何錯誤提示。
    pub fn set_pr_draft_flag(&mut self, number: u64, is_draft: bool) {
        if let Some(pr) = self.pull_requests.iter_mut().find(|p| p.number == number) {
            pr.is_draft = is_draft;
        }
    }

    /// 選取項目的 (編號, 型別, 狀態)，兩個分頁共用。
    fn selected_state_target(&self) -> Option<(u64, GhItemKind, &str)> {
        let idx = self.actual_index(self.selected_index);
        match self.active_tab {
            GitHubTab::Issues => self
                .issues
                .get(idx)
                .map(|i| (i.number, GhItemKind::Issue, i.state.as_str())),
            GitHubTab::PullRequests => self
                .pull_requests
                .get(idx)
                .map(|p| (p.number, GhItemKind::PullRequest, p.state.as_str())),
        }
    }

    fn trigger_toggle_state(&mut self) {
        let Some((number, kind, state)) = self.selected_state_target() else {
            return;
        };
        let Some(action) = StateAction::for_state(state) else {
            // merged 的 PR 不能 reopen。要有回饋，否則按下去毫無反應 ——
            // 與同分頁的 merge / draft 切換遇到不可操作狀態時的行為一致。
            let msg = format!("{} #{number} is {}", kind.noun(), state.to_lowercase());
            self.set_flash(msg, true);
            return;
        };
        self.tx.send(AppEvent::OpenToggleStatePrompt {
            number,
            kind,
            action,
            filter_state: self.state_filter.as_str().to_string(),
        });
    }

    fn action_hints(&self) -> Vec<(UserEvent, &'static str)> {
        let Some((_, kind, state)) = self.selected_state_target() else {
            return Vec::new();
        };
        let mut hints = Vec::new();

        // merge 與 draft 切換只對 open 的 PR 有意義
        if matches!(self.active_tab, GitHubTab::PullRequests) && state == "OPEN" {
            let idx = self.actual_index(self.selected_index);
            if let Some(pr) = self.pull_requests.get(idx) {
                if !pr.is_draft {
                    hints.push((UserEvent::MergePr, "merge PR"));
                }
                hints.push((
                    UserEvent::TogglePrDraft,
                    PrDraftAction::for_pr(pr.is_draft).hint_label(),
                ));
            }
        }
        if let Some(action) = StateAction::for_state(state) {
            hints.push((UserEvent::ToggleIssueState, action.hint_label(kind)));
        }
        hints
    }

    fn open_related_picker(&self) {
        let items = self.selected_related_items();
        self.tx.send(AppEvent::OpenRelatedPicker { items });
    }

    pub fn jump_to_issue(&mut self, number: u64) -> bool {
        let Some(raw_idx) = self.issues.iter().position(|i| i.number == number) else {
            return false;
        };
        self.active_tab = GitHubTab::Issues;
        self.focus = GitHubFocus::List;
        self.search_input.reset();
        self.filtered_issue_indices.clear();
        self.filtered_pr_indices.clear();
        self.selected_index = raw_idx;
        self.offset = 0;
        self.preview_offset = 0;
        self.adjust_scroll();
        true
    }

    fn selected_has_related(&self) -> bool {
        let idx = self.actual_index(self.selected_index);
        match self.active_tab {
            GitHubTab::Issues => self
                .issues
                .get(idx)
                .is_some_and(|i| i.parent.is_some() || !i.sub_issues.is_empty()),
            GitHubTab::PullRequests => self
                .pull_requests
                .get(idx)
                .is_some_and(|p| !p.linked_issues.is_empty()),
        }
    }

    fn selected_related_items(&self) -> Vec<RelatedItem> {
        let idx = self.actual_index(self.selected_index);
        let mut items: Vec<RelatedItem> = Vec::new();
        match self.active_tab {
            GitHubTab::Issues => {
                let Some(issue) = self.issues.get(idx) else {
                    return items;
                };
                if let Some(p) = &issue.parent {
                    items.push(RelatedItem {
                        number: p.number,
                        state: p.state.clone(),
                        group: RelatedGroup::Parent,
                    });
                }
                for s in &issue.sub_issues {
                    items.push(RelatedItem {
                        number: s.number,
                        state: s.state.clone(),
                        group: RelatedGroup::Sub,
                    });
                    if items.len() >= 9 {
                        break;
                    }
                }
            }
            GitHubTab::PullRequests => {
                let Some(pr) = self.pull_requests.get(idx) else {
                    return items;
                };
                for l in &pr.linked_issues {
                    items.push(RelatedItem {
                        number: l.number,
                        state: l.state.clone(),
                        group: RelatedGroup::Linked,
                    });
                    if items.len() >= 9 {
                        break;
                    }
                }
            }
        }
        items
    }

    fn handle_prompt_event(&mut self, event: UserEvent, count: usize, key: KeyEvent) {
        match event {
            UserEvent::Cancel | UserEvent::Close => {
                if self.search_input.value().is_empty() {
                    // Empty query → close view
                    self.tx.send(AppEvent::CloseGitHub);
                } else {
                    // Clear query → back to unfiltered list
                    self.search_input.reset();
                    self.filtered_issue_indices.clear();
                    self.filtered_pr_indices.clear();
                    self.selected_index = 0;
                    self.offset = 0;
                    self.preview_offset = 0;
                    self.focus = GitHubFocus::List;
                }
            }
            UserEvent::Confirm => {
                let trimmed = self.search_input.value().trim();
                if let Ok(number) = trimmed.parse::<u64>() {
                    self.search_input.reset();
                    self.filtered_issue_indices.clear();
                    self.filtered_pr_indices.clear();
                    self.focus = GitHubFocus::List;
                    self.start_jump(number);
                } else {
                    self.focus = GitHubFocus::List;
                }
            }
            UserEvent::NavigateDown | UserEvent::SelectDown => {
                // Move list selection without leaving prompt
                let max = self.current_list_len().saturating_sub(1);
                for _ in 0..count {
                    if self.selected_index < max {
                        self.selected_index += 1;
                    }
                }
                self.preview_offset = 0;
                self.adjust_scroll();
            }
            UserEvent::NavigateUp | UserEvent::SelectUp => {
                for _ in 0..count {
                    self.selected_index = self.selected_index.saturating_sub(1);
                }
                self.preview_offset = 0;
                self.adjust_scroll();
            }
            UserEvent::RefList => {
                // Tab: switch Issues ⇄ PRs (keep query)
                self.active_tab = match self.active_tab {
                    GitHubTab::Issues => GitHubTab::PullRequests,
                    GitHubTab::PullRequests => GitHubTab::Issues,
                };
                self.selected_index = 0;
                self.offset = 0;
                self.preview_offset = 0;
            }
            _ => {
                // Forward key to tui-input; only rebuild if value actually changed
                let before = self.search_input.value().to_string();
                self.search_input.handle_event(&Event::Key(key));
                if self.search_input.value() != before {
                    self.rebuild_filtered_indices();
                }
            }
        }
    }

    fn try_enter_checkbox_edit(&mut self) {
        let body = self.selected_body();
        if body.is_empty() {
            return;
        }
        let items = github::parse_checkboxes(&body);
        if items.is_empty() {
            self.set_flash("No tasks found".to_string(), false);
            return;
        }
        if let Some((number, kind)) = self.selected_number_and_kind() {
            let original_checked = items.iter().map(|i| i.checked).collect();
            self.task_panel = Some(TaskListPanel {
                number,
                kind,
                items,
                original_checked,
                selected: 0,
            });
            self.focus = GitHubFocus::CheckboxEdit;
        }
    }

    fn selected_body(&self) -> String {
        let idx = self.actual_index(self.selected_index);
        match self.active_tab {
            GitHubTab::Issues => self
                .issues
                .get(idx)
                .map(|i| i.body.clone())
                .unwrap_or_default(),
            GitHubTab::PullRequests => self
                .pull_requests
                .get(idx)
                .map(|p| p.body.clone())
                .unwrap_or_default(),
        }
    }

    fn selected_number_and_kind(&self) -> Option<(u64, GhItemKind)> {
        let idx = self.actual_index(self.selected_index);
        match self.active_tab {
            GitHubTab::Issues => self.issues.get(idx).map(|i| (i.number, GhItemKind::Issue)),
            GitHubTab::PullRequests => self
                .pull_requests
                .get(idx)
                .map(|p| (p.number, GhItemKind::PullRequest)),
        }
    }

    fn selected_url(&self) -> Option<String> {
        let idx = self.actual_index(self.selected_index);
        match self.active_tab {
            GitHubTab::Issues => self.issues.get(idx).map(|i| i.url.clone()),
            GitHubTab::PullRequests => self.pull_requests.get(idx).map(|p| p.url.clone()),
        }
    }

    fn with_selected_url(&self, on_url: impl FnOnce(String) -> AppEvent) {
        match self.selected_url() {
            Some(url) if !url.is_empty() => self.tx.send(on_url(url)),
            Some(_) => self
                .tx
                .send(AppEvent::NotifyWarn("No URL for this item".into())),
            None => {}
        }
    }

    fn current_list_len(&self) -> usize {
        if self.has_active_filter() {
            self.current_filtered_indices().len()
        } else {
            match self.active_tab {
                GitHubTab::Issues => self.issues.len(),
                GitHubTab::PullRequests => self.pull_requests.len(),
            }
        }
    }

    fn current_filtered_indices(&self) -> &[usize] {
        match self.active_tab {
            GitHubTab::Issues => &self.filtered_issue_indices,
            GitHubTab::PullRequests => &self.filtered_pr_indices,
        }
    }

    fn has_active_filter(&self) -> bool {
        !self.search_input.value().is_empty()
    }

    fn rebuild_filtered_indices(&mut self) {
        let query = self.search_input.value().to_string();
        if query.is_empty() {
            self.filtered_issue_indices.clear();
            self.filtered_pr_indices.clear();
            return;
        }
        let matcher = SearchMatcher::new(&query, true, true);

        self.filtered_issue_indices = self
            .issues
            .iter()
            .enumerate()
            .filter(|(_, i)| {
                let target = format!(
                    "#{} {} @{} {}",
                    i.number,
                    i.title,
                    i.author.login,
                    i.labels
                        .iter()
                        .map(|l| l.name.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                );
                matcher.matches(&target)
            })
            .map(|(idx, _)| idx)
            .collect();

        self.filtered_pr_indices = self
            .pull_requests
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                let target = format!(
                    "#{} {} @{} {}",
                    p.number,
                    p.title,
                    p.author.login,
                    p.labels
                        .iter()
                        .map(|l| l.name.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                );
                matcher.matches(&target)
            })
            .map(|(idx, _)| idx)
            .collect();

        // Clamp selected_index
        let max = self.current_list_len().saturating_sub(1);
        if self.selected_index > max {
            self.selected_index = max;
        }
        self.offset = 0;
        self.preview_offset = 0;
    }

    /// Map visible index to actual data index (through filter if active)
    fn actual_index(&self, visible_idx: usize) -> usize {
        if self.has_active_filter() {
            self.current_filtered_indices()
                .get(visible_idx)
                .copied()
                .unwrap_or(0)
        } else {
            visible_idx
        }
    }

    fn adjust_scroll(&mut self) {
        if self.height == 0 {
            return;
        }
        let visible = self.height.saturating_sub(3);
        if self.selected_index < self.offset {
            self.offset = self.selected_index;
        } else if self.selected_index >= self.offset + visible {
            self.offset = self.selected_index - visible + 1;
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect, marquee_frame: u64) {
        self.height = area.height as usize;
        // Render is the single source of truth for overflow — reset at entry
        // so focuses that skip render_list (CheckboxEdit, Prompt) auto-clear.
        self.selected_row_overflows.set(false);

        // ── 三區 split：頂部 tab/prompt + 下半 list|preview ──
        let [header_area, content_area] =
            Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).areas(area);

        self.render_header(f, header_area);

        // ── Loading / 錯誤提示 ──
        if self.current_list_len() == 0 {
            let (text, color) = match &self.load_state {
                LoadState::Loading => ("Loading GitHub data...".to_string(), Color::DarkGray),
                LoadState::Error(err) => (err.clone(), Color::Red),
                LoadState::Idle => ("No items".to_string(), Color::DarkGray),
            };
            render_centered_message(f, content_area, text, color);
            self.clear_image_area(area);
            return;
        }

        let [list_area, preview_area] =
            Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
                .areas(content_area);

        self.render_list(f, list_area, marquee_frame);
        self.render_preview(f, preview_area);

        // ── Flash message ──
        if let Some((ref msg, is_error)) = self.flash_message {
            let color = if is_error {
                Color::Red
            } else {
                Color::DarkGray
            };
            let flash_area = Rect::new(
                content_area.x,
                content_area.bottom().saturating_sub(1),
                content_area.width,
                1,
            );
            let flash = Paragraph::new(Line::from(Span::styled(
                msg.clone(),
                Style::default().fg(color),
            )))
            .alignment(ratatui::layout::Alignment::Center);
            f.render_widget(flash, flash_area);
        }

        self.clear_image_area(area);
    }

    fn render_header(&self, f: &mut Frame, area: Rect) {
        let filter_label = self.state_filter.as_str();
        let count = self.current_list_len();
        let issues_label = format!(" Issues ({}) ", self.issues.len());
        let prs_label = format!(" PRs ({}) ", self.pull_requests.len());

        let tab_line = Line::from(vec![
            Span::styled(
                issues_label,
                if self.active_tab == GitHubTab::Issues {
                    Style::default().fg(Color::Black).bg(Color::Cyan).bold()
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ),
            Span::raw(" "),
            Span::styled(
                prs_label,
                if self.active_tab == GitHubTab::PullRequests {
                    Style::default().fg(Color::Black).bg(Color::Cyan).bold()
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ),
            Span::raw("  "),
            Span::styled(
                format!("[{filter_label}]"),
                Style::default().fg(Color::DarkGray),
            ),
            if self.has_active_filter() {
                Span::styled(
                    format!("  {count} matched"),
                    Style::default().fg(Color::DarkGray),
                )
            } else {
                Span::raw("")
            },
            if matches!(self.load_state, LoadState::Loading) {
                Span::styled("  ⟳ 重新抓取中…", Style::default().fg(Color::Yellow))
            } else {
                Span::raw("")
            },
        ]);

        // Prompt input line
        let prompt_color = if self.focus == GitHubFocus::Prompt {
            Color::Cyan
        } else {
            Color::DarkGray
        };
        let prompt_prefix = Span::styled("> ", Style::default().fg(prompt_color));
        let prompt_value = Span::raw(self.search_input.value().to_string());
        let prompt_line = Line::from(vec![
            Span::raw("  "), // left padding
            prompt_prefix,
            prompt_value,
        ]);

        let [tab_area, prompt_area] =
            Layout::vertical([Constraint::Length(2), Constraint::Length(1)]).areas(area);

        f.render_widget(
            Paragraph::new(tab_line).block(Block::default().padding(Padding::new(2, 2, 1, 0))),
            tab_area,
        );

        f.render_widget(Paragraph::new(prompt_line), prompt_area);

        // Show cursor in prompt when focused
        if self.focus == GitHubFocus::Prompt {
            let cursor_x = prompt_area.x + 2 /* pad */ + 2 /* "> " */ + self.search_input.visual_cursor() as u16;
            f.set_cursor_position((cursor_x, prompt_area.y));
        }
    }

    fn render_list(&self, f: &mut Frame, area: Rect, marquee_frame: u64) {
        // 內層 Paragraph 的 Padding 不覆蓋最左欄，會留下 list view text-mode
        // graph 的字元；先 Clear 整個 list area 擋住殘留。
        f.render_widget(Clear, area);

        let list_border_color = if self.focus == GitHubFocus::List {
            Color::Cyan
        } else {
            Color::DarkGray
        };
        let block = Block::default()
            .borders(Borders::RIGHT)
            .border_style(Style::default().fg(list_border_color));
        let inner = block.inner(area);
        f.render_widget(block, area);

        let has_next = self.current_has_next_cursor();
        // Reserve one row for the load-more indicator when there's a next page
        let visible_height = if has_next {
            (inner.height as usize).saturating_sub(1)
        } else {
            inner.height as usize
        };

        let rows = self.current_viewport_rows(visible_height, inner.width, marquee_frame);
        let mut lines: Vec<Line<'static>> = rows.iter().map(|r| r.line.clone()).collect();

        if has_next {
            let hint = if self.loading_more {
                " Loading more…"
            } else {
                " ↓ more"
            };
            lines.push(Line::styled(hint, Style::default().fg(Color::DarkGray)));
        }

        let list_paragraph =
            Paragraph::new(lines).block(Block::default().padding(Padding::horizontal(1)));
        f.render_widget(list_paragraph, inner);

        // OSC 8 overlay on `#N` for each visible row. Cell layout lives in
        // `LIST_LINK_COL_OFFSET` — keep in sync with the indicator + padding.
        // tmux DCS passthrough loses cursor positioning, so the host terminal
        // renders the label at an arbitrary column — skip overlay inside tmux.
        if crate::external::is_tmux() {
            return;
        }
        let buf = f.buffer_mut();
        let x = inner.left().saturating_add(LIST_LINK_COL_OFFSET);
        if x >= inner.right() {
            return;
        }
        let remaining = inner.right() - x;
        for (i, row) in rows.iter().enumerate() {
            if row.url.is_empty() {
                continue;
            }
            let y = inner.top() + i as u16;
            if y >= inner.bottom() {
                break;
            }
            let label = format!("#{}", row.number);
            let label_width = console::measure_text_width(&label) as u16;
            // Too narrow to fit the whole `#N` — skip overlay (partial hyperlink is worse than none)
            if label_width > remaining {
                continue;
            }
            let payload = crate::external::format_osc8_hyperlink(&row.url, &label);
            buf[(x, y)].set_symbol(&payload);
            for j in 1..label_width {
                buf[(x + j, y)].set_skip(true);
            }
        }
    }

    fn labels_pad_width_for_tab(&self) -> usize {
        match self.active_tab {
            GitHubTab::Issues => self
                .issues
                .iter()
                .map(|i| labels_display_width(&i.labels))
                .max()
                .unwrap_or(0),
            GitHubTab::PullRequests => self
                .pull_requests
                .iter()
                .map(|p| labels_display_width(&p.labels))
                .max()
                .unwrap_or(0),
        }
    }

    fn current_viewport_rows(
        &self,
        visible_height: usize,
        inner_width: u16,
        marquee_frame: u64,
    ) -> Vec<RowData> {
        let pad = self.labels_pad_width_for_tab();
        // Paragraph has Padding::horizontal(1) inside → inner content width is -2.
        let content_width = inner_width.saturating_sub(2) as usize;
        let mut rows = Vec::with_capacity(visible_height);
        let mut overflow = false;

        let make_issue = |issue: &GhIssue, vis_i: usize| -> (RowData, bool) {
            let is_selected = vis_i == self.selected_index;
            let frame = is_selected.then_some(marquee_frame);
            let (line, did_scroll) =
                render_issue_line(issue, is_selected, pad, content_width, frame);
            (
                RowData {
                    line,
                    url: issue.url.clone(),
                    number: issue.number,
                },
                did_scroll,
            )
        };
        let make_pr = |pr: &GhPullRequest, vis_i: usize| -> (RowData, bool) {
            let is_selected = vis_i == self.selected_index;
            let frame = is_selected.then_some(marquee_frame);
            let (line, did_scroll) = render_pr_line(pr, is_selected, pad, content_width, frame);
            (
                RowData {
                    line,
                    url: pr.url.clone(),
                    number: pr.number,
                },
                did_scroll,
            )
        };

        if !self.has_active_filter() {
            match self.active_tab {
                GitHubTab::Issues => {
                    for (i, issue) in self
                        .issues
                        .iter()
                        .enumerate()
                        .skip(self.offset)
                        .take(visible_height)
                    {
                        let (row, ovf) = make_issue(issue, i);
                        overflow |= ovf;
                        rows.push(row);
                    }
                }
                GitHubTab::PullRequests => {
                    for (i, pr) in self
                        .pull_requests
                        .iter()
                        .enumerate()
                        .skip(self.offset)
                        .take(visible_height)
                    {
                        let (row, ovf) = make_pr(pr, i);
                        overflow |= ovf;
                        rows.push(row);
                    }
                }
            }
        } else {
            let indices = self.current_filtered_indices();
            match self.active_tab {
                GitHubTab::Issues => {
                    for (vis_i, &data_i) in indices
                        .iter()
                        .enumerate()
                        .skip(self.offset)
                        .take(visible_height)
                    {
                        let (row, ovf) = make_issue(&self.issues[data_i], vis_i);
                        overflow |= ovf;
                        rows.push(row);
                    }
                }
                GitHubTab::PullRequests => {
                    for (vis_i, &data_i) in indices
                        .iter()
                        .enumerate()
                        .skip(self.offset)
                        .take(visible_height)
                    {
                        let (row, ovf) = make_pr(&self.pull_requests[data_i], vis_i);
                        overflow |= ovf;
                        rows.push(row);
                    }
                }
            }
        }
        self.selected_row_overflows.set(overflow);
        rows
    }

    fn render_preview(&mut self, f: &mut Frame, area: Rect) {
        if self.focus == GitHubFocus::CheckboxEdit {
            self.render_checkbox_preview(f, area);
            return;
        }

        let block = Block::default().padding(Padding::horizontal(1));
        let inner = block.inner(area);
        f.render_widget(block, area);

        // Render is the source of truth for the preview's usable height; the
        // scroll handlers read it back rather than re-deriving it from `height`.
        self.preview_height = inner.height as usize;

        let visual_len = self.refresh_preview_cache(inner.width);
        self.last_preview_len = visual_len;
        // Clamp preview_offset to avoid scrolling past content. Both sides are
        // visual (post-wrap) lines — `Paragraph::scroll` skips wrapped lines,
        // not source lines. The `u16` bound belongs here too, so state and
        // screen cannot disagree about where the bottom is.
        let max_offset = visual_len
            .saturating_sub(inner.height as usize)
            .min(u16::MAX as usize);
        self.preview_offset = self.preview_offset.min(max_offset);
        let scroll = self.preview_offset as u16;

        let cache = self
            .preview_cache
            .as_ref()
            .expect("refresh_preview_cache always populates");
        let paragraph = Paragraph::new(borrow_lines(&cache.lines))
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0));
        f.render_widget(paragraph, inner);

        // Overlay `#N` cells with OSC 8 hyperlinks. Must run after Paragraph
        // render so we overwrite the pre-drawn plain `#N` glyph.
        // tmux DCS passthrough loses cursor positioning, so the host terminal
        // renders the label at an arbitrary column — skip overlay inside tmux.
        if crate::external::is_tmux() {
            return;
        }
        // The header's position is only trustworthy before scrolling: the
        // scroll offset counts wrapped lines, not source lines, so the header
        // itself scrolls off screen the moment `scroll != 0`. Restoring a
        // hyperlink after that needs links attached to spans rather than a
        // stored coordinate.
        if scroll != 0 || inner.height == 0 {
            return;
        }
        let Some(ov) = cache.overlay.as_ref() else {
            return;
        };
        let (x, y) = (inner.left(), inner.top());
        if x >= inner.right() {
            return;
        }
        let payload = crate::external::format_osc8_hyperlink(&ov.url, &ov.label);
        let label_width = console::measure_text_width(&ov.label) as u16;
        let buf = f.buffer_mut();
        buf[(x, y)].set_symbol(&payload);
        let remaining = inner.right() - x;
        for i in 1..label_width.min(remaining) {
            buf[(x + i, y)].set_skip(true);
        }
    }

    /// Borrows the selected issue/PR plus its timeline entry into one value —
    /// everything `build_preview_content` and `PreviewInput::cache_key` read,
    /// so neither can drift from the other by reading a field the other
    /// doesn't know about.
    fn preview_input(&self, width: u16) -> PreviewInput<'_> {
        let (number, kind) = self
            .selected_number_and_kind()
            .unwrap_or((0, GhItemKind::Issue));
        let idx = self.actual_index(self.selected_index);
        let item = match self.active_tab {
            GitHubTab::Issues => self.issues.get(idx).map(|issue| SelectedItem {
                title: issue.title.as_str(),
                state: issue.state.as_str(),
                author: issue.author.login.as_str(),
                labels: issue.labels.as_slice(),
                body: issue.body.as_str(),
                url: issue.url.as_str(),
                extra: SelectedItemExtra::Issue {
                    parent: issue.parent.as_ref(),
                    sub_issues: issue.sub_issues.as_slice(),
                },
            }),
            GitHubTab::PullRequests => self.pull_requests.get(idx).map(|pr| SelectedItem {
                title: pr.title.as_str(),
                state: pr.state.as_str(),
                author: pr.author.login.as_str(),
                labels: pr.labels.as_slice(),
                body: pr.body.as_str(),
                url: pr.url.as_str(),
                extra: SelectedItemExtra::PullRequest {
                    base_ref_name: pr.base_ref_name.as_str(),
                    head_ref_name: pr.head_ref_name.as_str(),
                },
            }),
        };
        PreviewInput {
            tab: self.active_tab,
            number,
            width,
            body_rev: self.body_rev,
            entry: self.timeline.get(&(kind, number)),
            item,
        }
    }

    /// Rebuild the preview only when its inputs changed, returning the wrapped
    /// line count. `render` runs at the marquee tick rate (10 Hz) whenever the
    /// selected row overflows, and both `markdown::render` and `line_count`
    /// walk the entire body plus every comment — so recomputing per frame
    /// burns CPU while idle.
    ///
    /// Wrapping is left to `Paragraph` rather than reusing
    /// `commit_detail::wrap_line_spans`: that one breaks mid-word, which would
    /// mangle the English prose common in PR bodies.
    fn refresh_preview_cache(&mut self, width: u16) -> usize {
        let input = self.preview_input(width);
        let key = input.cache_key();
        if let Some(cache) = self.preview_cache.as_ref().filter(|c| c.key == key) {
            return cache.visual_len;
        }
        let (lines, overlay) = build_preview_content(&input);
        let visual_len = Paragraph::new(borrow_lines(&lines))
            .wrap(Wrap { trim: false })
            .line_count(width);
        self.preview_cache = Some(PreviewCache {
            key,
            lines,
            overlay,
            visual_len,
        });
        visual_len
    }

    fn render_checkbox_preview(&self, f: &mut Frame, area: Rect) {
        let Some(ref panel) = self.task_panel else {
            return;
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow))
            .title(" Tasks (editing) ")
            .padding(Padding::horizontal(1));
        let inner = block.inner(area);
        f.render_widget(block, area);

        // Available height minus footer line
        let content_height = inner.height.saturating_sub(1) as usize;

        // Scroll offset for long task lists
        let offset = if panel.selected >= content_height {
            panel.selected - content_height + 1
        } else {
            0
        };

        let mut lines: Vec<Line> = panel
            .items
            .iter()
            .enumerate()
            .skip(offset)
            .take(content_height)
            .map(|(i, item)| {
                let selected = i == panel.selected;
                let indicator = if selected { "▸ " } else { "  " };
                let checkbox = if item.checked { "☑ " } else { "☐ " };
                let checkbox_color = if item.checked {
                    Color::Green
                } else {
                    Color::DarkGray
                };
                let label_style = if selected {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };

                Line::from(vec![
                    Span::styled(indicator.to_string(), label_style),
                    Span::styled(checkbox.to_string(), Style::default().fg(checkbox_color)),
                    Span::styled(item.label.clone(), label_style),
                ])
            })
            .collect();

        // Footer
        lines.push(Line::from(Span::styled(
            " h/l:toggle  Enter:submit  Esc:cancel",
            Style::default().fg(Color::DarkGray),
        )));

        let paragraph = Paragraph::new(lines);
        f.render_widget(paragraph, inner);
    }

    fn clear_image_area(&self, area: Rect) {
        for y in area.top()..area.bottom() {
            self.ctx.image_protocol.clear_line(y);
        }
    }
}

fn build_preview_content(input: &PreviewInput) -> (Vec<Line<'static>>, Option<PreviewOverlay>) {
    let mut overlay = None;
    let width = input.width as usize;
    let number = input.number;
    let Some(item) = input.item.as_ref() else {
        return (
            vec![Line::styled(
                "(no item selected)",
                Style::default().fg(Color::DarkGray),
            )],
            overlay,
        );
    };

    let mut lines = Vec::new();

    // Header: #number title  (#N hyperlink overlay)
    if !item.url.is_empty() {
        overlay = Some(PreviewOverlay {
            url: item.url.to_string(),
            label: format!("#{number}"),
        });
    }
    lines.push(Line::from(vec![
        Span::styled(format!("#{number} "), Style::default().fg(Color::DarkGray)),
        Span::styled(
            item.title.to_string(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    let mut meta_spans = vec![
        Span::styled(
            item.state.to_lowercase(),
            Style::default().fg(state_color(item.state)),
        ),
        Span::styled(
            format!("  @{}", item.author),
            Style::default().fg(Color::DarkGray),
        ),
    ];
    if !item.labels.is_empty() {
        meta_spans.push(Span::raw("  "));
        meta_spans.extend(label_spans(item.labels));
    }
    lines.push(Line::from(meta_spans));

    if let SelectedItemExtra::PullRequest {
        base_ref_name,
        head_ref_name,
    } = item.extra
    {
        lines.push(Line::from(vec![
            Span::styled(base_ref_name.to_string(), Style::default().fg(Color::Cyan)),
            Span::styled("  ←  ", Style::default().fg(Color::DarkGray)),
            Span::styled(head_ref_name.to_string(), Style::default().fg(Color::Cyan)),
        ]));
    }

    lines.push(super::markdown::rule_line(width));

    if let SelectedItemExtra::Issue { parent, sub_issues } = item.extra {
        append_relation_lines(&mut lines, parent, sub_issues, width);
    }

    if item.body.is_empty() {
        lines.push(Line::styled(
            "(no body)",
            Style::default().fg(Color::DarkGray),
        ));
    } else {
        lines.extend(super::markdown::render(item.body, width));
    }

    append_comment_lines(&mut lines, input.entry, width);

    (lines, overlay)
}

fn append_comment_lines(
    lines: &mut Vec<Line<'static>>,
    entry: Option<&TimelineEntry>,
    width: usize,
) {
    let mut prev = Section::Body;
    for item in build_timeline(entry) {
        let section = item.section();
        lines.push(prev.divider(width));
        item.render(lines, width);
        prev = section;
    }
}

/// Flattens every state a `TimelineEntry` can be in — pending, failed,
/// loaded (empty or not), paginating — into one list of renderable rows.
/// The render loop that walks the result has no branches of its own: every
/// "what state am I in" question is answered once, here.
///
/// Returns borrowed items rather than an owned copy, so a `None`/`NotRequested`
/// entry (nothing to borrow from) has to be handled before the `Loaded` match
/// arm rather than folded into it via a local default — that would dangle.
fn build_timeline(entry: Option<&TimelineEntry>) -> Vec<TimelineItem<'_>> {
    let Some(entry) = entry else {
        return vec![TimelineItem::notice("(loading comments…)", Color::DarkGray)];
    };

    match &entry.state {
        TimelineLoad::NotRequested | TimelineLoad::Loading => {
            vec![TimelineItem::notice("(loading comments…)", Color::DarkGray)]
        }
        TimelineLoad::Error(e) => vec![TimelineItem::notice(
            format!("(comments failed: {e})"),
            Color::Red,
        )],
        TimelineLoad::Loaded => {
            let mut items: Vec<TimelineItem<'_>> = entry
                .items
                .iter()
                .filter_map(TimelineItem::from_gh)
                .collect();
            // Checked on the *filtered* list, not `entry.items`: a page of
            // nothing but `Unknown` nodes must still fall back to a notice
            // instead of rendering zero rows (and thus no divider at all).
            if items.is_empty() {
                items.push(TimelineItem::notice("(no comments)", Color::DarkGray));
            } else if entry.next_cursor.is_some() {
                let text = if entry.loading_more {
                    "(loading more…)"
                } else {
                    "(more comments — scroll down to load)"
                };
                items.push(TimelineItem::notice(text, Color::DarkGray));
            }
            items
        }
    }
}

/// One renderable row of the timeline: a comment, a commit, or a status
/// notice standing in for either (loading/error/empty/pagination footer).
/// Borrows from the `TimelineEntry` it was built from — this is rebuilt from
/// scratch on every cache miss, so there's nothing to hold onto past render.
enum TimelineItem<'a> {
    Comment {
        author: &'a str,
        created_at: &'a str,
        body: &'a str,
    },
    Commit {
        oid: &'a str,
        headline: &'a str,
        ci_state: Option<&'a str>,
    },
    Notice(Line<'static>),
}

impl<'a> TimelineItem<'a> {
    fn notice(text: impl Into<String>, color: Color) -> Self {
        TimelineItem::Notice(Line::styled(text.into(), Style::default().fg(color)))
    }

    /// `Unknown` nodes — an `__typename` `itemTypes` wasn't supposed to
    /// produce — are dropped rather than rendered as an error. One
    /// unrecognized node shouldn't put a scary message in the middle of an
    /// otherwise normal timeline.
    fn from_gh(item: &'a GhTimelineItem) -> Option<Self> {
        match item {
            GhTimelineItem::IssueComment {
                body,
                created_at,
                author,
            } => Some(TimelineItem::Comment {
                author: author.as_ref().map_or("ghost", |a| a.login.as_str()),
                created_at,
                body,
            }),
            GhTimelineItem::PullRequestCommit { commit } => Some(TimelineItem::Commit {
                oid: &commit.abbreviated_oid,
                headline: &commit.message_headline,
                ci_state: commit
                    .status_check_rollup
                    .as_ref()
                    .map(|r| r.state.as_str()),
            }),
            GhTimelineItem::Unknown => None,
        }
    }

    fn section(&self) -> Section {
        match self {
            TimelineItem::Comment { .. } | TimelineItem::Notice(_) => Section::Comment,
            TimelineItem::Commit { .. } => Section::Commit,
        }
    }

    fn render(self, lines: &mut Vec<Line<'static>>, width: usize) {
        match self {
            TimelineItem::Comment {
                author,
                created_at,
                body,
            } => {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("@{author}"),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  {created_at}"),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
                lines.extend(super::markdown::render(body, width));
            }
            TimelineItem::Commit {
                oid,
                headline,
                ci_state,
            } => {
                lines.push(commit_line(oid, headline, ci_state, width));
            }
            TimelineItem::Notice(line) => lines.push(line),
        }
    }
}

/// Marker + colour for a commit's CI state. The two-space fallback when
/// there's no rollup at all keeps the oid column aligned with commits that
/// do have one.
fn commit_ci_marker(state: Option<&str>) -> (&'static str, Color) {
    match state {
        Some("SUCCESS") => ("✓ ", Color::Green),
        Some("FAILURE" | "ERROR") => ("✗ ", Color::Red),
        Some("PENDING" | "EXPECTED") => ("● ", Color::Yellow),
        _ => ("  ", Color::DarkGray),
    }
}

fn commit_line(oid: &str, headline: &str, ci_state: Option<&str>, width: usize) -> Line<'static> {
    let (marker, marker_color) = commit_ci_marker(ci_state);
    let prefix_width = console::measure_text_width(marker) + console::measure_text_width(oid) + 2;
    let headline =
        console::truncate_str(headline, width.saturating_sub(prefix_width), "…").to_string();
    Line::from(vec![
        Span::styled(marker, Style::default().fg(marker_color)),
        Span::styled(oid.to_string(), Style::default().fg(Color::DarkGray)),
        Span::raw("  "),
        Span::raw(headline),
    ])
}

/// OSC 8 hyperlink drawn over the preview's header line (`#N`) — the only
/// preview row an overlay is ever attached to; see `render_preview`.
#[derive(Debug, Clone)]
struct PreviewOverlay {
    url: String,
    label: String,
}

/// Everything `build_preview_content` reads, borrowed from the selected
/// issue/PR and its timeline entry, plus the width the result is wrapped
/// against. `cache_key` reads the same struct, so it cannot silently miss a
/// field that content-building depends on.
struct PreviewInput<'v> {
    tab: GitHubTab,
    number: u64,
    width: u16,
    body_rev: u64,
    entry: Option<&'v TimelineEntry>,
    item: Option<SelectedItem<'v>>,
}

impl PreviewInput<'_> {
    fn cache_key(&self) -> PreviewKey {
        // Count alone is not enough: "loaded but empty" and "still loading"
        // both have zero items yet render differently. Exhaustive on purpose —
        // a new `TimelineLoad` variant must not silently fold into Pending and
        // freeze the preview.
        let stage = match self.entry.map(|e| &e.state) {
            None | Some(TimelineLoad::NotRequested | TimelineLoad::Loading) => {
                TimelineStage::Pending
            }
            Some(TimelineLoad::Loaded) => TimelineStage::Ready,
            Some(TimelineLoad::Error(_)) => TimelineStage::Failed,
        };
        PreviewKey {
            tab: self.tab,
            number: self.number,
            stage,
            item_count: self.entry.map_or(0, |e| e.items.len()),
            has_more: self.entry.is_some_and(|e| e.next_cursor.is_some()),
            loading_more: self.entry.is_some_and(|e| e.loading_more),
            body_rev: self.body_rev,
            width: self.width,
        }
    }
}

/// Borrowed fields of the selected issue/PR, common to both plus whichever
/// extra bits are specific to the tab it came from.
#[derive(Clone, Copy)]
struct SelectedItem<'v> {
    title: &'v str,
    state: &'v str,
    author: &'v str,
    labels: &'v [crate::github::GhLabel],
    body: &'v str,
    url: &'v str,
    extra: SelectedItemExtra<'v>,
}

#[derive(Clone, Copy)]
enum SelectedItemExtra<'v> {
    Issue {
        parent: Option<&'v crate::github::GhRelatedIssue>,
        sub_issues: &'v [crate::github::GhRelatedIssue],
    },
    PullRequest {
        base_ref_name: &'v str,
        head_ref_name: &'v str,
    },
}

/// Everything the preview content depends on. Equal key ⇒ identical output, so
/// the cache can be reused. A content key rather than a dirty flag: there are
/// ~20 sites that reset `preview_offset`, and relying on each to also mark the
/// cache stale would eventually miss one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PreviewKey {
    tab: GitHubTab,
    number: u64,
    stage: TimelineStage,
    item_count: usize,
    /// Drives the footer between "(loading more…)" and "(more comments — …)".
    has_more: bool,
    loading_more: bool,
    body_rev: u64,
    width: u16,
}

/// Which of `append_comment_lines`' branches the preview will take. Derived
/// from `TimelineLoad` rather than reusing it, so the key stays `Copy`/`Eq`
/// without dragging the error string along.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimelineStage {
    Pending,
    Ready,
    Failed,
}

#[derive(Debug)]
struct PreviewCache {
    key: PreviewKey,
    lines: Vec<Line<'static>>,
    overlay: Option<PreviewOverlay>,
    /// Line count *after* wrapping — what `preview_offset` is measured in.
    visual_len: usize,
}

/// Re-borrow cached lines instead of cloning them: `Paragraph` needs an owned
/// `Text`, but the spans can point at the cache's strings, so only the `Vec`s
/// are allocated per frame — no string copies.
fn borrow_lines<'a>(lines: &'a [Line<'static>]) -> Vec<Line<'a>> {
    lines
        .iter()
        // Struct literal, not `Line::from(..)` plus assignments: a field added
        // upstream then fails to compile instead of being silently dropped.
        .map(|l| Line {
            spans: l
                .spans
                .iter()
                .map(|s| Span::styled(s.content.as_ref(), s.style))
                .collect(),
            style: l.style,
            alignment: l.alignment,
        })
        .collect()
}

#[derive(Debug, Clone)]
struct RowData {
    line: Line<'static>,
    url: String,
    number: u64,
}

/// List row cell layout: paragraph padding (1) + indicator (2).
/// Keep in sync with `render_issue_line` / `render_pr_line` — if the indicator
/// width or the Paragraph padding changes, adjust this constant too.
const LIST_LINK_COL_OFFSET: u16 = 3;

fn state_color(state: &str) -> Color {
    match state {
        "OPEN" => Color::Green,
        "CLOSED" => Color::Red,
        "MERGED" => Color::Magenta,
        _ => Color::Gray,
    }
}

fn related_issue_line(indent: &'static str, r: &crate::github::GhRelatedIssue) -> Line<'static> {
    Line::from(vec![
        Span::raw(indent),
        Span::styled(
            format!("#{} ", r.number),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(r.title.clone()),
        Span::raw(" "),
        Span::styled(
            format!("({})", r.state.to_lowercase()),
            Style::default().fg(state_color(&r.state)),
        ),
    ])
}

fn append_relation_lines(
    lines: &mut Vec<Line<'static>>,
    parent: Option<&crate::github::GhRelatedIssue>,
    sub_issues: &[crate::github::GhRelatedIssue],
    width: usize,
) {
    if let Some(parent) = parent {
        let prefix = "Parent: ";
        lines.push(Line::from(vec![
            Span::styled(prefix, Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("#{} ", parent.number),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw(parent.title.clone()),
            Span::raw(" "),
            Span::styled(
                format!("({})", parent.state.to_lowercase()),
                Style::default().fg(state_color(&parent.state)),
            ),
        ]));
    }
    if !sub_issues.is_empty() {
        let indent = "  ";
        lines.push(Line::styled(
            format!("Sub-issues ({}):", sub_issues.len()),
            Style::default().fg(Color::DarkGray),
        ));
        for sub in sub_issues {
            lines.push(related_issue_line(indent, sub));
        }
    }
    if parent.is_some() || !sub_issues.is_empty() {
        lines.push(super::markdown::rule_line(width));
    }
}

fn modal_yesno_aliases(event: UserEvent) -> UserEvent {
    match event {
        UserEvent::NavigateRight => UserEvent::Confirm,
        UserEvent::NavigateLeft => UserEvent::Cancel,
        _ => event,
    }
}

// ── 渲染輔助函數 ──

fn render_centered_message(f: &mut Frame, list_area: Rect, text: String, color: Color) {
    let msg = Paragraph::new(Line::from(Span::styled(text, Style::default().fg(color))))
        .alignment(ratatui::layout::Alignment::Center)
        .block(Block::default().padding(Padding::vertical(list_area.height.saturating_sub(1) / 2)));
    f.render_widget(msg, list_area);
}

fn hex_to_color(hex: &str) -> Color {
    let hex = hex.trim_start_matches('#');
    if hex.len() == 6 {
        let r = u8::from_str_radix(&hex[0..2], 16);
        let g = u8::from_str_radix(&hex[2..4], 16);
        let b = u8::from_str_radix(&hex[4..6], 16);
        if let (Ok(r), Ok(g), Ok(b)) = (r, g, b) {
            return Color::Rgb(r, g, b);
        }
    }
    Color::Yellow
}

fn label_spans(labels: &[crate::github::GhLabel]) -> Vec<Span<'static>> {
    if labels.is_empty() {
        return vec![];
    }
    let mut spans = vec![Span::raw(" [")];
    for (i, label) in labels.iter().enumerate() {
        let color = label
            .color
            .as_deref()
            .map(hex_to_color)
            .unwrap_or(Color::Yellow);
        spans.push(Span::styled(label.name.clone(), Style::default().fg(color)));
        if i < labels.len() - 1 {
            spans.push(Span::raw(", "));
        }
    }
    spans.push(Span::raw("]"));
    spans
}

/// Returns `(line, scrolled)`. `scrolled=true` means the title+author tail
/// got a marquee treatment due to overflow — caller keeps the ticker alive.
fn render_issue_line(
    issue: &GhIssue,
    selected: bool,
    labels_pad_width: usize,
    content_width: usize,
    marquee_frame: Option<u64>,
) -> (Line<'static>, bool) {
    let indicator = if selected { "▸ " } else { "  " };
    let state_color = match issue.state.as_str() {
        "OPEN" => Color::Green,
        "CLOSED" => Color::Red,
        _ => Color::Gray,
    };
    let style = if selected {
        Style::default().fg(Color::Cyan).bold()
    } else {
        Style::default()
    };

    let mut spans = vec![
        Span::styled(indicator.to_string(), style),
        Span::styled(
            format!("#{:<5} ", issue.number),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("{:<6}", issue.state.to_lowercase()),
            Style::default().fg(state_color),
        ),
    ];
    spans.extend(label_spans(&issue.labels));
    let used = labels_display_width(&issue.labels);
    if labels_pad_width > used {
        spans.push(Span::raw(" ".repeat(labels_pad_width - used)));
    }
    spans.push(Span::raw(" "));

    let tail = format!("{}  @{}", issue.title, issue.author.login);
    // 2 (indicator) + 7 (#N block `#XXXXX `) + 6 (state `{:<6}`) + labels_pad + 1 (space)
    let prefix_width = 2 + 7 + 6 + labels_pad_width + 1;
    let (tail_spans, scrolled) = tail_spans(
        &tail,
        content_width.saturating_sub(prefix_width),
        marquee_frame,
        style,
    );
    spans.extend(tail_spans);
    (Line::from(spans), scrolled)
}

/// Render `title  @author` (or similar) either truncated/untouched when not
/// overflowing, or scrolled via marquee when selected + overflow + frame.
fn tail_spans(
    tail: &str,
    available: usize,
    marquee_frame: Option<u64>,
    style_title: Style,
) -> (Vec<Span<'static>>, bool) {
    let tail_width = console::measure_text_width(tail);
    if available == 0 {
        return (vec![], false);
    }
    if tail_width > available {
        if let Some(frame) = marquee_frame {
            let slice = crate::widget::marquee::scroll_window(tail, available, frame);
            return (vec![Span::styled(slice.text, style_title)], true);
        }
        // Non-selected overflow row: truncate with ellipsis
        let truncated = console::truncate_str(tail, available, "…").to_string();
        return (vec![Span::styled(truncated, style_title)], false);
    }
    (vec![Span::styled(tail.to_string(), style_title)], false)
}

fn render_pr_line(
    pr: &GhPullRequest,
    selected: bool,
    labels_pad_width: usize,
    content_width: usize,
    marquee_frame: Option<u64>,
) -> (Line<'static>, bool) {
    let indicator = if selected { "▸ " } else { "  " };
    let (state_color, state_label) = if pr.is_draft {
        (Color::Gray, "draft".to_string())
    } else {
        let color = match pr.state.as_str() {
            "OPEN" => Color::Green,
            "CLOSED" => Color::Red,
            "MERGED" => Color::Magenta,
            _ => Color::Gray,
        };
        (color, pr.state.to_lowercase())
    };
    let style = if selected {
        Style::default().fg(Color::Cyan).bold()
    } else {
        Style::default()
    };

    let mut spans = vec![
        Span::styled(indicator.to_string(), style),
        Span::styled(
            format!("#{:<5} ", pr.number),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("{state_label:<6}"),
            Style::default().fg(state_color),
        ),
    ];
    spans.extend(label_spans(&pr.labels));
    let used = labels_display_width(&pr.labels);
    if labels_pad_width > used {
        spans.push(Span::raw(" ".repeat(labels_pad_width - used)));
    }
    spans.push(Span::raw(" "));

    let tail = format!("{}  ← {}  @{}", pr.title, pr.head_ref_name, pr.author.login);
    let prefix_width = 2 + 7 + 6 + labels_pad_width + 1;
    let (tail_spans, scrolled) = tail_spans(
        &tail,
        content_width.saturating_sub(prefix_width),
        marquee_frame,
        style,
    );
    spans.extend(tail_spans);
    (Line::from(spans), scrolled)
}

/// Sum of the visible cells occupied by `label_spans(labels)`: `" [a, b]"`.
fn labels_display_width(labels: &[crate::github::GhLabel]) -> usize {
    if labels.is_empty() {
        return 0;
    }
    let names: usize = labels
        .iter()
        .map(|l| console::measure_text_width(&l.name))
        .sum();
    let seps = labels.len().saturating_sub(1) * 2; // ", "
                                                   // " [" + names + seps + "]"
    3 + names + seps
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    use crate::{
        color::ColorTheme,
        config::{CoreConfig, UiConfig},
        github::{GhAuthor, GhCommit, GhStatusCheckRollup},
        keybind::KeyBind,
        protocol::ImageProtocol,
    };

    const TERM_W: u16 = 60;
    const TERM_H: u16 = 20;
    const LAST_MARKER: &str = "尾端標記";

    fn test_ctx() -> Rc<AppContext> {
        Rc::new(AppContext {
            keybind: KeyBind::new(None),
            core_config: CoreConfig::default(),
            ui_config: UiConfig::default(),
            color_theme: ColorTheme::default(),
            image_protocol: ImageProtocol::Text,
        })
    }

    /// A body long enough that every source line wraps several times at the
    /// preview width — the condition the old slice-then-wrap code got wrong.
    fn long_body() -> String {
        let mut body = String::new();
        for i in 0..10 {
            body.push_str(&format!("第{i}行內容刻意寫得很長好觸發折行折行折行折行\n"));
        }
        body.push_str(LAST_MARKER);
        body
    }

    fn view_with_long_body() -> GitHubView<'static> {
        view_with_body(long_body())
    }

    fn view_with_body(body: String) -> GitHubView<'static> {
        let pr = GhPullRequest {
            number: 1,
            title: "t".to_string(),
            state: "OPEN".to_string(),
            labels: Vec::new(),
            author: GhAuthor {
                login: "alice".to_string(),
            },
            head_ref_name: "topic".to_string(),
            base_ref_name: "main".to_string(),
            is_draft: false,
            body,
            url: String::new(),
            closed_at: None,
            updated_at: String::new(),
            linked_issues: Vec::new(),
        };

        let (tx, _rx) = Sender::channel_for_test();
        let mut view = GitHubView::new(
            View::Default,
            Vec::new(),
            vec![pr],
            None,
            None,
            "open",
            test_ctx(),
            tx,
        );
        view.active_tab = GitHubTab::PullRequests;
        view
    }

    fn render_to_string(view: &mut GitHubView<'_>) -> String {
        let mut terminal = Terminal::new(TestBackend::new(TERM_W, TERM_H)).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                view.render(f, area, 0);
            })
            .unwrap();
        // `TestBackend`'s Display skips the blank filler cell that follows a
        // double-width glyph, so CJK text comes back contiguous.
        terminal.backend().to_string()
    }

    #[test]
    fn preview_scrolls_all_the_way_to_the_last_line() {
        let mut view = view_with_long_body();
        // First render populates `last_preview_len` (visual lines).
        render_to_string(&mut view);
        // Ask for far more scroll than exists; render clamps it to the bottom.
        view.preview_offset = usize::MAX / 2;
        let screen = render_to_string(&mut view);

        assert!(
            screen.contains(LAST_MARKER),
            "bottom of the body must be reachable, got:\n{screen}"
        );
    }

    #[test]
    fn preview_offset_is_clamped_to_visual_lines() {
        let mut view = view_with_long_body();
        render_to_string(&mut view);
        let total = view.last_preview_len;

        view.preview_offset = usize::MAX / 2;
        render_to_string(&mut view);

        let expected = total.saturating_sub(view.preview_height);
        assert_eq!(view.preview_offset, expected);

        // The clamp must be in wrapped lines, not source lines. Comparing
        // against the cache's own source count is what gives this test teeth:
        // the old logical-line arithmetic made the two equal.
        let source_lines = view.preview_cache.as_ref().map_or(0, |c| c.lines.len());
        assert!(
            total > source_lines,
            "last_preview_len must count wrapped lines ({total}) not source lines ({source_lines})"
        );
    }

    #[test]
    fn preview_cache_is_reused_until_an_input_changes() {
        let mut view = view_with_long_body();
        render_to_string(&mut view);
        let key = view.preview_cache.as_ref().map(|c| c.key);
        assert!(key.is_some());

        render_to_string(&mut view);
        assert_eq!(view.preview_cache.as_ref().map(|c| c.key), key);

        // A body swap must invalidate it even though the PR number is unchanged.
        view.update_body_for_item(1, GhItemKind::PullRequest, "short".to_string());
        render_to_string(&mut view);
        assert_ne!(view.preview_cache.as_ref().map(|c| c.key), key);
    }

    #[test]
    fn preview_cache_invalidates_when_comments_load_empty() {
        // Short body so the comment section is on screen without scrolling.
        let mut view = view_with_body("short".to_string());
        let screen = render_to_string(&mut view);
        assert!(screen.contains("loading comments"), "got:\n{screen}");

        // Zero comments, but *loaded* — item count stays 0, so only the stage
        // distinguishes this from the pending state.
        view.append_timeline_items(1, GhItemKind::PullRequest, Vec::new(), None);
        let screen = render_to_string(&mut view);

        assert!(
            !screen.contains("loading comments"),
            "preview must leave the loading state, got:\n{screen}"
        );
        assert!(screen.contains("no comments"), "got:\n{screen}");
    }

    fn timeline_comment(login: &str, body: &str) -> GhTimelineItem {
        GhTimelineItem::IssueComment {
            body: body.to_string(),
            created_at: "2026-07-27".to_string(),
            author: Some(GhAuthor {
                login: login.to_string(),
            }),
        }
    }

    fn timeline_commit(oid: &str, state: &str) -> GhTimelineItem {
        GhTimelineItem::PullRequestCommit {
            commit: GhCommit {
                abbreviated_oid: oid.to_string(),
                message_headline: format!("headline for {oid}"),
                status_check_rollup: Some(GhStatusCheckRollup {
                    state: state.to_string(),
                }),
            },
        }
    }

    #[test]
    fn dividers_are_colour_coded_by_section() {
        let mut view = view_with_body("body".to_string());
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            vec![
                timeline_commit("aaaaaaa", "SUCCESS"),
                timeline_comment("a", "one"),
                timeline_commit("bbbbbbb", "SUCCESS"),
            ],
            None,
        );

        let (lines, _) = build_preview_content(&view.preview_input(40));
        let dividers: Vec<(char, Option<Color>)> = lines
            .iter()
            .filter_map(|l| {
                let text: String = l.spans.iter().map(|s| s.content.to_string()).collect();
                let first = text.chars().next()?;
                if first != '─' {
                    return None;
                }
                Some((first, l.style.fg))
            })
            .collect();

        assert_eq!(
            dividers,
            vec![
                // meta → body: markdown's own grey, unchanged
                ('─', Some(Color::DarkGray)),
                // body → first commit
                ('─', Some(Section::Body.color())),
                // commit → comment
                ('─', Some(Section::Commit.color())),
                // comment → commit
                ('─', Some(Section::Comment.color())),
            ],
        );
    }

    #[test]
    fn preview_cache_invalidates_when_more_comments_start_loading() {
        let mut view = view_with_body("short".to_string());
        // One page in, with another page available.
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            vec![timeline_comment("bob", "hi")],
            Some("cursor".to_string()),
        );
        let screen = render_to_string(&mut view);
        // Short fragment: the full footer wraps at this width.
        assert!(screen.contains("more comments"), "got:\n{screen}");

        // Fetching the next page changes only `loading_more` — no item count,
        // no stage change — so the footer only updates if the key tracks it.
        view.preview_offset = usize::MAX / 2;
        view.maybe_load_more_timeline();
        let screen = render_to_string(&mut view);

        assert!(
            screen.contains("loading more"),
            "footer must follow loading_more, got:\n{screen}"
        );
    }

    fn view_with_issue(body: String) -> GitHubView<'static> {
        let issue = GhIssue {
            number: 1,
            title: "t".to_string(),
            state: "OPEN".to_string(),
            labels: Vec::new(),
            author: GhAuthor {
                login: "alice".to_string(),
            },
            created_at: String::new(),
            body,
            url: String::new(),
            closed_at: None,
            updated_at: String::new(),
            parent: None,
            sub_issues: Vec::new(),
        };

        let (tx, _rx) = Sender::channel_for_test();
        let mut view = GitHubView::new(
            View::Default,
            vec![issue],
            Vec::new(),
            None,
            None,
            "open",
            test_ctx(),
            tx,
        );
        view.active_tab = GitHubTab::Issues;
        view
    }

    /// The Issues tab shares every code path with PRs from `TimelineEntry`
    /// down — this pins down that the shared `timelineItems` plumbing still
    /// renders an Issue's body and comments the same way it did before 3b.
    #[test]
    fn issue_timeline_renders_like_pr_timeline() {
        let mut view = view_with_issue("issue body".to_string());
        view.append_timeline_items(
            1,
            GhItemKind::Issue,
            vec![timeline_comment("carol", "issue comment")],
            None,
        );

        let screen = render_to_string(&mut view);
        assert!(screen.contains("issue body"), "got:\n{screen}");
        assert!(screen.contains("carol"), "got:\n{screen}");
        assert!(screen.contains("issue comment"), "got:\n{screen}");
    }

    #[test]
    fn empty_timeline_still_draws_the_body_divider() {
        let mut view = view_with_body("body".to_string());
        view.append_timeline_items(1, GhItemKind::PullRequest, Vec::new(), None);

        let (lines, _) = build_preview_content(&view.preview_input(40));
        let has_body_divider = lines.iter().any(|l| {
            let text: String = l.spans.iter().map(|s| s.content.to_string()).collect();
            text.starts_with('─') && l.style.fg == Some(Section::Body.color())
        });
        assert!(
            has_body_divider,
            "loaded-but-empty timeline must still draw the body divider, got: {lines:?}"
        );
    }

    /// A page consisting entirely of nodes `TimelineItem::from_gh` drops
    /// (unrecognized `__typename`) must fall back the same way a genuinely
    /// empty page does — filtering happens *after* `entry.items.is_empty()`
    /// would already have said "not empty".
    #[test]
    fn all_unknown_timeline_still_draws_the_body_divider() {
        let mut view = view_with_body("body".to_string());
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            vec![GhTimelineItem::Unknown, GhTimelineItem::Unknown],
            None,
        );

        let screen = render_to_string(&mut view);
        assert!(screen.contains("no comments"), "got:\n{screen}");

        let (lines, _) = build_preview_content(&view.preview_input(40));
        let has_body_divider = lines.iter().any(|l| {
            let text: String = l.spans.iter().map(|s| s.content.to_string()).collect();
            text.starts_with('─') && l.style.fg == Some(Section::Body.color())
        });
        assert!(
            has_body_divider,
            "an all-Unknown page must still draw the body divider, got: {lines:?}"
        );
    }

    #[test]
    fn commit_ci_marker_covers_all_states() {
        assert_eq!(commit_ci_marker(Some("SUCCESS")).0, "✓ ");
        assert_eq!(commit_ci_marker(Some("FAILURE")).0, "✗ ");
        assert_eq!(commit_ci_marker(Some("ERROR")).0, "✗ ");
        assert_eq!(commit_ci_marker(Some("PENDING")).0, "● ");
        assert_eq!(commit_ci_marker(Some("EXPECTED")).0, "● ");
        // No rollup at all (null in the API): two spaces, not the marker
        // column collapsing, so the oid stays aligned across commits.
        assert_eq!(commit_ci_marker(None).0, "  ");
    }

    #[test]
    fn commit_line_truncates_long_headline_to_width() {
        let width = 20;
        let line = commit_line("abc1234", &"x".repeat(100), Some("SUCCESS"), width);
        let rendered: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(
            console::measure_text_width(&rendered) <= width,
            "line must not exceed width {width}, got {} cells: {rendered:?}",
            console::measure_text_width(&rendered)
        );
    }
}

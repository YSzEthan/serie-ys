use std::cell::Cell;
use std::rc::Rc;

use ratatui::{
    crossterm::event::{Event, KeyEvent},
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Padding, Paragraph, Wrap},
    Frame,
};
use rustc_hash::FxHashMap;
use tui_input::{backend::crossterm::EventHandler, Input};

use crate::{
    app::AppContext,
    event::{AppEvent, RelatedGroup, RelatedItem, Sender, UserEvent, UserEventWithCount},
    fuzzy::SearchMatcher,
    github::{self, CheckboxItem, GhComment, GhIssue, GhItemKind, GhPullRequest, IssueAction},
    view::View,
};

const PREFETCH_THRESHOLD: usize = 5;
const COMMENTS_LOAD_MORE_THRESHOLD: usize = 5;

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
enum CommentLoad {
    #[default]
    NotRequested,
    Loading,
    Loaded,
    Error(String),
}

#[derive(Debug, Default)]
struct CommentEntry {
    state: CommentLoad,
    items: Vec<GhComment>,
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

    comments: FxHashMap<(GhItemKind, u64), CommentEntry>,
    last_preview_len: usize,

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
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> GitHubView<'a> {
        let load_state = if issues.is_empty() && pull_requests.is_empty() {
            LoadState::Loading
        } else {
            LoadState::Idle
        };
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
            state_filter: StateFilter::Open,
            task_panel: None,
            load_state,
            flash_message: None,
            selected_row_overflows: Cell::new(false),
            issues_next_cursor,
            prs_next_cursor,
            loading_more: false,
            request_generation: 0,
            pending_jump: None,
            comments: FxHashMap::default(),
            last_preview_len: 0,
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
        self.comments.clear();
        self.bump_generation();
        // 修正選取索引避免越界
        let max = self.current_list_len().saturating_sub(1);
        if self.selected_index > max {
            self.selected_index = max;
        }
        self.preview_offset = 0;
        self.request_comments_for_selected();
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
            self.request_comments_for_selected();
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

    fn request_comments_for_selected(&mut self) {
        let Some((number, kind)) = self.selected_number_and_kind() else {
            return;
        };
        let entry = self.comments.entry((kind, number)).or_default();
        if matches!(entry.state, CommentLoad::NotRequested) {
            entry.state = CommentLoad::Loading;
            self.tx.send(AppEvent::LoadGitHubComments {
                number,
                kind,
                after: None,
            });
        }
    }

    fn maybe_load_more_comments(&mut self) {
        let visible = self.height.saturating_sub(2);
        let near_bottom = self
            .preview_offset
            .saturating_add(visible)
            .saturating_add(COMMENTS_LOAD_MORE_THRESHOLD)
            >= self.last_preview_len;
        if !near_bottom {
            return;
        }
        let Some((number, kind)) = self.selected_number_and_kind() else {
            return;
        };
        let Some(entry) = self.comments.get_mut(&(kind, number)) else {
            return;
        };
        if entry.state != CommentLoad::Loaded || entry.loading_more || entry.next_cursor.is_none() {
            return;
        }
        let cursor = entry.next_cursor.clone();
        entry.loading_more = true;
        self.tx.send(AppEvent::LoadGitHubComments {
            number,
            kind,
            after: cursor,
        });
    }

    pub fn append_comments(
        &mut self,
        number: u64,
        kind: GhItemKind,
        items: Vec<GhComment>,
        next_cursor: Option<String>,
    ) {
        let entry = self.comments.entry((kind, number)).or_default();
        entry.items.extend(items);
        entry.next_cursor = next_cursor;
        entry.state = CommentLoad::Loaded;
        entry.loading_more = false;
    }

    pub fn set_comments_error(&mut self, number: u64, kind: GhItemKind, error: String) {
        let entry = self.comments.entry((kind, number)).or_default();
        entry.state = CommentLoad::Error(error);
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
                if self.selected_has_related() {
                    hints.push((UserEvent::DetailPaneToggle, "related"));
                }
                if let Some(h) = self.action_hint() {
                    hints.push(h);
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
                let mut hints = vec![
                    (UserEvent::RefList, "switch tab"),
                    (UserEvent::Search, "search"),
                    (UserEvent::Confirm, "preview"),
                    (UserEvent::Refresh, "refresh"),
                    (UserEvent::Filter, "filter"),
                    (UserEvent::ShortCopy, "copy url / C open / v #num"),
                ];
                if self.selected_has_related() {
                    hints.push((UserEvent::DetailPaneToggle, "related"));
                }
                if let Some(h) = self.action_hint() {
                    hints.push(h);
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
            self.request_comments_for_selected();
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
                self.maybe_load_more_comments();
            }
            UserEvent::NavigateUp | UserEvent::SelectUp => {
                for _ in 0..count {
                    self.preview_offset = self.preview_offset.saturating_sub(1);
                }
            }
            UserEvent::PageDown => {
                let page = self.height.saturating_sub(2).max(1);
                self.preview_offset = self.preview_offset.saturating_add(page);
                self.maybe_load_more_comments();
            }
            UserEvent::PageUp => {
                let page = self.height.saturating_sub(2).max(1);
                self.preview_offset = self.preview_offset.saturating_sub(page);
            }
            UserEvent::HalfPageDown => {
                let half = self.height.saturating_sub(2).max(1) / 2;
                self.preview_offset = self.preview_offset.saturating_add(half);
                self.maybe_load_more_comments();
            }
            UserEvent::HalfPageUp => {
                let half = self.height.saturating_sub(2).max(1) / 2;
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
            UserEvent::ToggleIssueState if matches!(self.active_tab, GitHubTab::Issues) => {
                self.trigger_toggle_issue();
            }
            UserEvent::MergePr if matches!(self.active_tab, GitHubTab::PullRequests) => {
                self.try_merge_selected_pr();
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
            UserEvent::ToggleIssueState if matches!(self.active_tab, GitHubTab::Issues) => {
                self.trigger_toggle_issue();
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

    fn trigger_toggle_issue(&self) {
        let idx = self.actual_index(self.selected_index);
        let Some(issue) = self.issues.get(idx) else {
            return;
        };
        let action = if issue.state == "OPEN" {
            IssueAction::Close
        } else {
            IssueAction::Reopen
        };
        self.tx.send(AppEvent::OpenToggleIssuePrompt {
            number: issue.number,
            action,
            filter_state: self.state_filter.as_str().to_string(),
        });
    }

    fn action_hint(&self) -> Option<(UserEvent, &'static str)> {
        match self.active_tab {
            GitHubTab::PullRequests => {
                let idx = self.actual_index(self.selected_index);
                self.pull_requests.get(idx).and_then(|pr| {
                    (!pr.is_draft && pr.state == "OPEN").then_some((UserEvent::MergePr, "merge PR"))
                })
            }
            GitHubTab::Issues => self
                .selected_issue_action_label()
                .map(|label| (UserEvent::ToggleIssueState, label)),
        }
    }

    fn selected_issue_action_label(&self) -> Option<&'static str> {
        let idx = self.actual_index(self.selected_index);
        self.issues.get(idx).map(|i| {
            if i.state == "OPEN" {
                "close issue"
            } else {
                "reopen issue"
            }
        })
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

        let (preview_lines, overlays) = self.build_preview_content();
        self.last_preview_len = preview_lines.len();

        // Clamp preview_offset to avoid scrolling past content
        let max_offset = preview_lines.len().saturating_sub(inner.height as usize);
        self.preview_offset = self.preview_offset.min(max_offset);

        let visible: Vec<Line> = preview_lines
            .into_iter()
            .skip(self.preview_offset)
            .take(inner.height as usize)
            .collect();

        let paragraph = Paragraph::new(visible).wrap(Wrap { trim: false });
        f.render_widget(paragraph, inner);

        // Overlay `#N` cells with OSC 8 hyperlinks. Must run after Paragraph
        // render so we overwrite the pre-drawn plain `#N` glyph.
        let buf = f.buffer_mut();
        for ov in &overlays {
            let Some(rel) = ov.line_idx.checked_sub(self.preview_offset) else {
                continue;
            };
            if rel as u16 >= inner.height {
                continue;
            }
            let y = inner.top() + rel as u16;
            let x = inner.left().saturating_add(ov.x_offset);
            if x >= inner.right() {
                continue;
            }
            let payload = crate::external::format_osc8_hyperlink(&ov.url, &ov.label);
            let label_width = console::measure_text_width(&ov.label) as u16;
            buf[(x, y)].set_symbol(&payload);
            let remaining = inner.right() - x;
            for i in 1..label_width.min(remaining) {
                buf[(x + i, y)].set_skip(true);
            }
        }
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

    /// Extract common fields from the selected issue or PR for preview rendering.
    #[allow(clippy::type_complexity)]
    fn selected_item_fields(
        &self,
    ) -> Option<(&str, &str, &str, &[crate::github::GhLabel], &str, u64, &str)> {
        let idx = self.actual_index(self.selected_index);
        match self.active_tab {
            GitHubTab::Issues => self.issues.get(idx).map(|i| {
                (
                    i.title.as_str(),
                    i.state.as_str(),
                    i.author.login.as_str(),
                    i.labels.as_slice(),
                    i.body.as_str(),
                    i.number,
                    i.url.as_str(),
                )
            }),
            GitHubTab::PullRequests => self.pull_requests.get(idx).map(|p| {
                (
                    p.title.as_str(),
                    p.state.as_str(),
                    p.author.login.as_str(),
                    p.labels.as_slice(),
                    p.body.as_str(),
                    p.number,
                    p.url.as_str(),
                )
            }),
        }
    }

    fn build_preview_content(&self) -> (Vec<Line<'static>>, Vec<PreviewOverlay>) {
        let mut overlays: Vec<PreviewOverlay> = Vec::new();
        let Some((title, state, author, labels, body, number, url)) = self.selected_item_fields()
        else {
            return (
                vec![Line::styled(
                    "(no item selected)",
                    Style::default().fg(Color::DarkGray),
                )],
                overlays,
            );
        };

        let title = title.to_string();
        let state = state.to_string();
        let author = author.to_string();
        let body = body.to_string();
        let url = url.to_string();
        let labels: Vec<crate::github::GhLabel> = labels.to_vec();

        let mut lines = Vec::new();

        // Header: #number title  (#N hyperlink overlay at x=0)
        if !url.is_empty() {
            overlays.push(PreviewOverlay {
                line_idx: lines.len(),
                x_offset: 0,
                url: url.clone(),
                label: format!("#{number}"),
            });
        }
        lines.push(Line::from(vec![
            Span::styled(format!("#{number} "), Style::default().fg(Color::DarkGray)),
            Span::styled(
                title,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));

        let mut meta_spans = vec![
            Span::styled(
                state.to_lowercase(),
                Style::default().fg(state_color(&state)),
            ),
            Span::styled(format!("  @{author}"), Style::default().fg(Color::DarkGray)),
        ];
        if !labels.is_empty() {
            meta_spans.push(Span::raw("  "));
            meta_spans.extend(label_spans(&labels));
        }
        lines.push(Line::from(meta_spans));

        if let GitHubTab::PullRequests = self.active_tab {
            let idx = self.actual_index(self.selected_index);
            if let Some(pr) = self.pull_requests.get(idx) {
                lines.push(Line::from(vec![
                    Span::styled(pr.base_ref_name.clone(), Style::default().fg(Color::Cyan)),
                    Span::styled("  ←  ", Style::default().fg(Color::DarkGray)),
                    Span::styled(pr.head_ref_name.clone(), Style::default().fg(Color::Cyan)),
                ]));
            }
        }

        lines.push(Line::styled(
            "─".repeat(40),
            Style::default().fg(Color::DarkGray),
        ));

        if let GitHubTab::Issues = self.active_tab {
            let idx = self.actual_index(self.selected_index);
            if let Some(issue) = self.issues.get(idx) {
                append_relation_lines(&mut lines, &mut overlays, issue);
            }
        }

        if body.is_empty() {
            lines.push(Line::styled(
                "(no body)",
                Style::default().fg(Color::DarkGray),
            ));
        } else {
            lines.extend(super::markdown::render(&body));
        }

        let entry = self
            .selected_number_and_kind()
            .and_then(|(n, k)| self.comments.get(&(k, n)));
        append_comment_lines(&mut lines, &mut overlays, entry);

        (lines, overlays)
    }

    fn clear_image_area(&self, area: Rect) {
        for y in area.top()..area.bottom() {
            self.ctx.image_protocol.clear_line(y);
        }
    }
}

fn append_comment_lines(
    lines: &mut Vec<Line<'static>>,
    overlays: &mut Vec<PreviewOverlay>,
    entry: Option<&CommentEntry>,
) {
    lines.push(Line::styled(
        "─".repeat(40),
        Style::default().fg(Color::DarkGray),
    ));

    let entry = match entry {
        Some(e) => e,
        None => {
            lines.push(Line::styled(
                "(loading comments…)",
                Style::default().fg(Color::DarkGray),
            ));
            return;
        }
    };

    match &entry.state {
        CommentLoad::NotRequested | CommentLoad::Loading => {
            lines.push(Line::styled(
                "(loading comments…)",
                Style::default().fg(Color::DarkGray),
            ));
            return;
        }
        CommentLoad::Error(e) => {
            lines.push(Line::styled(
                format!("(comments failed: {e})"),
                Style::default().fg(Color::Red),
            ));
            return;
        }
        CommentLoad::Loaded => {}
    }

    if entry.items.is_empty() {
        lines.push(Line::styled(
            "(no comments)",
            Style::default().fg(Color::DarkGray),
        ));
        return;
    }

    for (i, c) in entry.items.iter().enumerate() {
        let header_idx = lines.len();
        if !c.url.is_empty() {
            overlays.push(PreviewOverlay {
                line_idx: header_idx,
                x_offset: 0,
                url: c.url.clone(),
                label: format!("@{}", c.author.login),
            });
        }
        lines.push(Line::from(vec![
            Span::styled(
                format!("@{}", c.author.login),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {}", c.created_at),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
        lines.extend(super::markdown::render(&c.body));
        if i + 1 < entry.items.len() {
            lines.push(Line::styled(
                "·".repeat(20),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }

    if entry.next_cursor.is_some() {
        let suffix = if entry.loading_more {
            "(loading more…)"
        } else {
            "(more comments — scroll down to load)"
        };
        lines.push(Line::styled(suffix, Style::default().fg(Color::DarkGray)));
    }
}

#[derive(Debug, Clone)]
struct PreviewOverlay {
    line_idx: usize,
    x_offset: u16,
    url: String,
    label: String,
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
    overlays: &mut Vec<PreviewOverlay>,
    issue: &GhIssue,
) {
    if let Some(ref parent) = issue.parent {
        let prefix = "Parent: ";
        if !parent.url.is_empty() {
            overlays.push(PreviewOverlay {
                line_idx: lines.len(),
                x_offset: console::measure_text_width(prefix) as u16,
                url: parent.url.clone(),
                label: format!("#{}", parent.number),
            });
        }
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
    if !issue.sub_issues.is_empty() {
        let indent = "  ";
        lines.push(Line::styled(
            format!("Sub-issues ({}):", issue.sub_issues.len()),
            Style::default().fg(Color::DarkGray),
        ));
        for sub in &issue.sub_issues {
            if !sub.url.is_empty() {
                overlays.push(PreviewOverlay {
                    line_idx: lines.len(),
                    x_offset: console::measure_text_width(indent) as u16,
                    url: sub.url.clone(),
                    label: format!("#{}", sub.number),
                });
            }
            lines.push(related_issue_line(indent, sub));
        }
    }
    if issue.parent.is_some() || !issue.sub_issues.is_empty() {
        lines.push(Line::styled(
            "─".repeat(40),
            Style::default().fg(Color::DarkGray),
        ));
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

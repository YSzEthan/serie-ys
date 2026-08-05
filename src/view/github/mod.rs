mod event;
mod preview;
mod render;
mod timeline;

use std::cell::Cell;

use ratatui::{style::Color, text::Line};
use rustc_hash::FxHashMap;
use tui_input::Input;

use crate::{
    event::{AppEvent, Sender, UserEvent},
    github::{
        CheckboxItem, GhIssue, GhItemKind, GhPullRequest, GhTimelinePage, PrDraftAction,
        StateAction,
    },
    view::View,
};

use preview::{PreviewCache, PreviewInput, SelectedItem, SelectedItemExtra};
use timeline::{TimelineEntry, TimelineLoad};

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
    /// Whether the commit log shows individually or as one collapsed summary
    /// line. All-or-nothing (`z` toggles the whole log), not per-commit.
    expand_commits: bool,

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
            expand_commits: true,
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

    pub fn append_timeline_items(&mut self, number: u64, kind: GhItemKind, page: GhTimelinePage) {
        let entry = self.timeline.entry((kind, number)).or_default();
        entry.items.extend(page.items);
        entry.next_cursor = page.next_cursor;
        entry.mergeable = page.mergeable;
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
                hints.extend(self.commit_log_hint());
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
                hints.extend(self.commit_log_hint());
                if self.selected_has_related() {
                    hints.push((UserEvent::DetailPaneToggle, "related"));
                }
                hints.push((UserEvent::GitHubToggle, "close"));
                hints
            }
        }
    }

    /// Collapsing/expanding doesn't reset `preview_offset` — unlike the ~20
    /// sites that reset it on navigation, this is a content-density toggle,
    /// not a "you're looking at something else now" moment. The existing
    /// clamp in `render_preview` keeps it from scrolling past the new end.
    fn toggle_commit_log(&mut self) {
        self.expand_commits = !self.expand_commits;
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

    /// Unlike `action_hints`, not gated on `state == "OPEN"` — a closed or
    /// merged PR still has a commit log worth collapsing.
    fn commit_log_hint(&self) -> Option<(UserEvent, &'static str)> {
        if !matches!(self.active_tab, GitHubTab::PullRequests) {
            return None;
        }
        let label = if self.expand_commits {
            "collapse commits"
        } else {
            "expand commits"
        };
        Some((UserEvent::ToggleCommitLog, label))
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
            expand_commits: self.expand_commits,
            item,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    use crate::github::{GhAuthor, GhCommit, GhStatusCheckRollup, GhTimelineItem, Mergeable};

    use super::preview::build_preview_content;

    const TERM_W: u16 = 60;
    const TERM_H: u16 = 20;
    const LAST_MARKER: &str = "尾端標記";

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
        let mut view = GitHubView::new(View::Default, Vec::new(), vec![pr], None, None, "open", tx);
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

    fn timeline_page(items: Vec<GhTimelineItem>, next_cursor: Option<String>) -> GhTimelinePage {
        GhTimelinePage {
            items,
            next_cursor,
            mergeable: None,
        }
    }

    #[test]
    fn preview_cache_invalidates_when_comments_load_empty() {
        // Short body so the comment section is on screen without scrolling.
        let mut view = view_with_body("short".to_string());
        let screen = render_to_string(&mut view);
        assert!(screen.contains("loading comments"), "got:\n{screen}");

        // Zero comments, but *loaded* — item count stays 0, so only the stage
        // distinguishes this from the pending state.
        view.append_timeline_items(1, GhItemKind::PullRequest, timeline_page(Vec::new(), None));
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
            timeline_page(
                vec![
                    timeline_commit("aaaaaaa", "SUCCESS"),
                    timeline_comment("a", "one"),
                    timeline_commit("bbbbbbb", "SUCCESS"),
                ],
                None,
            ),
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
            timeline_page(
                vec![timeline_comment("bob", "hi")],
                Some("cursor".to_string()),
            ),
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
            timeline_page(vec![timeline_comment("carol", "issue comment")], None),
        );

        let screen = render_to_string(&mut view);
        assert!(screen.contains("issue body"), "got:\n{screen}");
        assert!(screen.contains("carol"), "got:\n{screen}");
        assert!(screen.contains("issue comment"), "got:\n{screen}");
    }

    #[test]
    fn empty_timeline_still_draws_the_body_divider() {
        let mut view = view_with_body("body".to_string());
        view.append_timeline_items(1, GhItemKind::PullRequest, timeline_page(Vec::new(), None));

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
            timeline_page(vec![GhTimelineItem::Unknown, GhTimelineItem::Unknown], None),
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

    fn rendered_mergeable_marker(view: &GitHubView<'_>) -> Option<(String, Option<Color>)> {
        let (lines, _) = build_preview_content(&view.preview_input(40));
        lines.iter().find_map(|l| {
            l.spans
                .iter()
                .find(|s| s.content.contains("(mergeable)") || s.content.contains("(conflicts)"))
                .map(|s| (s.content.to_string(), s.style.fg))
        })
    }

    #[test]
    fn mergeable_state_renders_into_the_base_head_line() {
        let mut view = view_with_body("body".to_string());
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            GhTimelinePage {
                mergeable: Some(Mergeable::Conflicting),
                ..timeline_page(Vec::new(), None)
            },
        );
        assert_eq!(
            rendered_mergeable_marker(&view),
            Some(("  (conflicts)".to_string(), Some(Color::Red)))
        );
    }

    /// mergeable rides along on `TimelineEntry`, not a separate field on
    /// `pull_requests[idx]` — this pins down that the cache key tracks it in
    /// isolation. The page is loaded *before* capturing `key_before`, so the
    /// only thing that changes between the two snapshots is `mergeable`
    /// itself — otherwise `stage` flipping Pending → Ready on first load
    /// would change the key regardless of whether `mergeable` was tracked.
    #[test]
    fn cache_key_changes_once_mergeable_arrives() {
        let mut view = view_with_body("body".to_string());
        view.append_timeline_items(1, GhItemKind::PullRequest, timeline_page(Vec::new(), None));
        render_to_string(&mut view);
        let key_before = view.preview_cache.as_ref().map(|c| c.key);

        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            GhTimelinePage {
                mergeable: Some(Mergeable::Mergeable),
                ..timeline_page(Vec::new(), None)
            },
        );
        render_to_string(&mut view);
        let key_after = view.preview_cache.as_ref().map(|c| c.key);

        assert_ne!(key_before, key_after);
    }

    #[test]
    fn collapsing_commits_replaces_them_with_a_summary_and_changes_the_key() {
        let mut view = view_with_body("body".to_string());
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            timeline_page(
                vec![
                    timeline_commit("aaaaaaa", "SUCCESS"),
                    timeline_comment("carol", "one"),
                    timeline_commit("bbbbbbb", "SUCCESS"),
                ],
                None,
            ),
        );
        render_to_string(&mut view);
        let key_expanded = view.preview_cache.as_ref().map(|c| c.key);

        view.toggle_commit_log();
        let screen = render_to_string(&mut view);
        let key_collapsed = view.preview_cache.as_ref().map(|c| c.key);

        assert!(!screen.contains("aaaaaaa"), "got:\n{screen}");
        assert!(!screen.contains("bbbbbbb"), "got:\n{screen}");
        assert!(screen.contains("2 commits"), "got:\n{screen}");
        // The comment in between must survive collapsing — only commits fold.
        assert!(screen.contains("one"), "got:\n{screen}");
        assert_ne!(key_expanded, key_collapsed);
    }

    #[test]
    fn toggle_commit_log_has_no_effect_on_issues_tab() {
        let mut view = view_with_issue("issue body".to_string());
        assert!(view.expand_commits);

        view.handle_preview_event(UserEvent::ToggleCommitLog, 1);
        assert!(view.expand_commits, "Issues tab must ignore the toggle");

        view.handle_list_event(UserEvent::ToggleCommitLog, 1);
        assert!(view.expand_commits, "Issues tab must ignore the toggle");

        assert!(
            !view
                .status_hints()
                .iter()
                .any(|(e, _)| *e == UserEvent::ToggleCommitLog),
            "no hint should be offered for a key that does nothing here"
        );
    }

    #[test]
    fn toggle_commit_log_flips_expand_commits_on_pr_tab() {
        let mut view = view_with_body("body".to_string());
        assert!(view.expand_commits);

        view.handle_preview_event(UserEvent::ToggleCommitLog, 1);
        assert!(!view.expand_commits);

        view.handle_list_event(UserEvent::ToggleCommitLog, 1);
        assert!(view.expand_commits);
    }
}

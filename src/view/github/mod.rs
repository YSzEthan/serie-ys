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
        CheckboxItem, GhIssue, GhItemKind, GhPullRequest, GhTimelinePage, GitHubData,
        PrDraftAction, StateAction, StateFilter,
    },
    view::View,
};

use preview::{PreviewCache, PreviewInput, SelectedItem, SelectedItemExtra};
use timeline::{TimelineEntry, TimelineLoad};

const PREFETCH_THRESHOLD: usize = 5;
const TIMELINE_LOAD_MORE_THRESHOLD: usize = 5;

/// 分隔線關閉的是 timeline 的哪個區段。顏色由分隔線*之前*的內容決定，
/// 不是後面的——由上往下讀時，那才是眼睛在捲動時需要的上下文。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Body,
    Comment,
    Commit,
}

impl Section {
    /// 用 Indexed 而非 Rgb，讓這些顏色在不支援 truecolor 的終端機上也能顯示。
    fn color(self) -> Color {
        match self {
            Section::Body => Color::Indexed(146),    // 粉藍灰 (#afafd7)
            Section::Comment => Color::Indexed(151), // 粉綠灰 (#afd7af)
            Section::Commit => Color::Indexed(186),  // 粉黃灰 (#d7d787)
        }
    }

    fn divider(self, width: usize) -> Line<'static> {
        super::markdown::rule_line_colored(width, self.color())
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

    /// 渲染時的溢出旗標（選取列的 title+author 寬度超過可用空間）。
    /// App 讀取這個值來決定要不要跳動 `marquee_frame`。
    selected_row_overflows: Cell<bool>,

    issues_next_cursor: Option<String>,
    prs_next_cursor: Option<String>,
    loading_more: bool,
    request_generation: u64,

    pending_jump: Option<u64>,

    timeline: FxHashMap<(GhItemKind, u64), TimelineEntry>,
    last_preview_len: usize,

    /// 在 preview 能顯示的任何就地編輯時遞增——目前是 body 取代與批次重新
    /// 載入。`set_pr_draft_flag` 刻意不遞增，因為 `is_draft` 從未傳到
    /// `build_preview_content`；等哪天 preview 開始顯示 draft 狀態，這點就要改。
    body_rev: u64,
    preview_cache: PreviewCache,
    /// Preview 內容高度，由 `render_preview` 記錄。捲動處理函式直接用這個值，
    /// 不會從 `height` 重新推算。
    preview_height: usize,
    /// commit log 是逐筆顯示還是收合成單一摘要列。全有全無（`z` 切換整個
    /// log），不是逐筆切換。
    expand_commits: bool,

    tx: Sender,
}

impl<'a> GitHubView<'a> {
    pub fn new(before: View<'a>, data: GitHubData, tx: Sender) -> GitHubView<'a> {
        let load_state = if data.issues.is_empty() && data.pull_requests.is_empty() {
            LoadState::Loading
        } else {
            LoadState::Idle
        };
        GitHubView {
            before,
            focus: GitHubFocus::List,
            active_tab: GitHubTab::Issues,
            issues: data.issues,
            pull_requests: data.pull_requests,
            selected_index: 0,
            offset: 0,
            height: 0,
            preview_offset: 0,
            search_input: Input::default(),
            filtered_issue_indices: Vec::new(),
            filtered_pr_indices: Vec::new(),
            state_filter: data.state_filter,
            task_panel: None,
            load_state,
            flash_message: None,
            selected_row_overflows: Cell::new(false),
            issues_next_cursor: data.issues_next_cursor,
            prs_next_cursor: data.prs_next_cursor,
            loading_more: false,
            request_generation: 0,
            pending_jump: None,
            timeline: FxHashMap::default(),
            last_preview_len: 0,
            body_rev: 0,
            preview_cache: PreviewCache::default(),
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

    /// 交出目前持有的資料快照，供 `App` 在關閉 view 時暫存。比照
    /// [`Self::take_before_view`]：view 即將被丟棄，直接 take 沒有殘留成本。
    pub fn take_data(&mut self) -> GitHubData {
        GitHubData {
            issues: std::mem::take(&mut self.issues),
            pull_requests: std::mem::take(&mut self.pull_requests),
            state_filter: self.state_filter,
            issues_next_cursor: self.issues_next_cursor.take(),
            prs_next_cursor: self.prs_next_cursor.take(),
        }
    }

    pub fn state_filter(&self) -> StateFilter {
        self.state_filter
    }

    pub fn next_cursor(&self, kind: GhItemKind) -> Option<String> {
        match kind {
            GhItemKind::Issue => self.issues_next_cursor.clone(),
            GhItemKind::PullRequest => self.prs_next_cursor.clone(),
        }
    }

    pub fn set_flash(&mut self, msg: String, is_error: bool) {
        self.flash_message = Some((msg, is_error));
    }

    pub fn set_error(&mut self, msg: String) {
        if matches!(self.load_state, LoadState::Loading) {
            self.load_state = LoadState::Error(msg);
        }
    }

    /// 資料跟目前持有的一模一樣時只收尾 Loading 指示器，不重置捲動位置、
    /// 選取列或 timeline 快取——背景自動 refresh 拿到同樣的資料時，
    /// 不該把使用者正在看的位置甩掉。游標仍然要換新：這批資料雖然跟畫面上
    /// 的一樣，但可能是從一個游標已經往前推進的請求抓回來的，沿用舊游標
    /// 會讓「載入更多」拿過期游標重複抓同一頁。
    pub fn update_data(
        &mut self,
        issues: Vec<GhIssue>,
        pull_requests: Vec<GhPullRequest>,
        issues_next_cursor: Option<String>,
        prs_next_cursor: Option<String>,
    ) {
        if self.issues == issues && self.pull_requests == pull_requests {
            self.issues_next_cursor = issues_next_cursor;
            self.prs_next_cursor = prs_next_cursor;
            self.finish_loading();
            return;
        }
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

    fn finish_loading(&mut self) {
        if matches!(self.load_state, LoadState::Loading) {
            self.load_state = LoadState::Idle;
        }
    }

    /// generation 不符（view 關閉重開、或又切換了 filter）就直接丟棄這頁——
    /// 分頁游標是靠 generation 驗證新舊的增量更新，跟 `update_data` 的整批
    /// 快照替換不是同一種東西，見 `App::on_github_data_loaded` 的註解。
    pub fn append_issues(
        &mut self,
        items: Vec<GhIssue>,
        next_cursor: Option<String>,
        generation: u64,
    ) {
        if generation != self.request_generation {
            return;
        }
        self.issues.extend(items);
        self.issues_next_cursor = next_cursor;
        self.loading_more = false;
        if self.pending_jump.is_some() {
            self.try_resolve_jump();
        }
    }

    pub fn append_pull_requests(
        &mut self,
        items: Vec<GhPullRequest>,
        next_cursor: Option<String>,
        generation: u64,
    ) {
        if generation != self.request_generation {
            return;
        }
        self.pull_requests.extend(items);
        self.prs_next_cursor = next_cursor;
        self.loading_more = false;
        if self.pending_jump.is_some() {
            self.try_resolve_jump();
        }
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

    /// 收合／展開不會重置 `preview_offset`——跟另外約 20 個在導覽時會重置
    /// 它的地方不同，這只是內容密度的切換，不是「你現在看的是別的東西了」
    /// 那種時刻。`render_preview` 裡既有的 clamp 會擋住捲過新結尾的情況。
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

    /// 跟 `action_hints` 不同，不受 `state == "OPEN"` 限制——已關閉或已
    /// merge 的 PR 一樣有值得收合的 commit log。
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

    /// 把可視索引對應到實際資料索引（若有篩選則透過篩選對應）
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

    /// 把選取的 issue/PR 及其 timeline entry 一併借出成單一值——這是
    /// `build_preview_content` 與 `PreviewInput::cache_key` 讀取的全部內容，
    /// 兩者不會因為讀到對方不知道的欄位而彼此失準。
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

    /// `App` 在 view 開／關之間交接資料靠的就是這個 round-trip：
    /// 建構子放進去什麼，`take_data` 就該原封不動交還什麼。
    #[test]
    fn take_data_round_trips_everything_the_view_was_constructed_with() {
        let issue = GhIssue {
            number: 7,
            title: "t".to_string(),
            state: "OPEN".to_string(),
            labels: Vec::new(),
            author: GhAuthor {
                login: "alice".to_string(),
            },
            created_at: String::new(),
            body: "b".to_string(),
            url: String::new(),
            closed_at: None,
            updated_at: String::new(),
            parent: None,
            sub_issues: Vec::new(),
        };
        let data = GitHubData {
            issues: vec![issue],
            pull_requests: Vec::new(),
            state_filter: StateFilter::Closed,
            issues_next_cursor: Some("cursor-a".to_string()),
            prs_next_cursor: None,
        };
        let (tx, _rx) = Sender::channel_for_test();
        let mut view = GitHubView::new(View::Default, data, tx);

        let taken = view.take_data();

        assert_eq!(taken.issues.len(), 1);
        assert_eq!(taken.issues[0].number, 7);
        assert!(taken.pull_requests.is_empty());
        assert_eq!(taken.state_filter, StateFilter::Closed);
        assert_eq!(taken.issues_next_cursor.as_deref(), Some("cursor-a"));
        assert_eq!(taken.prs_next_cursor, None);
    }

    /// 資料跟目前持有的一模一樣時（背景 auto-refresh 的常態），游標仍然要
    /// 換新——沿用舊游標會讓「載入更多」拿過期游標重複抓同一頁。
    #[test]
    fn update_data_refreshes_cursor_even_when_items_are_unchanged() {
        let data = GitHubData {
            issues_next_cursor: Some("cursor-a".to_string()),
            ..Default::default()
        };
        let (tx, _rx) = Sender::channel_for_test();
        let mut view = GitHubView::new(View::Default, data, tx);

        view.update_data(Vec::new(), Vec::new(), Some("cursor-b".to_string()), None);

        assert_eq!(
            view.next_cursor(GhItemKind::Issue).as_deref(),
            Some("cursor-b")
        );
    }

    /// 每一行原始內容在 preview 寬度下都會折行好幾次的長 body——這正是
    /// 舊版「先切片再折行」程式碼會出錯的情況。
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
        let data = GitHubData {
            pull_requests: vec![pr],
            ..Default::default()
        };
        let mut view = GitHubView::new(View::Default, data, tx);
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
        // `TestBackend` 的 Display 會跳過雙寬字元後面接的空白填充格，
        // 所以 CJK 文字讀回來會是連續的。
        terminal.backend().to_string()
    }

    #[test]
    fn preview_scrolls_all_the_way_to_the_last_line() {
        let mut view = view_with_long_body();
        // 第一次 render 會填入 `last_preview_len`（視覺行數）。
        render_to_string(&mut view);
        // 要求遠超過實際內容的捲動量；render 會把它限制在底端。
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

        // 限制範圍算的必須是折行後的行數，不是原始行數。跟 cache 自己的
        // 原始行數比對，才是這個測試真正有威力的地方：舊版用邏輯行計算，
        // 會讓兩者算出一樣的值。
        let source_lines = view.preview_cache.lines().len();
        assert!(
            total > source_lines,
            "last_preview_len must count wrapped lines ({total}) not source lines ({source_lines})"
        );
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
        // body 短一點，讓留言區段不用捲動就在畫面上。
        let mut view = view_with_body("short".to_string());
        let screen = render_to_string(&mut view);
        assert!(screen.contains("loading comments"), "got:\n{screen}");

        // 零則留言，但*已載入*——項目數量仍是 0，所以只有 stage 能區分
        // 這跟 pending 狀態的差別。
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
                // meta → body：markdown 自己的灰色，維持不變
                ('─', Some(Color::DarkGray)),
                // body → 第一個 commit
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
        // 已載入一頁，且還有下一頁可以載入。
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            timeline_page(
                vec![timeline_comment("bob", "hi")],
                Some("cursor".to_string()),
            ),
        );
        let screen = render_to_string(&mut view);
        // 只取片段比對：在這個寬度下，完整的 footer 文字會折行。
        assert!(screen.contains("more comments"), "got:\n{screen}");

        // 抓取下一頁只會改變 `loading_more`——項目數量沒變、stage 也沒變——
        // 所以 footer 只有在 key 有追蹤這個欄位時才會更新。
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
        let data = GitHubData {
            issues: vec![issue],
            ..Default::default()
        };
        let mut view = GitHubView::new(View::Default, data, tx);
        view.active_tab = GitHubTab::Issues;
        view
    }

    /// Issues 分頁從 `TimelineEntry` 以下跟 PR 共用每一條程式碼路徑——
    /// 這個測試釘住了共用的 `timelineItems` 管線仍然跟 3b 之前一樣，
    /// 正確渲染 Issue 的 body 與留言。
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

    /// 一頁全部都是 `TimelineItem::from_gh` 會丟棄的節點（無法辨識的
    /// `__typename`）時，必須 fallback 成跟真正空頁一樣的行為——過濾動作
    /// 發生在 `entry.items.is_empty()` 已經判定「不是空的」之*後*。
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

    #[test]
    fn collapsing_commits_replaces_them_with_a_summary() {
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
        // 切換前先把 cache 熱好：冷 cache 一定會無條件重建，不管
        // `expand_commits` 有沒有被 cache key 追蹤，這種情況下就算追蹤機制
        // 壞了，這個測試也照樣會過。
        render_to_string(&mut view);

        view.toggle_commit_log();
        let screen = render_to_string(&mut view);

        assert!(!screen.contains("aaaaaaa"), "got:\n{screen}");
        assert!(!screen.contains("bbbbbbb"), "got:\n{screen}");
        assert!(screen.contains("2 commits"), "got:\n{screen}");
        // 中間夾的留言必須在收合後存活——只有 commit 會被折疊。
        assert!(screen.contains("one"), "got:\n{screen}");
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

use std::rc::Rc;

use crate::{
    app::AppContext,
    color::ColorTheme,
    config::UserListColumnType,
    diff::{self, DiffNotes, ModeNote, RenderedDiff},
    event::{AppEvent, Sender, UserEvent, UserEventWithCount},
    git::{Commit, DiffTarget, FileChange, Ref, Repository, WorkingChanges},
    view::{
        dispatch_branch_copy, dispatch_tag_copy, partition_branches, partition_tags,
        ListRefreshViewContext, RefreshViewContext,
    },
    widget::{
        commit_detail::{
            build_commit_tree_rows, build_working_changes_tree_rows, CommitDetail,
            CommitDetailState, DetailPane, TreeRow, WorkingChangesDetail,
        },
        commit_list::{CommitList, CommitListState},
        h,
        marquee::display_width,
        output_pane::{OutputPane, OutputPaneState},
        HintSpec, ELLIPSIS, ELLIPSIS_RESERVE,
    },
};
use ratatui::{
    crossterm::event::KeyEvent,
    layout::{Constraint, Layout, Rect},
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::Clear,
    Frame,
};

/// 狀態列提示。依 pane 分流 —— Info pane 列不到 diff 捲動鍵，Files pane 才列。
/// 優先序＝截斷時從尾端開始丟，所以當下情境最相關的排前面。
///
/// 寫成自由函式（而不是吃 `&self` 的方法）是為了可測：它只依賴 pane，
/// 測試可以直接餵 `DetailPane` 進來檢查每個 event 都有綁鍵、也都在說明頁列出。
pub fn status_hints_for(pane: DetailPane) -> Vec<HintSpec> {
    let mut hints = vec![h(&[UserEvent::DetailPaneToggle], "pane")];
    match pane {
        DetailPane::Info => {
            hints.push(h(
                &[UserEvent::NavigateDown, UserEvent::NavigateUp],
                "scroll",
            ));
        }
        DetailPane::Files => {
            hints.push(h(&[UserEvent::NavigateDown, UserEvent::NavigateUp], "file"));
            hints.push(h(&[UserEvent::SelectDown, UserEvent::SelectUp], "diff"));
            hints.push(h(&[UserEvent::GoToNext, UserEvent::GoToPrevious], "hunk"));
        }
    }
    hints.extend([
        h(
            &[UserEvent::NavigateLeft, UserEvent::NavigateRight],
            "commit",
        ),
        h(
            &[UserEvent::GoToParent, UserEvent::GoToChild],
            "parent/child",
        ),
        h(&[UserEvent::ShortCopy], "copy"),
    ]);
    if pane == DetailPane::Files {
        hints.extend([
            h(&[UserEvent::HalfPageDown], "half"),
            h(&[UserEvent::PageDown], "page"),
        ]);
    }
    hints.extend([
        h(&[UserEvent::RefList], "refs"),
        h(&[UserEvent::RemoteRefsToggle], "remote"),
        h(&[UserEvent::GitHubToggle], "github"),
        h(&[UserEvent::Refresh], "refresh"),
        h(&[UserEvent::HelpToggle], "help"),
        h(&[UserEvent::Cancel], "close"),
    ]);
    hints
}

#[derive(Debug)]
enum DetailContent {
    Commit {
        commit: Box<Commit>,
        refs: Vec<Ref>,
        rows: Vec<TreeRow>,
    },
    WorkingChanges {
        staged_count: usize,
        unstaged_count: usize,
        rows: Vec<TreeRow>,
    },
}

impl DetailContent {
    fn from_commit(
        commit: Commit,
        changes: Vec<FileChange>,
        refs: Vec<Ref>,
        theme: &ColorTheme,
    ) -> Self {
        let rows = build_commit_tree_rows(&changes, &commit.commit_hash, theme);
        DetailContent::Commit {
            commit: Box::new(commit),
            refs,
            rows,
        }
    }

    fn from_working_changes(working_changes: &WorkingChanges, theme: &ColorTheme) -> Self {
        let rows = build_working_changes_tree_rows(working_changes, theme);
        DetailContent::WorkingChanges {
            staged_count: working_changes.staged.len(),
            unstaged_count: working_changes.unstaged.len(),
            rows,
        }
    }

    fn rows(&self) -> &[TreeRow] {
        match self {
            DetailContent::Commit { rows, .. } => rows,
            DetailContent::WorkingChanges { rows, .. } => rows,
        }
    }

    /// Working Changes 是 virtual row（list 第 0 行），上下無 commit 需要看，
    /// 沒有 diff pane 時給滿可用高度；Commit Detail 受 cfg 限制，保留 commit
    /// row 給導航。diff pane 出現時兩者都要套 cfg 上限，否則 commit list
    /// 只剩兩行。
    fn max_height(&self, area_cap: u16, cfg_height: u16, diff_visible: bool) -> u16 {
        match self {
            DetailContent::Commit { .. } => area_cap.min(cfg_height),
            DetailContent::WorkingChanges { .. } => {
                if diff_visible {
                    area_cap.min(cfg_height)
                } else {
                    area_cap
                }
            }
        }
    }
}

#[derive(Debug)]
pub struct DetailView<'a> {
    commit_list_state: Option<CommitListState<'a>>,
    commit_detail_state: CommitDetailState,

    content: DetailContent,

    /// 游標所在檔案的 diff（已解析、已上色、已算好行號與 hunk 索引）。
    /// 永遠只裝「一個」檔案的內容——不是整包 commit diff。
    diff: RenderedDiff,
    diff_pane_state: OutputPaneState,
    /// `diff` 目前對應的目標；跟游標算出的「應該顯示的目標」不一致時
    /// 才重新載入，藉此避免同一個檔案被重複 spawn git。
    diff_target: Option<DiffTarget>,

    repository: &'a Repository,
    ctx: Rc<AppContext>,
    tx: Sender,
}

impl<'a> DetailView<'a> {
    pub fn new(
        commit_list_state: CommitListState<'a>,
        commit: Commit,
        changes: Vec<FileChange>,
        refs: Vec<Ref>,
        repository: &'a Repository,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> DetailView<'a> {
        let content = DetailContent::from_commit(commit, changes, refs, &ctx.color_theme);
        let mut commit_detail_state = CommitDetailState::default();
        commit_detail_state.reset(content.rows());

        DetailView {
            commit_list_state: Some(commit_list_state),
            commit_detail_state,
            content,
            diff: RenderedDiff::default(),
            diff_pane_state: OutputPaneState::default(),
            diff_target: None,
            repository,
            ctx,
            tx,
        }
    }

    pub fn new_working_changes(
        commit_list_state: CommitListState<'a>,
        working_changes: WorkingChanges,
        repository: &'a Repository,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> DetailView<'a> {
        let content = DetailContent::from_working_changes(&working_changes, &ctx.color_theme);
        let mut commit_detail_state = CommitDetailState::default();
        commit_detail_state.reset(content.rows());

        DetailView {
            commit_list_state: Some(commit_list_state),
            commit_detail_state,
            content,
            diff: RenderedDiff::default(),
            diff_pane_state: OutputPaneState::default(),
            diff_target: None,
            repository,
            ctx,
            tx,
        }
    }

    pub fn handle_event(&mut self, event_with_count: UserEventWithCount, _key: KeyEvent) {
        let event = event_with_count.event;
        let count = event_with_count.count;

        match event {
            UserEvent::DetailPaneToggle => {
                self.commit_detail_state.toggle_pane();
                self.sync_diff();
            }
            UserEvent::NavigateDown => match self.commit_detail_state.active_pane() {
                DetailPane::Info => {
                    for _ in 0..count {
                        self.commit_detail_state.scroll_info_down();
                    }
                }
                DetailPane::Files => {
                    for _ in 0..count {
                        if !self
                            .commit_detail_state
                            .move_file_cursor_down(self.content.rows())
                        {
                            break;
                        }
                    }
                    self.sync_diff();
                }
            },
            UserEvent::NavigateUp => match self.commit_detail_state.active_pane() {
                DetailPane::Info => {
                    for _ in 0..count {
                        self.commit_detail_state.scroll_info_up();
                    }
                }
                DetailPane::Files => {
                    for _ in 0..count {
                        if !self
                            .commit_detail_state
                            .move_file_cursor_up(self.content.rows())
                        {
                            break;
                        }
                    }
                    self.sync_diff();
                }
            },
            UserEvent::SelectDown => self.scroll_diff(count, OutputPaneState::scroll_down),
            UserEvent::SelectUp => self.scroll_diff(count, OutputPaneState::scroll_up),
            UserEvent::PageDown => self.scroll_diff(count, OutputPaneState::scroll_page_down),
            UserEvent::PageUp => self.scroll_diff(count, OutputPaneState::scroll_page_up),
            UserEvent::HalfPageDown => {
                self.scroll_diff(count, OutputPaneState::scroll_half_page_down)
            }
            UserEvent::HalfPageUp => self.scroll_diff(count, OutputPaneState::scroll_half_page_up),
            UserEvent::GoToNext => self.jump_hunk(count, next_hunk_start),
            UserEvent::GoToPrevious => self.jump_hunk(count, prev_hunk_start),
            UserEvent::NavigateRight => {
                self.tx.send(AppEvent::SelectOlderCommit);
            }
            UserEvent::NavigateLeft => {
                self.tx.send(AppEvent::SelectNewerCommit);
            }
            UserEvent::GoToParent => {
                self.tx.send(AppEvent::SelectParentCommit);
            }
            UserEvent::GoToChild => {
                self.tx.send(AppEvent::SelectChildCommit);
            }
            UserEvent::ShortCopy => {
                self.copy_commit_short_hash();
            }
            UserEvent::FullCopy => {
                self.copy_commit_subject();
            }
            UserEvent::BranchCopy => {
                self.handle_branch_copy(false);
            }
            UserEvent::FullBranchCopy => {
                self.handle_branch_copy(true);
            }
            UserEvent::TagCopy => {
                self.handle_tag_copy();
            }
            UserEvent::RemoteRefsToggle => {
                if let Some(ref mut cls) = self.commit_list_state {
                    let show = cls.toggle_remote_refs();
                    if show {
                        self.tx
                            .send(AppEvent::NotifyInfo("Remote refs: shown".into()));
                    } else {
                        self.tx
                            .send(AppEvent::NotifyInfo("Remote refs: hidden".into()));
                    }
                    self.tx
                        .send_after(AppEvent::ClearStatusLine, std::time::Duration::from_secs(3));
                }
            }
            UserEvent::Confirm | UserEvent::Cancel | UserEvent::Close => {
                self.tx.send(AppEvent::CloseDetail);
            }
            UserEvent::RefList => {
                self.tx.send(AppEvent::OpenRefs);
            }
            UserEvent::ShellToggle => {
                self.tx.send(AppEvent::OpenShell);
            }
            UserEvent::Refresh => {
                self.refresh();
            }
            _ => {}
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect, marquee_frame: u64) {
        let pane_wants_diff = self.commit_detail_state.active_pane() == DetailPane::Files
            && self.commit_detail_state.file_cursor().is_some();
        // 不 clamp 的話：終端機矮於 diff pane 高度時 list_area 會被壓到 0，
        // CommitList::update_state 的 `state.selected - state.height + 1` 會
        // usize underflow panic；`.saturating_sub(3)` 保留至少 3 列給
        // commit list + inline detail。
        let diff_height = if pane_wants_diff {
            self.ctx
                .ui_config
                .pane_height
                .diff
                .min(area.height.saturating_sub(3))
        } else {
            0
        };
        let show_diff = diff_height > 0;

        let [list_area, diff_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(diff_height)]).areas(area);

        let area_cap = list_area.height.saturating_sub(2);
        let max_height =
            self.content
                .max_height(area_cap, self.ctx.ui_config.pane_height.detail, show_diff);
        let content_height = match &self.content {
            DetailContent::Commit { commit, refs, rows } => {
                CommitDetail::new(commit, rows, refs, self.ctx.clone(), marquee_frame)
                    .content_height()
            }
            DetailContent::WorkingChanges {
                staged_count,
                unstaged_count,
                rows,
            } => WorkingChangesDetail::new(*staged_count, *unstaged_count, rows, self.ctx.clone())
                .content_height(),
        };
        let detail_height = max_height.min(content_height);

        let commit_list_state = self
            .commit_list_state
            .as_mut()
            .expect("commit_list_state already taken");

        // 設定 inline detail 高度，讓 CommitList 渲染出空隙
        commit_list_state.set_inline_detail_height(detail_height);

        // 用 list_area（已扣掉 diff pane）渲染 CommitList — 內部會自行處理空隙
        let commit_list = CommitList::new(self.ctx.clone(), 0);
        f.render_stateful_widget(commit_list, list_area, commit_list_state);

        // 計算 graph+marker 的寬度，用於 inline detail 定位
        let graph_marker_width = calc_graph_marker_width(commit_list_state, &self.ctx);

        // 取得內容區域（表頭以下）
        let content_area = Rect::new(
            list_area.left(),
            list_area.top() + 1, // 跳過表頭列
            list_area.width,
            list_area.height.saturating_sub(1),
        );

        if let Some(detail_rect) =
            commit_list_state.inline_detail_rect(content_area, graph_marker_width)
        {
            // 清除 detail 區域的文字內容
            f.render_widget(Clear, detail_rect);

            match &self.content {
                DetailContent::Commit { commit, refs, rows } => {
                    let commit_detail =
                        CommitDetail::new(commit, rows, refs, self.ctx.clone(), marquee_frame);
                    f.render_stateful_widget(
                        commit_detail,
                        detail_rect,
                        &mut self.commit_detail_state,
                    );
                }
                DetailContent::WorkingChanges {
                    staged_count,
                    unstaged_count,
                    rows,
                } => {
                    let wc_detail = WorkingChangesDetail::new(
                        *staged_count,
                        *unstaged_count,
                        rows,
                        self.ctx.clone(),
                    );
                    f.render_stateful_widget(wc_detail, detail_rect, &mut self.commit_detail_state);
                }
            }
        }

        if show_diff {
            f.render_widget(Clear, diff_area);
            let mut output_pane = OutputPane::new(&self.diff.lines, self.ctx.clone());
            if let Some(target) = self.commit_detail_state.selected_file(self.content.rows()) {
                output_pane = output_pane.title(diff_pane_title(
                    target.path(),
                    &self.diff.notes,
                    self.current_hunk_display(),
                    diff_area.width,
                    &self.ctx.color_theme,
                ));
            }
            f.render_stateful_widget(output_pane, diff_area, &mut self.diff_pane_state);
        }
    }
}

/// 計算 Graph + Marker 欄位的合計寬度，當作 inline detail 面板的左緣。
/// 緊湊模式下 Graph／Marker 不保留固定寬度，面板要貼齊選取列自己的
/// `text_x`，不是整張圖的全域寬度 —— 否則會留下這個功能原本要消滅的
/// 那片空白。
fn calc_graph_marker_width(state: &CommitListState<'_>, ctx: &AppContext) -> u16 {
    if state.is_compact() {
        return state.selected_row_text_x();
    }
    let mut width: u16 = 0;
    for col in &ctx.ui_config.list.columns {
        match col {
            UserListColumnType::Graph => {
                width += state.graph_area_cell_width();
            }
            UserListColumnType::Marker => {
                width += 1;
            }
            _ => {}
        }
    }
    width
}

impl<'a> DetailView<'a> {
    pub fn take_graph_clear(&mut self) -> bool {
        self.commit_list_state
            .as_mut()
            .is_some_and(|s| s.take_graph_clear())
    }

    pub fn request_graph_clear(&mut self) {
        if let Some(s) = self.commit_list_state.as_mut() {
            s.request_graph_clear();
        }
    }

    pub fn take_list_state(&mut self) -> Option<CommitListState<'a>> {
        let mut state = self.commit_list_state.take();
        if let Some(ref mut s) = state {
            s.set_inline_detail_height(0);
        }
        state
    }

    pub fn as_list_state(&self) -> &CommitListState<'_> {
        self.commit_list_state.as_ref().unwrap()
    }

    pub fn marquee_id(&self) -> Option<std::sync::Arc<str>> {
        match &self.content {
            DetailContent::Commit { commit, .. } => Some(commit.commit_hash.as_arc()),
            DetailContent::WorkingChanges { .. } => None,
        }
    }

    pub fn marquee_needed(&self) -> bool {
        self.commit_detail_state.subject_overflows()
    }

    pub fn status_hints(&self) -> Vec<HintSpec> {
        status_hints_for(self.commit_detail_state.active_pane())
    }

    pub fn select_older_commit(&mut self, repository: &Repository) {
        self.update_selected_commit(repository, |state| state.select_next());
    }

    pub fn select_newer_commit(&mut self, repository: &Repository) {
        self.update_selected_commit(repository, |state| state.select_prev());
    }

    pub fn select_parent_commit(&mut self, repository: &Repository) {
        self.update_selected_commit(repository, |state| state.select_parent());
    }

    pub fn select_child_commit(&mut self, repository: &Repository) {
        self.update_selected_commit(repository, |state| state.select_child());
    }

    fn update_selected_commit<F>(&mut self, repository: &Repository, update_commit_list_state: F)
    where
        F: FnOnce(&mut CommitListState<'a>),
    {
        let Some(commit_list_state) = self.commit_list_state.as_mut() else {
            return;
        };
        update_commit_list_state(commit_list_state);

        if commit_list_state.is_virtual_row_selected() {
            if let Some(wc) = commit_list_state.working_changes() {
                self.content = DetailContent::from_working_changes(wc, &self.ctx.color_theme);
            }
        } else {
            let selected = commit_list_state.selected_commit_hash().clone();
            let (commit, changes) = repository.commit_detail(&selected);
            let (_, refs) = repository.commit_refs(&selected);
            self.content =
                DetailContent::from_commit(commit.clone(), changes, refs, &self.ctx.color_theme);
        }

        self.commit_detail_state.reset(self.content.rows());
        self.sync_diff();
    }

    /// 把游標算出來的「應該顯示的目標」跟 `diff` 目前對應的目標比對，
    /// 不一致才重新載入——擋住同一個檔案被重複 spawn git，但不做任何快取
    /// （切到別的檔案／commit 後舊內容直接丟棄）。只在 Files pane 才實際
    /// 載入：pane 是 Info 時即使內容換了也不用白跑一次 git diff。
    fn sync_diff(&mut self) {
        if self.commit_detail_state.active_pane() != DetailPane::Files {
            return;
        }
        let desired = self
            .commit_detail_state
            .selected_file(self.content.rows())
            .cloned();
        if desired == self.diff_target {
            return;
        }
        self.diff_pane_state.select_first();
        let tab_width = self.ctx.core_config.user_command.tab_width;
        self.diff = match &desired {
            None => RenderedDiff::default(),
            Some(target) => match self.repository.file_diff(target) {
                Ok((text, truncated)) => {
                    let mut rendered = diff::parse(&text, tab_width, &self.ctx.color_theme);
                    rendered.notes.truncated = truncated;
                    rendered
                }
                Err(err) => RenderedDiff {
                    lines: vec![error_line(&err, &self.ctx)],
                    ..RenderedDiff::default()
                },
            },
        };
        self.diff_target = desired;
    }

    /// `SelectDown`／`SelectUp`／`Page*`／`HalfPage*` 六個 diff pane 捲動鍵共用的
    /// 分派：只在 Files pane 生效，其餘語意由呼叫端傳入的 `OutputPaneState` 方法決定。
    fn scroll_diff(&mut self, count: usize, scroll: fn(&mut OutputPaneState)) {
        if self.commit_detail_state.active_pane() != DetailPane::Files {
            return;
        }
        for _ in 0..count {
            scroll(&mut self.diff_pane_state);
        }
    }

    /// `]`／`[`：跳到下一個／上一個 hunk 起點。只在 Files pane 生效，跟
    /// `scroll_diff` 同一種分派方式——語意差異外包給呼叫端傳入的
    /// `next_hunk_start`／`prev_hunk_start`。到頭到尾就停住，不繞回：
    /// 找不到就回傳 `None`，迴圈當場 `break`，`offset` 留在原地。
    /// 用 `scroll_to` 而不是逐步呼叫 `scroll_down`/`up`：hunk 之間可能隔了
    /// 上百行 context，逐格捲會讓 `count` 次 `]` 變成非常慢的操作。
    fn jump_hunk(&mut self, count: usize, find: fn(&[usize], usize) -> Option<usize>) {
        if self.commit_detail_state.active_pane() != DetailPane::Files {
            return;
        }
        let line_count = self.diff.lines.len();
        let mut offset = self.diff_pane_state.offset();
        for _ in 0..count {
            let Some(next) = find(&self.diff.hunk_starts, offset) else {
                break;
            };
            offset = next;
        }
        self.diff_pane_state.scroll_to(offset, line_count);
    }

    /// 目前 offset 落在第幾個 hunk（1-based）／總共幾個 hunk，給 title 的
    /// `hunk n/m` 用。沒有 hunk（binary、純 mode 變更）就回 `None`——
    /// 不要顯示 `hunk 1/0`。
    fn current_hunk_display(&self) -> Option<(usize, usize)> {
        let hunk_starts = &self.diff.hunk_starts;
        if hunk_starts.is_empty() {
            return None;
        }
        let offset = self.diff_pane_state.offset();
        let current = hunk_starts.partition_point(|&s| s <= offset);
        Some((current, hunk_starts.len()))
    }

    fn copy_commit_short_hash(&self) {
        if let DetailContent::Commit { commit, .. } = &self.content {
            self.copy_to_clipboard(
                "Commit SHA (short)".into(),
                commit.commit_hash.as_short_hash().into(),
            );
        }
    }

    fn copy_commit_subject(&self) {
        if let DetailContent::Commit { commit, .. } = &self.content {
            self.copy_to_clipboard("Commit Subject".into(), commit.subject.clone());
        }
    }

    fn handle_branch_copy(&self, full: bool) {
        let DetailContent::Commit { refs, .. } = &self.content else {
            return;
        };
        let (local, remote) = partition_branches(refs.iter());
        dispatch_branch_copy(&self.tx, &local, &remote, full);
    }

    fn handle_tag_copy(&self) {
        let DetailContent::Commit { refs, .. } = &self.content else {
            return;
        };
        let tags = partition_tags(refs.iter());
        dispatch_tag_copy(&self.tx, &tags);
    }

    fn copy_to_clipboard(&self, name: String, value: String) {
        self.tx.send(AppEvent::CopyToClipboard { name, value });
    }

    pub fn refresh(&self) {
        let list_state = self.as_list_state();
        let list_context = ListRefreshViewContext::from(list_state);
        let context = RefreshViewContext::Detail { list_context };
        self.tx.send(AppEvent::Refresh(context));
    }
}

fn error_line(msg: &str, ctx: &AppContext) -> Line<'static> {
    Line::styled(
        msg.to_string(),
        Style::default().fg(ctx.color_theme.status_error_fg),
    )
}

/// diff pane title 各段之間的分隔符——路徑內部的旗標之間、路徑段與 hunk
/// 段之間都用它，三處共用同一個值，改一次全部同步，不會漂移。
const TITLE_SEP: &str = " · ";

/// diff pane 邊框 title。組出「路徑＋只在異常時出現的旗標（binary／mode／
/// truncated）· hunk n/m」這一整行並直接上色——降級判斷（該不該印 hunk
/// 後綴）跟實際渲染（要不要含 hunk 那段 `Span`）共用同一份 `TITLE_SEP`
/// 與同一個函式，不會有「算得下但沒畫出來」這種兩處各自維護才會出現的
/// 漂移。變更類型、增刪統計、比較範圍都跟隔壁 pane 重複（tree row 已印
/// 增刪統計、commit hash 就在正上方的 inline detail），不放進來。
///
/// 空間不足只有一條規則：先丟 hunk 後綴，還不夠才犧牲路徑前段——異常旗標
/// 永遠不丟，路徑的檔名跟旗標比路徑前綴更值得保留。
fn diff_pane_title(
    path: &str,
    notes: &DiffNotes,
    hunk_display: Option<(usize, usize)>,
    area_width: u16,
    theme: &ColorTheme,
) -> Line<'static> {
    let mut flags = Vec::new();
    if notes.binary {
        flags.push("binary");
    }
    if let Some(label) = notes.mode.map(ModeNote::label) {
        flags.push(label);
    }
    if notes.truncated {
        flags.push("truncated");
    }
    let head = if flags.is_empty() {
        path.to_string()
    } else {
        format!("{path}{TITLE_SEP}{}", flags.join(TITLE_SEP))
    };
    let hunk = hunk_display.map(|(current, total)| format!("hunk {current}/{total}"));

    let width = area_width as usize;
    let head_width = display_width(&head);
    let hunk_suffix_width = hunk
        .as_ref()
        .map_or(0, |h| display_width(h) + display_width(TITLE_SEP));
    let (head, hunk) = if head_width + hunk_suffix_width <= width {
        (head, hunk)
    } else {
        (truncate_head(&head, width), None)
    };

    let mut spans = vec![Span::raw(head).fg(theme.diff_title_path_fg).bold()];
    if let Some(hunk) = hunk {
        spans.push(Span::raw(TITLE_SEP));
        spans.push(Span::raw(hunk).fg(theme.diff_title_hunk_fg).bold());
    }
    Line::from(spans)
}

/// 從字串「頭部」省略到剛好塞進 `max_width`，前面補一個 `…`——保留尾端
/// （檔名與旗標都在尾端），犧牲最沒用的路徑前段目錄。寬度計算全程走
/// `display_width`（跟 `ELLIPSIS_RESERVE` 同一套定義），不另外手刻一份
/// `UnicodeWidthChar` 逐字元累加。
fn truncate_head(s: &str, max_width: usize) -> String {
    if display_width(s) <= max_width {
        return s.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    let budget = max_width.saturating_sub(ELLIPSIS_RESERVE);
    let start = s
        .char_indices()
        .find(|(i, _)| display_width(&s[*i..]) <= budget)
        .map_or(s.len(), |(i, _)| i);
    format!("{ELLIPSIS}{}", &s[start..])
}

/// 目前 offset 之後最近的 hunk 起點；已經在最後一個 hunk（或更後面）就回
/// `None`——`jump_hunk` 靠這個 `None` 決定「不繞回第一個」。
fn next_hunk_start(hunk_starts: &[usize], offset: usize) -> Option<usize> {
    let idx = hunk_starts.partition_point(|&s| s <= offset);
    hunk_starts.get(idx).copied()
}

/// 目前 offset 之前最近的 hunk 起點；已經在第一個 hunk（或更前面）就回
/// `None`。offset 落在某個 hunk 中間（不是恰好在起點）時，先跳回「目前
/// 這個 hunk」的起點，而不是直接跳去更前一個——跟一般編輯器的「上一個
/// 段落」語意一致。
fn prev_hunk_start(hunk_starts: &[usize], offset: usize) -> Option<usize> {
    let idx = hunk_starts.partition_point(|&s| s < offset);
    idx.checked_sub(1).map(|i| hunk_starts[i])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_pane_title_shows_only_path_when_nothing_is_wrong() {
        let notes = DiffNotes::default();
        let theme = ColorTheme::default();
        let line = diff_pane_title("src/main.rs", &notes, Some((2, 5)), 80, &theme);
        assert_eq!(line.to_string(), "src/main.rs · hunk 2/5");
    }

    #[test]
    fn diff_pane_title_lists_every_active_flag_and_hides_hunk_when_there_is_none() {
        let notes = DiffNotes {
            binary: true,
            mode: Some(ModeNote::ModeChangedToExecutable),
            truncated: true,
        };
        let theme = ColorTheme::default();
        let line = diff_pane_title("scripts/run.sh", &notes, None, 200, &theme);
        assert_eq!(
            line.to_string(),
            "scripts/run.sh · binary · mode → executable · truncated",
            "沒有 hunk（binary/純 mode 變更）就不該印出 hunk 後綴"
        );
    }

    /// 分隔符寬度是唯一沒有其他測試釘住的數字，這裡卡在邊界上：剛好放得下
    /// vs 差一格放不下。邊界數字故意硬寫 `3`，不呼叫
    /// `display_width(TITLE_SEP)`——否則會變成跟實作互相印證的同義反覆
    /// 測試，抓不到「有人把 TITLE_SEP 改長」這種真實回歸。
    #[test]
    fn diff_pane_title_drops_the_hunk_suffix_before_touching_the_path() {
        let notes = DiffNotes::default();
        let theme = ColorTheme::default();
        let path = "src/main.rs";
        let fits = display_width(path) + display_width("hunk 1/1") + 3;

        let line = diff_pane_title(path, &notes, Some((1, 1)), fits as u16, &theme);
        assert_eq!(
            line.to_string(),
            "src/main.rs · hunk 1/1",
            "剛好放得下就不該丟"
        );

        let line = diff_pane_title(path, &notes, Some((1, 1)), (fits - 1) as u16, &theme);
        assert_eq!(line.to_string(), path, "差一格只丟 hunk 後綴，路徑不動");
    }

    #[test]
    fn diff_pane_title_truncates_path_head_only_when_path_alone_does_not_fit() {
        let notes = DiffNotes::default();
        let theme = ColorTheme::default();
        let path = "src/very/long/path/to/some/module/that/is/quite/deep.rs";
        let text = diff_pane_title(path, &notes, Some((1, 1)), 20, &theme).to_string();
        assert!(
            !text.contains("hunk"),
            "路徑都塞不下了，hunk 後綴不該出現：{text}"
        );
        assert!(text.starts_with('…'), "省略路徑前段用 … 開頭：{text}");
        assert!(text.ends_with("deep.rs"), "檔名比路徑前綴更該保留：{text}");
        assert!(display_width(&text) <= 20);
    }

    #[test]
    fn truncate_head_keeps_the_tail_and_only_touches_strings_that_do_not_fit() {
        assert_eq!(truncate_head("hello world", 6), "…orld");
        assert_eq!(truncate_head("short", 10), "short");
    }
}

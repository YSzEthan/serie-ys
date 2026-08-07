use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use ratatui::{
    crossterm::event::{KeyCode, KeyEvent},
    layout::{Constraint, Layout, Rect},
    style::{Color, Stylize},
    widgets::Block,
    DefaultTerminal, Frame,
};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::{
    color::{ColorTheme, GraphColorSet},
    config::{CoreConfig, UiConfig, UserCommand, UserCommandType},
    event::{AppEvent, EventController, UserEvent, UserEventWithCount},
    external::{
        copy_to_clipboard, exec_user_command, exec_user_command_suspend, ExternalCommandParameters,
    },
    git::{Commit, CommitHash, FileChange, Head, Ref, RefType, Repository},
    github::{
        is_merge_conflict_error, merge_pr, set_item_state, set_pr_draft, GhItemKind, MergeMethod,
        PrDraftAction, StateAction, StateFilter,
    },
    graph::{Graph, GraphStyle},
    keybind::KeyBind,
    view::{dispatch_delete_branch, RefreshViewContext, RefsOrigin, View},
    widget::{
        commit_list::{CommitInfo, CommitListState, RawCommitIdx},
        pending_overlay::PendingOverlay,
    },
    CompactType, GraphWidthType,
};

use status_line::StatusLineState;

mod status_line;

const SSH_OPEN_PREFIX: &str = "SSH: ⌘/⌥/⇧+Click ";

/// OSC 8 超連結的簡短標籤。GitHub issue/PR 網址會縮成
/// `[#N]`；其他一律 fallback 成 `[open]`。用 host 限定在 `github.com`，
/// 避免像 `https://example.com/blog/issues/2024` 這種網址被誤判。
fn short_link_label(url: &str) -> String {
    let on_github = url.starts_with("https://github.com/") || url.starts_with("http://github.com/");
    let is_issue_or_pr = url.contains("/issues/") || url.contains("/pull/");
    let tail = url.trim_end_matches('/').rsplit('/').next();
    if let (true, true, Some(n)) = (
        on_github,
        is_issue_or_pr,
        tail.and_then(|s| s.parse::<u64>().ok()),
    ) {
        return format!("[#{n}]");
    }
    "[open]".to_string()
}

#[derive(Clone, Copy)]
pub enum InitialSelection {
    Latest,
    Head,
}

pub enum Ret {
    Quit,
    Refresh(RefreshRequest),
}

pub struct RefreshRequest {
    pub context: RefreshViewContext,
}

#[derive(Debug)]
pub struct AppContext {
    pub keybind: KeyBind,
    pub core_config: CoreConfig,
    pub ui_config: UiConfig,
    pub color_theme: ColorTheme,
    /// 已解析完成的 graph 風格（CLI flag 優先於設定檔，已經合併過——
    /// 不同於 `core_config.option.graph_style`，那是設定檔的原始值，
    /// 不會反映 `-s` 的覆寫）。
    pub graph_style: GraphStyle,
    /// 已合併 CLI／設定檔的寬度偏好。跟 `graph_style` 不同，這裡存的不是
    /// 最終寬度，而是偏好本身（`Auto` 需要 `area.width` 才能解出最終寬度，
    /// 那是每幀才知道的資訊，見 `widget::commit_list::layout::decide`）。
    pub graph_width: Option<GraphWidthType>,
    /// 已合併 CLI／設定檔的緊湊模式偏好，理由跟 `graph_width` 一樣。
    pub compact: Option<CompactType>,
}

/// `q` 雙擊退出的判定結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuitDecision {
    /// 這是視窗內的第一次按下，只顯示提示。
    First,
    /// 上一次按下還在視窗內，這次要真的退出。
    Second,
}

/// 按鍵處理過程中會累積、之後又清空的狀態——數字前綴與 `q` 雙擊退出計時。
/// 獨立於 `App`，不吃 `Repository`/`EventController`，可以直接 `new` 出來
/// 測純轉移邏輯，不用假裝建一個完整的 `App`。
#[derive(Debug, Default)]
struct KeyState {
    numeric_prefix: String,
    last_quit_press: Option<Instant>,
}

impl KeyState {
    /// `c` 是數字、且不是「前綴目前是空的」情況下的前導零，就接到前綴後面。
    /// 擋掉前導零是既有規則——`0` 自己會被當成「沒有前綴」而非數字 0。
    fn push_digit(&mut self, c: char) {
        if c.is_ascii_digit() && (c != '0' || !self.numeric_prefix.is_empty()) {
            self.numeric_prefix.push(c);
        }
    }

    /// 取出目前的數字前綴並清空——單一動作完成「讀取＋歸零」，呼叫端不用
    /// 再自己補一次 `.clear()`。
    fn take_count(&mut self) -> String {
        std::mem::take(&mut self.numeric_prefix)
    }

    fn clear_count(&mut self) {
        self.numeric_prefix.clear();
    }

    /// 距離上一次按下 `q` 是否還在雙擊視窗內。`now` 由呼叫端傳入而非內部
    /// 呼叫 `Instant::now()`，狀態轉移才能脫離真實時鐘單獨測試。
    fn register_quit_press(&mut self, now: Instant) -> QuitDecision {
        if self
            .last_quit_press
            .is_some_and(|t| now.duration_since(t) < Duration::from_millis(500))
        {
            self.last_quit_press = None;
            QuitDecision::Second
        } else {
            self.last_quit_press = Some(now);
            QuitDecision::First
        }
    }

    fn reset_quit_press(&mut self) {
        self.last_quit_press = None;
    }
}

#[derive(Debug)]
pub struct App<'a> {
    repository: &'a Repository,
    view: View<'a>,
    key_state: KeyState,
    /// 上一次 `render()` 算出的 view 區域，唯一寫入點是 `update_state`
    /// （由 `render` 呼叫）。
    view_area: Rect,
    status_line_state: StatusLineState,
    pending_message: Option<String>,
    /// `None` 代表資料現在活在開著的 `GitHubView` 裡；兩者不會同時各存一份。
    github_data: Option<crate::github::GitHubData>,
    github_load: GitHubLoad,
    ctx: Rc<AppContext>,
    ec: &'a EventController,
    marquee_frame: u64,
    marquee_needed: bool,
    last_marquee_id: Option<std::sync::Arc<str>>,
}

/// GitHub 資料的排程狀態機。`(loading=false, pending=Some(_))`
/// 這種非法組合曾經可達（Bug A）—— 用 enum 讓它在型別層直接不存在。
///
/// 跟 `GitHubView::LoadState`（`view/github/mod.rs`）是兩台刻意分開的狀態
/// 機：這個回答「有沒有請求在飛／有沒有排隊的重抓」，是排程狀態；
/// `LoadState` 回答「畫面上要顯示 spinner 還是錯誤字」，是呈現狀態。
/// 合併等於把 presentation 揉進 `App`，不要做。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitHubLoad {
    Idle,
    Loading,
    /// 載入中使用者又切了 filter：這次載入（不論成功或失敗）結束後都要
    /// 改抓這個。
    LoadingThenRefetch(StateFilter),
}

impl GitHubLoad {
    /// 要求以 `filter` 重新整理。`true` 代表呼叫端現在就該 spawn 一次載入；
    /// `false` 代表已有一次載入在飛，這次要求記到它結束後處理。
    fn on_refresh_requested(&mut self, filter: StateFilter) -> bool {
        match self {
            GitHubLoad::Idle => {
                *self = GitHubLoad::Loading;
                true
            }
            GitHubLoad::Loading | GitHubLoad::LoadingThenRefetch(_) => {
                *self = GitHubLoad::LoadingThenRefetch(filter);
                false
            }
        }
    }

    /// 一次載入結束——不論成功或失敗都呼叫這個方法，這就是 Bug A 的修法：
    /// 失敗路徑當年沒有消費 `pending`，資料留在一個永遠不會被重抓的狀態。
    /// 回傳 `Some(f)` 代表要立刻改抓 `f`；`None` 代表閒置下來。
    fn on_load_settled(&mut self) -> Option<StateFilter> {
        match std::mem::replace(self, GitHubLoad::Idle) {
            GitHubLoad::LoadingThenRefetch(filter) => Some(filter),
            GitHubLoad::Idle | GitHubLoad::Loading => None,
        }
    }
}

impl<'a> App<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        repository: &'a Repository,
        graph: &Rc<Graph>,
        filtered_graph: Option<Rc<Graph>>,
        remote_only_commits: FxHashSet<CommitHash>,
        graph_color_set: &'a GraphColorSet,
        initial_selection: InitialSelection,
        ctx: Rc<AppContext>,
        ec: &'a EventController,
        refresh_view_context: Option<RefreshViewContext>,
    ) -> Self {
        let graph_colors: Vec<Color> = graph_color_set
            .colors
            .iter()
            .map(|c| c.to_ratatui_color())
            .collect();
        let head_commit_hash = crate::resolve_head_commit_hash(repository);

        let mut ref_name_to_commit_index_map = FxHashMap::default();
        let commits = graph
            .commit_hashes
            .iter()
            .enumerate()
            .map(|(i, commit_hash)| {
                let commit = repository
                    .commit(commit_hash)
                    .expect("commit hash from graph must exist in repository");
                let refs = repository.refs(commit_hash);
                for r in &refs {
                    ref_name_to_commit_index_map.insert(r.name().to_string(), RawCommitIdx(i));
                }
                let (pos_x, _) = graph.commit_pos_map[commit_hash];
                let graph_color = graph_color_set.get(pos_x).to_ratatui_color();
                CommitInfo::new(commit, refs, graph_color)
            })
            .collect();
        let filtered_colors: Option<FxHashMap<CommitHash, ratatui::style::Color>> =
            filtered_graph.as_ref().map(|fg| {
                fg.commit_hashes
                    .iter()
                    .map(|commit_hash| {
                        let (pos_x, _) = fg.commit_pos_map[commit_hash];
                        (
                            commit_hash.clone(),
                            graph_color_set.get(pos_x).to_ratatui_color(),
                        )
                    })
                    .collect()
            });

        let head = repository.head().clone();
        let working_changes = repository.working_changes().clone();
        let working_changes_opt = if working_changes.is_empty() {
            None
        } else {
            Some(working_changes)
        };
        let mut commit_list_state = CommitListState::new(
            commits,
            Rc::clone(graph),
            graph_colors,
            head_commit_hash,
            head,
            ref_name_to_commit_index_map,
            ctx.core_config.search.ignore_case,
            ctx.core_config.search.fuzzy,
            filtered_graph,
            filtered_colors,
            remote_only_commits,
            working_changes_opt,
        );
        if let InitialSelection::Head = initial_selection {
            match repository.head() {
                Head::Branch { name } => commit_list_state.select_ref(name),
                Head::Detached { target } => commit_list_state.select_commit_hash(target),
                Head::None => {}
            }
        }
        let view = View::of_list(commit_list_state, ctx.clone(), ec.sender());
        let status_line_state = StatusLineState::new(ctx.clone(), ec.sender());

        let mut app = Self {
            repository,
            view,
            key_state: KeyState::default(),
            view_area: Rect::default(),
            status_line_state,
            pending_message: None,
            github_data: None,
            github_load: GitHubLoad::Idle,
            ctx,
            ec,
            marquee_frame: 0,
            marquee_needed: false,
            last_marquee_id: None,
        };

        if let Some(context) = refresh_view_context {
            app.init_with_context(context);
        }

        app
    }

    pub fn into_parts(self) -> (Option<Rc<Graph>>, FxHashSet<CommitHash>) {
        self.view.into_commit_list_state().into_graph_parts()
    }
}

impl App<'_> {
    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<Ret, std::io::Error> {
        let mut skip_draw = false;
        loop {
            if !skip_draw {
                let current_id = self.view.marquee_id();
                if self.last_marquee_id != current_id {
                    self.marquee_frame = 0;
                    self.last_marquee_id = current_id;
                }

                if self.view.take_graph_clear() {
                    // 完整清空 backing buffer 並重繪，由
                    // `request_graph_clear()` 觸發（例如 `toggle_remote_refs`）。
                    // 跟 `set_show_remote_refs` 文件註解裡提到的無關，
                    // 那個是 `lib.rs` 裡另一處獨立的 `terminal.clear()`。
                    terminal.clear()?;
                }
                terminal.draw(|f| self.render(f))?;

                self.marquee_needed = self.view.marquee_needed();
            }
            skip_draw = false;

            match self.ec.recv() {
                AppEvent::Tick => {
                    if self.marquee_needed {
                        self.marquee_frame = self.marquee_frame.wrapping_add(1);
                    } else {
                        skip_draw = true;
                    }
                    continue;
                }
                AppEvent::Key(key) => {
                    self.handle_key(key);
                }
                AppEvent::Resize(w, h) => {
                    let _ = (w, h);
                }
                AppEvent::Quit => {
                    return Ok(Ret::Quit);
                }
                AppEvent::OpenDetail => {
                    self.open_detail();
                }
                AppEvent::CloseDetail => {
                    terminal.clear()?;
                    self.close_detail();
                }
                AppEvent::OpenUserCommand(n) => {
                    self.open_user_command(n, Some(terminal));
                }
                AppEvent::CloseUserCommand => {
                    terminal.clear()?;
                    self.close_user_command();
                }
                AppEvent::OpenRefs => {
                    self.open_refs();
                }
                AppEvent::CloseRefs => {
                    self.close_refs();
                }
                AppEvent::OpenCreateTag => {
                    self.open_create_tag();
                }
                AppEvent::CloseCreateTag => {
                    self.close_create_tag();
                }
                AppEvent::OpenDeleteTag => {
                    self.open_delete_tag();
                }
                AppEvent::CloseDeleteTag => {
                    self.close_delete_tag();
                }
                AppEvent::OpenDeleteRef { ref_name, ref_type } => {
                    self.open_delete_ref(ref_name, ref_type);
                }
                AppEvent::CloseDeleteRef => {
                    self.close_delete_ref();
                }
                AppEvent::OpenHelp => {
                    self.open_help();
                }
                AppEvent::CloseHelp => {
                    terminal.clear()?;
                    self.close_help();
                }
                AppEvent::OpenGitHub => {
                    self.open_github();
                }
                AppEvent::CloseGitHub => {
                    self.close_github();
                }
                AppEvent::RefreshGitHub { state } => {
                    self.refresh_github(state);
                }
                AppEvent::GitHubDataLoaded { data, warnings } => {
                    self.on_github_data_loaded(data, warnings);
                }
                AppEvent::LoadMoreGitHub { kind, generation } => {
                    self.load_more_github(kind, generation);
                }
                // LoadMoreGitHub 只在 view 開著時送出（view/github/event.rs），
                // 所以這裡直接丟棄「view 關著」的情況——不像 GitHubDataLoaded
                // 那樣在 view 關閉後仍要保留。分頁游標是靠 generation 驗證
                // 新舊的增量更新，view 關掉後 generation 就沒有意義，丟棄才
                // 安全（代價只是重開後捲到底要重抓一頁）。
                AppEvent::GitHubMoreIssuesLoaded {
                    items,
                    next_cursor,
                    generation,
                } => {
                    if let View::GitHub(ref mut view) = self.view {
                        view.append_issues(items, next_cursor, generation);
                    }
                }
                AppEvent::GitHubMorePrsLoaded {
                    items,
                    next_cursor,
                    generation,
                } => {
                    if let View::GitHub(ref mut view) = self.view {
                        view.append_pull_requests(items, next_cursor, generation);
                    }
                }
                AppEvent::LoadGitHubTimeline {
                    number,
                    kind,
                    after,
                } => {
                    self.load_github_timeline(number, kind, after);
                }
                AppEvent::GitHubTimelineLoaded {
                    number,
                    kind,
                    after,
                    page,
                } => {
                    if let View::GitHub(ref mut view) = self.view {
                        view.append_timeline_items(number, kind, after, page);
                    }
                }
                AppEvent::GitHubTimelineFailed {
                    number,
                    kind,
                    error,
                } => {
                    if let View::GitHub(ref mut view) = self.view {
                        view.set_timeline_error(number, kind, error);
                    }
                }
                AppEvent::GitHubFlash { message, is_error } => {
                    if let View::GitHub(ref mut view) = self.view {
                        view.set_flash(message, is_error);
                    }
                }
                AppEvent::GitHubLoadFailed { error } => {
                    self.on_github_load_failed(error);
                }
                AppEvent::BatchToggleCheckboxes {
                    number,
                    kind,
                    checkbox_indices,
                } => {
                    self.batch_toggle_checkboxes(number, kind, checkbox_indices);
                }
                AppEvent::CheckboxToggled {
                    number,
                    kind,
                    new_body,
                } => {
                    self.on_checkbox_toggled(number, kind, &new_body);
                }
                AppEvent::SelectOlderCommit => {
                    self.select_older_commit();
                }
                AppEvent::SelectNewerCommit => {
                    self.select_newer_commit();
                }
                AppEvent::SelectParentCommit => {
                    self.select_parent_commit();
                }
                AppEvent::CopyToClipboard { name, value } => {
                    self.copy_to_clipboard(name, value);
                }
                AppEvent::OpenUrl(url) => {
                    self.open_url(url);
                }
                AppEvent::Refresh(context) => {
                    let request = RefreshRequest { context };
                    return Ok(Ret::Refresh(request));
                }
                AppEvent::ClearStatusLine => {
                    self.status_line_state.clear();
                }
                AppEvent::UpdateStatusInput(msg, cursor_pos, msg_r) => {
                    self.status_line_state.update_input(msg, cursor_pos, msg_r);
                }
                AppEvent::NotifyInfo(msg) => {
                    self.status_line_state.set_notification_info(msg);
                }
                AppEvent::NotifySuccess(msg) => {
                    self.status_line_state.set_notification_success(msg);
                }
                AppEvent::NotifyWarn(msg) => {
                    self.status_line_state.set_notification_warn(msg);
                }
                AppEvent::NotifyError(msg) => {
                    self.status_line_state.set_notification_error(msg);
                }
                AppEvent::ShowPendingOverlay { message } => {
                    self.pending_message = Some(message);
                }
                AppEvent::HidePendingOverlay => {
                    self.pending_message = None;
                }
                AppEvent::FetchAll => {
                    self.fetch_all();
                }
                AppEvent::CheckoutCommit { target } => {
                    self.checkout_commit(target);
                }
                AppEvent::AutoRefresh => {
                    self.ec.clear_pending_refresh();
                    self.view.refresh();
                }
                AppEvent::OpenRefPicker { options, kind } => {
                    self.status_line_state.open_ref_picker(options, kind);
                }
                AppEvent::OpenCheckoutPicker { options, kind } => {
                    self.status_line_state.open_checkout_picker(options, kind);
                }
                AppEvent::OpenRelatedPicker { items } => {
                    self.status_line_state.open_related_picker(items);
                }
                AppEvent::OpenDeleteBranch { names } => {
                    let head_branch = match self.repository.head() {
                        Head::Branch { name } => Some(name.as_str()),
                        _ => None,
                    };
                    dispatch_delete_branch(&self.ec.sender(), &names, head_branch);
                }
                AppEvent::OpenDeleteBranchPicker { options, total } => {
                    self.status_line_state
                        .open_delete_branch_picker(options, total);
                }
                AppEvent::OpenDeleteBranchConfirm { name } => {
                    self.status_line_state.open_delete_branch_confirm(name);
                }
                AppEvent::OpenMergePrMethodPicker {
                    number,
                    head_ref,
                    state,
                } => {
                    self.status_line_state
                        .open_merge_pr_prompt(number, head_ref, state);
                }
                AppEvent::OpenToggleStatePrompt {
                    number,
                    kind,
                    action,
                    filter_state,
                } => {
                    self.status_line_state.open_toggle_state_prompt(
                        number,
                        kind,
                        action,
                        filter_state,
                    );
                }
                AppEvent::OpenTogglePrDraftPrompt {
                    number,
                    action,
                    filter_state,
                } => {
                    self.status_line_state.open_toggle_pr_draft_prompt(
                        number,
                        action,
                        filter_state,
                    );
                }
                AppEvent::PrDraftToggled { number, is_draft } => {
                    if let View::GitHub(ref mut view) = self.view {
                        view.set_pr_draft_flag(number, is_draft);
                    } else if let Some(ref mut data) = self.github_data {
                        if let Some(pr) = data.pull_requests.iter_mut().find(|p| p.number == number)
                        {
                            pr.is_draft = is_draft;
                        }
                    }
                }
                AppEvent::DeleteBranchRequested { name, force } => {
                    let list_context = self.current_list_refresh_context();
                    spawn_delete_branch(self.repository.path(), self.ec, name, force, list_context);
                }
                AppEvent::MergePrRequested {
                    number,
                    state,
                    method,
                    delete_branch,
                } => {
                    spawn_merge_pr(
                        self.repository.path(),
                        self.ec,
                        number,
                        state,
                        method,
                        delete_branch,
                    );
                }
                AppEvent::ToggleItemStateRequested {
                    number,
                    kind,
                    action,
                    filter_state,
                } => {
                    spawn_toggle_state(
                        self.repository.path(),
                        self.ec,
                        number,
                        kind,
                        action,
                        filter_state,
                    );
                }
                AppEvent::TogglePrDraftRequested {
                    number,
                    action,
                    filter_state,
                } => {
                    spawn_toggle_pr_draft(
                        self.repository.path(),
                        self.ec,
                        number,
                        action,
                        filter_state,
                    );
                }
                AppEvent::GitHubJumpToIssue { number } => {
                    if let View::GitHub(ref mut view) = self.view {
                        if !view.jump_to_issue(number) {
                            self.ec.send(AppEvent::NotifyWarn(format!(
                                "Issue #{number} not in current list (check state filter)"
                            )));
                        }
                    }
                }
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        // 處理 pending overlay——Esc 會把它藏起來
        if self.pending_message.is_some() {
            if let Some(UserEvent::Cancel) = self.ctx.keybind.get(&key) {
                self.pending_message = None;
                self.ec.send(AppEvent::NotifyInfo(
                    "Operation continues in background".into(),
                ));
                return;
            }
            // pending 期間擋掉其他按鍵
            return;
        }

        // Picker 會攔截輸入；ForceQuit（Ctrl-C）例外放行，讓
        // 使用者在 picker 中仍能離開程式。
        if !matches!(self.ctx.keybind.get(&key), Some(UserEvent::ForceQuit))
            && self.status_line_state.handle_intercepting_key(key)
        {
            return;
        }

        if self.status_line_state.dismiss_notification() {
            return;
        }

        let user_event = self.ctx.keybind.get(&key);

        if let Some(UserEvent::Cancel) = user_event {
            // 清掉數字前綴並取消這個事件
            if !self.key_state.take_count().is_empty() {
                return;
            }
        }

        match user_event {
            Some(UserEvent::ForceQuit) => {
                self.ec.send(AppEvent::Quit);
            }
            Some(UserEvent::Quit) => {
                if self.is_input_mode() {
                    self.view
                        .handle_event(UserEventWithCount::from_event(UserEvent::Unknown), key);
                } else if self.key_state.register_quit_press(Instant::now()) == QuitDecision::Second
                {
                    self.ec.send(AppEvent::Quit);
                    return;
                } else {
                    self.status_line_state
                        .set_notification_info("Press q again to quit".into());
                    self.ec
                        .sender()
                        .send_after(AppEvent::ClearStatusLine, Duration::from_millis(600));
                }
                self.key_state.clear_count();
            }
            Some(ue) => {
                self.key_state.reset_quit_press();
                let prefix = self.key_state.take_count();
                let event_with_count = process_numeric_prefix(&prefix, *ue, key);
                if let Some(app_event) = global_app_event(
                    event_with_count.event,
                    self.view.is_browsing_view(),
                    self.is_input_mode(),
                ) {
                    self.ec.send(app_event);
                    return;
                }
                self.view.handle_event(event_with_count, key);
            }
            None => {
                self.key_state.reset_quit_press();
                if self.is_input_mode() || matches!(self.view, View::Detail(_)) {
                    self.key_state.clear_count();
                    self.view
                        .handle_event(UserEventWithCount::from_event(UserEvent::Unknown), key);
                } else if let KeyCode::Char(c) = key.code {
                    self.key_state.push_digit(c);
                }
            }
        }
    }

    fn render(&mut self, f: &mut Frame) {
        let base = Block::default()
            .fg(self.ctx.color_theme.fg)
            .bg(self.ctx.color_theme.bg);
        f.render_widget(base, f.area());

        let [view_area, status_line_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(2)]).areas(f.area());

        self.update_state(view_area);

        let marquee_frame = self.marquee_frame;
        self.view.render(f, view_area, marquee_frame);
        self.status_line_state.render(
            f,
            status_line_area,
            &self.view,
            &self.key_state.numeric_prefix,
        );

        if let Some(message) = &self.pending_message {
            let overlay = PendingOverlay::new(message, &self.ctx.color_theme);
            f.render_widget(overlay, f.area());
        }
    }
}

impl<'a> App<'a> {
    fn enter_detail(&mut self, cls: CommitListState<'a>) {
        if cls.is_virtual_row_selected() {
            unreachable!("virtual row must be handled before reaching Detail");
        }
        let (commit, changes, refs) = selected_commit_details(self.repository, &cls);
        self.view = View::of_detail(
            cls,
            commit,
            changes,
            refs,
            self.ctx.clone(),
            self.ec.sender(),
        );
    }
}

impl App<'_> {
    fn update_state(&mut self, view_area: Rect) {
        self.view_area = view_area;
    }

    fn is_input_mode(&self) -> bool {
        self.status_line_state.is_input_mode_variant() || matches!(self.view, View::CreateTag(_))
    }

    fn current_list_refresh_context(&self) -> crate::view::ListRefreshViewContext {
        use crate::view::ListRefreshViewContext;
        match &self.view {
            View::List(v) => ListRefreshViewContext::from(v.as_list_state()),
            _ => ListRefreshViewContext {
                commit_hash: CommitHash::default(),
                selected: 0,
                height: 0,
                scroll_to_top: true,
                show_remote_refs: true,
            },
        }
    }

    fn open_detail(&mut self) {
        let commit_list_state = match self.view {
            View::List(ref mut view) => view.take_list_state(),
            View::UserCommand(ref mut view) => view.take_list_state(),
            _ => return,
        };
        let Some(commit_list_state) = commit_list_state else {
            return;
        };

        if commit_list_state.is_virtual_row_selected() {
            if let Some(wc) = commit_list_state.working_changes().cloned() {
                self.view = View::of_working_changes_detail(
                    commit_list_state,
                    wc,
                    self.ctx.clone(),
                    self.ec.sender(),
                );
            } else {
                self.view = View::of_list(commit_list_state, self.ctx.clone(), self.ec.sender());
            }
            return;
        }

        self.enter_detail(commit_list_state);
    }

    fn close_detail(&mut self) {
        if let View::Detail(ref mut view) = self.view {
            let Some(commit_list_state) = view.take_list_state() else {
                return;
            };
            self.view = View::of_list(commit_list_state, self.ctx.clone(), self.ec.sender());
        }
    }

    fn open_user_command(
        &mut self,
        user_command_number: usize,
        terminal: Option<&mut DefaultTerminal>,
    ) {
        let clear = match extract_user_command_by_number(user_command_number, &self.ctx)
            .map(|c| &c.r#type)
        {
            Ok(UserCommandType::Inline) => {
                self.open_user_command_inline(user_command_number);
                false
            }
            Ok(UserCommandType::Silent) => {
                self.open_user_command_silent(user_command_number);
                true
            }
            Ok(UserCommandType::Suspend) => {
                self.open_user_command_suspend(user_command_number);
                true
            }
            Err(err) => {
                self.ec.send(AppEvent::NotifyError(err));
                false
            }
        };
        if clear {
            if let Some(t) = terminal {
                if let Err(err) = t.clear() {
                    let msg = format!("Failed to clear terminal: {err:?}");
                    self.ec.send(AppEvent::NotifyError(msg));
                }
            }
        }
    }

    fn open_user_command_inline(&mut self, user_command_number: usize) {
        // Guard：略過 virtual row
        let is_virtual = match &self.view {
            View::List(view) => view.as_list_state().is_virtual_row_selected(),
            View::Detail(view) => view.as_list_state().is_virtual_row_selected(),
            View::UserCommand(view) => view.as_list_state().is_virtual_row_selected(),
            _ => false,
        };
        if is_virtual {
            return;
        }
        let commit_list_state = match self.view {
            View::List(ref mut view) => view.as_list_state(),
            View::Detail(ref mut view) => view.as_list_state(),
            View::UserCommand(ref mut view) => view.as_list_state(),
            _ => return,
        };
        let (commit, _, refs) = selected_commit_details(self.repository, commit_list_state);
        let result = build_external_command_parameters_and_exec_command(
            &commit,
            &refs,
            user_command_number,
            self.view_area,
            &self.ctx,
        );
        match result {
            Ok(output) => {
                // 只有指令執行成功才取出 list state，避免指令失敗時把 state 弄丟
                let commit_list_state = match self.view {
                    View::List(ref mut view) => view.take_list_state(),
                    View::Detail(ref mut view) => view.take_list_state(),
                    View::UserCommand(ref mut view) => view.take_list_state(),
                    _ => return,
                };
                let Some(commit_list_state) = commit_list_state else {
                    return;
                };
                self.view = View::of_user_command(
                    commit_list_state,
                    output,
                    user_command_number,
                    self.ctx.clone(),
                    self.ec.sender(),
                );
            }
            Err(err) => {
                self.ec.send(AppEvent::NotifyError(err));
            }
        };
    }

    fn open_user_command_silent(&mut self, user_command_number: usize) {
        let commit_list_state = match self.view {
            View::List(ref mut view) => view.as_list_state(),
            View::Detail(ref mut view) => view.as_list_state(),
            View::UserCommand(ref mut view) => view.as_list_state(),
            _ => return,
        };
        if commit_list_state.is_virtual_row_selected() {
            return;
        }
        let (commit, _, refs) = selected_commit_details(self.repository, commit_list_state);
        let result = build_external_command_parameters_and_exec_command(
            &commit,
            &refs,
            user_command_number,
            self.view_area,
            &self.ctx,
        );
        match result {
            Ok(_) => {
                if extract_user_command_refresh_by_number(user_command_number, &self.ctx) {
                    self.view.refresh();
                }
            }
            Err(err) => {
                self.ec.send(AppEvent::NotifyError(err));
            }
        }
    }

    fn open_user_command_suspend(&mut self, user_command_number: usize) {
        let commit_list_state = match self.view {
            View::List(ref mut view) => view.as_list_state(),
            View::Detail(ref mut view) => view.as_list_state(),
            View::UserCommand(ref mut view) => view.as_list_state(),
            _ => return,
        };
        if commit_list_state.is_virtual_row_selected() {
            return;
        }
        let (commit, _, refs) = selected_commit_details(self.repository, commit_list_state);
        match build_external_command_parameters(
            &commit,
            &refs,
            user_command_number,
            self.view_area,
            &self.ctx,
        ) {
            Ok(params) => {
                self.ec.suspend();
                let exec_result = exec_user_command_suspend(params);
                self.ec.resume();
                self.marquee_frame = 0;

                if extract_user_command_refresh_by_number(user_command_number, &self.ctx) {
                    self.view.refresh();
                }

                // 在 resume 與 refresh 之後才通知
                if let Err(err) = exec_result {
                    self.ec.send(AppEvent::NotifyError(err));
                }
            }
            Err(err) => {
                self.ec.send(AppEvent::NotifyError(err));
            }
        }
    }

    fn close_user_command(&mut self) {
        if let View::UserCommand(ref mut view) = self.view {
            let commit_list_state = view.take_list_state();
            if let Some(commit_list_state) = commit_list_state {
                self.view = View::of_list(commit_list_state, self.ctx.clone(), self.ec.sender());
                self.view.request_graph_clear();
            }
        }
    }

    fn open_refs(&mut self) {
        let origin = match self.view {
            View::List(_) => RefsOrigin::List,
            View::Detail(_) => RefsOrigin::Detail,
            _ => return,
        };
        self.open_refs_with_origin(origin);
    }

    fn open_refs_with_origin(&mut self, origin: RefsOrigin) {
        let commit_list_state = match self.view {
            View::List(ref mut view) => view.take_list_state(),
            View::Detail(ref mut view) => view.take_list_state(),
            _ => return,
        };
        let Some(commit_list_state) = commit_list_state else {
            return;
        };
        let refs: Vec<Ref> = self.repository.all_refs().into_iter().cloned().collect();
        self.view = View::of_refs(
            commit_list_state,
            refs,
            origin,
            self.ctx.clone(),
            self.ec.sender(),
        );
    }

    fn close_refs(&mut self) {
        if let View::Refs(ref mut view) = self.view {
            let origin = view.origin();
            let Some(commit_list_state) = view.take_list_state() else {
                return;
            };
            match origin {
                RefsOrigin::List => {
                    self.view =
                        View::of_list(commit_list_state, self.ctx.clone(), self.ec.sender());
                }
                RefsOrigin::Detail => {
                    self.enter_detail(commit_list_state);
                }
            }
            self.view.request_graph_clear();
        }
    }

    fn open_create_tag(&mut self) {
        if let View::List(ref mut view) = self.view {
            if view.as_list_state().is_virtual_row_selected() {
                return;
            }
            let Some(commit_list_state) = view.take_list_state() else {
                return;
            };
            let commit_hash = commit_list_state.selected_commit_hash().clone();
            self.view = View::of_create_tag(
                commit_list_state,
                commit_hash,
                self.repository.path().to_path_buf(),
                self.ctx.clone(),
                self.ec.sender(),
            );
        }
    }

    fn close_create_tag(&mut self) {
        if let View::CreateTag(ref mut view) = self.view {
            let Some(commit_list_state) = view.take_list_state() else {
                return;
            };
            self.view = View::of_list(commit_list_state, self.ctx.clone(), self.ec.sender());
            self.view.request_graph_clear();
        }
    }

    fn open_delete_tag(&mut self) {
        if let View::List(ref mut view) = self.view {
            if view.as_list_state().is_virtual_row_selected() {
                return;
            }
            let Some(commit_list_state) = view.take_list_state() else {
                return;
            };
            let commit_hash = commit_list_state.selected_commit_hash().clone();
            let tags: Vec<Ref> = commit_list_state
                .selected_commit_refs()
                .iter()
                .map(|r| (*r).clone())
                .collect();
            let has_tags = tags.iter().any(|r| matches!(r, Ref::Tag { .. }));
            if !has_tags {
                self.view = View::of_list(commit_list_state, self.ctx.clone(), self.ec.sender());
                self.ec
                    .send(AppEvent::NotifyWarn("No tags on this commit".into()));
                return;
            }
            self.view = View::of_delete_tag(
                commit_list_state,
                commit_hash,
                tags,
                self.repository.path().to_path_buf(),
                self.ctx.clone(),
                self.ec.sender(),
            );
        }
    }

    fn close_delete_tag(&mut self) {
        if let View::DeleteTag(ref mut view) = self.view {
            let Some(commit_list_state) = view.take_list_state() else {
                return;
            };
            self.view = View::of_list(commit_list_state, self.ctx.clone(), self.ec.sender());
            self.view.request_graph_clear();
        }
    }

    fn open_delete_ref(&mut self, ref_name: String, ref_type: RefType) {
        if let View::Refs(ref mut view) = self.view {
            let refs_origin = view.origin();
            let Some(commit_list_state) = view.take_list_state() else {
                return;
            };
            let ref_list_state = view.take_ref_list_state();
            let refs = view.take_refs();
            self.view = View::of_delete_ref(
                commit_list_state,
                ref_list_state,
                refs,
                self.repository.path().to_path_buf(),
                ref_name,
                ref_type,
                refs_origin,
                self.ctx.clone(),
                self.ec.sender(),
            );
        }
    }

    fn close_delete_ref(&mut self) {
        if let View::DeleteRef(ref mut view) = self.view {
            let refs_origin = view.refs_origin();
            let Some(commit_list_state) = view.take_list_state() else {
                return;
            };
            let ref_list_state = view.take_ref_list_state();
            let refs = view.take_refs();
            self.view = View::of_refs_with_state(
                commit_list_state,
                ref_list_state,
                refs,
                refs_origin,
                self.ctx.clone(),
                self.ec.sender(),
            );
        }
    }

    fn open_help(&mut self) {
        let before_view = std::mem::take(&mut self.view);
        self.view = View::of_help(before_view, self.ctx.clone(), self.ec.sender());
    }

    fn close_help(&mut self) {
        if let View::Help(ref mut view) = self.view {
            self.view = view.take_before_view();
            self.view.request_graph_clear();
        }
    }

    fn open_github(&mut self) {
        let data = match self.github_data.take() {
            Some(data) => data,
            None => {
                self.refresh_github(StateFilter::Open);
                crate::github::GitHubData::default()
            }
        };

        let before_view = std::mem::take(&mut self.view);
        self.view = View::of_github(before_view, data, self.ec.sender());
    }

    fn on_github_data_loaded(&mut self, data: crate::github::GitHubData, warnings: Vec<String>) {
        // 載入中使用者又切換了 filter → 丟棄這批資料，改抓新的
        if let Some(refetch) = self.github_load.on_load_settled() {
            self.refresh_github(refetch);
            return;
        }

        if let View::GitHub(ref mut view) = self.view {
            // 已在 GitHub 視圖：資料只有 view 這一份，直接寫進去；
            // 有沒有變更、要不要重置捲動位置由 view 自己決定
            view.update_data(
                data.issues,
                data.pull_requests,
                data.issues_next_cursor,
                data.prs_next_cursor,
            );
            if !warnings.is_empty() {
                view.set_flash(warnings.join("; "), false);
            }
        } else {
            // view 關著：整批快照直接覆蓋。這是背景操作（spawn_toggle_state
            // 等）觸發的 refresh，即使 view 已經關閉也要保留，重開時才看
            // 得到最新資料——跟下面 GitHubMore*Loaded「view 關著就丟棄」
            // 是刻意的不對稱，不是漏改的不一致，理由見那裡的註解。
            self.github_data = Some(data);
        }
    }

    /// 載入失敗與載入成功共用同一套「結束後有沒有排隊的 refetch」判斷——
    /// 失敗路徑漏接這段曾經是 Bug A。有排隊的 refetch 時不顯示這次的錯誤，
    /// 因為新的載入馬上就會蓋過它。
    fn on_github_load_failed(&mut self, error: String) {
        if let Some(refetch) = self.github_load.on_load_settled() {
            self.refresh_github(refetch);
            return;
        }
        if let View::GitHub(ref mut view) = self.view {
            view.set_error(error);
        }
    }

    fn refresh_github(&mut self, filter: StateFilter) {
        if !self.github_load.on_refresh_requested(filter) {
            return;
        }

        let repo_path = self.repository.path().to_path_buf();
        let tx = self.ec.sender();

        std::thread::spawn(move || {
            let state = filter.as_str();
            let issues_result = crate::github::list_issues(&repo_path, state, None);
            let prs_result = crate::github::list_pull_requests(&repo_path, state, None);

            let mut any_ok = false;
            let mut warnings = Vec::new();

            let (issues, issues_next_cursor) = match issues_result {
                Ok(page) => {
                    any_ok = true;
                    (page.items, page.next_cursor)
                }
                Err(e) => {
                    warnings.push(format!("GitHub issues unavailable: {e}"));
                    (Vec::new(), None)
                }
            };
            let (pull_requests, prs_next_cursor) = match prs_result {
                Ok(page) => {
                    any_ok = true;
                    (page.items, page.next_cursor)
                }
                Err(e) => {
                    warnings.push(format!("GitHub PRs unavailable: {e}"));
                    (Vec::new(), None)
                }
            };

            if any_ok {
                tx.send(AppEvent::GitHubDataLoaded {
                    data: crate::github::GitHubData {
                        issues,
                        pull_requests,
                        state_filter: filter,
                        issues_next_cursor,
                        prs_next_cursor,
                    },
                    warnings,
                });
            } else {
                tx.send(AppEvent::GitHubLoadFailed {
                    error: warnings.join("; "),
                });
            }
        });
    }

    /// `LoadMoreGitHub` 只在 view 開著時送出（見 view/github/event.rs），
    /// 所以這裡固定從 view 讀，不是從 `github_data`——view 開著時
    /// `github_data` 是 `None`，讀那邊會讓「載入更多」永久失效。
    fn load_more_github(&mut self, kind: GhItemKind, generation: u64) {
        let View::GitHub(ref view) = self.view else {
            return;
        };
        let state = view.state_filter();
        let Some(cursor) = view.next_cursor(kind) else {
            return;
        };
        let repo_path = self.repository.path().to_path_buf();
        let tx = self.ec.sender();

        std::thread::spawn(move || match kind {
            GhItemKind::Issue => {
                match crate::github::list_issues(&repo_path, state.as_str(), Some(&cursor)) {
                    Ok(page) => tx.send(AppEvent::GitHubMoreIssuesLoaded {
                        items: page.items,
                        next_cursor: page.next_cursor,
                        generation,
                    }),
                    Err(e) => tx.send(AppEvent::GitHubFlash {
                        message: format!("Load more failed: {e}"),
                        is_error: true,
                    }),
                }
            }
            GhItemKind::PullRequest => {
                match crate::github::list_pull_requests(&repo_path, state.as_str(), Some(&cursor)) {
                    Ok(page) => tx.send(AppEvent::GitHubMorePrsLoaded {
                        items: page.items,
                        next_cursor: page.next_cursor,
                        generation,
                    }),
                    Err(e) => tx.send(AppEvent::GitHubFlash {
                        message: format!("Load more failed: {e}"),
                        is_error: true,
                    }),
                }
            }
        });
    }

    fn load_github_timeline(&self, number: u64, kind: GhItemKind, after: Option<String>) {
        let repo_path = self.repository.path().to_path_buf();
        let tx = self.ec.sender();
        std::thread::spawn(move || {
            match crate::github::get_timeline(&repo_path, number, kind, after.as_deref()) {
                Ok(page) => tx.send(AppEvent::GitHubTimelineLoaded {
                    number,
                    kind,
                    after,
                    page,
                }),
                Err(e) => tx.send(AppEvent::GitHubTimelineFailed {
                    number,
                    kind,
                    error: e,
                }),
            }
        });
    }

    fn close_github(&mut self) {
        if let View::GitHub(ref mut view) = self.view {
            self.github_data = Some(view.take_data());
            self.view = view.take_before_view();
            self.view.request_graph_clear();
        }
    }

    fn batch_toggle_checkboxes(
        &mut self,
        number: u64,
        kind: GhItemKind,
        checkbox_indices: Vec<usize>,
    ) {
        self.pending_message = Some("Updating checkboxes...".to_string());

        let repo_path = self.repository.path().to_path_buf();
        let tx = self.ec.sender();
        let count = checkbox_indices.len();

        std::thread::spawn(move || {
            let result = (|| -> Result<String, String> {
                let body = crate::github::get_body(&repo_path, number, kind)?;
                let new_body = crate::github::toggle_checkboxes(&body, &checkbox_indices);
                crate::github::update_body(&repo_path, number, kind, &new_body)?;
                Ok(new_body)
            })();

            tx.send(AppEvent::HidePendingOverlay);

            match result {
                Ok(new_body) => {
                    tx.send(AppEvent::GitHubFlash {
                        message: format!("{count} checkbox(es) updated"),
                        is_error: false,
                    });
                    tx.send(AppEvent::CheckboxToggled {
                        number,
                        kind,
                        new_body,
                    });
                }
                Err(e) => {
                    tx.send(AppEvent::GitHubFlash {
                        message: format!("Batch toggle failed: {e}"),
                        is_error: true,
                    });
                }
            }
        });
    }

    fn on_checkbox_toggled(&mut self, number: u64, kind: GhItemKind, new_body: &str) {
        // 更新 list item 的 body 欄位，preview 直接從 body 渲染（零 API）。
        // 寫進資料目前唯一的擁有者：view 開著寫 view，關著寫 github_data。
        if let View::GitHub(ref mut view) = self.view {
            view.update_body_for_item(number, kind, new_body.to_string());
        } else if let Some(ref mut data) = self.github_data {
            match kind {
                GhItemKind::Issue => {
                    if let Some(issue) = data.issues.iter_mut().find(|i| i.number == number) {
                        issue.body = new_body.to_string();
                    }
                }
                GhItemKind::PullRequest => {
                    if let Some(pr) = data.pull_requests.iter_mut().find(|p| p.number == number) {
                        pr.body = new_body.to_string();
                    }
                }
            }
        }
    }

    fn select_older_commit(&mut self) {
        if let View::Detail(ref mut view) = self.view {
            view.select_older_commit(self.repository);
        } else if let View::UserCommand(ref mut view) = self.view {
            view.select_older_commit(
                self.repository,
                self.view_area,
                build_external_command_parameters_and_exec_command,
            );
        }
    }

    fn select_newer_commit(&mut self) {
        if let View::Detail(ref mut view) = self.view {
            view.select_newer_commit(self.repository);
        } else if let View::UserCommand(ref mut view) = self.view {
            view.select_newer_commit(
                self.repository,
                self.view_area,
                build_external_command_parameters_and_exec_command,
            );
        }
    }

    fn select_parent_commit(&mut self) {
        if let View::Detail(ref mut view) = self.view {
            view.select_parent_commit(self.repository);
        } else if let View::UserCommand(ref mut view) = self.view {
            view.select_parent_commit(
                self.repository,
                self.view_area,
                build_external_command_parameters_and_exec_command,
            );
        }
    }

    fn init_with_context(&mut self, context: RefreshViewContext) {
        if let View::List(ref mut view) = self.view {
            view.reset_commit_list_with(context.list_context());
        }
        match context {
            RefreshViewContext::List { .. } => {}
            RefreshViewContext::Detail { .. } => {
                self.open_detail();
            }
            RefreshViewContext::UserCommand {
                user_command_context,
                ..
            } => {
                self.open_user_command(user_command_context.n, None);
            }
            RefreshViewContext::Refs {
                refs_context,
                origin,
                ..
            } => {
                // origin 只是 close Refs 時「要回哪」的記號，不需要先把 view 切成 Detail
                // 再立刻被 open_refs_with_origin take 走 list_state——那會白跑一次 git diff。
                self.open_refs_with_origin(origin);
                if let View::Refs(ref mut view) = self.view {
                    view.reset_refs_with(refs_context);
                }
            }
        }
    }

    fn copy_to_clipboard(&self, name: String, value: String) {
        match copy_to_clipboard(value, &self.ctx.core_config.external.clipboard) {
            Ok(_) => {
                let msg = format!("Copied {name} to clipboard successfully");
                self.ec.send(AppEvent::NotifySuccess(msg));
            }
            Err(msg) => {
                self.ec.send(AppEvent::NotifyError(msg));
            }
        }
    }

    fn open_url(&mut self, url: String) {
        match crate::external::open_url(&url) {
            Ok(crate::external::OpenUrlOutcome::Spawned) => {
                self.ec.send(AppEvent::NotifyInfo(format!("Opening {url}")));
            }
            Ok(crate::external::OpenUrlOutcome::Hyperlinked(url)) => {
                self.status_line_state.set_notification_hyperlink(
                    SSH_OPEN_PREFIX,
                    short_link_label(&url),
                    url,
                );
            }
            Err(msg) => {
                self.ec.send(AppEvent::NotifyError(msg));
            }
        }
    }

    fn fetch_all(&self) {
        spawn_git_task(
            self.repository.path(),
            self.ec,
            &["fetch", "--all"],
            "Fetching...".into(),
            "Fetch completed".into(),
            "Fetch failed",
        );
    }

    fn checkout_commit(&self, target: String) {
        spawn_git_task(
            self.repository.path(),
            self.ec,
            &["checkout", &target],
            format!("Checking out '{target}'..."),
            format!("Checked out '{target}'"),
            "Checkout failed",
        );
    }
}

fn spawn_merge_pr(
    repo: &Path,
    ec: &EventController,
    number: u64,
    state: StateFilter,
    method: MergeMethod,
    delete_branch: bool,
) {
    let repo_path = repo.to_path_buf();
    let tx = ec.sender();
    ec.send(AppEvent::ShowPendingOverlay {
        message: format!("Merging PR #{number}..."),
    });
    std::thread::spawn(move || {
        let result = merge_pr(&repo_path, number, method.as_flag(), delete_branch);
        tx.send(AppEvent::HidePendingOverlay);
        match result {
            Ok(()) => {
                tx.send(AppEvent::NotifySuccess(format!(
                    "PR #{number} merged ({})",
                    method.display()
                )));
                tx.send(AppEvent::RefreshGitHub { state });
            }
            Err(e) => {
                if is_merge_conflict_error(&e) {
                    tx.send(AppEvent::NotifyWarn(format!(
                        "PR #{number} has conflicts — resolve before merging"
                    )));
                } else {
                    tx.send(AppEvent::NotifyError(e));
                }
            }
        }
    });
}

fn spawn_toggle_pr_draft(
    repo: &Path,
    ec: &EventController,
    number: u64,
    action: PrDraftAction,
    filter_state: StateFilter,
) {
    let repo_path = repo.to_path_buf();
    let tx = ec.sender();
    ec.send(AppEvent::ShowPendingOverlay {
        message: action.pending(number),
    });
    std::thread::spawn(move || {
        let result = set_pr_draft(&repo_path, number, action);
        tx.send(AppEvent::HidePendingOverlay);
        match result {
            Ok(()) => {
                tx.send(AppEvent::NotifySuccess(action.success(number)));
                tx.send(AppEvent::PrDraftToggled {
                    number,
                    is_draft: action.result_is_draft(),
                });
                tx.send(AppEvent::RefreshGitHub {
                    state: filter_state,
                });
            }
            Err(e) => {
                tx.send(AppEvent::NotifyError(e));
            }
        }
    });
}

fn spawn_toggle_state(
    repo: &Path,
    ec: &EventController,
    number: u64,
    kind: GhItemKind,
    action: StateAction,
    filter_state: StateFilter,
) {
    let repo_path = repo.to_path_buf();
    let tx = ec.sender();
    ec.send(AppEvent::ShowPendingOverlay {
        message: action.pending(kind, number),
    });
    std::thread::spawn(move || {
        let result = set_item_state(&repo_path, kind, number, action);
        tx.send(AppEvent::HidePendingOverlay);
        match result {
            Ok(()) => {
                tx.send(AppEvent::NotifySuccess(action.success(kind, number)));
                tx.send(AppEvent::RefreshGitHub {
                    state: filter_state,
                });
            }
            Err(e) => {
                tx.send(AppEvent::NotifyError(e));
            }
        }
    });
}

fn spawn_delete_branch(
    repo: &Path,
    ec: &EventController,
    name: String,
    force: bool,
    list_context: crate::view::ListRefreshViewContext,
) {
    use crate::git::{delete_branch, delete_branch_force};

    let repo_path = repo.to_path_buf();
    let tx = ec.sender();

    let pending = if force {
        format!("Force deleting branch '{name}'...")
    } else {
        format!("Deleting branch '{name}'...")
    };
    ec.send(AppEvent::ShowPendingOverlay { message: pending });

    std::thread::spawn(move || {
        let result = if force {
            delete_branch_force(&repo_path, &name)
        } else {
            delete_branch(&repo_path, &name)
        };
        tx.send(AppEvent::HidePendingOverlay);
        match result {
            Ok(()) => {
                tx.send(AppEvent::NotifySuccess(format!("Branch '{name}' deleted")));
                tx.send(AppEvent::Refresh(RefreshViewContext::List { list_context }));
            }
            Err(e) => {
                let hint = if !force && e.contains("not fully merged") {
                    format!("{e}  (press d → f to force delete)")
                } else {
                    e
                };
                tx.send(AppEvent::NotifyError(hint));
            }
        }
    });
}

fn spawn_git_task(
    repo: &Path,
    ec: &EventController,
    args: &[&str],
    pending_msg: String,
    success_msg: String,
    error_prefix: &str,
) {
    let repo_path = repo.to_path_buf();
    let tx = ec.sender();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let error_prefix = error_prefix.to_string();
    // 預先 set pending flag，讓 git watcher 在 debounce 視窗內偵測到的 fs 事件
    // 被吞掉；主動 refresh 走完後，watcher 不會重複觸發 slow-path。
    ec.mark_pending_refresh();

    tx.send(AppEvent::ShowPendingOverlay {
        message: pending_msg,
    });

    std::thread::spawn(move || {
        let output = std::process::Command::new("git")
            .args(&args)
            .current_dir(&repo_path)
            .output();

        tx.send(AppEvent::HidePendingOverlay);
        match output {
            Ok(o) if o.status.success() => {
                let detail = String::from_utf8_lossy(&o.stderr).trim().to_string();
                let msg = if detail.is_empty() {
                    success_msg
                } else {
                    detail
                };
                tx.send(AppEvent::NotifySuccess(msg));
                tx.send(AppEvent::AutoRefresh);
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr).trim().to_string();
                tx.send(AppEvent::NotifyError(format!("{error_prefix}: {stderr}")));
            }
            Err(e) => {
                tx.send(AppEvent::NotifyError(format!("{error_prefix}: {e}")));
            }
        }
    });
}

fn selected_commit_details(
    repository: &Repository,
    commit_list_state: &CommitListState,
) -> (Commit, Vec<FileChange>, Vec<Ref>) {
    let selected = commit_list_state.selected_commit_hash().clone();
    let (commit, changes) = repository.commit_detail(&selected);
    let refs: Vec<Ref> = repository.refs(&selected).into_iter().cloned().collect();
    (commit.clone(), changes, refs)
}

/// 這個事件該不該在 app 層攔成全域事件，而不是往下交給 view 處理。
///
/// 兩個條件缺一不可：
///
/// - `browsing_view`：modal / input view（CreateTag、DeleteTag、DeleteRef、
///   UserCommand、GitHub）保有自己的 keymap，不能被攔。
/// - `!input_mode`：**這條原本漏了**。filter 與 search 的輸入框活在 List view
///   的狀態列裡，而 List 本身就是 browsing view，所以只看前一個條件會把使用者
///   正在打的 `g` 與 `?` 攔成 github / help —— `log`、`config`、`graph`、
///   `merge` 這些字通通打不進去，打到一半畫面還會跳走。
///
/// 放行之後不需要另外轉換事件：`view::list` 的 `resolve_input_action` 有
/// catch-all，非控制類事件一律當成文字輸入。
fn global_app_event(event: UserEvent, browsing_view: bool, input_mode: bool) -> Option<AppEvent> {
    if !browsing_view || input_mode {
        return None;
    }
    match event {
        UserEvent::GitHubToggle => Some(AppEvent::OpenGitHub),
        UserEvent::HelpToggle => Some(AppEvent::OpenHelp),
        _ => None,
    }
}

fn process_numeric_prefix(
    numeric_prefix: &str,
    user_event: UserEvent,
    _key_event: KeyEvent,
) -> UserEventWithCount {
    if user_event.is_countable() {
        let count = if numeric_prefix.is_empty() {
            1
        } else {
            numeric_prefix.parse::<usize>().unwrap_or(1)
        };
        UserEventWithCount::new(user_event, count)
    } else {
        UserEventWithCount::from_event(user_event)
    }
}

fn extract_user_command_by_number(
    user_command_number: usize,
    ctx: &AppContext,
) -> Result<&UserCommand, String> {
    ctx.core_config
        .user_command
        .commands
        .get(&user_command_number.to_string())
        .ok_or_else(|| format!("No user command configured for number {user_command_number}",))
}

fn extract_user_command_refresh_by_number(user_command_number: usize, ctx: &AppContext) -> bool {
    extract_user_command_by_number(user_command_number, ctx)
        .map(|c| c.refresh)
        .unwrap_or_default()
}

fn build_external_command_parameters_and_exec_command(
    commit: &Commit,
    refs: &[Ref],
    user_command_number: usize,
    view_area: Rect,
    ctx: &AppContext,
) -> Result<String, String> {
    build_external_command_parameters(commit, refs, user_command_number, view_area, ctx)
        .and_then(exec_user_command)
}

fn build_external_command_parameters<'a>(
    commit: &'a Commit,
    refs: &'a [Ref],
    user_command_number: usize,
    view_area: Rect,
    ctx: &'a AppContext,
) -> Result<ExternalCommandParameters<'a>, String> {
    let command = &extract_user_command_by_number(user_command_number, ctx)?.commands;
    let target_hash = commit.commit_hash.as_str();
    let parent_hashes = commit
        .parent_commit_hashes
        .iter()
        .map(|c| c.as_str())
        .collect();

    let mut all_refs = vec![];
    let mut branches = vec![];
    let mut remote_branches = vec![];
    let mut tags = vec![];
    let mut stash = None;
    for r in refs {
        match r {
            Ref::Tag { .. } => tags.push(r.name()),
            Ref::Branch { .. } => branches.push(r.name()),
            Ref::RemoteBranch { .. } => remote_branches.push(r.name()),
            Ref::Stash { .. } => {
                stash = Some(r.name());
                continue; // {{refs}} 不列入 stash
            }
        }
        all_refs.push(r.name());
    }

    let area_width = view_area.width.saturating_sub(4); // 扣掉左右 padding
    let area_height = (view_area.height.saturating_sub(1))
        .min(ctx.ui_config.user_command.height)
        .saturating_sub(1); // 扣掉上邊框
    Ok(ExternalCommandParameters {
        command,
        target_hash,
        parent_hashes,
        all_refs,
        branches,
        remote_branches,
        tags,
        stash,
        area_width,
        area_height,
    })
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rustfmt::skip]
    #[rstest]
    #[case("",    UserEvent::NavigateDown, UserEventWithCount::new(UserEvent::NavigateDown, 1))] // 無前綴
    #[case("5",   UserEvent::NavigateUp,   UserEventWithCount::new(UserEvent::NavigateUp, 5))] // 有前綴
    #[case("0",   UserEvent::PageDown,     UserEventWithCount::new(UserEvent::PageDown, 1))] // 零應轉換成 1
    #[case("42",  UserEvent::ScrollDown,   UserEventWithCount::new(UserEvent::ScrollDown, 42))] // 多位數
    #[case("999", UserEvent::PageDown,     UserEventWithCount::new(UserEvent::PageDown, 999))] // 大數字
    #[case("abc", UserEvent::ScrollUp,     UserEventWithCount::new(UserEvent::ScrollUp, 1))] // 應該 fallback 成 1
    #[case("5",   UserEvent::Quit,         UserEventWithCount::new(UserEvent::Quit, 1))] // 不可計數事件，有前綴
    #[case("",    UserEvent::Confirm,      UserEventWithCount::new(UserEvent::Confirm, 1))] // 不可計數事件，無前綴
    fn test_process_numeric_prefix(
        #[case] numeric_prefix: &str,
        #[case] user_event: UserEvent,
        #[case] expected: UserEventWithCount,
    ) {
        let dummy_key_event = KeyEvent::from(KeyCode::Enter); // 邏輯中不會用到 KeyEvent
        let actual = process_numeric_prefix(numeric_prefix, user_event, dummy_key_event);
        assert_eq!(actual, expected);
    }

    /// 打字時 `g` 與 `?` 必須進得了輸入框。
    ///
    /// 原本的判斷只有 `is_browsing_view()`，但 filter / search 的輸入框就長在
    /// List view 裡，於是 `log`、`config`、`graph`、`merge`⋯⋯ 打到含 g 的那個
    /// 字母就被攔去開 GitHub view，`?` 則跳出說明頁。
    #[rustfmt::skip]
    #[rstest]
    // browsing view + 非輸入模式：正常攔截
    #[case(UserEvent::GitHubToggle, true,  false, true)]
    #[case(UserEvent::HelpToggle,   true,  false, true)]
    // 輸入模式：一律放行給 view 當文字處理
    #[case(UserEvent::GitHubToggle, true,  true,  false)]
    #[case(UserEvent::HelpToggle,   true,  true,  false)]
    // 非 browsing view（modal）：本來就不攔
    #[case(UserEvent::GitHubToggle, false, false, false)]
    #[case(UserEvent::HelpToggle,   false, false, false)]
    // 其他事件無論如何都不是全域事件
    #[case(UserEvent::NavigateDown, true,  false, false)]
    #[case(UserEvent::Search,       true,  false, false)]
    fn global_app_event_never_swallows_typed_keys(
        #[case] event: UserEvent,
        #[case] browsing_view: bool,
        #[case] input_mode: bool,
        #[case] intercepted: bool,
    ) {
        let actual = global_app_event(event, browsing_view, input_mode);
        assert_eq!(
            actual.is_some(),
            intercepted,
            "event={event:?} browsing={browsing_view} input={input_mode} 得到 {actual:?}"
        );
    }

    #[test]
    fn global_app_event_maps_to_the_matching_view() {
        assert!(matches!(
            global_app_event(UserEvent::GitHubToggle, true, false),
            Some(AppEvent::OpenGitHub)
        ));
        assert!(matches!(
            global_app_event(UserEvent::HelpToggle, true, false),
            Some(AppEvent::OpenHelp)
        ));
    }

    #[rstest]
    #[case("https://github.com/o/r/issues/123", "[#123]")]
    #[case("https://github.com/o/r/pull/456", "[#456]")]
    #[case("https://github.com/o/r/issues/123/", "[#123]")]
    #[case("https://github.com/o/r/commit/abcd1234", "[open]")]
    #[case("https://example.com/", "[open]")]
    #[case("https://github.com/o/r/issues/not-a-number", "[open]")]
    fn short_link_label_extracts_issue_pr_number(#[case] url: &str, #[case] expected: &str) {
        assert_eq!(short_link_label(url), expected);
    }

    // ---- GitHubLoad -----------------------------------------------------

    #[test]
    fn github_load_first_request_starts_loading_immediately() {
        let mut load = GitHubLoad::Idle;
        assert!(load.on_refresh_requested(StateFilter::Open));
        assert_eq!(load, GitHubLoad::Loading);
    }

    #[test]
    fn github_load_request_while_loading_queues_instead_of_spawning() {
        let mut load = GitHubLoad::Loading;
        assert!(!load.on_refresh_requested(StateFilter::Closed));
        assert_eq!(load, GitHubLoad::LoadingThenRefetch(StateFilter::Closed));
    }

    /// 載入中連續切了兩次 filter，只留最後一次要抓的——不是排隊兩次。
    #[test]
    fn github_load_second_switch_while_loading_overwrites_the_queued_filter() {
        let mut load = GitHubLoad::Loading;
        load.on_refresh_requested(StateFilter::Closed);
        load.on_refresh_requested(StateFilter::All);
        assert_eq!(load, GitHubLoad::LoadingThenRefetch(StateFilter::All));
    }

    #[test]
    fn github_load_settles_to_idle_when_nothing_queued() {
        let mut load = GitHubLoad::Loading;
        assert_eq!(load.on_load_settled(), None);
        assert_eq!(load, GitHubLoad::Idle);
    }

    /// Bug A 的回歸測試：失敗結束也要能消費排隊的 refetch，不是只有成功
    /// 路徑才處理——失敗與成功呼叫的是同一個方法，天生對稱。
    #[test]
    fn github_load_settled_after_failure_still_returns_the_queued_refetch() {
        let mut load = GitHubLoad::LoadingThenRefetch(StateFilter::Closed);
        assert_eq!(load.on_load_settled(), Some(StateFilter::Closed));
        assert_eq!(load, GitHubLoad::Idle);
    }

    /// 消費完 pending 之後，緊接著呼叫 `on_refresh_requested` 必須真的能
    /// 再 spawn 一次（狀態已經回到 `Idle`），否則 refetch 只會被記錄卻永遠
    /// 不會真的發生。
    #[test]
    fn github_load_refetch_after_settle_actually_spawns() {
        let mut load = GitHubLoad::LoadingThenRefetch(StateFilter::Closed);
        let refetch = load.on_load_settled().expect("queued filter");
        assert!(load.on_refresh_requested(refetch));
        assert_eq!(load, GitHubLoad::Loading);
    }

    // ---- KeyState ---------------------------------------------------------

    #[test]
    fn key_state_push_digit_accumulates() {
        let mut ks = KeyState::default();
        ks.push_digit('5');
        assert_eq!(ks.numeric_prefix, "5");
        ks.push_digit('4');
        assert_eq!(ks.numeric_prefix, "54");
    }

    /// 前導零是既有規則：`0` 自己不算前綴，只有已經有前綴時才能接上。
    #[test]
    fn key_state_push_digit_rejects_leading_zero() {
        let mut ks = KeyState::default();
        ks.push_digit('0');
        assert_eq!(ks.numeric_prefix, "");

        ks.push_digit('1');
        ks.push_digit('0');
        assert_eq!(ks.numeric_prefix, "10");
    }

    #[test]
    fn key_state_push_digit_rejects_non_digit() {
        let mut ks = KeyState::default();
        ks.push_digit('a');
        assert_eq!(ks.numeric_prefix, "");
    }

    #[test]
    fn key_state_take_count_reads_and_clears_in_one_step() {
        let mut ks = KeyState::default();
        ks.push_digit('4');
        ks.push_digit('2');
        assert_eq!(ks.take_count(), "42");
        assert_eq!(ks.numeric_prefix, "");
    }

    #[test]
    fn key_state_clear_count_empties_the_prefix() {
        let mut ks = KeyState::default();
        ks.push_digit('9');
        ks.clear_count();
        assert_eq!(ks.numeric_prefix, "");
    }

    #[test]
    fn key_state_first_quit_press_only_arms_the_window() {
        let mut ks = KeyState::default();
        let t0 = Instant::now();
        assert_eq!(ks.register_quit_press(t0), QuitDecision::First);
    }

    #[test]
    fn key_state_second_quit_press_within_window_confirms_quit() {
        let mut ks = KeyState::default();
        let t0 = Instant::now();
        ks.register_quit_press(t0);
        let decision = ks.register_quit_press(t0 + Duration::from_millis(100));
        assert_eq!(decision, QuitDecision::Second);
    }

    /// 超過視窗就當作重新開始一輪，不是「累積更久的雙擊」。
    #[test]
    fn key_state_quit_press_after_window_restarts_as_first() {
        let mut ks = KeyState::default();
        let t0 = Instant::now();
        ks.register_quit_press(t0);
        let decision = ks.register_quit_press(t0 + Duration::from_millis(700));
        assert_eq!(decision, QuitDecision::First);
    }

    #[test]
    fn key_state_reset_quit_press_breaks_the_double_press_window() {
        let mut ks = KeyState::default();
        let t0 = Instant::now();
        ks.register_quit_press(t0);
        ks.reset_quit_press();
        let decision = ks.register_quit_press(t0 + Duration::from_millis(100));
        assert_eq!(decision, QuitDecision::First, "reset 之後視窗不該還算數");
    }
}

use std::path::{Path, PathBuf};
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
    auto_fetch,
    color::{ColorTheme, GraphColorSet},
    config::{CoreConfig, CoreShellConfig, UiConfig, UserCommand, UserCommandType},
    event::{AppEvent, EventController, UserEvent, UserEventWithCount},
    external::{
        copy_to_clipboard, exec_user_command, exec_user_command_suspend, is_posix_shell,
        ExternalCommandParameters,
    },
    git::{background_command, Commit, CommitHash, FileChange, Head, Ref, RefType, Repository},
    github::{
        delete_remote_branch as gh_delete_remote_branch, is_merge_conflict_error, merge_pr,
        set_item_state, set_pr_draft, GhItemKind, MergeMethod, PrDraftAction, StateAction,
        StateFilter,
    },
    graph::{Graph, GraphStyle},
    keybind::KeyBind,
    process::run_with_timeout,
    update::UpdateSettings,
    view::{dispatch_delete_branch, RefreshViewContext, RefsOrigin, View, ViewContext},
    widget::{
        commit_list::{CommitInfo, CommitListState, RawCommitIdx},
        pending_overlay::PendingOverlay,
    },
    CompactType, GraphWidthType,
};

use status_line::StatusLineState;

mod status_line;

#[derive(Clone, Copy)]
pub enum InitialSelection {
    Latest,
    Head,
}

pub enum Ret {
    /// `Some` = 離開前 exec 這個路徑的執行檔（自我更新完成後的重啟）。
    Quit(Option<PathBuf>),
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
    /// 已合併 CLI／設定檔的自動更新設定，`update::spawn_check` 與
    /// `auto_restart` 的中斷判斷共用同一份，不各自再 `.or()` 一遍。
    pub update: UpdateSettings,
    /// 已合併 CLI／設定檔的自動 fetch 設定，`AppEvent::AutoFetchPoll` 的
    /// handler 讀 `interval` 來重新武裝下一輪。
    pub auto_fetch: auto_fetch::AutoFetchSettings,
    /// 內嵌命令列（`/`）執行指令用的 `[程式, 旗標...]`——已解析完成的
    /// 最終值，不同於 `core_config.shell.command` 那個可能是 `None` 的
    /// 原始設定，見 `resolve_shell_command`。
    pub shell_command: Vec<String>,
}

/// `AppContext.shell_command` 的解析邏輯：設定檔的原始值到 `resolve_shell_command`
/// 這裡就結束了，這是已經決定好「開什麼、帶什麼旗標」的最終值。`shell_env`
/// 由呼叫端傳入（而不是內部呼叫 `env::var`），純函式，方便測試決定性。
pub(crate) fn resolve_shell_command(cfg: &CoreShellConfig, shell_env: Option<&str>) -> Vec<String> {
    if let Some(command) = &cfg.command {
        return command.clone();
    }
    default_shell_command(shell_env)
}

#[cfg(target_os = "windows")]
fn default_shell_command(_shell_env: Option<&str>) -> Vec<String> {
    vec!["cmd".to_string(), "/C".to_string()]
}

/// 非 Windows 平台的自動判斷：`$SHELL`（沒有就退回 `sh`）——POSIX shell
/// （`is_posix_shell` 的 basename allowlist）額外帶 `-i`，讓內嵌命令列吃得
/// 到 `~/.zshrc`／`~/.bashrc` 定義的 alias／function（非互動 shell 只讀
/// `~/.zshenv` 這類極簡設定檔，讀不到）；fish、nushell 這些不在表內的
/// shell 不冒然加只有 POSIX shell 才認得的旗標。
#[cfg(not(target_os = "windows"))]
fn default_shell_command(shell_env: Option<&str>) -> Vec<String> {
    let shell = shell_env
        .filter(|s| !s.is_empty())
        .unwrap_or("sh")
        .to_string();
    if is_posix_shell(&shell) {
        vec![shell, "-i".to_string(), "-c".to_string()]
    } else {
        vec![shell, "-c".to_string()]
    }
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
    /// `AppEvent::ExeReplacedCheck` 自動重啟後，新 process 顯示的一次性
    /// 通知。跟 `pending_message` 分開存：它不擋鍵盤（不是「操作進行
    /// 中」），Cancel 只是關掉它，不像 `pending_message` 的 Cancel 要送
    /// 「Operation continues in background」——沒有背景操作可言。
    notice_message: Option<String>,
    /// `None` 代表資料現在活在開著的 `GitHubView` 裡；兩者不會同時各存一份。
    github_data: Option<crate::github::GitHubData>,
    github_load: GitHubLoad,
    ctx: Rc<AppContext>,
    ec: &'a EventController,
    marquee_frame: u64,
    marquee_needed: bool,
    last_marquee_id: Option<std::sync::Arc<str>>,
    /// 上一幀畫出來的 auto-fetch 倒數秒數，`Tick` 用它判斷這一幀要不要重畫
    /// （見 `run()` 的 `Tick` arm）。跟上面兩個欄位同一類「上一幀畫了什麼」的
    /// 狀態；`App` 重建時歸 `None`，最多多畫一幀，無妨。
    last_countdown_secs: Option<u64>,
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
        let status_line_state =
            StatusLineState::new(ctx.clone(), ec.sender(), ec.auto_fetch_clock());

        let mut app = Self {
            repository,
            view,
            key_state: KeyState::default(),
            view_area: Rect::default(),
            status_line_state,
            pending_message: None,
            notice_message: None,
            github_data: None,
            github_load: GitHubLoad::Idle,
            ctx,
            ec,
            marquee_frame: 0,
            marquee_needed: false,
            last_marquee_id: None,
            last_countdown_secs: None,
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
                    // Tick 是 100ms 一次，但倒數每秒才需要動一次——以「該顯示
                    // 的秒數變了」當重畫條件，沒開 auto-fetch 時
                    // `countdown_secs()` 恆為 `None`，`changed` 恆為 false，
                    // `skip_draw` 的行為跟這個功能出現之前完全一樣。
                    //
                    // 這三行放在 `if` 外面、無條件執行，不要搬進 `else`
                    // 分支：那樣跑馬燈期間根本不會算，欄位會一路發霉，跑馬燈
                    // 停下來的那一刻就用一個過期值去比對。
                    let countdown = self.status_line_state.countdown_secs();
                    let changed = countdown != self.last_countdown_secs;
                    self.last_countdown_secs = countdown;

                    if self.marquee_needed {
                        self.marquee_frame = self.marquee_frame.wrapping_add(1);
                    } else if !changed {
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
                    return Ok(Ret::Quit(None));
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
                AppEvent::OpenShell => {
                    self.open_shell();
                }
                AppEvent::CloseShell => {
                    self.close_shell();
                }
                AppEvent::ShellOutputReady => {
                    self.view.poll_shell_output();
                    // 指令跑完那一刻——watcher 在指令執行期間設過旗標的話
                    // （`mark_refresh_pending`），現在才是安全的重建時機。
                    if let View::Shell(ref mut view) = self.view {
                        if let Some(context) = view.take_pending_refresh_context() {
                            return Ok(Ret::Refresh(RefreshRequest { context }));
                        }
                    }
                }
                AppEvent::OpenReleaseNotes { body } => {
                    self.open_release_notes(body);
                }
                AppEvent::CloseReleaseNotes => {
                    terminal.clear()?;
                    self.close_release_notes();
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
                AppEvent::SelectChildCommit => {
                    self.select_child_commit();
                }
                AppEvent::SelectChildCommitByHash { hash } => {
                    self.select_commit_by_hash(&hash);
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
                    if let Some(request) = self.shell_refresh_request() {
                        return Ok(Ret::Refresh(request));
                    }
                    self.view.refresh();
                }
                AppEvent::AutoFetchPoll => {
                    // `remaining()` 只有從未 arm 過才是 `None`（等同已到期）；
                    // 三個分支是互斥的線性優先序，用 if/else 比 match 上兩個
                    // 跟 scrutinee 無關的 `_ if` guard 直白。
                    let remaining = self.ec.auto_fetch_clock().remaining().unwrap_or_default();
                    if !remaining.is_zero() {
                        // 醒早了——deadline 在我排定之後被手動 fetch 的重算
                        // （`spawn_resync`）往後推過，睡到真正的 deadline，
                        // 不做任何網路動作。這個分支讓「舊排程自然收斂成
                        // 一條」不需要額外的世代比對。
                        self.ec
                            .sender()
                            .send_after(AppEvent::AutoFetchPoll, remaining);
                    } else if self.pending_message.is_some() {
                        // 有 blocking overlay：這一輪跳過網路工作，deadline
                        // 原封不動往後推一個 interval——連跳過都不能連重新
                        // 武裝一起跳過，否則這條鏈永久死掉。
                        //
                        // 走 `rearm` 而不是直接 `send_after`：倒數的 deadline
                        // 必須跟著這一輪一起往後推。overlay 蓋著的時候狀態列
                        // 本來就看不見，症狀要等 overlay 關掉才顯現——倒數卡在
                        // `00:00` 一整個 interval，看起來像 auto-fetch 死了。
                        self.rearm_auto_fetch();
                    } else {
                        auto_fetch::spawn_poll(self.ec, self.repository.path());
                    }
                }
                AppEvent::AutoFetchPolled { fingerprint } => {
                    // 唯一的比對點：`ls-remote` 失敗（`None`）、基準暫時是
                    // `None`（resync 進行中）、或兩者相等，都落進 `_`，
                    // 統一當「這輪什麼都不用做」處理——尤其是 `None` 基準
                    // 那個情況：若誤判成「有差異」，手動 fetch 剛清空基準
                    // 的空窗期就會蓋出重複的 overlay，這是整個重新設計要
                    // 關上的那扇窗，只能有一處判準，不能有第二份。
                    let due = match (fingerprint, self.ec.auto_fetch_clock().baseline()) {
                        (Some(fp), Some(base)) if fp != base => Some(fp),
                        _ => None,
                    };
                    match due {
                        Some(candidate) if self.can_interrupt() => {
                            auto_fetch::spawn_due_fetch(
                                self.ec,
                                self.repository.path(),
                                candidate,
                                self.ctx.auto_fetch.interval,
                            );
                        }
                        _ => self.rearm_auto_fetch(),
                    }
                }
                AppEvent::AutoFetchResync => {
                    if self.ctx.auto_fetch.mode == auto_fetch::AutoFetch::On {
                        auto_fetch::spawn_resync(
                            self.ec,
                            self.repository.path(),
                            self.ctx.auto_fetch.interval,
                        );
                    }
                }
                AppEvent::AutoFetchCompleted => {
                    // 使用者正在 picker／輸入框裡的時候不搶 status line；
                    // refresh 照做，只有要不要出聲用這道守衛。刻意不用
                    // `can_interrupt()`——它連 GitHub view 都排除，而盯著
                    // GitHub view 等 PR 被 merge 正是這個功能存在的理由。
                    if self.status_line_state.is_idle_or_notification() {
                        self.status_line_state.set_notification_success(
                            status_line::AUTO_FETCH_SUCCESS_MSG.to_string(),
                        );
                    }
                    if let Some(request) = self.shell_refresh_request() {
                        return Ok(Ret::Refresh(request));
                    }
                    self.view.refresh();
                }
                AppEvent::OpenRefPicker { options, kind } => {
                    self.status_line_state.open_ref_picker(options, kind);
                }
                AppEvent::OpenCheckoutPicker { options, kind } => {
                    self.status_line_state.open_checkout_picker(options, kind);
                }
                AppEvent::OpenChildPicker { options } => {
                    self.status_line_state.open_child_picker(options);
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
                    deletable,
                } => {
                    self.status_line_state
                        .open_merge_pr_prompt(number, head_ref, state, deletable);
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
                    delete_remote_branch,
                } => {
                    spawn_merge_pr(
                        self.repository.path(),
                        self.ec,
                        number,
                        state,
                        method,
                        delete_remote_branch,
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
                AppEvent::CheckUpdate => {
                    crate::update::spawn_check(self.ec, true, self.ctx.update);
                }
                AppEvent::PeriodicUpdateCheck => {
                    // 磁碟上的執行檔已經不是我在跑的那一份（本 process 裝好
                    // 還沒重啟，或別的 ysgit 實例／手動部署換掉了）：不再
                    // 檢查，也不重新武裝——鏈就停在這裡，下一輪 interval
                    // 不會再有這個事件。重啟後是全新 process，
                    // `lib.rs::run()` 會重新排第一次；`auto_restart` 開著
                    // 的話，`AppEvent::ExeReplacedCheck` 會在使用者閒置時
                    // 主動觸發那次重啟，這裡不必自己等。
                    if !crate::update::exe_is_stale() {
                        crate::update::spawn_check(self.ec, false, self.ctx.update);
                        self.ec
                            .sender()
                            .send_after(AppEvent::PeriodicUpdateCheck, self.ctx.update.interval);
                    }
                }
                AppEvent::ExeReplacedCheck => {
                    // 無條件、放在任何 if 之前重新武裝——這條鏈跟
                    // `PeriodicUpdateCheck` 語意相反：那條在 stale 時刻意
                    // 停鏈，這條的守衛沒過只代表「這輪不方便」，鏈絕對不能
                    // 死，否則使用者永遠等不到自動重啟。
                    //
                    // 排在收到事件的當下、不是像 `AutoFetchPoll` 那樣在
                    // worker 尾端：一輪的工作是一次 `stat(2)` 加偶爾一次
                    // `--version` fork，不可能跟下一輪疊在一起。
                    self.ec.sender().send_after(
                        AppEvent::ExeReplacedCheck,
                        crate::update::EXE_CHECK_INTERVAL,
                    );
                    // `can_interrupt()` 先問：幾個欄位比對，比 stat／fork
                    // 都便宜。不在這裡重複檢查 `auto_restart`——武裝點
                    // （`lib.rs::run()`）是唯一判斷點，跟 `can_interrupt()`
                    // 的 doc comment 講的紀律一致：不准各自長出一份守衛。
                    if self.can_interrupt() {
                        if let Some(exe) = crate::update::replacement_ready() {
                            return Ok(Ret::Quit(Some(exe.to_path_buf())));
                        }
                    }
                }
                AppEvent::ShowNoticeOverlay { message } => {
                    self.notice_message = Some(message);
                }
                AppEvent::OpenUpdatePrompt { tag } => {
                    self.maybe_open_update_prompt(tag);
                }
                AppEvent::UpdateRequested { tag } => {
                    // `spawn_update_download` 一啟動就蓋全螢幕 pending overlay、
                    // 凍結鍵盤（`handle_key` 對 `pending_message.is_some()` 的
                    // 處理）。手動確認這條路徑一定過得了這個守衛——
                    // `handle_yes_no_prompt_key` 送這個事件前已經
                    // `mem::take` 清空 `status_line_state`，`can_interrupt()`
                    // 這時必為真；真正靠這個守衛擋下的是 `mode = Auto` 背景
                    // 觸發的那一路，避免使用者在輸入框打字打到一半被憑空
                    // 凍結。沒過就靜默跳過，等下一個 interval——跟
                    // `maybe_open_update_prompt` 守衛沒過就不開提示同一套
                    // 哲學。
                    if self.can_interrupt() {
                        spawn_update_download(self.ec, tag);
                    }
                }
                AppEvent::UpdateInstalled { tag, exe } => {
                    // auto_restart 開著時不問，但仍要走 can_interrupt()
                    // ——它跟重啟提示共用同一個「現在能不能打斷使用者」判斷，
                    // 背景 fetch 在跑或輸入框在打字時不准抽地毯。
                    if self.ctx.update.auto_restart && self.can_interrupt() {
                        return Ok(Ret::Quit(Some(exe)));
                    }
                    self.maybe_open_restart_prompt(tag, exe);
                }
                AppEvent::RestartRequested { exe } => {
                    return Ok(Ret::Quit(Some(exe)));
                }
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        // 一次性重啟通知——任何鍵都關掉它，不像下面 pending overlay 只認
        // Cancel：這裡沒有背景操作要保留，純粹是「讓使用者知道剛才發生
        // 了什麼」，第一個按鍵就該讓路。
        if self.notice_message.is_some() {
            self.notice_message = None;
            return;
        }

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
            let overlay =
                PendingOverlay::working(message, &self.ctx.color_theme, &self.ctx.keybind);
            f.render_widget(overlay, f.area());
        }
        if let Some(message) = &self.notice_message {
            let overlay = PendingOverlay::notice(message, &self.ctx.color_theme);
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
            self.repository,
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
        self.status_line_state.is_input_mode_variant()
            || matches!(self.view, View::CreateTag(_) | View::Shell(_))
    }

    fn current_list_refresh_context(&self) -> crate::view::ListRefreshViewContext {
        use crate::view::ListRefreshViewContext;
        match &self.view {
            View::List(v) => ListRefreshViewContext::from(v.as_list_state()),
            _ => ListRefreshViewContext::default(),
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
                    self.repository,
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

    fn open_shell(&mut self) {
        let commit_list_state = match self.view {
            View::List(ref mut view) => view.as_list_state(),
            View::Detail(ref mut view) => view.as_list_state(),
            _ => return,
        };
        // Working changes（virtual row）沒有真正的 commit，`selected_commit_hash`
        // 會 fallback 回第一個 commit——`commit` 傳 `None`，`{{target_hash}}`
        // 系列 marker 交給 `ShellView::run()` 判斷指令有沒有真的用到，用到
        // 才報錯；`git status` 這類不含 marker 的指令照樣能在這一列跑。
        let (commit, refs) = if commit_list_state.is_virtual_row_selected() {
            (None, Vec::new())
        } else {
            let (commit, _, refs) = selected_commit_details(self.repository, commit_list_state);
            (Some(commit), refs)
        };
        let repo_path = self.repository.path().to_path_buf();
        // 在 `mem::take` 之前對還沒被包住的 List/Detail 設旗標——list/detail
        // 底下多擠出一行輸入列，graph 是用終端圖形協定畫的，區域縮小不清
        // 一次會留殘影。旗標設在這裡、`terminal.clear()` 由主迴圈的
        // `take_graph_clear()` 統一執行（見 `View::Shell` 對它的委派），
        // 不在這個 event arm 裡另外呼叫，才不會重複清兩次。
        self.view.request_graph_clear();
        let before_view = std::mem::take(&mut self.view);
        self.view = View::of_shell(
            before_view,
            commit,
            refs,
            repo_path,
            self.ctx.clone(),
            self.ec.sender(),
        );
    }

    fn close_shell(&mut self) {
        if let View::Shell(ref mut view) = self.view {
            let refresh_pending = view.take_refresh_pending();
            self.view = view.take_before_view();
            if refresh_pending {
                self.view.refresh();
            }
            self.view.request_graph_clear();
        }
    }

    /// watcher 觸發時（`AutoRefresh` / `AutoFetchCompleted`）呼叫，讓命令列
    /// 開著時背景 graph 也能立刻連動更新，不必等關掉命令列
    /// （`close_shell` 的 `refresh_pending` 路徑）。只有「目前是 Shell view
    /// 且指令沒在跑」才會回 `Some`；其他情況一律回 `None`，呼叫端照舊改呼叫
    /// `self.view.refresh()`——非 Shell view 走原本的 `AppEvent::Refresh`
    /// event queue，Shell 執行中則設 `refresh_pending`，等
    /// `AppEvent::ShellOutputReady` 才補送。
    fn shell_refresh_request(&mut self) -> Option<RefreshRequest> {
        let View::Shell(ref mut view) = self.view else {
            return None;
        };
        let context = view.take_refresh_context()?;
        Some(RefreshRequest { context })
    }

    /// 這裡才是「看過」這一版 release notes 的認定時機——不是
    /// `update::pending_release_notes()` 決定要跳的那一刻。view 真的建出
    /// 來、下一幀就會畫出來，才算數；`lib.rs::run()` 決定完之後還有
    /// `git::Repository::load(...)?` 這類會早退的路徑，早退的話畫面根本
    /// 沒出現過，這一版的 notes 不該被記成已看過。
    fn open_release_notes(&mut self, body: &'static str) {
        crate::update::mark_version_seen();
        let before_view = std::mem::take(&mut self.view);
        self.view = View::of_release_notes(before_view, body, self.ctx.clone(), self.ec.sender());
    }

    fn close_release_notes(&mut self) {
        if let View::ReleaseNotes(ref mut view) = self.view {
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
            // 兩個查詢彼此獨立，改用 thread::scope 併發——序列跑今天要
            // 2 個 gh 呼叫的 wall clock 疊加，併發只要較慢那個的時間。
            // join() 回傳 Result<_, Box<dyn Any>>：thread panic 要轉成
            // 錯誤訊息，不能讓它靜靜消失。今天 list_issues 若 panic，
            // 整條 refresh thread 直接死掉、沒有任何事件送回主執行緒，
            // GitHubLoad 永遠停在 Loading，之後每次 on_refresh_requested
            // 都回 false——GitHub refresh 對這個 process 永久失效，不只
            // 是這一次卡住，症狀跟這個 issue 要修的一模一樣。
            let (issues_result, prs_result) = std::thread::scope(|s| {
                let issues = s.spawn(|| crate::github::list_issues(&repo_path, state, None));
                let prs = s.spawn(|| crate::github::list_pull_requests(&repo_path, state, None));
                (
                    issues
                        .join()
                        .unwrap_or_else(|_| Err("GitHub issues thread panicked".into())),
                    prs.join()
                        .unwrap_or_else(|_| Err("GitHub PRs thread panicked".into())),
                )
            });

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

    fn select_child_commit(&mut self) {
        if let View::Detail(ref mut view) = self.view {
            view.select_child_commit(self.repository);
        } else if let View::UserCommand(ref mut view) = self.view {
            view.select_child_commit(
                self.repository,
                self.view_area,
                build_external_command_parameters_and_exec_command,
            );
        }
    }

    /// child picker 選定候選後的跳轉——跟 `select_child_commit` 不同，List
    /// 也要接（List 的 `GoToChild` 不經過 `AppEvent::SelectChildCommit`，
    /// 但分支點的 picker 選擇一律走這裡回來，三個 view 都要能接）。
    fn select_commit_by_hash(&mut self, hash: &CommitHash) {
        match self.view {
            View::List(ref mut view) => view.select_commit_by_hash(hash),
            View::Detail(ref mut view) => view.select_commit_by_hash(self.repository, hash),
            View::UserCommand(ref mut view) => view.select_commit_by_hash(
                self.repository,
                self.view_area,
                build_external_command_parameters_and_exec_command,
                hash,
            ),
            _ => {}
        }
    }

    fn init_with_context(&mut self, context: RefreshViewContext) {
        if let View::List(ref mut view) = self.view {
            view.reset_commit_list_with(&context.list);
        }
        match context.view {
            ViewContext::List => {}
            ViewContext::Detail => {
                self.open_detail();
            }
            ViewContext::UserCommand { n } => {
                self.open_user_command(n, None);
            }
            ViewContext::Refs {
                refs_context,
                origin,
            } => {
                // origin 只是 close Refs 時「要回哪」的記號，不需要先把 view 切成 Detail
                // 再立刻被 open_refs_with_origin take 走 list_state——那會白跑一次 git diff。
                self.open_refs_with_origin(origin);
                if let View::Refs(ref mut view) = self.view {
                    view.reset_refs_with(refs_context);
                }
            }
        }
        // Shell 是疊加在 List/Detail 之上的第三個維度，等底層 view 還原完才
        // 開——`open_shell()` 需要一個已經還原好選取狀態的 List/Detail 才能
        // 正確取出 `{{target_hash}}` 系列 marker 用的 commit/refs。
        if let Some(shell_context) = context.shell {
            self.open_shell();
            if let View::Shell(ref mut view) = self.view {
                view.restore_state(*shell_context);
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
            // 沒有本機瀏覽器可 spawn（SSH／mosh）。OSC 8 在這條路徑上沒有能
            // 動的版本——mosh 的終端模擬器直接吃掉整個 OSC 8 序列（沒有
            // hyperlink 欄位可存），tmux 的 DCS passthrough 則會讓 label
            // 印到錯的座標、後續畫面留下殘骸（見 git log 這次變更的說明）。
            // 改印純文字 URL：ghostty／iTerm2／WezTerm／Kitty 本來就有 URL
            // 偵測，⌘+Click 一樣能開，而且穿得過 mosh 和 tmux。
            //
            // 同步賦值，不透過 `ec` 送事件——理由同
            // `StatusLineState::open_related_picker` 文件註解：這一輪迴圈
            // 才不會先閃一下 hint 列才出現通知。
            Ok(crate::external::OpenUrlOutcome::NotSpawned) => {
                self.status_line_state
                    .set_notification_info(format!("SSH: {url}"));
            }
            Err(msg) => {
                self.ec.send(AppEvent::NotifyError(msg));
            }
        }
    }

    /// 背景檢查回來時，狀態列可能正被 picker／prompt／通知佔著，或整個蓋在
    /// pending overlay 底下（兩者都在 `handle_key` 攔截鍵，提示會被塞進看不到
    /// 也按不了的地方），或使用者根本不在 List/Detail/Refs 這三個 browsing
    /// view（GitHub 搜尋框、CreateTag 等對話框用的是自己的 `tui_input::Input`，
    /// 狀態列同樣顯示 `None`，提示一彈出來就會吃掉打字）。三個條件都過才開，
    /// 沒過就丟掉不問——節流間隔可設定（`core.update.interval_hours`），
    /// 下次再說，不做佇列。
    fn maybe_open_update_prompt(&mut self, tag: String) {
        if self.pending_message.is_none()
            && self.view.is_browsing_view()
            && self.status_line_state.is_idle()
        {
            self.status_line_state.open_update_prompt(tag);
        }
    }

    /// 現在能不能打斷使用者：沒有 pending overlay、在三個 browsing view 之一、
    /// 狀態列閒置或只是顯示一則通知（picker／prompt 都不算，理由見
    /// `StatusLineState::is_showing_notification`）。這是唯一一處判斷「可不可以
    /// 打斷『使用者當下正在看的 view』」，`auto_restart` 的無提示重啟、這裡的
    /// 重啟提示、`AppEvent::ExeReplacedCheck` 的自動重啟共用它，三者不准各自
    /// 長出一份守衛。
    ///
    /// 狀態列那半（閒置或僅顯示通知）另外抽成
    /// `StatusLineState::is_idle_or_notification`，`AppEvent::AutoFetchCompleted`
    /// 的守衛也共用同一份——它刻意不要求 `is_browsing_view()`（見該處註解），
    /// 所以不能直接呼叫這個函式。
    fn can_interrupt(&self) -> bool {
        self.pending_message.is_none()
            && self.view.is_browsing_view()
            && self.status_line_state.is_idle_or_notification()
    }

    /// `auto_fetch::rearm` 收兩個把手是為了背景 thread（`spawn_due_fetch`
    /// 尾端）；主執行緒這兩個呼叫點（忙碌跳過、沒有差異）沒有那個限制，
    /// 收成一行省掉重複的 `self.ec.sender()`／`self.ec.auto_fetch_clock()`。
    fn rearm_auto_fetch(&self) {
        auto_fetch::rearm(
            self.ec.sender(),
            self.ec.auto_fetch_clock(),
            self.ctx.auto_fetch.interval,
        );
    }

    /// 下載＋替換執行檔完成，問是否要離開並以新版重啟。守衛沒過就退回通知。
    fn maybe_open_restart_prompt(&mut self, tag: String, exe: PathBuf) {
        if self.can_interrupt() {
            self.status_line_state.open_restart_prompt(tag, exe);
        } else {
            self.status_line_state
                .set_notification_success(status_line::UPDATE_INSTALLED_HINT.to_string());
        }
    }

    fn fetch_all(&self) {
        spawn_git_task(
            self.ec,
            GitTask {
                repo: self.repository.path(),
                args: &["fetch", "--all"],
                pending_msg: "Fetching...".into(),
                success_msg: "Fetch completed".into(),
                error_prefix: "Fetch failed",
                timeout: GIT_FETCH_TIMEOUT,
                // 手動 fetch 成功等同「一輪 auto-fetch 已經跑過」，讓倒數與
                // 基準指紋重新計算，見 `auto_fetch::spawn_resync`。
                on_success: Some(AppEvent::AutoFetchResync),
            },
        );
    }

    fn checkout_commit(&self, target: String) {
        spawn_git_task(
            self.ec,
            GitTask {
                repo: self.repository.path(),
                args: &["checkout", &target],
                pending_msg: format!("Checking out '{target}'..."),
                success_msg: format!("Checked out '{target}'"),
                error_prefix: "Checkout failed",
                timeout: GIT_CHECKOUT_TIMEOUT,
                on_success: None,
            },
        );
    }
}

fn spawn_merge_pr(
    repo: &Path,
    ec: &EventController,
    number: u64,
    state: StateFilter,
    method: MergeMethod,
    delete_remote_branch: Option<String>,
) {
    let repo_path = repo.to_path_buf();
    let tx = ec.sender();
    ec.send(AppEvent::ShowPendingOverlay {
        message: format!("Merging PR #{number}..."),
    });
    std::thread::spawn(move || {
        let result = merge_pr(&repo_path, number, method.as_flag());
        match result {
            Ok(()) => {
                // 列表不必等刪除分支——先送，讓 merge 完成立刻反映在畫面上。
                tx.send(AppEvent::RefreshGitHub { state });

                let merged = format!("PR #{number} merged ({})", method.display());
                let notify = match delete_remote_branch {
                    None => AppEvent::NotifySuccess(merged),
                    Some(head_ref) => {
                        tx.send(AppEvent::ShowPendingOverlay {
                            message: "Deleting remote branch...".to_string(),
                        });
                        match gh_delete_remote_branch(&repo_path, &head_ref) {
                            Ok(()) => AppEvent::NotifySuccess(format!(
                                "{merged}, remote branch '{head_ref}' deleted"
                            )),
                            Err(e) => AppEvent::NotifyWarn(format!(
                                "{merged}, but failed to delete remote branch '{head_ref}': {e}"
                            )),
                        }
                    }
                };
                tx.send(AppEvent::HidePendingOverlay);
                tx.send(notify);
            }
            Err(e) => {
                tx.send(AppEvent::HidePendingOverlay);
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

fn spawn_update_download(ec: &EventController, tag: String) {
    let tx = ec.sender();
    ec.send(AppEvent::ShowPendingOverlay {
        message: format!("Downloading {tag}..."),
    });
    std::thread::spawn(move || {
        let result = crate::update::download_and_replace(&tag);
        tx.send(AppEvent::HidePendingOverlay);
        match result {
            Ok(exe) => {
                tx.send(AppEvent::UpdateInstalled { tag, exe });
            }
            Err(e) => {
                tx.send(AppEvent::NotifyError(e));
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
                tx.send(AppEvent::Refresh(RefreshViewContext::list(list_context)));
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

/// `fetch --all` 的逾時預算。大 repo 的 fetch 合法超過幾十秒很常見，砍掉
/// 一個今天會成功的操作就是 break userspace——這裡只防真的被網路黑洞吃掉
/// 的情況。
const GIT_FETCH_TIMEOUT: Duration = Duration::from_secs(60);

/// `checkout` 的逾時預算，刻意抓得比 fetch 大得多：中途被 kill 掉的
/// `checkout` 會留下 `.git/index.lock`，之後這個 repo 裡每一個 git 指令都
/// 會失敗，直到使用者手動去刪——這比洩漏一條背景 thread 嚴重得多，所以要
/// 大到任何合法操作都碰不到，這裡只防真的卡死（例如掛掉的 smudge/clean
/// filter）。
const GIT_CHECKOUT_TIMEOUT: Duration = Duration::from_secs(1800);

/// `spawn_git_task` 的參數包——原本 7 個位置參數裡 `pending_msg`／
/// `success_msg`／`error_prefix` 三個相鄰同型別，寫反編譯器抓不到（跟
/// `AutoFetchOverrides` doc comment 點名的同一類風險），加第 8 個參數
/// `on_success` 正好是收進具名欄位的時機。
struct GitTask<'a> {
    repo: &'a Path,
    args: &'a [&'a str],
    pending_msg: String,
    success_msg: String,
    error_prefix: &'a str,
    timeout: Duration,
    /// 指令成功時（除了固定的 `NotifySuccess` + `AutoRefresh`）額外要送的
    /// 一個事件；`None` 給不需要的呼叫端（例如 `checkout_commit`）用。目前
    /// 唯一用途是 `fetch_all()` 送 `AppEvent::AutoFetchResync`，讓手動
    /// fetch 成功後 auto-fetch 的倒數與基準跟著重整。
    on_success: Option<AppEvent>,
}

fn spawn_git_task(ec: &EventController, task: GitTask) {
    let GitTask {
        repo,
        args,
        pending_msg,
        success_msg,
        error_prefix,
        timeout,
        on_success,
    } = task;
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
        let cmd = background_command(&repo_path, &args);
        let output = run_with_timeout(cmd, None, timeout);

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
                if let Some(event) = on_success {
                    tx.send(event);
                }
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
        UserEvent::CheckUpdate => Some(AppEvent::CheckUpdate),
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

/// 兩條路徑共用的核心：把 `Commit` + `Ref` 分類成 `ExternalCommandParameters`
/// 的各個 marker 欄位。`command` 與面積數字由呼叫端決定——user command 走
/// 設定檔查表 + `pane_height.user_command`，shell 沒有查表、面積用自己的
/// 輸出面板高度，兩者不該共用同一個公式（沿用對方的會給錯的數字）。
pub(crate) fn external_command_parameters<'a>(
    command: &'a [String],
    commit: &'a Commit,
    refs: &'a [Ref],
    area_width: u16,
    area_height: u16,
) -> ExternalCommandParameters<'a> {
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

    ExternalCommandParameters {
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
    }
}

fn build_external_command_parameters<'a>(
    commit: &'a Commit,
    refs: &'a [Ref],
    user_command_number: usize,
    view_area: Rect,
    ctx: &'a AppContext,
) -> Result<ExternalCommandParameters<'a>, String> {
    let command = &extract_user_command_by_number(user_command_number, ctx)?.commands;
    let area_width = view_area.width.saturating_sub(4); // 扣掉左右 padding
    let area_height = (view_area.height.saturating_sub(1))
        .min(ctx.ui_config.pane_height.user_command)
        .saturating_sub(1); // 扣掉上邊框
    Ok(external_command_parameters(
        command,
        commit,
        refs,
        area_width,
        area_height,
    ))
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
    #[case(UserEvent::CheckUpdate,  true,  false, true)]
    // 輸入模式：一律放行給 view 當文字處理
    #[case(UserEvent::GitHubToggle, true,  true,  false)]
    #[case(UserEvent::HelpToggle,   true,  true,  false)]
    #[case(UserEvent::CheckUpdate,  true,  true,  false)]
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

    #[test]
    fn resolve_shell_command_prefers_explicit_config() {
        let cfg = CoreShellConfig {
            command: Some(vec!["fish".to_string(), "-c".to_string()]),
        };
        assert_eq!(
            resolve_shell_command(&cfg, Some("zsh")),
            vec!["fish".to_string(), "-c".to_string()]
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn resolve_shell_command_posix_shell_env_gets_interactive_flag() {
        let cfg = CoreShellConfig { command: None };
        assert_eq!(
            resolve_shell_command(&cfg, Some("/bin/zsh")),
            vec!["/bin/zsh".to_string(), "-i".to_string(), "-c".to_string()]
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn resolve_shell_command_non_posix_shell_env_skips_interactive_flag() {
        let cfg = CoreShellConfig { command: None };
        assert_eq!(
            resolve_shell_command(&cfg, Some("fish")),
            vec!["fish".to_string(), "-c".to_string()]
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn resolve_shell_command_missing_shell_env_falls_back_to_sh() {
        let cfg = CoreShellConfig { command: None };
        assert_eq!(
            resolve_shell_command(&cfg, None),
            vec!["sh".to_string(), "-i".to_string(), "-c".to_string()]
        );
    }
}

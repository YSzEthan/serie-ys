use std::{path::PathBuf, rc::Rc};

use ratatui::{crossterm::event::KeyEvent, layout::Rect, text::Line, Frame};
use smart_default::SmartDefault;
use tui_input::Input;

use crate::{
    app::AppContext,
    event::{EventController, Sender, UserEventWithCount},
    git::{Commit, CommitExtra, CommitHash, FileChange, Ref, RefType, Repository, WorkingChanges},
    view::{
        create_tag::CreateTagView, delete_ref::DeleteRefView, delete_tag::DeleteTagView,
        detail::DetailView, github::GitHubView, help::HelpView, list::ListView, refs::RefsView,
        release_notes::ReleaseNotesView, shell::ShellView, user_command::UserCommandView,
        LabelMode, RefsOrigin,
    },
    widget::{
        commit_list::{CommitListState, MatchOptions, MatchQuery},
        ref_list::RefListState,
    },
};

#[derive(Debug, Default)]
pub enum View<'a> {
    #[default]
    Default, // 為了讓 #[default] 能運作的假 variant
    List(Box<ListView<'a>>),
    Detail(Box<DetailView<'a>>),
    UserCommand(Box<UserCommandView<'a>>),
    Refs(Box<RefsView<'a>>),
    CreateTag(Box<CreateTagView<'a>>),
    DeleteTag(Box<DeleteTagView<'a>>),
    DeleteRef(Box<DeleteRefView<'a>>),
    Help(Box<HelpView<'a>>),
    GitHub(Box<GitHubView<'a>>),
    ReleaseNotes(Box<ReleaseNotesView<'a>>),
    Shell(Box<ShellView<'a>>),
}

impl<'a> View<'a> {
    pub fn handle_event(&mut self, event_with_count: UserEventWithCount, key_event: KeyEvent) {
        match self {
            View::Default => {}
            View::List(view) => view.handle_event(event_with_count, key_event),
            View::Detail(view) => view.handle_event(event_with_count, key_event),
            View::UserCommand(view) => view.handle_event(event_with_count, key_event),
            View::Refs(view) => view.handle_event(event_with_count, key_event),
            View::CreateTag(view) => view.handle_event(event_with_count, key_event),
            View::DeleteTag(view) => view.handle_event(event_with_count, key_event),
            View::DeleteRef(view) => view.handle_event(event_with_count, key_event),
            View::Help(view) => view.handle_event(event_with_count, key_event),
            View::GitHub(view) => view.handle_event(event_with_count, key_event),
            View::ReleaseNotes(view) => view.handle_event(event_with_count, key_event),
            View::Shell(view) => view.handle_event(event_with_count, key_event),
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect, marquee_frame: u64) {
        match self {
            View::Default => {}
            View::List(view) => view.render(f, area, marquee_frame),
            View::Detail(view) => view.render(f, area, marquee_frame),
            View::UserCommand(view) => view.render(f, area),
            View::Refs(view) => view.render(f, area),
            View::CreateTag(view) => view.render(f, area),
            View::DeleteTag(view) => view.render(f, area),
            View::DeleteRef(view) => view.render(f, area),
            View::Help(view) => view.render(f, area),
            View::GitHub(view) => view.render(f, area, marquee_frame),
            View::ReleaseNotes(view) => view.render(f, area),
            View::Shell(view) => view.render(f, area, marquee_frame),
        }
    }

    /// 用於重置 marquee 的字串化識別值。當使用者切換到不同的選取項目、
    /// marquee 應該從第 0 個 frame 重新開始時，這個值就會改變。
    pub fn marquee_id(&self) -> Option<std::sync::Arc<str>> {
        match self {
            View::List(view) => Some(view.as_list_state().selected_commit_hash().as_arc()),
            View::Detail(view) => view.marquee_id(),
            View::GitHub(view) => view.marquee_id(),
            // `ShellView::render` 畫的是 `before`——不委派的話跑馬燈在命令列
            // 開著的時候會直接凍結（`marquee_id` 恆為 `None`，`Tick` 那條路
            // 永遠不會把 `marquee_frame` 往前推）。
            View::Shell(view) => view.marquee_id(),
            _ => None,
        }
    }

    /// 上一次渲染是否把選取列標記為溢出 — 也就是說
    /// marquee 跑馬燈是否要繼續執行。
    pub fn marquee_needed(&self) -> bool {
        match self {
            View::List(view) => view.as_list_state().selected_row_overflows.get(),
            View::Detail(view) => view.marquee_needed(),
            View::GitHub(view) => view.marquee_needed(),
            View::Shell(view) => view.marquee_needed(),
            _ => false,
        }
    }

    pub fn take_graph_clear(&mut self) -> bool {
        match self {
            View::List(view) => view.take_graph_clear(),
            View::Detail(view) => view.take_graph_clear(),
            // `open_shell()` 在包住 `before` 之前就對它呼叫過
            // `request_graph_clear()`，旗標設在被包住的 List/Detail 身上——
            // 不委派的話這個旗標永遠取不出來，`terminal.clear()` 也就永遠
            // 不會執行。
            View::Shell(view) => view.take_graph_clear(),
            _ => false,
        }
    }

    pub fn request_graph_clear(&mut self) {
        match self {
            View::List(view) => view.request_graph_clear(),
            View::Detail(view) => view.request_graph_clear(),
            View::Shell(view) => view.request_graph_clear(),
            _ => {}
        }
    }

    pub fn is_browsing_view(&self) -> bool {
        match self {
            View::List(_) | View::Detail(_) | View::Refs(_) => true,
            View::Default
            | View::UserCommand(_)
            | View::CreateTag(_)
            | View::DeleteTag(_)
            | View::DeleteRef(_)
            | View::Help(_)
            | View::GitHub(_)
            | View::ReleaseNotes(_)
            | View::Shell(_) => false,
        }
    }

    pub fn of_list(
        commit_list_state: CommitListState<'a>,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> Self {
        View::List(Box::new(ListView::new(commit_list_state, ctx, tx)))
    }

    pub fn of_detail(
        commit_list_state: CommitListState<'a>,
        commit: Commit,
        extra: CommitExtra,
        changes: Vec<FileChange>,
        refs: Vec<Ref>,
        repository: &'a Repository,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> Self {
        View::Detail(Box::new(DetailView::new(
            commit_list_state,
            commit,
            extra,
            changes,
            refs,
            repository,
            ctx,
            tx,
        )))
    }

    pub fn of_working_changes_detail(
        commit_list_state: CommitListState<'a>,
        working_changes: WorkingChanges,
        repository: &'a Repository,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> Self {
        View::Detail(Box::new(DetailView::new_working_changes(
            commit_list_state,
            working_changes,
            repository,
            ctx,
            tx,
        )))
    }

    pub fn of_user_command(
        commit_list_state: CommitListState<'a>,
        command_output: String,
        user_command_number: usize,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> Self {
        View::UserCommand(Box::new(UserCommandView::new(
            commit_list_state,
            command_output,
            user_command_number,
            ctx,
            tx,
        )))
    }

    pub fn of_refs(
        commit_list_state: CommitListState<'a>,
        refs: &[&Ref],
        origin: RefsOrigin,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> Self {
        View::Refs(Box::new(RefsView::new(
            commit_list_state,
            refs,
            origin,
            ctx,
            tx,
        )))
    }

    pub fn of_refs_with_state(
        commit_list_state: CommitListState<'a>,
        ref_list_state: RefListState,
        origin: RefsOrigin,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> Self {
        View::Refs(Box::new(RefsView::with_state(
            commit_list_state,
            ref_list_state,
            origin,
            ctx,
            tx,
        )))
    }

    pub fn of_create_tag(
        commit_list_state: CommitListState<'a>,
        commit_hash: CommitHash,
        repo_path: PathBuf,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> Self {
        View::CreateTag(Box::new(CreateTagView::new(
            commit_list_state,
            commit_hash,
            repo_path,
            ctx,
            tx,
        )))
    }

    pub fn of_delete_tag(
        commit_list_state: CommitListState<'a>,
        commit_hash: CommitHash,
        tags: Vec<Ref>,
        repo_path: PathBuf,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> Self {
        View::DeleteTag(Box::new(DeleteTagView::new(
            commit_list_state,
            commit_hash,
            tags,
            repo_path,
            ctx,
            tx,
        )))
    }

    pub fn of_delete_ref(
        commit_list_state: CommitListState<'a>,
        ref_list_state: RefListState,
        repo_path: PathBuf,
        ref_name: String,
        ref_type: RefType,
        refs_origin: RefsOrigin,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> Self {
        View::DeleteRef(Box::new(DeleteRefView::new(
            commit_list_state,
            ref_list_state,
            repo_path,
            ref_name,
            ref_type,
            refs_origin,
            ctx,
            tx,
        )))
    }

    pub fn of_help(before: View<'a>, ctx: Rc<AppContext>, tx: Sender) -> Self {
        View::Help(Box::new(HelpView::new(before, ctx, tx)))
    }

    pub fn of_github(
        before: View<'a>,
        data: crate::github::GitHubData,
        label_mode: LabelMode,
        tx: Sender,
    ) -> Self {
        View::GitHub(Box::new(
            GitHubView::new(before, data, tx).with_label_mode(label_mode),
        ))
    }

    pub fn of_release_notes(
        before: View<'a>,
        body: &'static str,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> Self {
        View::ReleaseNotes(Box::new(ReleaseNotesView::new(before, body, ctx, tx)))
    }

    pub fn of_shell(
        before: View<'a>,
        commit: Option<Commit>,
        refs: Vec<Ref>,
        repo_path: PathBuf,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> Self {
        View::Shell(Box::new(ShellView::new(
            before, commit, refs, repo_path, ctx, tx,
        )))
    }

    /// `AppEvent::ShellOutputReady` 抵達時呼叫——見
    /// `ShellView::poll_output` 的文件註解。非 Shell view 什麼都不做，
    /// 這是正確行為（過期喚醒打到已經換掉的 view）。
    pub fn poll_shell_output(&mut self) {
        if let View::Shell(view) = self {
            view.poll_output();
        }
    }

    /// 現在能不能換資料——`take_refresh_context()` 的 `&self` 版本，兩者
    /// 涵蓋的 view 必須同步（規則只有一份，改一邊就要回來改另一邊）：
    ///
    /// - List／Detail／UserCommand／Refs：能換
    /// - Shell：指令沒在跑才能換，見 `ShellView::can_swap`
    /// - CreateTag／DeleteTag／DeleteRef／Help／GitHub／ReleaseNotes：
    ///   不能換，等使用者關掉——這些 dialog／overlay 自己的狀態沒有對應的
    ///   `RefreshViewContext` 欄位可以裝，換資料只能等它們關閉之後才做。
    pub fn can_swap(&self) -> bool {
        match self {
            View::List(_) | View::Detail(_) | View::UserCommand(_) | View::Refs(_) => true,
            View::Shell(view) => view.can_swap(),
            View::Default
            | View::CreateTag(_)
            | View::DeleteTag(_)
            | View::DeleteRef(_)
            | View::Help(_)
            | View::GitHub(_)
            | View::ReleaseNotes(_) => false,
        }
    }

    /// 換資料時，把目前 view 的狀態轉成 `RefreshViewContext`，供
    /// `App::apply_carry` 重建後還原。取代舊版只涵蓋 List/Detail 的
    /// `refresh_context()`——舊機制下其餘 view 各自呼叫 `refresh()` 立刻
    /// 送出自己組好的 `AppEvent::Refresh(ctx)`；現在整個規則收斂到這一個
    /// 函式，涵蓋範圍與 `can_swap()` 同步，見該處文件。
    pub fn take_refresh_context(&mut self) -> Option<RefreshViewContext> {
        match self {
            View::List(view) => Some(RefreshViewContext::new(
                ListRefreshViewContext::from(view.as_list_state()),
                ViewContext::List,
            )),
            View::Detail(view) => Some(RefreshViewContext::new(
                ListRefreshViewContext::from(view.as_list_state()),
                ViewContext::Detail,
            )),
            View::UserCommand(view) => Some(view.refresh_context()),
            View::Refs(view) => Some(view.refresh_context()),
            // `open_shell` 只接受 List/Detail 當 `before`，`take_refresh_context`
            // 內部再委派一次同一個函式，定義域因此涵蓋得到。
            View::Shell(view) => view.take_refresh_context(),
            View::Default
            | View::CreateTag(_)
            | View::DeleteTag(_)
            | View::DeleteRef(_)
            | View::Help(_)
            | View::GitHub(_)
            | View::ReleaseNotes(_) => None,
        }
    }

    pub fn into_commit_list_state(self) -> CommitListState<'a> {
        let mut view = self;
        loop {
            view = match view {
                View::List(mut v) => return v.take_list_state().expect("missing state"),
                View::Detail(mut v) => return v.take_list_state().expect("missing state"),
                View::Refs(mut v) => return v.take_list_state().expect("missing state"),
                View::CreateTag(mut v) => return v.take_list_state().expect("missing state"),
                View::DeleteTag(mut v) => return v.take_list_state().expect("missing state"),
                View::DeleteRef(mut v) => return v.take_list_state().expect("missing state"),
                View::UserCommand(mut v) => return v.take_list_state().expect("missing state"),
                // Help/GitHub/Shell 是覆蓋層視圖；要展開回內部包著的 before_view。
                // take_before_view 會留下 View::Default，但無妨，因為 `v` 接著就會被 drop。
                View::Help(mut v) => v.take_before_view(),
                View::GitHub(mut v) => v.take_before_view(),
                View::ReleaseNotes(mut v) => v.take_before_view(),
                View::Shell(mut v) => v.take_before_view(),
                View::Default => unreachable!("no View::Default at runtime"),
            };
        }
    }

    /// 目前使用中的 `CommitListState`，不管有沒有被 Help／GitHub／
    /// ReleaseNotes／Shell 蓋住——跟 `into_commit_list_state` 一樣往
    /// overlay 的 `before` 走下去，但這裡是借用，不消費 `self`。
    /// Help／GitHub／ReleaseNotes／Shell 四種 overlay 共用的「往內層 view
    /// 走下去」——`list_state_mut`／`apply_working_changes` 都要穿過 overlay
    /// 才碰得到真正的 `CommitListState`，抽出來避免兩處各寫一份。
    fn overlay_before_mut(&mut self) -> Option<&mut View<'a>> {
        match self {
            View::Help(v) => Some(v.before_view_mut()),
            View::GitHub(v) => Some(v.before_view_mut()),
            View::ReleaseNotes(v) => Some(v.before_view_mut()),
            View::Shell(v) => Some(v.before_view_mut()),
            _ => None,
        }
    }

    /// 不能像 `apply_working_changes` 那樣先 `overlay_before_mut()` 早退再
    /// `match self`——這裡的回傳型別借用 `self`，borrow checker 會把
    /// `overlay_before_mut()` 那次借用的存續期直接綁到整個函式的
    /// `&mut self`，擋掉後面 `match self` 對 `self` 的使用。`()` 沒有這個
    /// 問題，`apply_working_changes` 才用得了那個寫法。
    pub fn list_state_mut(&mut self) -> Option<&mut CommitListState<'a>> {
        match self {
            View::List(v) => Some(v.as_mut_list_state()),
            View::Detail(v) => Some(v.as_mut_list_state()),
            View::UserCommand(v) => Some(v.as_mut_list_state()),
            View::Refs(v) => Some(v.as_mut_list_state()),
            View::CreateTag(v) => Some(v.as_mut_list_state()),
            View::DeleteTag(v) => Some(v.as_mut_list_state()),
            View::DeleteRef(v) => Some(v.as_mut_list_state()),
            View::Help(v) => v.before_view_mut().list_state_mut(),
            View::GitHub(v) => v.before_view_mut().list_state_mut(),
            View::ReleaseNotes(v) => v.before_view_mut().list_state_mut(),
            View::Shell(v) => v.before_view_mut().list_state_mut(),
            View::Default => None,
        }
    }

    /// `reload::Reloader` 背景重新整理送達時呼叫（`AppEvent::WorkingChangesReady`）。
    /// 更新目前使用中的 `CommitListState`（穿過 overlay，理由同
    /// `list_state_mut`）；`View::Detail` 額外處理兩種情況：
    ///
    /// - 正顯示 working changes、虛擬列卻消失了（例如這批變更被 commit
    ///   掉）——`CommitListState::set_working_changes` 已經把游標挪到第一個
    ///   真正的 commit，這裡換回 List，跟 `App::close_detail` 同一套動作；
    ///   即使這個 Detail 被 overlay 蓋住，換掉的也是 overlay 的
    ///   `before`，不是外層，overlay 關掉之後看到的就是新的 List。
    /// - 其餘情況交給 `DetailView::refresh_working_changes`——它自己判斷
    ///   目前顯示的是不是 working changes，是的話才重建內容，否則 no-op。
    pub fn apply_working_changes(
        &mut self,
        working_changes: Option<WorkingChanges>,
        ctx: &Rc<AppContext>,
        ec: &EventController,
    ) {
        if let Some(before) = self.overlay_before_mut() {
            return before.apply_working_changes(working_changes, ctx, ec);
        }
        if let View::Detail(view) = self {
            let state = view.as_mut_list_state();
            let was_virtual_selected = state.is_virtual_row_selected();
            state.set_working_changes(working_changes);
            let has_virtual_row = state.has_virtual_row();

            if was_virtual_selected && !has_virtual_row {
                let commit_list_state = view.take_list_state().expect("list state present");
                *self = View::of_list(commit_list_state, ctx.clone(), ec.sender());
            } else {
                view.refresh_working_changes();
            }
            return;
        }
        if let Some(state) = self.list_state_mut() {
            state.set_working_changes(working_changes);
        }
    }
}

/// 四種 browsing/dialog view 的 refresh context 曾經各是 `RefreshViewContext`
/// 的一個 variant，但全部都帶 `list_context`——那其實是「底層 view 長怎樣」
/// 跟「list 的捲動/選取狀態」兩個維度硬塞進同一個 enum。拆開成 `list` +
/// `view` 兩個欄位，`shell` 是疊加在 `view` 上面的第三個、獨立的維度
/// （只會跟 `ViewContext::List` / `ViewContext::Detail` 同時出現，因為
/// `open_shell` 只接受這兩種 view）。
#[derive(Debug, Clone)]
pub struct RefreshViewContext {
    pub list: ListRefreshViewContext,
    pub view: ViewContext,
    /// `Box` 是因為只有 Shell 這一條路徑會填它——不 box 的話
    /// `AppEvent::Refresh` 這個 variant 會為了這一條路徑，讓每顆
    /// `Tick`/`Key` 走 channel 都多搬一份沒用到的 bytes。
    pub shell: Option<Box<ShellRefreshViewContext>>,
}

impl RefreshViewContext {
    /// `shell` 只有 Shell 那一條路徑會填——這裡統一補 `None`，呼叫端不用
    /// 每個都記得寫一次。
    pub fn new(list: ListRefreshViewContext, view: ViewContext) -> Self {
        RefreshViewContext {
            list,
            view,
            shell: None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum ViewContext {
    List,
    Detail,
    UserCommand {
        n: usize,
    },
    Refs {
        refs_context: RefsRefreshViewContext,
        origin: RefsOrigin,
    },
}

#[derive(Debug, Clone, SmartDefault)]
pub struct ListRefreshViewContext {
    pub commit_hash: CommitHash,
    pub selected: usize,
    pub height: usize,
    #[default = true]
    pub scroll_to_top: bool,
    #[default = true]
    pub show_remote_refs: bool,
    pub search_options: Option<MatchOptions>,
    pub search: Option<MatchQuery>,
    pub filter: Option<MatchQuery>,
}

impl From<&CommitListState<'_>> for ListRefreshViewContext {
    fn from(list_state: &CommitListState<'_>) -> Self {
        let commit_hash = list_state.selected_commit_hash().clone();
        let (selected, offset, height) = list_state.current_list_status();
        // 如果選取的 commit 是最上面那個且沒有 offset，代表清單已經捲到最上面了。
        // 這種情況下把 scroll_to_top 設為 true，表示重新整理後畫面應該捲回最上面。
        let scroll_to_top = selected == 0 && offset == 0;
        ListRefreshViewContext {
            commit_hash,
            selected,
            height,
            scroll_to_top,
            show_remote_refs: list_state.show_remote_refs(),
            search_options: Some(list_state.search_options()),
            search: list_state.search_refresh_context(),
            filter: list_state.filter_refresh_context(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RefsRefreshViewContext {
    pub selected: Vec<String>,
    pub opened: Vec<Vec<String>>,
}

/// `ShellView` 重建時要還原的純資料——全部是 `'static`，重建當下
/// `rx`/`child` 早就結束（見 `ShellView::take_refresh_context` 的
/// 文件註解），不需要一起搬。
#[derive(Debug, Clone)]
pub struct ShellRefreshViewContext {
    pub input: Input,
    pub history: Vec<String>,
    pub history_index: Option<usize>,
    pub output_lines: Vec<Line<'static>>,
    /// 可能是 `usize::MAX`——`OutputPaneState::select_last()` 的貼底
    /// sentinel，還沒經過任何 render 的 clamp。還原時的 `scroll_to`
    /// 是第一次 clamp，下一次 render 是第二次，兩次都是必要的。
    pub output_offset: usize,
}

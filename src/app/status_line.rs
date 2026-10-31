use std::{borrow::Cow, path::PathBuf, rc::Rc};

use rust_i18n::t;

use ratatui::{
    crossterm::event::{KeyCode, KeyEvent},
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Padding, Paragraph},
    Frame,
};

use crate::{
    config::CursorType,
    event::{
        AppEvent, AutoFetchClock, CheckoutPickKind, RefCopyKind, RelatedItem, Sender, UserEvent,
    },
    github::{GhItemKind, MergeMethod, PrDraftAction, RerunScope, StateAction, StateFilter},
    reload::FullStatus,
    view::View,
    widget::{
        commit_list::ChildPickOption, h, hint_line, hint_pairs, keybind_hint_line, truncate_line,
        HintSpec,
    },
};

use super::AppContext;

fn esc_cancel() -> Cow<'static, str> {
    t!("common.esc_cancel")
}

/// 輸入框要顯示的整串，以及游標前那段內容。
///
/// Virtual 游標字元接在切點上而不是事後覆寫，游標下的字元才不會被蓋掉；
/// 接成單一字串（不拆 span）是為了讓 `truncate_line` 的截斷行為跟沒有游標
/// 時一樣，不會整段丟掉游標後的內容。`cursor` 不在 char boundary 時退回
/// 尾端——每一幀都會跑，不能 panic。
fn input_display<'a>(
    msg: &'a str,
    cursor: usize,
    cursor_type: &CursorType,
) -> (&'a str, Cow<'a, str>) {
    let (before, after) = msg.split_at_checked(cursor).unwrap_or((msg, ""));
    match cursor_type {
        CursorType::Native => (before, Cow::Borrowed(msg)),
        CursorType::Virtual(c) => (before, Cow::Owned(format!("{before}{c}{after}"))),
    }
}

/// `RestartPrompt` 取消後留下的提醒；`maybe_open_restart_prompt`（app.rs）
/// 守衛沒過時也是同一句，兩處共用同一份字面值不會漂移。
pub(super) fn update_installed_hint() -> Cow<'static, str> {
    t!("app.status.update_installed")
}

/// `AppEvent::AutoFetchCompleted` 顯示用的固定文案，唯一生產者是
/// `app.rs` 的處理端；`auto_fetch` 模組不帶 payload，見該事件的文件。
pub(super) fn auto_fetch_success_msg() -> Cow<'static, str> {
    t!("app.status.auto_fetched")
}

fn picker_digit_index(key: KeyEvent) -> Option<usize> {
    let KeyCode::Char(c) = key.code else {
        return None;
    };
    let digit = c.to_digit(10)?;
    (digit as usize).checked_sub(1)
}

/// 單階段 y/n 確認的狀態列，與 [`StatusLineState::yes_no_answer`] 接受的鍵一致。
/// `confirm_desc` 讓呼叫端換掉「confirm」這個字（例如合併 PR 用「execute」更貼切），
/// 按鍵本身固定是 `UserEvent::Confirm`／`UserEvent::Cancel`。
fn confirm_line(
    prompt: String,
    confirm_desc: Cow<'static, str>,
    ctx: &AppContext,
) -> Line<'static> {
    let specs = [
        h(&[UserEvent::Confirm], confirm_desc),
        h(&[UserEvent::Cancel], t!("common.hint.cancel")),
    ];
    let hints = hint_pairs(&ctx.keybind, &specs);
    let mut spans: Vec<Span<'static>> = vec![prompt.into()];
    let hint_fg = ctx.color_theme.status_interactive_fg;
    spans.extend(hint_line(&ctx.color_theme, &hints, hint_fg).spans);
    Line::from(spans)
}

fn build_hotkey_hints(view: &View, ctx: &AppContext) -> Line<'static> {
    let hints: Vec<HintSpec> = match view {
        View::List(_) => vec![
            h(&[UserEvent::Search], t!("common.hint.search")),
            h(&[UserEvent::Filter], t!("common.hint.filter")),
            h(&[UserEvent::IgnoreCaseToggle], t!("common.hint.case")),
            h(&[UserEvent::FuzzyToggle], t!("common.hint.fuzzy")),
            h(&[UserEvent::TargetToggle], t!("common.hint.target")),
            h(&[UserEvent::CreateTag], t!("common.hint.tag")),
            h(&[UserEvent::RefList], t!("common.hint.refs")),
            h(&[UserEvent::RemoteRefsToggle], t!("common.hint.remote")),
            h(&[UserEvent::GitHubToggle], t!("common.hint.github")),
            h(&[UserEvent::Refresh], t!("common.hint.refresh")),
            h(&[UserEvent::HelpToggle], t!("common.hint.help")),
        ],
        View::Detail(ref view) => view.status_hints(),
        View::Refs(_) => vec![
            h(
                &[UserEvent::NavigateDown, UserEvent::NavigateUp],
                t!("common.hint.move"),
            ),
            h(&[UserEvent::Checkout], t!("common.hint.checkout")),
            h(&[UserEvent::DeleteRef], t!("common.hint.delete")),
            h(&[UserEvent::Refresh], t!("common.hint.refresh")),
            h(&[UserEvent::HelpToggle], t!("common.hint.help")),
            h(
                &[UserEvent::Cancel, UserEvent::RefList],
                t!("common.hint.close"),
            ),
        ],
        View::CreateTag(_) | View::DeleteTag(_) | View::DeleteRef(_) => vec![
            h(&[UserEvent::Confirm], t!("common.hint.confirm")),
            h(&[UserEvent::Cancel], t!("common.hint.cancel")),
        ],
        View::UserCommand(_) => crate::view::user_command::status_hints(),
        // Up/Down／PageUp/PageDown 不經過 `KeyBind`（見
        // `view::shell::resolve` 的文件註解），沒有 `UserEvent` 可以掛，早
        // return 手動補上這兩組固定提示——其餘 view 都走尾端共用的
        // `keybind_hint_line` 入口，不必為了這一個特例把它拆開。
        View::Shell(_) => {
            let specs = [
                h(&[UserEvent::Confirm], t!("common.hint.run")),
                h(&[UserEvent::Cancel], t!("common.hint.close")),
            ];
            let history = t!("common.hint.history");
            let scroll = t!("common.hint.scroll");
            let mut pairs = hint_pairs(&ctx.keybind, &specs);
            pairs.extend([
                ("↑↓".to_string(), history.as_ref()),
                ("PgUp/PgDn".to_string(), scroll.as_ref()),
            ]);
            return hint_line(&ctx.color_theme, &pairs, ctx.color_theme.help_key_fg);
        }
        View::Help(_) => vec![
            h(
                &[UserEvent::NavigateDown, UserEvent::NavigateUp],
                t!("common.hint.scroll"),
            ),
            h(&[UserEvent::Close], t!("common.hint.close")),
        ],
        View::GitHub(ref view) => view.status_hints(),
        View::ReleaseNotes(_) => vec![
            h(
                &[UserEvent::NavigateDown, UserEvent::NavigateUp],
                t!("common.hint.scroll"),
            ),
            h(&[UserEvent::HalfPageDown], t!("common.hint.half")),
            h(&[UserEvent::PageDown], t!("common.hint.page")),
            h(&[UserEvent::Close], t!("common.hint.close")),
        ],
        // 窮舉而非 `_`：新增一個 view 時要在這裡被編譯器叫住，
        // 而不是靜默得到一條空提示列。
        View::Default => vec![],
    };

    keybind_hint_line(&ctx.color_theme, &ctx.keybind, &hints)
}

/// merge PR 表單裡可勾選的 head branch 處置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MergeDelete {
    /// 本機同名分支
    Local,
    /// 遠端（GitHub）上的分支
    Remote,
    /// 本機追蹤 ref（`origin/<head_ref>`）
    Tracking,
}

/// merge PR 的兩個彈窗。`rows` 只含「可以問」的項目——不可刪的（fork／default
/// branch／head==base、本機找不到或正是 HEAD、沒有追蹤 ref）直接不出現，順序固定
/// Local → Remote → Tracking。
#[derive(Debug, Clone)]
enum MergePrStage {
    /// 表單：游標 0 是「合併方式」列，其後依序是 `rows`。
    Form {
        method: MergeMethod,
        rows: Vec<(MergeDelete, bool)>,
        cursor: usize,
    },
    /// 第二個彈窗：最後確認。
    Confirm {
        method: MergeMethod,
        rows: Vec<(MergeDelete, bool)>,
    },
}

/// merge 方式在表單裡 ←／→ 循環：merge → squash → rebase → merge。
fn cycle_method(method: MergeMethod, forward: bool) -> MergeMethod {
    use MergeMethod::*;
    match (method, forward) {
        (Merge, true) | (Rebase, false) => Squash,
        (Squash, true) | (Merge, false) => Rebase,
        (Rebase, true) | (Squash, false) => Merge,
    }
}

/// 開啟 merge PR 表單。預設：合併方式 squash、本機分支 ✔、遠端分支 ✔、
/// 本機追蹤 ref ✘。
fn merge_pr_prompt_line(
    number: u64,
    head_ref: String,
    state: StateFilter,
    remote_deletable: bool,
    local_delete: Option<bool>,
    tracking_ref: Option<String>,
) -> StatusLine {
    let mut rows = Vec::with_capacity(3);
    if local_delete.is_some() {
        rows.push((MergeDelete::Local, true));
    }
    if remote_deletable {
        rows.push((MergeDelete::Remote, true));
    }
    if tracking_ref.is_some() {
        rows.push((MergeDelete::Tracking, false));
    }
    StatusLine::MergePrPrompt {
        number,
        head_ref,
        state,
        local_delete,
        tracking_ref,
        stage: MergePrStage::Form {
            method: MergeMethod::Squash,
            rows,
            cursor: 0,
        },
    }
}

/// 單階段 y/n 確認的答案。
#[derive(Debug, Clone, Copy)]
enum Answer {
    Confirm,
    Cancel,
    Ignore,
}

#[derive(Debug, Default)]
#[cfg_attr(test, derive(Clone))]
enum StatusLine {
    #[default]
    None,
    /// 訊息、游標在訊息內的 byte 位置、右側的 transient 訊息。
    Input(String, usize, Option<String>),
    RefPicker {
        options: Vec<String>,
        kind: RefCopyKind,
    },
    CheckoutPicker {
        options: Vec<String>,
        kind: CheckoutPickKind,
    },
    /// `options` 已經截斷到 9 個（數字鍵上限），`total` 是截斷前的實際候選數，
    /// 供 render 顯示「還有幾個沒列出」。截斷在 `open_child_picker` 做，
    /// 不是在送 `AppEvent::OpenChildPicker` 之前——呼叫端有 List/Detail/
    /// UserCommand 三處，收在這個唯一消費端才不會抄三份 `.take(9)`。
    ChildPicker {
        options: Vec<ChildPickOption>,
        total: usize,
    },
    DeleteBranchPicker {
        options: Vec<String>,
        total: usize,
    },
    DeleteBranchConfirm {
        name: String,
    },
    MergePrPrompt {
        number: u64,
        head_ref: String,
        state: StateFilter,
        /// `None` = 本地找不到同名分支，或該分支正是目前 HEAD——表單不出現 Local 列。
        /// `Some(force_safe)` = 可以問，`force_safe` 由
        /// `app::local_branch_delete_check` 依 `headRefOid` 算好帶進來，
        /// 不是使用者選項，決定送出 `MergePrRequested` 時要不要強刪。
        local_delete: Option<bool>,
        /// `Some("origin/<head_ref>")` = 本機有這條追蹤分支且可以問；`None` 表單不出現
        /// Tracking 列，見 `app::tracking_ref_to_delete`。
        tracking_ref: Option<String>,
        stage: MergePrStage,
    },
    ToggleStatePrompt {
        number: u64,
        kind: GhItemKind,
        action: StateAction,
        filter_state: StateFilter,
    },
    TogglePrDraftPrompt {
        number: u64,
        action: PrDraftAction,
        filter_state: StateFilter,
    },
    /// CI 監測 tab 的重跑選擇：`f` 只重跑失敗、`a` 全部重跑。兩份 run id 都由
    /// view 依卡片的 check 算好，空的那個選項不出現也不接受。
    CiRerunPrompt {
        number: u64,
        failed_runs: Vec<u64>,
        all_runs: Vec<u64>,
    },
    /// 有新版 GitHub Release 可更新。跟其他 y/n prompt 共用
    /// `handle_yes_no_prompt_key`，唯一差異是確認後送的事件不同。
    UpdatePrompt {
        tag: String,
    },
    /// 下載＋替換執行檔完成，問是否要離開並以新版重啟。
    RestartPrompt {
        tag: String,
        exe: PathBuf,
    },
    RelatedPicker {
        items: Vec<RelatedItem>,
    },
    NotificationInfo(String),
    NotificationSuccess(String),
    NotificationWarn(String),
    NotificationError(String),
}

/// Phase 6 換資料時要保留的通知——不透明包住 `StatusLine`，`app.rs` 拿到手
/// 只能原樣帶著走，不能塞 picker／prompt 進來。`take_notification()`／
/// `restore_notification()` 是唯一的生產與消費端；`blocks_swap()` 已經先
/// 擋掉 picker／prompt／輸入框，走到這裡的 `StatusLine` 只可能是四種
/// `Notification*` variant 之一。
#[derive(Debug)]
pub(super) struct Notification(StatusLine);

#[derive(Debug)]
pub(super) struct StatusLineState {
    line: StatusLine,
    ctx: Rc<AppContext>,
    tx: Sender,
    /// 下一輪 auto-fetch 的 deadline，倒數用。在這裡持有而不是每次 render
    /// 從 `App` 傳進來：`App::render` 對倒數沒有任何話語權，讓它收一個純轉手
    /// 參數等於逼呼叫端知道一件與它無關的事。
    auto_fetch_clock: AutoFetchClock,
}

impl StatusLineState {
    pub(super) fn new(ctx: Rc<AppContext>, tx: Sender, auto_fetch_clock: AutoFetchClock) -> Self {
        Self {
            line: StatusLine::default(),
            ctx,
            tx,
            auto_fetch_clock,
        }
    }

    /// 這一幀該顯示的倒數秒數。`None` = 沒開 auto-fetch、還沒排第一輪，或
    /// 狀態列正被別的東西佔用。
    ///
    /// **重畫判斷（`App::run` 的 `Tick`）與實際繪製共用這一個函式**——兩邊
    /// 各寫一次條件的話，顯示條件一改重畫條件就會默默不同步，變成開著
    /// picker／輸入框時每秒白畫一次全螢幕（倒數那時根本沒顯示）。
    ///
    /// 取上界而不是截斷：`interval` 是 600 時，`arm` 當下的 remaining 是
    /// 599.999，截斷的話永遠不會顯示 `10:00`，而且 `00:00` 會多停一整秒。
    /// 取上界之後 `00:00` 精確等於「已過期 = worker 正在抓」。
    pub(super) fn countdown_secs(&self) -> Option<u64> {
        if !self.is_idle() {
            return None;
        }
        self.auto_fetch_clock
            .remaining()
            // 先除再 cast：結果必定落在 u64 內，讀者不必回頭確認
            // `as_millis()` 的 u128 上界。
            .map(|d| d.as_millis().div_ceil(1000) as u64)
    }

    pub(super) fn open_ref_picker(&mut self, options: Vec<String>, kind: RefCopyKind) {
        self.line = StatusLine::RefPicker { options, kind };
    }

    pub(super) fn open_checkout_picker(&mut self, options: Vec<String>, kind: CheckoutPickKind) {
        self.line = StatusLine::CheckoutPicker { options, kind };
    }

    pub(super) fn open_child_picker(&mut self, options: Vec<ChildPickOption>) {
        let total = options.len();
        let options = options.into_iter().take(9).collect();
        self.line = StatusLine::ChildPicker { options, total };
    }

    /// 空清單走同步賦值，不透過 `tx` 送 `AppEvent::NotifyInfo`——`App::run()`
    /// 是「畫一幀 → recv 一個事件 → 處理 → 再畫一幀」的迴圈，送事件的話這
    /// 一輪迴圈會先用 `line` 還沒變的狀態畫一幀（顯示 hotkey hints），下一
    /// 輪才變成通知，等於平白多一幀「先閃一下 hint 列才出現提示」。
    pub(super) fn open_related_picker(&mut self, items: Vec<RelatedItem>) {
        if items.is_empty() {
            self.set_notification_info(t!("app.status.no_related").into_owned());
        } else {
            self.line = StatusLine::RelatedPicker { items };
        }
    }

    pub(super) fn open_delete_branch_picker(&mut self, options: Vec<String>, total: usize) {
        self.line = StatusLine::DeleteBranchPicker { options, total };
    }

    pub(super) fn open_delete_branch_confirm(&mut self, name: String) {
        self.line = StatusLine::DeleteBranchConfirm { name };
    }

    pub(super) fn open_merge_pr_prompt(
        &mut self,
        number: u64,
        head_ref: String,
        state: StateFilter,
        remote_deletable: bool,
        local_delete: Option<bool>,
        tracking_ref: Option<String>,
    ) {
        self.line = merge_pr_prompt_line(
            number,
            head_ref,
            state,
            remote_deletable,
            local_delete,
            tracking_ref,
        );
    }

    pub(super) fn open_toggle_state_prompt(
        &mut self,
        number: u64,
        kind: GhItemKind,
        action: StateAction,
        filter_state: StateFilter,
    ) {
        self.line = StatusLine::ToggleStatePrompt {
            number,
            kind,
            action,
            filter_state,
        };
    }

    pub(super) fn open_toggle_pr_draft_prompt(
        &mut self,
        number: u64,
        action: PrDraftAction,
        filter_state: StateFilter,
    ) {
        self.line = StatusLine::TogglePrDraftPrompt {
            number,
            action,
            filter_state,
        };
    }

    pub(super) fn open_ci_rerun_prompt(
        &mut self,
        number: u64,
        failed_runs: Vec<u64>,
        all_runs: Vec<u64>,
    ) {
        self.line = StatusLine::CiRerunPrompt {
            number,
            failed_runs,
            all_runs,
        };
    }

    pub(super) fn open_update_prompt(&mut self, tag: String) {
        self.line = StatusLine::UpdatePrompt { tag };
    }

    pub(super) fn open_restart_prompt(&mut self, tag: String, exe: PathBuf) {
        self.line = StatusLine::RestartPrompt { tag, exe };
    }

    /// 給 `App::maybe_open_update_prompt` 的守衛用：狀態列現在有沒有被別的
    /// 東西佔著（picker、prompt、甚至一則通知）。更新提示是背景檢查回來時
    /// 才會出現的，不能覆蓋掉使用者當下正在看的任何東西。
    pub(super) fn is_idle(&self) -> bool {
        matches!(self.line, StatusLine::None)
    }

    /// 給 `App::maybe_open_restart_prompt` 用：比 `is_idle` 寬一格，容許狀態列
    /// 正顯示一則通知。重啟提示是使用者自己按 `U`、自己按 `y`、自己等下載完
    /// 的結果，蓋掉一則同樣在講這件事的通知不算冒犯——跟 `is_idle` 服務的
    /// 「背景不請自來」語意不同，不要合併這兩個判斷式。
    ///
    /// **不含 `NotificationError`**：跟 `dismiss_notification` 既有的三分類
    /// 一致——錯誤要使用者主動按鍵確認過才能清掉（見該方法），這裡若也蓋得掉
    /// 錯誤，使用者原本想按任意鍵去確認錯誤，卻可能誤觸重啟提示的 y/n
    /// （例如習慣性按 `y`），後果比通知被覆寫嚴重得多。
    pub(super) fn is_showing_notification(&self) -> bool {
        matches!(
            self.line,
            StatusLine::NotificationInfo(_)
                | StatusLine::NotificationSuccess(_)
                | StatusLine::NotificationWarn(_)
        )
    }

    /// 狀態列現在能不能被「背景不請自來」的通知寫入：閒置，或只是壓著
    /// 另一則通知。picker／prompt 都不算。`is_idle`／`is_showing_notification`
    /// 各自的語意仍然分開（見上），這裡只是把兩者的 `||` 組合具名化，讓
    /// `can_interrupt` 與背景通知的守衛共用同一份真值，不要各自手寫一次。
    pub(super) fn is_idle_or_notification(&self) -> bool {
        self.is_idle() || self.is_showing_notification()
    }

    /// 換資料能不能進行的判斷之一：狀態列現在是不是擋著——picker、prompt、
    /// 或正在輸入的 search／filter 輸入框（`StatusLine::Input`）。跟
    /// `is_idle_or_notification` 不同的地方是這裡把 `NotificationError`
    /// 也算「不擋」：重載失敗的錯誤通知不該卡住下一次成功結果的顯示，
    /// 使用者不需要先按鍵確認錯誤才能看到新資料。`Input` 則反過來一定要擋
    /// ——換資料會重建 `StatusLineState`（回到 `None`），使用者半打的搜尋
    /// 字串會憑空消失。
    pub(super) fn blocks_swap(&self) -> bool {
        !matches!(
            self.line,
            StatusLine::None
                | StatusLine::NotificationInfo(_)
                | StatusLine::NotificationSuccess(_)
                | StatusLine::NotificationWarn(_)
                | StatusLine::NotificationError(_)
        )
    }

    /// 換資料時取出目前的通知（如果有）——只有四種 `Notification*`
    /// variant 會被取到並清空狀態列；其餘情況原樣放回、回 `None`。呼叫端
    /// （`App::try_swap`）在呼叫這個之前已經用 `blocks_swap()` 確認過狀態列
    /// 不是 picker／prompt／輸入框，所以「其餘情況」實務上只有 `None`。
    pub(super) fn take_notification(&mut self) -> Option<Notification> {
        match std::mem::take(&mut self.line) {
            line @ (StatusLine::NotificationInfo(_)
            | StatusLine::NotificationSuccess(_)
            | StatusLine::NotificationWarn(_)
            | StatusLine::NotificationError(_)) => Some(Notification(line)),
            other => {
                self.line = other;
                None
            }
        }
    }

    /// 換資料重建 `App` 之後呼叫，把 `take_notification()` 取走的通知放回
    /// 新的 `StatusLineState` 上。
    pub(super) fn restore_notification(&mut self, notification: Notification) {
        self.line = notification.0;
    }

    /// Full 重載狀態指示——插在 hotkey hints／輸入框那一列最前面，跟倒數
    /// span 共用同一個插入點模式（見 `render` 尾端的插入點註解）。
    /// `Idle` 不畫任何東西；`Loading` 代表 worker 正在跑（含冷卻中排隊）；
    /// `Ready` 只可能在 `render()` 被呼叫到時出現——它代表換資料被目前狀態
    /// 擋住了（`App::try_swap()` 每輪都先試過，能換的話早就換掉、根本不會
    /// 走到這裡渲染）。
    fn full_reload_span(&self, status: FullStatus) -> Option<Span<'static>> {
        let text = match status {
            FullStatus::Idle => return None,
            FullStatus::Loading => t!("app.status.reload_loading"),
            FullStatus::Ready => t!("app.status.reload_pending"),
        };
        Some(Span::styled(
            text,
            Style::default().fg(self.ctx.color_theme.status_input_transient_fg),
        ))
    }

    pub(super) fn clear(&mut self) {
        self.line = StatusLine::None;
    }

    pub(super) fn update_input(
        &mut self,
        msg: String,
        cursor: usize,
        transient_msg: Option<String>,
    ) {
        self.line = StatusLine::Input(msg, cursor, transient_msg);
    }

    pub(super) fn set_notification_info(&mut self, msg: String) {
        self.line = StatusLine::NotificationInfo(msg);
    }

    pub(super) fn set_notification_success(&mut self, msg: String) {
        self.line = StatusLine::NotificationSuccess(msg);
    }

    pub(super) fn set_notification_warn(&mut self, msg: String) {
        self.line = StatusLine::NotificationWarn(msg);
    }

    pub(super) fn set_notification_error(&mut self, msg: String) {
        self.line = StatusLine::NotificationError(msg);
    }

    /// 回傳 true 表示這是 12 個攔截變體之一（9 個 handler：`ToggleStatePrompt`、
    /// `TogglePrDraftPrompt`、`UpdatePrompt`、`RestartPrompt` 四個共用
    /// `handle_yes_no_prompt_key`，其餘各自一個 handler）、鍵已被吃掉。
    ///
    /// 尾巴刻意窮舉非攔截變體而非 `_ => false`：漏接一個新變體只會讓它卡在
    /// 畫面上完全不吃鍵，編譯器不會提醒（比照 app.rs 原本就有的同款警告，
    /// 這裡原封不動繼承）。
    pub(super) fn handle_intercepting_key(&mut self, key: KeyEvent) -> bool {
        match self.line {
            StatusLine::RefPicker { .. } => {
                self.handle_ref_picker_key(key);
                true
            }
            StatusLine::CheckoutPicker { .. } => {
                self.handle_checkout_picker_key(key);
                true
            }
            StatusLine::ChildPicker { .. } => {
                self.handle_child_picker_key(key);
                true
            }
            StatusLine::RelatedPicker { .. } => {
                self.handle_related_picker_key(key);
                true
            }
            StatusLine::DeleteBranchPicker { .. } => {
                self.handle_delete_branch_picker_key(key);
                true
            }
            StatusLine::DeleteBranchConfirm { .. } => {
                self.handle_delete_branch_confirm_key(key);
                true
            }
            StatusLine::MergePrPrompt { .. } => {
                self.handle_merge_pr_prompt_key(key);
                true
            }
            StatusLine::CiRerunPrompt { .. } => {
                self.handle_ci_rerun_prompt_key(key);
                true
            }
            StatusLine::ToggleStatePrompt { .. }
            | StatusLine::TogglePrDraftPrompt { .. }
            | StatusLine::UpdatePrompt { .. }
            | StatusLine::RestartPrompt { .. } => {
                self.handle_yes_no_prompt_key(key);
                true
            }
            StatusLine::None
            | StatusLine::Input(_, _, _)
            | StatusLine::NotificationInfo(_)
            | StatusLine::NotificationSuccess(_)
            | StatusLine::NotificationWarn(_)
            | StatusLine::NotificationError(_) => false,
        }
    }

    /// 清掉目前的通知（若有）。回傳 true 表示這把鍵已被吞掉——只有 Error
    /// 通知會吞鍵，其餘通知清完後讓鍵繼續往下走。窮盡 match，理由同
    /// `handle_intercepting_key`。
    pub(super) fn dismiss_notification(&mut self) -> bool {
        match self.line {
            StatusLine::None
            | StatusLine::Input(_, _, _)
            | StatusLine::RefPicker { .. }
            | StatusLine::CheckoutPicker { .. }
            | StatusLine::ChildPicker { .. }
            | StatusLine::RelatedPicker { .. }
            | StatusLine::DeleteBranchPicker { .. }
            | StatusLine::DeleteBranchConfirm { .. }
            | StatusLine::MergePrPrompt { .. }
            | StatusLine::CiRerunPrompt { .. }
            | StatusLine::ToggleStatePrompt { .. }
            | StatusLine::TogglePrDraftPrompt { .. }
            | StatusLine::UpdatePrompt { .. }
            | StatusLine::RestartPrompt { .. } => false,
            StatusLine::NotificationInfo(_)
            | StatusLine::NotificationSuccess(_)
            | StatusLine::NotificationWarn(_) => {
                self.line = StatusLine::None;
                false
            }
            StatusLine::NotificationError(_) => {
                self.line = StatusLine::None;
                true
            }
        }
    }

    /// 對應 `App::is_input_mode()` 判斷式裡跟狀態列相關的那部分。**故意跟
    /// `handle_intercepting_key` 涵蓋的變體不同步**：6 個 prompt
    /// （`MergePrPrompt`/`CiRerunPrompt`/`ToggleStatePrompt`/`TogglePrDraftPrompt`/`UpdatePrompt`/
    /// `RestartPrompt`）不在這份清單裡，因為它們在 `handle_intercepting_key`
    /// 永遠回傳 `true`，唯一能繞過的只有 `ForceQuit`，而 `ForceQuit` 分支根本
    /// 不查這個方法。`MergePrPrompt` 的兩個彈窗一樣靠這個攔截擋鍵。動
    /// `ForceQuit` 分支或動 `handle_intercepting_key` 涵蓋範圍時，回來檢查這裡。
    pub(super) fn is_input_mode_variant(&self) -> bool {
        match self.line {
            StatusLine::Input(_, _, _)
            | StatusLine::RefPicker { .. }
            | StatusLine::CheckoutPicker { .. }
            | StatusLine::ChildPicker { .. }
            | StatusLine::RelatedPicker { .. }
            | StatusLine::DeleteBranchPicker { .. }
            | StatusLine::DeleteBranchConfirm { .. } => true,
            StatusLine::None
            | StatusLine::MergePrPrompt { .. }
            | StatusLine::CiRerunPrompt { .. }
            | StatusLine::ToggleStatePrompt { .. }
            | StatusLine::TogglePrDraftPrompt { .. }
            | StatusLine::UpdatePrompt { .. }
            | StatusLine::RestartPrompt { .. }
            | StatusLine::NotificationInfo(_)
            | StatusLine::NotificationSuccess(_)
            | StatusLine::NotificationWarn(_)
            | StatusLine::NotificationError(_) => false,
        }
    }

    fn handle_ref_picker_key(&mut self, key: KeyEvent) {
        if self.ctx.keybind.has(&key, UserEvent::Cancel) {
            self.line = StatusLine::None;
            return;
        }
        let StatusLine::RefPicker { options, kind } = &self.line else {
            return;
        };
        let Some(idx) = picker_digit_index(key) else {
            return;
        };
        let Some(name) = options.get(idx) else {
            return;
        };
        let label = kind.copy_label();
        let value = name.clone();
        self.line = StatusLine::None;
        self.tx.send(AppEvent::CopyToClipboard {
            name: label.into(),
            value,
        });
    }

    fn handle_related_picker_key(&mut self, key: KeyEvent) {
        if self.ctx.keybind.has(&key, UserEvent::Cancel)
            || self.ctx.keybind.has(&key, UserEvent::DetailPaneToggle)
        {
            self.line = StatusLine::None;
            return;
        }
        let StatusLine::RelatedPicker { items } = &self.line else {
            return;
        };
        let Some(idx) = picker_digit_index(key) else {
            return;
        };
        let Some(item) = items.get(idx) else {
            return;
        };
        let number = item.number;
        self.line = StatusLine::None;
        self.tx.send(AppEvent::GitHubJumpToIssue { number });
    }

    fn handle_checkout_picker_key(&mut self, key: KeyEvent) {
        if self.ctx.keybind.has(&key, UserEvent::Cancel) {
            self.line = StatusLine::None;
            return;
        }
        let StatusLine::CheckoutPicker { options, .. } = &self.line else {
            return;
        };
        let Some(idx) = picker_digit_index(key) else {
            return;
        };
        let Some(name) = options.get(idx) else {
            return;
        };
        let target = name.clone();
        self.line = StatusLine::None;
        self.tx.send(AppEvent::CheckoutCommit { target });
    }

    fn handle_child_picker_key(&mut self, key: KeyEvent) {
        if self.ctx.keybind.has(&key, UserEvent::Cancel) {
            self.line = StatusLine::None;
            return;
        }
        let StatusLine::ChildPicker { options, .. } = &self.line else {
            return;
        };
        let Some(idx) = picker_digit_index(key) else {
            return;
        };
        let Some(option) = options.get(idx) else {
            return;
        };
        let hash = option.commit_hash.clone();
        self.line = StatusLine::None;
        self.tx.send(AppEvent::SelectChildCommitByHash { hash });
    }

    fn handle_delete_branch_picker_key(&mut self, key: KeyEvent) {
        if self.ctx.keybind.has(&key, UserEvent::Cancel) {
            self.line = StatusLine::None;
            return;
        }
        let StatusLine::DeleteBranchPicker { options, .. } = &self.line else {
            return;
        };
        let Some(idx) = picker_digit_index(key) else {
            return;
        };
        let Some(name) = options.get(idx) else {
            return;
        };
        let name = name.clone();
        self.line = StatusLine::DeleteBranchConfirm { name };
    }

    fn handle_delete_branch_confirm_key(&mut self, key: KeyEvent) {
        if self.ctx.keybind.has(&key, UserEvent::Cancel) {
            self.line = StatusLine::None;
            return;
        }
        let StatusLine::DeleteBranchConfirm { name } = &self.line else {
            return;
        };
        let force = if self.ctx.keybind.has(&key, UserEvent::Confirm) {
            false
        } else if matches!(key.code, KeyCode::Char('f') | KeyCode::Char('F')) {
            true
        } else {
            return;
        };
        let name = name.clone();
        self.line = StatusLine::None;
        self.tx
            .send(AppEvent::DeleteBranchRequested { name, force });
    }

    fn handle_ci_rerun_prompt_key(&mut self, key: KeyEvent) {
        if self.ctx.keybind.has(&key, UserEvent::Cancel) {
            self.line = StatusLine::None;
            return;
        }
        let StatusLine::CiRerunPrompt {
            number,
            failed_runs,
            all_runs,
        } = &self.line
        else {
            return;
        };
        let (run_ids, scope) = match key.code {
            KeyCode::Char('f' | 'F') if !failed_runs.is_empty() => {
                (failed_runs.clone(), RerunScope::FailedOnly)
            }
            KeyCode::Char('a' | 'A') if !all_runs.is_empty() => (all_runs.clone(), RerunScope::All),
            _ => return,
        };
        let number = *number;
        self.line = StatusLine::None;
        self.tx.send(AppEvent::CiRerunRequested {
            number,
            run_ids,
            scope,
        });
    }

    fn handle_merge_pr_prompt_key(&mut self, key: KeyEvent) {
        let keybind = &self.ctx.keybind;
        let StatusLine::MergePrPrompt {
            number,
            head_ref,
            state,
            local_delete,
            tracking_ref,
            stage,
        } = &mut self.line
        else {
            return;
        };
        let is_cancel = keybind.has(&key, UserEvent::Cancel);
        let is_confirm = keybind.has(&key, UserEvent::Confirm) || key.code == KeyCode::Enter;

        let mut close = false;
        let mut request = None;
        match stage {
            MergePrStage::Form {
                method,
                rows,
                cursor,
            } => {
                // 游標 0 是方式列，其後才是 rows，所以總列數要 +1。
                let len = rows.len() + 1;
                if is_cancel {
                    close = true;
                } else if is_confirm {
                    *stage = MergePrStage::Confirm {
                        method: *method,
                        rows: std::mem::take(rows),
                    };
                } else if keybind.has(&key, UserEvent::NavigateDown) {
                    *cursor = (*cursor + 1) % len;
                } else if keybind.has(&key, UserEvent::NavigateUp) {
                    *cursor = (*cursor + len - 1) % len;
                } else {
                    let forward = keybind.has(&key, UserEvent::NavigateRight);
                    if forward || keybind.has(&key, UserEvent::NavigateLeft) {
                        match *cursor {
                            0 => *method = cycle_method(*method, forward),
                            i => rows[i - 1].1 = !rows[i - 1].1,
                        }
                    }
                }
            }
            MergePrStage::Confirm { method, rows } => {
                if is_cancel {
                    close = true;
                } else if is_confirm {
                    let enabled = |opt| rows.iter().any(|&(o, checked)| o == opt && checked);
                    request = Some(AppEvent::MergePrRequested {
                        number: *number,
                        state: *state,
                        method: *method,
                        delete_local_branch: enabled(MergeDelete::Local)
                            .then(|| (head_ref.clone(), *local_delete == Some(true))),
                        delete_remote_branch: enabled(MergeDelete::Remote)
                            .then(|| head_ref.clone()),
                        delete_tracking_ref: tracking_ref
                            .clone()
                            .filter(|_| enabled(MergeDelete::Tracking)),
                    });
                    close = true;
                }
            }
        }
        if close {
            self.line = StatusLine::None;
        }
        if let Some(event) = request {
            self.tx.send(event);
        }
    }

    /// 解讀 y/n 確認鍵。cancel 必須先判 — 預設 `cancel = ["esc", "n"]` 含 `n`，
    /// 順序反了「no」就會被當成確認。把這個順序依賴收在這裡，呼叫端不必再記。
    fn yes_no_answer(&self, key: KeyEvent) -> Answer {
        if self.ctx.keybind.has(&key, UserEvent::Cancel)
            || matches!(key.code, KeyCode::Char('n') | KeyCode::Char('N'))
        {
            return Answer::Cancel;
        }
        if self.ctx.keybind.has(&key, UserEvent::Confirm)
            || matches!(
                key.code,
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y')
            )
        {
            return Answer::Confirm;
        }
        Answer::Ignore
    }

    /// 單階段 y/n 確認的 modal。答完就收掉狀態列，取出的 prompt 決定要跑什麼。
    fn handle_yes_no_prompt_key(&mut self, key: KeyEvent) {
        let answer = self.yes_no_answer(key);
        if matches!(answer, Answer::Ignore) {
            return;
        }
        // take 同時結束對 line 的借用並清掉 modal
        let prompt = std::mem::take(&mut self.line);
        if matches!(answer, Answer::Cancel) {
            // RestartPrompt 取消時更新其實已經做完了，跟其餘 prompt「取消 =
            // 什麼都沒發生」不同——靜默清空會讓使用者忘記要手動重啟，留一句話。
            if matches!(prompt, StatusLine::RestartPrompt { .. }) {
                self.line = StatusLine::NotificationSuccess(update_installed_hint().into_owned());
            }
            return;
        }
        match prompt {
            StatusLine::ToggleStatePrompt {
                number,
                kind,
                action,
                filter_state,
            } => {
                self.tx.send(AppEvent::ToggleItemStateRequested {
                    number,
                    kind,
                    action,
                    filter_state,
                });
            }
            StatusLine::TogglePrDraftPrompt {
                number,
                action,
                filter_state,
            } => {
                self.tx.send(AppEvent::TogglePrDraftRequested {
                    number,
                    action,
                    filter_state,
                });
            }
            StatusLine::UpdatePrompt { tag } => {
                self.tx.send(AppEvent::UpdateRequested { tag });
            }
            StatusLine::RestartPrompt { exe, .. } => {
                self.tx.send(AppEvent::RestartRequested { exe });
            }
            _ => {}
        }
    }

    /// `nf-cod-git_fetch`（U+EC1D）+ 剩餘時間。
    ///
    /// 不足一分鐘印 `29s`，超過才印 `9:58`——`interval` 最小值是 30 秒，
    /// 用固定 `mm:ss` 的話那一整段時間都在顯示一個恆為 `00:` 的前綴，是純
    /// 噪音。帶 `s` 字尾讓「這是秒」不必靠上下文猜（少了 `:` 之後純數字會
    /// 有歧義）。
    ///
    /// 分鐘數不補零：`interval` 在 CLI 與設定檔兩個入口都夾在 30–3600 秒，
    /// 最寬就是 `60:00`。
    ///
    /// 沒有 Nerd Font 的終端會看到豆腐字——auto-fetch 本來就預設關閉、要明確
    /// 開啟，不為此加一個顯示開關。
    fn countdown_span(&self, secs: u64) -> Span<'static> {
        let remaining = if secs < 60 {
            format!("{secs}s")
        } else {
            format!("{}:{:02}", secs / 60, secs % 60)
        };
        Span::styled(
            format!("\u{EC1D} {remaining}  "),
            Style::default().fg(self.ctx.color_theme.status_input_transient_fg),
        )
    }

    pub(super) fn render(
        &self,
        f: &mut Frame,
        area: Rect,
        view: &View,
        numeric_prefix: &str,
        full_status: FullStatus,
    ) {
        // 提早算好、只算一次：下面 `Input` 分支的 exact-fit padding 與
        // 游標位置都要扣掉／讓開這個前綴的寬度——`full_reload_span` 不像
        // `countdown_span` 有 `is_idle()` 擋著，Input 輸入框開著時一樣可能
        // 同時被插進最前面，兩處若各自重算一次容易對不上。
        let full_reload_span = self.full_reload_span(full_status);
        let full_reload_w = full_reload_span.as_ref().map_or(0, Span::width);

        // `Input` 分支算出的「游標前內容的顯示寬度」，渲染後定位游標用。
        let mut cursor_col = 0usize;

        let mut text: Line = match &self.line {
            StatusLine::None => {
                if numeric_prefix.is_empty() {
                    build_hotkey_hints(view, &self.ctx)
                } else {
                    Line::raw(numeric_prefix).fg(self.ctx.color_theme.status_input_transient_fg)
                }
            }
            StatusLine::Input(msg, cursor, transient_msg) => {
                let (before, display) =
                    input_display(msg, *cursor, &self.ctx.ui_config.cursor_type);
                cursor_col = console::measure_text_width(before);
                let msg_w = console::measure_text_width(&display);
                if let Some(t_msg) = transient_msg {
                    let t_msg_w = console::measure_text_width(t_msg.as_str());
                    // saturating：窄終端 + 長 transient message 會讓這串減法
                    // underflow，接著 `" ".repeat(天文數字)` 直接吃光記憶體。
                    let pad_w = (area.width as usize)
                        .saturating_sub(msg_w)
                        .saturating_sub(t_msg_w)
                        .saturating_sub(2 /* pad */)
                        .saturating_sub(full_reload_w);
                    Line::from(vec![
                        display.fg(self.ctx.color_theme.status_input_fg),
                        " ".repeat(pad_w).into(),
                        t_msg
                            .as_str()
                            .fg(self.ctx.color_theme.status_input_transient_fg),
                    ])
                } else {
                    Line::raw(display).fg(self.ctx.color_theme.status_input_fg)
                }
            }
            StatusLine::RefPicker { options, kind } => self.render_picker_line(
                kind.picker_prompt(),
                options.iter().map(String::as_str),
                esc_cancel(),
            ),
            StatusLine::CheckoutPicker { options, kind } => self.render_picker_line(
                kind.picker_prompt(),
                options.iter().map(String::as_str),
                esc_cancel(),
            ),
            StatusLine::ChildPicker { options, total } => {
                let tail = if *total > options.len() {
                    t!("app.status.more", n = total - options.len())
                } else {
                    esc_cancel()
                };
                self.render_picker_line(
                    t!("app.status.go_to_child"),
                    options.iter().map(|o| o.label.as_str()),
                    tail,
                )
            }
            StatusLine::RelatedPicker { items } => self.render_related_picker_line(items),
            StatusLine::DeleteBranchPicker { options, total } => {
                let tail = if *total > options.len() {
                    t!("app.status.more_tab_view", n = total - options.len())
                } else {
                    esc_cancel()
                };
                self.render_picker_line(
                    t!("app.status.delete_branch"),
                    options.iter().map(String::as_str),
                    tail,
                )
            }
            StatusLine::DeleteBranchConfirm { name } => {
                let hint_fg = self.ctx.color_theme.status_interactive_fg;
                Line::from(vec![
                    t!("app.status.delete_branch_confirm", name = name).into(),
                    format!("[y] {}", t!("common.yes")).fg(hint_fg),
                    " / ".into(),
                    format!("[n] {}", t!("common.no")).fg(hint_fg),
                    " / ".into(),
                    format!("[f] {}", t!("common.force")).fg(hint_fg),
                ])
            }
            // 提示全部在彈窗裡（`render_popup`），狀態列留白。
            StatusLine::MergePrPrompt { .. } => Line::raw(""),
            StatusLine::CiRerunPrompt {
                number,
                failed_runs,
                all_runs,
            } => {
                let hint_fg = self.ctx.color_theme.status_interactive_fg;
                let mut spans: Vec<Span<'static>> =
                    vec![t!("app.status.ci_rerun", number = number)
                        .into_owned()
                        .into()];
                let options = [
                    (!failed_runs.is_empty()).then(|| ("f", t!("app.status.ci_rerun_failed"))),
                    (!all_runs.is_empty()).then(|| ("a", t!("app.status.ci_rerun_all"))),
                ];
                for (i, (key, label)) in options.into_iter().flatten().enumerate() {
                    if i > 0 {
                        spans.push(" / ".into());
                    }
                    spans.push(format!("[{key}] {label}").fg(hint_fg));
                }
                spans.push(" / ".into());
                spans.push(format!("[Esc] {}", t!("common.hint.cancel")).fg(hint_fg));
                Line::from(spans)
            }
            StatusLine::ToggleStatePrompt {
                number,
                kind,
                action,
                ..
            } => confirm_line(
                action.prompt(*kind, *number),
                t!("common.hint.confirm"),
                &self.ctx,
            ),
            StatusLine::TogglePrDraftPrompt { number, action, .. } => {
                confirm_line(action.prompt(*number), t!("common.hint.confirm"), &self.ctx)
            }
            StatusLine::UpdatePrompt { tag } => confirm_line(
                format!("v{} → {tag}", env!("CARGO_PKG_VERSION")),
                t!("common.hint.update"),
                &self.ctx,
            ),
            StatusLine::RestartPrompt { tag, .. } => confirm_line(
                t!("app.status.updated_to", tag = tag).into_owned(),
                t!("common.hint.restart"),
                &self.ctx,
            ),
            StatusLine::NotificationInfo(msg) => {
                Line::raw(msg).fg(self.ctx.color_theme.status_info_fg)
            }
            StatusLine::NotificationSuccess(msg) => Line::raw(msg)
                .add_modifier(Modifier::BOLD)
                .fg(self.ctx.color_theme.status_success_fg),
            StatusLine::NotificationWarn(msg) => Line::raw(msg)
                .add_modifier(Modifier::BOLD)
                .fg(self.ctx.color_theme.status_warn_fg),
            StatusLine::NotificationError(msg) => Line::raw(t!("app.status.error", msg = msg))
                .add_modifier(Modifier::BOLD)
                .fg(self.ctx.color_theme.status_error_fg),
        };

        // 插入點只有這一個，不在 match 的個別 arm 裡各插一次——要不要擴到
        // 別的 variant（例如通知）是改 `countdown_secs` 那個述詞，不是回來
        // 翻這裡的每一支。
        //
        // 上面幾支 `Line::raw(..).fg(..)` 的樣式是設在 *line* 上的，這個
        // span 帶自己的 `fg`，不會被蓋掉也不會污染別人。
        if let Some(secs) = self.countdown_secs() {
            text.spans.insert(0, self.countdown_span(secs));
        }
        if let Some(span) = full_reload_span {
            text.spans.insert(0, span);
        }

        let block = Block::default()
            .borders(Borders::TOP)
            .style(Style::default().fg(self.ctx.color_theme.divider_fg))
            .padding(Padding::horizontal(1));

        // 截斷統一在這裡套一次，不分支 —— picker 那幾行才是真正沒有上限的
        // （`render_picker_line` 把每個 branch 名 inline 印出來），提示列反而是
        // 人工策劃過的。可用寬度從 `block.inner()` 拿，不要手算 padding。
        let inner = block.inner(area);
        let max_width = inner.width as usize;
        let paragraph = Paragraph::new(truncate_line(text, max_width)).block(block);
        f.render_widget(paragraph, area);

        if let StatusLine::Input(..) = &self.line {
            // `+ full_reload_w`：Full 重載指示插在 `msg` 前面時會把整行往右
            // 推，游標必須跟著讓開，否則會畫在指示文字底下而不是輸入內容
            // 對應的字元位置。
            let offset = u16::try_from(full_reload_w + cursor_col).unwrap_or(u16::MAX);
            let (x, y) = (inner.x.saturating_add(offset), inner.y);
            match &self.ctx.ui_config.cursor_type {
                CursorType::Native => {
                    f.set_cursor_position((x, y));
                }
                CursorType::Virtual(cursor) => {
                    // 游標字元已由 `input_display` 接進字串，這裡只上色、不覆寫。
                    // 被 `truncate_line` 截掉時那格已是 `…`／空白，染色會誤導，
                    // 所以先確認格子上真的是游標字元。
                    let w = u16::try_from(console::measure_text_width(cursor)).unwrap_or(u16::MAX);
                    if x.saturating_add(w) <= inner.right()
                        && f.buffer_mut()
                            .cell((x, y))
                            .is_some_and(|c| c.symbol() == cursor.as_str())
                    {
                        let style = Style::default().fg(self.ctx.color_theme.virtual_cursor_fg);
                        f.buffer_mut().set_style(Rect::new(x, y, w, 1), style);
                    }
                }
            }
        }
    }

    fn render_related_picker_line<'s>(&self, items: &'s [RelatedItem]) -> Line<'s> {
        use crate::event::RelatedGroup;
        let mut spans: Vec<Span<'s>> = Vec::new();
        let mut last_group: Option<RelatedGroup> = None;
        for (i, item) in items.iter().enumerate() {
            if last_group != Some(item.group) {
                if last_group.is_some() {
                    spans.push(" ; ".into());
                }
                spans.push(format!("{} - ", item.group.label()).into());
                last_group = Some(item.group);
            } else {
                spans.push(t!("app.status.list_separator").into());
            }
            spans.push(format!("{}", i + 1).fg(self.ctx.color_theme.status_interactive_fg));
            let num = format!(":#{}", item.number);
            let span: Span = if item.state.eq_ignore_ascii_case("CLOSED")
                || item.state.eq_ignore_ascii_case("MERGED")
            {
                num.add_modifier(Modifier::DIM)
            } else {
                num.into()
            };
            spans.push(span);
        }
        spans.push("  ".into());
        spans.push(esc_cancel().fg(self.ctx.color_theme.status_interactive_fg));
        Line::from(spans)
    }

    /// `labels` 收 `impl Iterator<Item = &'s str>` 而非 `&'s [String]`：
    /// `RefPicker`/`CheckoutPicker`/`DeleteBranchPicker` 的候選本來就是
    /// `String`，`ChildPicker` 的候選是 `ChildPickOption`，只借它的
    /// `label` 欄位——不用為了統一型別多 clone 一份 `Vec<String>`。
    fn render_picker_line<'s>(
        &self,
        prompt: Cow<'s, str>,
        labels: impl Iterator<Item = &'s str>,
        tail: Cow<'s, str>,
    ) -> Line<'s> {
        let mut spans: Vec<Span<'s>> = vec![Span::raw(prompt)];
        for (i, name) in labels.enumerate() {
            spans.push(format!("[{}]", i + 1).fg(self.ctx.color_theme.status_interactive_fg));
            spans.push(name.into());
            spans.push("  ".into());
        }
        spans.push(tail.fg(self.ctx.color_theme.status_interactive_fg));
        Line::from(spans)
    }

    /// merge PR 的兩個彈窗（表單／確認），畫在整個 view 區域上方。其餘狀態不畫。
    pub(super) fn render_popup(&self, f: &mut Frame, area: Rect) {
        let StatusLine::MergePrPrompt {
            number,
            head_ref,
            tracking_ref,
            stage,
            ..
        } = &self.line
        else {
            return;
        };
        let theme = &self.ctx.color_theme;
        let fg = theme.fg;
        let row_label = |opt: MergeDelete| match opt {
            MergeDelete::Local => t!("view.delete_ref.opt_local", name = head_ref),
            MergeDelete::Remote => t!("view.delete_ref.opt_remote", name = head_ref),
            MergeDelete::Tracking => t!(
                "view.delete_ref.opt_tracking",
                name = tracking_ref.as_deref().unwrap_or_default()
            ),
        };

        let (title, lines, hints) = match stage {
            MergePrStage::Form {
                method,
                rows,
                cursor,
            } => {
                let style_at = |i: usize| {
                    let style = Style::default().fg(fg);
                    if i == *cursor {
                        style.add_modifier(Modifier::REVERSED)
                    } else {
                        style
                    }
                };
                let mut lines = vec![Line::from(Span::styled(
                    format!(
                        "{}: < {} >",
                        t!("app.merge.dialog.method"),
                        method.display()
                    ),
                    style_at(0),
                ))];
                if !rows.is_empty() {
                    lines.push(Line::from(Span::styled(
                        t!("app.merge.dialog.delete_after"),
                        Style::default().fg(fg).add_modifier(Modifier::DIM),
                    )));
                }
                lines.extend(rows.iter().enumerate().map(|(i, &(opt, checked))| {
                    let checkbox = if checked { "[x]" } else { "[ ]" };
                    Line::from(Span::styled(
                        format!("{checkbox}{}", row_label(opt)),
                        style_at(i + 1),
                    ))
                }));
                let hints = vec![
                    h(&[UserEvent::Confirm], t!("common.hint.next")),
                    h(&[UserEvent::Cancel], t!("common.hint.cancel")),
                    h(
                        &[UserEvent::NavigateLeft, UserEvent::NavigateRight],
                        t!("common.hint.toggle"),
                    ),
                    h(
                        &[UserEvent::NavigateUp, UserEvent::NavigateDown],
                        t!("common.hint.move"),
                    ),
                ];
                (t!("app.merge.dialog.title", number = number), lines, hints)
            }
            MergePrStage::Confirm { method, rows } => {
                let mut lines = vec![Line::from(
                    t!(
                        "app.merge.dialog.confirm_summary",
                        number = number,
                        method = method.display()
                    )
                    .into_owned(),
                )];
                let mut deleting = rows.iter().filter(|&&(_, checked)| checked).peekable();
                if deleting.peek().is_none() {
                    lines.push(Line::from(
                        t!("app.merge.dialog.keep_branches").into_owned(),
                    ));
                }
                lines.extend(deleting.map(|&(opt, _)| {
                    Line::from(format!(
                        "{}{}",
                        t!("app.merge.dialog.delete_prefix"),
                        row_label(opt)
                    ))
                }));
                let hints = vec![
                    h(&[UserEvent::Confirm], t!("common.hint.merge_pr")),
                    h(&[UserEvent::Cancel], t!("common.hint.cancel")),
                ];
                (
                    t!("app.merge.dialog.confirm_title", number = number),
                    lines,
                    hints,
                )
            }
        };

        // 上下框線 2 + head_ref 1 + 提示 1 + 內容列
        let width = 50u16.min(area.width.saturating_sub(4));
        let height = (lines.len() as u16 + 4).min(area.height.saturating_sub(2));
        let popup = Rect::new(
            area.x + area.width.saturating_sub(width) / 2,
            area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        );
        f.render_widget(Clear, popup);
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.divider_fg))
            .style(Style::default().bg(theme.bg).fg(fg))
            .padding(Padding::horizontal(1));
        let inner = block.inner(popup);
        f.render_widget(block, popup);

        let [name_area, body_area, hint_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .areas(inner);
        f.render_widget(
            Paragraph::new(Line::from(Span::raw(head_ref.as_str()).fg(fg).bold())),
            name_area,
        );
        f.render_widget(Paragraph::new(lines), body_area);
        f.render_widget(
            Paragraph::new(keybind_hint_line(theme, &self.ctx.keybind, &hints)).centered(),
            hint_area,
        );
    }
}

#[cfg(test)]
mod tests {
    use std::{path::Path, sync::mpsc};

    use ratatui::crossterm::event::KeyModifiers;

    use crate::{
        color::ColorTheme,
        config::{CoreConfig, UiConfig},
        git::CommitHash,
        github::{GhItemKind, PrDraftAction, StateAction},
        graph::GraphStyle,
        keybind::KeyBind,
    };

    use super::*;

    /// 建構測試用 `AppContext`。`keybind` 必須用 `KeyBind::new(None)`（讀
    /// `assets/default-keybind.toml`），不能用 `KeyBind::default()`——那是
    /// 一張空 map，會讓 `self.ctx.keybind.has(&key, _)` 對所有鍵都回 `false`，
    /// 測出來的綠燈/紅燈都是巧合，不是邏輯真的對或錯。以下幾條測試要驗的
    /// 正是「靠預設鍵位撐著的順序依賴」（例如 `f` 綁在 `fetch` 而非沒綁到
    /// 任何東西），換一份空 keybind 根本測不出這些依賴存不存在。
    fn test_ctx() -> Rc<AppContext> {
        Rc::new(AppContext {
            keybind: KeyBind::new(None),
            core_config: CoreConfig::default(),
            ui_config: UiConfig::default(),
            color_theme: ColorTheme::default(),
            graph_style: GraphStyle::default(),
            graph_width: None,
            compact: None,
            update: crate::update::UpdateSettings::default(),
            auto_fetch: crate::auto_fetch::AutoFetchSettings::default(),
            fetch_prune: crate::git::FetchPrune::default(),
            shell_command: Vec::new(),
        })
    }

    fn test_state() -> (StatusLineState, mpsc::Receiver<AppEvent>) {
        let (tx, rx) = Sender::channel_for_test();
        (
            StatusLineState::new(test_ctx(), tx, AutoFetchClock::default()),
            rx,
        )
    }

    fn char_key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::empty())
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    const RED: ratatui::style::Color = ratatui::style::Color::Red;

    fn virtual_bar() -> CursorType {
        CursorType::Virtual("|".into())
    }

    /// 輸入框狀態；`virtual_cursor_fg` 固定成紅色，才能用顏色分辨「這格有沒有被當游標染色」。
    fn input_state(
        cursor_type: CursorType,
        msg: &str,
        cursor: usize,
        transient: Option<&str>,
    ) -> StatusLineState {
        let (tx, _rx) = Sender::channel_for_test();
        let mut ctx = test_ctx();
        let c = Rc::get_mut(&mut ctx).unwrap();
        c.ui_config.cursor_type = cursor_type;
        c.color_theme.virtual_cursor_fg = RED;
        let mut state = StatusLineState::new(ctx, tx, AutoFetchClock::default());
        state.update_input(msg.into(), cursor, transient.map(Into::into));
        state
    }

    /// 畫出狀態列（2 列：上邊框＋輸入列），回傳 buffer。
    fn render_input(state: &StatusLineState, width: u16) -> ratatui::buffer::Buffer {
        use ratatui::{backend::TestBackend, Terminal};

        let mut terminal = Terminal::new(TestBackend::new(width, 2)).unwrap();
        terminal
            .draw(|f| state.render(f, f.area(), &View::Default, "", FullStatus::Idle))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn input_row(buf: &ratatui::buffer::Buffer) -> String {
        (0..buf.area.width).map(|x| buf[(x, 1)].symbol()).collect()
    }

    fn red_cells(buf: &ratatui::buffer::Buffer) -> Vec<u16> {
        (0..buf.area.width)
            .filter(|&x| buf[(x, 1)].fg == RED)
            .collect()
    }

    #[test]
    fn input_display_virtual_inserts_cursor_without_dropping_chars() {
        let (before, display) = input_display("/abc", 2, &virtual_bar());
        assert_eq!(before, "/a");
        assert_eq!(display, "/a|bc");

        let (_, display) = input_display("/abc", 4, &virtual_bar());
        assert_eq!(display, "/abc|");

        let (before, display) = input_display("/界a", 4, &virtual_bar());
        assert_eq!(before, "/界");
        assert_eq!(display, "/界|a");
    }

    #[test]
    fn input_display_native_keeps_message_untouched() {
        let (before, display) = input_display("/abc", 2, &CursorType::Native);
        assert_eq!(before, "/a");
        assert_eq!(display, "/abc");
    }

    /// 切點落在多 byte 字元中間時退回尾端，不 panic。
    #[test]
    fn input_display_falls_back_when_cursor_is_not_on_char_boundary() {
        let (before, display) = input_display("/界", 2, &virtual_bar());
        assert_eq!(before, "/界");
        assert_eq!(display, "/界|");

        let (before, _) = input_display("/ab", 99, &virtual_bar());
        assert_eq!(before, "/ab");
    }

    /// 回歸 #198：游標在中間時，它底下的字元不能被蓋掉。
    #[test]
    fn virtual_cursor_renders_between_chars_and_is_colored() {
        let state = input_state(virtual_bar(), "/abc", 2, None);
        let buf = render_input(&state, 20);

        let row = input_row(&buf);
        assert!(row.contains("/a|bc"), "{row}");
        assert_eq!(red_cells(&buf), vec![3]); // 左 padding 1 + "/a" 2 = x 3
    }

    /// 長 query、游標在中間、終端太窄：游標後的內容仍要以截斷形式看得到，
    /// 不能像拆 span 那樣整段消失。
    #[test]
    fn virtual_cursor_in_long_query_keeps_text_after_cursor_visible() {
        let state = input_state(virtual_bar(), "/abcdefghijklmnopqrstuvwxyz", 3, None);
        let buf = render_input(&state, 14);
        let row = input_row(&buf);

        assert!(row.contains("/ab|cdef"), "{row}");
        assert!(row.contains('…'), "{row}");
        assert_eq!(red_cells(&buf).len(), 1);
    }

    /// 游標在被截掉的尾端：該格已是 `…`，不能被染成游標色。
    #[test]
    fn virtual_cursor_truncated_away_colors_nothing() {
        let state = input_state(virtual_bar(), "/abcdefghijklmnopqrstuvwxyz", 27, None);
        let buf = render_input(&state, 14);

        let row = input_row(&buf);
        assert!(row.contains('…'), "{row}");
        assert!(red_cells(&buf).is_empty());
    }

    /// 游標多佔一格後，右側 transient 訊息仍要貼齊右緣、不被擠掉。
    #[test]
    fn virtual_cursor_keeps_transient_message_right_aligned() {
        let state = input_state(virtual_bar(), "/ab", 1, Some("OK"));
        let buf = render_input(&state, 20);
        let row = input_row(&buf);

        assert!(row.contains("/|ab"), "{row}");
        assert!(row.trim_end().ends_with("OK"), "{row}");
        assert_eq!(buf[(17, 1)].symbol(), "O"); // 右 padding 1 → 內容到 x 18
    }

    /// Native 游標只定位、不改字串。
    #[test]
    fn native_cursor_does_not_alter_rendered_text() {
        let state = input_state(CursorType::Native, "/abc", 2, None);
        let buf = render_input(&state, 20);

        let row = input_row(&buf);
        assert!(row.contains("/abc"), "{row}");
        assert!(red_cells(&buf).is_empty());
    }

    #[test]
    fn ref_picker_selects_and_sends_copy_to_clipboard() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::RefPicker {
            options: vec!["main".into(), "dev".into()],
            kind: RefCopyKind::Local,
        };

        state.handle_ref_picker_key(char_key('2'));

        assert!(matches!(state.line, StatusLine::None));
        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::CopyToClipboard { name, value })
                if name == RefCopyKind::Local.copy_label() && value == "dev"
        ));
    }

    #[test]
    fn ref_picker_cancel_clears_without_sending() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::RefPicker {
            options: vec!["main".into()],
            kind: RefCopyKind::Local,
        };

        state.handle_ref_picker_key(key(KeyCode::Esc));

        assert!(matches!(state.line, StatusLine::None));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn ref_picker_out_of_range_digit_is_ignored() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::RefPicker {
            options: vec!["main".into()],
            kind: RefCopyKind::Local,
        };

        state.handle_ref_picker_key(char_key('9'));

        assert!(matches!(state.line, StatusLine::RefPicker { .. }));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn checkout_picker_selects_and_sends_checkout_commit() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::CheckoutPicker {
            options: vec!["v1.0".into(), "v2.0".into()],
            kind: CheckoutPickKind::Tag,
        };

        state.handle_checkout_picker_key(char_key('1'));

        assert!(matches!(state.line, StatusLine::None));
        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::CheckoutCommit { target }) if target == "v1.0"
        ));
    }

    #[test]
    fn child_picker_selects_and_sends_select_child_commit_by_hash() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::ChildPicker {
            options: vec![
                ChildPickOption {
                    label: "feature/x: fix a".into(),
                    commit_hash: CommitHash::from("aaa"),
                },
                ChildPickOption {
                    label: "fix b".into(),
                    commit_hash: CommitHash::from("bbb"),
                },
            ],
            total: 2,
        };

        state.handle_child_picker_key(char_key('2'));

        assert!(matches!(state.line, StatusLine::None));
        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::SelectChildCommitByHash { hash }) if hash.as_str() == "bbb"
        ));
    }

    #[test]
    fn child_picker_cancel_clears_without_sending() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::ChildPicker {
            options: vec![ChildPickOption {
                label: "fix a".into(),
                commit_hash: CommitHash::from("aaa"),
            }],
            total: 1,
        };

        state.handle_child_picker_key(key(KeyCode::Esc));

        assert!(matches!(state.line, StatusLine::None));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn open_child_picker_truncates_to_nine_but_keeps_total() {
        let (mut state, _rx) = test_state();
        let options: Vec<ChildPickOption> = (0..12)
            .map(|i| ChildPickOption {
                label: format!("child {i}"),
                commit_hash: CommitHash::from(format!("hash{i}").as_str()),
            })
            .collect();

        state.open_child_picker(options);

        let StatusLine::ChildPicker { options, total } = &state.line else {
            panic!("expected ChildPicker, got {:?}", state.line);
        };
        assert_eq!(options.len(), 9);
        assert_eq!(*total, 12);
    }

    #[test]
    fn delete_branch_picker_selection_transitions_to_confirm() {
        let (mut state, _rx) = test_state();
        state.line = StatusLine::DeleteBranchPicker {
            options: vec!["feature/x".into()],
            total: 1,
        };

        state.handle_delete_branch_picker_key(char_key('1'));

        assert!(matches!(
            state.line,
            StatusLine::DeleteBranchConfirm { ref name } if name == "feature/x"
        ));
    }

    #[test]
    fn delete_branch_confirm_y_sends_non_force_request() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::DeleteBranchConfirm {
            name: "feature/x".into(),
        };

        state.handle_delete_branch_confirm_key(char_key('y'));

        assert!(matches!(state.line, StatusLine::None));
        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::DeleteBranchRequested { name, force: false }) if name == "feature/x"
        ));
    }

    /// `f` 綁在 `fetch`（assets/default-keybind.toml），不是沒綁到任何東西。
    /// `handle_delete_branch_confirm_key` 靠 `_ if key.code == 'f'|'F'` 這個
    /// guard arm、而不是靠 keybind 查出的 `UserEvent` 接住強制刪除，這條
    /// 順序依賴只有真 keybind 才驗得到（見 test_ctx 的說明）。
    #[test]
    fn delete_branch_confirm_f_sends_force_request_despite_being_bound_to_fetch() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::DeleteBranchConfirm {
            name: "feature/x".into(),
        };

        state.handle_delete_branch_confirm_key(char_key('f'));

        assert!(matches!(state.line, StatusLine::None));
        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::DeleteBranchRequested { name, force: true }) if name == "feature/x"
        ));
    }

    #[test]
    fn delete_branch_confirm_n_clears_without_sending() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::DeleteBranchConfirm {
            name: "feature/x".into(),
        };

        state.handle_delete_branch_confirm_key(char_key('n'));

        assert!(matches!(state.line, StatusLine::None));
        assert!(rx.try_recv().is_err());
    }

    fn merge_pr_prompt() -> StatusLine {
        merge_pr_prompt_with_deletable(true, Some(false), Some("origin/feature/x"))
    }

    fn merge_pr_prompt_with_deletable(
        remote_deletable: bool,
        local_delete: Option<bool>,
        tracking_ref: Option<&str>,
    ) -> StatusLine {
        merge_pr_prompt_line(
            42,
            "feature/x".into(),
            StateFilter::Open,
            remote_deletable,
            local_delete,
            tracking_ref.map(String::from),
        )
    }

    /// 取出目前表單的 (method, rows, cursor)。
    fn merge_form(state: &StatusLineState) -> (MergeMethod, Vec<(MergeDelete, bool)>, usize) {
        match &state.line {
            StatusLine::MergePrPrompt {
                stage:
                    MergePrStage::Form {
                        method,
                        rows,
                        cursor,
                    },
                ..
            } => (*method, rows.clone(), *cursor),
            other => panic!("不在表單：{other:?}"),
        }
    }

    fn is_merge_confirm(state: &StatusLineState) -> bool {
        matches!(
            state.line,
            StatusLine::MergePrPrompt {
                stage: MergePrStage::Confirm { .. },
                ..
            }
        )
    }

    /// 預設：squash、本機 ✔、遠端 ✔、追蹤 ref ✘，順序 Local → Remote → Tracking。
    #[test]
    fn merge_pr_form_defaults() {
        let (mut state, _rx) = test_state();
        state.line = merge_pr_prompt();

        let (method, rows, cursor) = merge_form(&state);
        assert_eq!(method.display(), "squash");
        assert_eq!(
            rows,
            vec![
                (MergeDelete::Local, true),
                (MergeDelete::Remote, true),
                (MergeDelete::Tracking, false),
            ]
        );
        assert_eq!(cursor, 0);
    }

    /// 不可刪的項目整列不出現：本機找不到、遠端不可刪、沒有追蹤 ref。
    #[test]
    fn merge_pr_form_omits_rows_that_cannot_be_deleted() {
        let (mut state, _rx) = test_state();

        state.line = merge_pr_prompt_with_deletable(false, None, None);
        assert!(merge_form(&state).1.is_empty());

        state.line = merge_pr_prompt_with_deletable(true, None, None);
        assert_eq!(merge_form(&state).1, vec![(MergeDelete::Remote, true)]);

        state.line = merge_pr_prompt_with_deletable(false, Some(false), Some("origin/feature/x"));
        assert_eq!(
            merge_form(&state).1,
            vec![(MergeDelete::Local, true), (MergeDelete::Tracking, false)]
        );
    }

    /// 方式列 ←／→ 循環（含繞回）；游標不在方式列時不動 method。
    #[test]
    fn merge_pr_form_cycles_method_on_first_row() {
        let (mut state, _rx) = test_state();
        state.line = merge_pr_prompt();

        state.handle_merge_pr_prompt_key(key(KeyCode::Right)); // squash → rebase
        assert_eq!(merge_form(&state).0.display(), "rebase");
        state.handle_merge_pr_prompt_key(key(KeyCode::Right)); // rebase → merge
        assert_eq!(merge_form(&state).0.display(), "merge");
        state.handle_merge_pr_prompt_key(key(KeyCode::Left)); // merge → rebase
        assert_eq!(merge_form(&state).0.display(), "rebase");

        state.handle_merge_pr_prompt_key(key(KeyCode::Down));
        state.handle_merge_pr_prompt_key(key(KeyCode::Right));
        assert_eq!(merge_form(&state).0.display(), "rebase");
    }

    /// ↑／↓ 循環移動；勾選列 ←／→ 切換該列。
    #[test]
    fn merge_pr_form_moves_cursor_and_toggles_rows() {
        let (mut state, _rx) = test_state();
        state.line = merge_pr_prompt();

        state.handle_merge_pr_prompt_key(key(KeyCode::Up)); // 0 → 最後一列
        assert_eq!(merge_form(&state).2, 3);
        state.handle_merge_pr_prompt_key(key(KeyCode::Right)); // 切換 Tracking
        assert_eq!(merge_form(&state).1[2], (MergeDelete::Tracking, true));
        state.handle_merge_pr_prompt_key(key(KeyCode::Left));
        assert_eq!(merge_form(&state).1[2], (MergeDelete::Tracking, false));
        state.handle_merge_pr_prompt_key(key(KeyCode::Down)); // 最後一列 → 方式列
        assert_eq!(merge_form(&state).2, 0);
    }

    /// 沒有任何可刪項目時只剩方式列：上下移動留在 0，左右只切方式，不 panic。
    #[test]
    fn merge_pr_form_with_only_method_row_does_not_panic() {
        let (mut state, _rx) = test_state();
        state.line = merge_pr_prompt_with_deletable(false, None, None);

        state.handle_merge_pr_prompt_key(key(KeyCode::Down));
        state.handle_merge_pr_prompt_key(key(KeyCode::Up));
        state.handle_merge_pr_prompt_key(key(KeyCode::Right));

        let (method, rows, cursor) = merge_form(&state);
        assert_eq!(method.display(), "rebase");
        assert!(rows.is_empty());
        assert_eq!(cursor, 0);
    }

    /// 表單 Enter 只進確認彈窗，不送事件；確認彈窗 Enter 才送出，欄位與勾選一致。
    #[test]
    fn merge_pr_form_then_confirm_sends_request_with_checked_rows() {
        let (mut state, rx) = test_state();
        state.line = merge_pr_prompt();

        state.handle_merge_pr_prompt_key(key(KeyCode::Enter));
        assert!(is_merge_confirm(&state));
        assert!(rx.try_recv().is_err());

        state.handle_merge_pr_prompt_key(key(KeyCode::Enter));
        assert!(matches!(state.line, StatusLine::None));
        // 預設：本機（force_safe 原樣帶出 `Some(false)` → false）＋遠端，不含追蹤 ref。
        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::MergePrRequested {
                number: 42,
                method: MergeMethod::Squash,
                delete_local_branch: Some((local, false)),
                delete_remote_branch: Some(remote),
                delete_tracking_ref: None,
                ..
            }) if local == "feature/x" && remote == "feature/x"
        ));
    }

    /// 改選方式與勾選後送出：追蹤 ref 原樣帶出，取消勾選的本機分支為 `None`。
    #[test]
    fn merge_pr_request_reflects_edited_form() {
        let (mut state, rx) = test_state();
        state.line = merge_pr_prompt();

        state.handle_merge_pr_prompt_key(key(KeyCode::Left)); // squash → merge
        state.handle_merge_pr_prompt_key(key(KeyCode::Down)); // Local
        state.handle_merge_pr_prompt_key(key(KeyCode::Right)); // 取消勾選
        state.handle_merge_pr_prompt_key(key(KeyCode::Down)); // Remote
        state.handle_merge_pr_prompt_key(key(KeyCode::Down)); // Tracking
        state.handle_merge_pr_prompt_key(key(KeyCode::Right)); // 勾選
        state.handle_merge_pr_prompt_key(key(KeyCode::Enter));
        state.handle_merge_pr_prompt_key(key(KeyCode::Enter));

        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::MergePrRequested {
                method: MergeMethod::Merge,
                delete_local_branch: None,
                delete_remote_branch: Some(_),
                delete_tracking_ref: Some(t),
                ..
            }) if t == "origin/feature/x"
        ));
    }

    /// 本機分支需強刪（`local_delete: Some(true)`）時，force_safe 要原樣傳出去。
    #[test]
    fn merge_pr_request_passes_force_safe_through() {
        let (mut state, rx) = test_state();
        state.line = merge_pr_prompt_with_deletable(false, Some(true), None);

        state.handle_merge_pr_prompt_key(key(KeyCode::Enter));
        state.handle_merge_pr_prompt_key(key(KeyCode::Enter));

        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::MergePrRequested {
                delete_local_branch: Some((_, true)),
                delete_remote_branch: None,
                ..
            })
        ));
    }

    #[test]
    fn merge_pr_cancel_in_form_clears_without_sending() {
        let (mut state, rx) = test_state();
        state.line = merge_pr_prompt();

        state.handle_merge_pr_prompt_key(key(KeyCode::Esc));

        assert!(matches!(state.line, StatusLine::None));
        assert!(rx.try_recv().is_err());
    }

    /// 確認彈窗取消 = 整個流程取消（不退回表單），`n` 也是取消。
    #[test]
    fn merge_pr_cancel_in_confirm_clears_without_sending() {
        let (mut state, rx) = test_state();
        for cancel in [key(KeyCode::Esc), char_key('n')] {
            state.line = merge_pr_prompt();
            state.handle_merge_pr_prompt_key(key(KeyCode::Enter));
            assert!(is_merge_confirm(&state));

            state.handle_merge_pr_prompt_key(cancel);

            assert!(matches!(state.line, StatusLine::None));
            assert!(rx.try_recv().is_err());
        }
    }

    /// 兩個彈窗各自畫出該有的內容：表單有方式列與勾選框，確認有摘要。
    #[test]
    fn merge_pr_popups_render_expected_text() {
        use ratatui::{backend::TestBackend, Terminal};

        fn screen(state: &StatusLineState) -> String {
            let mut terminal = Terminal::new(TestBackend::new(70, 20)).unwrap();
            terminal.draw(|f| state.render_popup(f, f.area())).unwrap();
            let buf = terminal.backend().buffer().clone();
            (0..buf.area.height)
                .map(|y| {
                    (0..buf.area.width)
                        .map(|x| buf[(x, y)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        }

        let (mut state, _rx) = test_state();
        state.line = merge_pr_prompt();
        let form = screen(&state);
        assert!(form.contains("< squash >"), "{form}");
        assert!(form.contains("[x]"), "{form}");
        assert!(form.contains("[ ]"), "{form}");

        state.handle_merge_pr_prompt_key(key(KeyCode::Enter));
        let confirm = screen(&state);
        assert!(confirm.contains("squash"), "{confirm}");
        assert!(!confirm.contains("[x]"), "{confirm}");

        state.line = StatusLine::None;
        assert!(!screen(&state).contains("squash"));
    }

    #[test]
    fn yes_no_prompt_confirm_sends_toggle_state_request() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::ToggleStatePrompt {
            number: 7,
            kind: GhItemKind::Issue,
            action: StateAction::Close,
            filter_state: StateFilter::Open,
        };

        state.handle_yes_no_prompt_key(char_key('y'));

        assert!(matches!(state.line, StatusLine::None));
        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::ToggleItemStateRequested {
                number: 7,
                kind: GhItemKind::Issue,
                action: StateAction::Close,
                ..
            })
        ));
    }

    #[test]
    fn ci_rerun_prompt_only_accepts_options_that_have_runs() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::CiRerunPrompt {
            number: 3,
            failed_runs: vec![],
            all_runs: vec![10, 11],
        };

        // 沒有可重跑的失敗 run：f 無效，prompt 留著
        state.handle_ci_rerun_prompt_key(char_key('f'));
        assert!(matches!(state.line, StatusLine::CiRerunPrompt { .. }));
        assert!(rx.try_recv().is_err());

        state.handle_ci_rerun_prompt_key(char_key('a'));
        assert!(matches!(state.line, StatusLine::None));
        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::CiRerunRequested {
                number: 3,
                scope: RerunScope::All,
                run_ids,
            }) if run_ids == vec![10, 11]
        ));

        state.line = StatusLine::CiRerunPrompt {
            number: 3,
            failed_runs: vec![10],
            all_runs: vec![10, 11],
        };
        state.handle_ci_rerun_prompt_key(char_key('f'));
        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::CiRerunRequested {
                scope: RerunScope::FailedOnly,
                run_ids,
                ..
            }) if run_ids == vec![10]
        ));
    }

    #[test]
    fn yes_no_prompt_confirm_sends_toggle_pr_draft_request() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::TogglePrDraftPrompt {
            number: 9,
            action: PrDraftAction::MarkReady,
            filter_state: StateFilter::Open,
        };

        state.handle_yes_no_prompt_key(key(KeyCode::Enter));

        assert!(matches!(state.line, StatusLine::None));
        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::TogglePrDraftRequested {
                number: 9,
                action: PrDraftAction::MarkReady,
                ..
            })
        ));
    }

    #[test]
    fn yes_no_prompt_n_clears_without_sending() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::ToggleStatePrompt {
            number: 7,
            kind: GhItemKind::Issue,
            action: StateAction::Close,
            filter_state: StateFilter::Open,
        };

        state.handle_yes_no_prompt_key(char_key('n'));

        assert!(matches!(state.line, StatusLine::None));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn restart_prompt_confirm_sends_restart_request() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::RestartPrompt {
            tag: "v2.6.0".to_string(),
            exe: PathBuf::from("/usr/local/bin/ysgit"),
        };

        state.handle_yes_no_prompt_key(char_key('y'));

        assert!(matches!(state.line, StatusLine::None));
        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::RestartRequested { exe }) if exe == Path::new("/usr/local/bin/ysgit")
        ));
    }

    /// 跟其餘三個 y/n prompt 不同：`RestartPrompt` 取消時更新其實已經做完了，
    /// 靜默清空會讓使用者忘記要手動重啟，所以要留一句提醒——這是整批改動裡
    /// 唯一的特判，沒有這個測試擋著，日後 `/simplify` 很容易把它當成多餘的
    /// 分歧併掉。
    #[test]
    fn restart_prompt_cancel_leaves_a_reminder_instead_of_clearing() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::RestartPrompt {
            tag: "v2.6.0".to_string(),
            exe: PathBuf::from("/usr/local/bin/ysgit"),
        };

        state.handle_yes_no_prompt_key(char_key('n'));

        assert!(matches!(
            &state.line,
            StatusLine::NotificationSuccess(msg) if *msg == update_installed_hint()
        ));
        assert!(rx.try_recv().is_err());
    }

    /// `StatusLine` 全部 18 個變體各自對 `handle_intercepting_key`／
    /// `is_input_mode_variant` 的回傳值，把「兩者故意不同步」這件事從一句
    /// 註解變成有東西擋著的不變式（做法比照這個 session 的 `2fa4064` 先
    /// 例）。6 個 prompt 變體在 handle_intercepting_key 永遠攔截，但
    /// is_input_mode_variant 故意不含它們——見兩個方法各自的文件註解。
    #[test]
    fn intercept_and_input_mode_tables_stay_intentionally_out_of_sync() {
        fn make(line: StatusLine) -> StatusLineState {
            let (tx, _rx) = Sender::channel_for_test();
            StatusLineState {
                line,
                ctx: test_ctx(),
                tx,
                auto_fetch_clock: AutoFetchClock::default(),
            }
        }

        let cases: Vec<(&str, StatusLine, bool, bool)> = vec![
            ("None", StatusLine::None, false, false),
            (
                "Input",
                StatusLine::Input(String::new(), 0, None),
                false,
                true,
            ),
            (
                "RefPicker",
                StatusLine::RefPicker {
                    options: vec![],
                    kind: RefCopyKind::Local,
                },
                true,
                true,
            ),
            (
                "CheckoutPicker",
                StatusLine::CheckoutPicker {
                    options: vec![],
                    kind: CheckoutPickKind::Branch,
                },
                true,
                true,
            ),
            (
                "ChildPicker",
                StatusLine::ChildPicker {
                    options: vec![],
                    total: 0,
                },
                true,
                true,
            ),
            (
                "RelatedPicker",
                StatusLine::RelatedPicker { items: vec![] },
                true,
                true,
            ),
            (
                "DeleteBranchPicker",
                StatusLine::DeleteBranchPicker {
                    options: vec![],
                    total: 0,
                },
                true,
                true,
            ),
            (
                "DeleteBranchConfirm",
                StatusLine::DeleteBranchConfirm {
                    name: String::new(),
                },
                true,
                true,
            ),
            ("MergePrPrompt", merge_pr_prompt(), true, false),
            (
                "CiRerunPrompt",
                StatusLine::CiRerunPrompt {
                    number: 0,
                    failed_runs: vec![],
                    all_runs: vec![],
                },
                true,
                false,
            ),
            (
                "ToggleStatePrompt",
                StatusLine::ToggleStatePrompt {
                    number: 0,
                    kind: GhItemKind::Issue,
                    action: StateAction::Close,
                    filter_state: StateFilter::Open,
                },
                true,
                false,
            ),
            (
                "TogglePrDraftPrompt",
                StatusLine::TogglePrDraftPrompt {
                    number: 0,
                    action: PrDraftAction::MarkReady,
                    filter_state: StateFilter::Open,
                },
                true,
                false,
            ),
            (
                "UpdatePrompt",
                StatusLine::UpdatePrompt { tag: String::new() },
                true,
                false,
            ),
            (
                "RestartPrompt",
                StatusLine::RestartPrompt {
                    tag: String::new(),
                    exe: PathBuf::new(),
                },
                true,
                false,
            ),
            (
                "NotificationInfo",
                StatusLine::NotificationInfo(String::new()),
                false,
                false,
            ),
            (
                "NotificationSuccess",
                StatusLine::NotificationSuccess(String::new()),
                false,
                false,
            ),
            (
                "NotificationWarn",
                StatusLine::NotificationWarn(String::new()),
                false,
                false,
            ),
            (
                "NotificationError",
                StatusLine::NotificationError(String::new()),
                false,
                false,
            ),
        ];

        assert_eq!(
            cases.len(),
            18,
            "StatusLine 有 18 個變體，表格漏列或多列了，先檢查表格本身"
        );

        for (label, line, want_intercept, want_input_mode) in cases {
            // handle_intercepting_key 在 12 個攔截變體上會呼叫對應 handler、
            // 可能改變 self.line 或送事件——這裡只關心它的回傳值，用一個
            // 全新的 state 避免副作用互相汙染。
            let mut for_intercept = make(line.clone());
            let got_intercept = for_intercept.handle_intercepting_key(key(KeyCode::Null));
            assert_eq!(
                got_intercept, want_intercept,
                "{label}: handle_intercepting_key 回傳值不對"
            );

            let for_input_mode = make(line);
            let got_input_mode = for_input_mode.is_input_mode_variant();
            assert_eq!(
                got_input_mode, want_input_mode,
                "{label}: is_input_mode_variant 回傳值不對"
            );
        }
    }

    /// `blocks_swap` 決定 Phase 6 換資料能不能進行。跟
    /// `is_idle_or_notification` 唯一的差異是 `NotificationError` 也算
    /// 「不擋」（重載失敗不該卡住下一次成功結果），`Input`（正在打字的
    /// search／filter）則反過來一定要擋，換資料會重建 `StatusLineState`
    /// 把半打的字串吃掉。
    #[test]
    fn blocks_swap_matches_expected_table() {
        fn make(line: StatusLine) -> StatusLineState {
            let (tx, _rx) = Sender::channel_for_test();
            StatusLineState {
                line,
                ctx: test_ctx(),
                tx,
                auto_fetch_clock: AutoFetchClock::default(),
            }
        }

        let cases: Vec<(&str, StatusLine, bool)> = vec![
            ("None", StatusLine::None, false),
            (
                "NotificationInfo",
                StatusLine::NotificationInfo(String::new()),
                false,
            ),
            (
                "NotificationSuccess",
                StatusLine::NotificationSuccess(String::new()),
                false,
            ),
            (
                "NotificationWarn",
                StatusLine::NotificationWarn(String::new()),
                false,
            ),
            (
                "NotificationError",
                StatusLine::NotificationError(String::new()),
                false,
            ),
            ("Input", StatusLine::Input(String::new(), 0, None), true),
            (
                "RefPicker",
                StatusLine::RefPicker {
                    options: vec![],
                    kind: RefCopyKind::Local,
                },
                true,
            ),
            ("MergePrPrompt", merge_pr_prompt(), true),
        ];

        for (label, line, want_blocks) in cases {
            let got = make(line).blocks_swap();
            assert_eq!(got, want_blocks, "{label}: blocks_swap 回傳值不對");
        }
    }

    /// 換資料時取出通知、重建之後放回去，內容原樣還原；其餘狀態（picker、
    /// prompt、輸入框）不受影響，`take_notification` 回 `None`。
    #[test]
    fn take_and_restore_notification_round_trips() {
        let (mut state, _rx) = test_state();
        state.set_notification_error("boom".to_string());

        let notification = state.take_notification().expect("應該取到通知");
        assert!(matches!(state.line, StatusLine::None));

        state.restore_notification(notification);
        assert!(matches!(&state.line, StatusLine::NotificationError(m) if m == "boom"));
    }

    #[test]
    fn take_notification_on_picker_returns_none_and_leaves_it_untouched() {
        let (mut state, _rx) = test_state();
        state.line = StatusLine::RefPicker {
            options: vec!["main".into()],
            kind: RefCopyKind::Local,
        };

        assert!(state.take_notification().is_none());
        assert!(matches!(state.line, StatusLine::RefPicker { .. }));
    }
}

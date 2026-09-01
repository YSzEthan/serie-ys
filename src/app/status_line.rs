use std::{path::PathBuf, rc::Rc};

use ratatui::{
    crossterm::event::{KeyCode, KeyEvent},
    layout::Rect,
    style::{Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Padding, Paragraph},
    Frame,
};

use crate::{
    config::CursorType,
    event::{
        AppEvent, AutoFetchClock, CheckoutPickKind, RefCopyKind, RelatedItem, Sender, UserEvent,
    },
    github::{GhItemKind, MergeMethod, PrDraftAction, StateAction, StateFilter},
    view::View,
    widget::{
        commit_list::ChildPickOption, h, hint_line, hint_pairs, keybind_hint_line, truncate_line,
        HintSpec,
    },
};

use super::AppContext;

const ESC_CANCEL: &str = "(Esc to cancel)";

/// `RestartPrompt` 取消後留下的提醒；`maybe_open_restart_prompt`（app.rs）
/// 守衛沒過時也是同一句，兩處共用同一份字面值不會漂移。
pub(super) const UPDATE_INSTALLED_HINT: &str = "Updated — restart ysgit to apply";

/// `AppEvent::AutoFetchCompleted` 顯示用的固定文案，唯一生產者是
/// `app.rs` 的處理端；`auto_fetch` 模組不帶 payload，見該事件的文件。
pub(super) const AUTO_FETCH_SUCCESS_MSG: &str = "Auto-fetched new commits";

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
fn confirm_line(prompt: String, confirm_desc: &'static str, ctx: &AppContext) -> Line<'static> {
    let hints = hint_pairs(
        &ctx.keybind,
        &[
            h(&[UserEvent::Confirm], confirm_desc),
            h(&[UserEvent::Cancel], "cancel"),
        ],
    );
    let mut spans: Vec<Span<'static>> = vec![prompt.into()];
    let hint_fg = ctx.color_theme.status_interactive_fg;
    spans.extend(hint_line(&ctx.color_theme, &hints, hint_fg).spans);
    Line::from(spans)
}

fn build_hotkey_hints(view: &View, ctx: &AppContext) -> Line<'static> {
    let hints: Vec<HintSpec> = match view {
        View::List(_) => vec![
            h(&[UserEvent::Search], "search"),
            h(&[UserEvent::Filter], "filter"),
            h(&[UserEvent::IgnoreCaseToggle], "case"),
            h(&[UserEvent::CreateTag], "tag"),
            h(&[UserEvent::RefList], "refs"),
            h(&[UserEvent::RemoteRefsToggle], "remote"),
            h(&[UserEvent::GitHubToggle], "github"),
            h(&[UserEvent::Refresh], "refresh"),
            h(&[UserEvent::HelpToggle], "help"),
        ],
        View::Detail(ref view) => view.status_hints(),
        View::Refs(_) => vec![
            h(&[UserEvent::NavigateDown, UserEvent::NavigateUp], "move"),
            h(&[UserEvent::Checkout], "checkout"),
            h(&[UserEvent::DeleteRef], "delete"),
            h(&[UserEvent::Refresh], "refresh"),
            h(&[UserEvent::HelpToggle], "help"),
            h(&[UserEvent::Cancel], "close"),
        ],
        View::CreateTag(_) | View::DeleteTag(_) | View::DeleteRef(_) => vec![
            h(&[UserEvent::Confirm], "confirm"),
            h(&[UserEvent::Cancel], "cancel"),
        ],
        View::UserCommand(_) => crate::view::user_command::status_hints(),
        // Up/Down／PageUp/PageDown 不經過 `KeyBind`（見
        // `view::shell::resolve` 的文件註解），沒有 `UserEvent` 可以掛，早
        // return 手動補上這兩組固定提示——其餘 view 都走尾端共用的
        // `keybind_hint_line` 入口，不必為了這一個特例把它拆開。
        View::Shell(_) => {
            let mut pairs = hint_pairs(
                &ctx.keybind,
                &[
                    h(&[UserEvent::Confirm], "run"),
                    h(&[UserEvent::Cancel], "close"),
                ],
            );
            pairs.extend([
                ("↑↓".to_string(), "history"),
                ("PgUp/PgDn".to_string(), "scroll"),
            ]);
            return hint_line(&ctx.color_theme, &pairs, ctx.color_theme.help_key_fg);
        }
        View::Help(_) => vec![
            h(&[UserEvent::NavigateDown, UserEvent::NavigateUp], "scroll"),
            h(&[UserEvent::Close], "close"),
        ],
        View::GitHub(ref view) => view.status_hints(),
        View::ReleaseNotes(_) => vec![
            h(&[UserEvent::NavigateDown, UserEvent::NavigateUp], "scroll"),
            h(&[UserEvent::HalfPageDown], "half"),
            h(&[UserEvent::PageDown], "page"),
            h(&[UserEvent::Close], "close"),
        ],
        // 窮舉而非 `_`：新增一個 view 時要在這裡被編譯器叫住，
        // 而不是靜默得到一條空提示列。
        View::Default => vec![],
    };

    keybind_hint_line(&ctx.color_theme, &ctx.keybind, &hints)
}

#[derive(Debug, Clone, Copy)]
enum MergePrStage {
    PickMethod,
    AskDeleteBranch {
        method: MergeMethod,
    },
    Confirm {
        method: MergeMethod,
        delete_branch: bool,
    },
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
    Input(String, Option<u16>, Option<String>),
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
            self.set_notification_info("No related issues".into());
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
    ) {
        self.line = StatusLine::MergePrPrompt {
            number,
            head_ref,
            state,
            stage: MergePrStage::PickMethod,
        };
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

    pub(super) fn clear(&mut self) {
        self.line = StatusLine::None;
    }

    pub(super) fn update_input(
        &mut self,
        msg: String,
        cursor_pos: Option<u16>,
        transient_msg: Option<String>,
    ) {
        self.line = StatusLine::Input(msg, cursor_pos, transient_msg);
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

    /// 回傳 true 表示這是 11 個攔截變體之一（8 個 handler：`ToggleStatePrompt`、
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
    /// `handle_intercepting_key` 涵蓋的變體不同步**：5 個 prompt
    /// （`MergePrPrompt`/`ToggleStatePrompt`/`TogglePrDraftPrompt`/`UpdatePrompt`/
    /// `RestartPrompt`）不在這份清單裡，因為它們在 `handle_intercepting_key`
    /// 永遠回傳 `true`，唯一能繞過的只有 `ForceQuit`，而 `ForceQuit` 分支根本
    /// 不查這個方法。動 `ForceQuit` 分支或動 `handle_intercepting_key` 涵蓋範圍
    /// 時，回來檢查這裡。
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
        if let Some(UserEvent::Cancel) = self.ctx.keybind.get(&key) {
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
        if matches!(
            self.ctx.keybind.get(&key),
            Some(UserEvent::Cancel | UserEvent::DetailPaneToggle)
        ) {
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
        if let Some(UserEvent::Cancel) = self.ctx.keybind.get(&key) {
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
        if let Some(UserEvent::Cancel) = self.ctx.keybind.get(&key) {
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
        if let Some(UserEvent::Cancel) = self.ctx.keybind.get(&key) {
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
        let user_event = self.ctx.keybind.get(&key);
        if matches!(user_event, Some(UserEvent::Cancel)) {
            self.line = StatusLine::None;
            return;
        }
        let StatusLine::DeleteBranchConfirm { name } = &self.line else {
            return;
        };
        let name = name.clone();
        match user_event {
            Some(UserEvent::Confirm) => {
                self.line = StatusLine::None;
                self.tx
                    .send(AppEvent::DeleteBranchRequested { name, force: false });
            }
            _ if matches!(key.code, KeyCode::Char('f') | KeyCode::Char('F')) => {
                self.line = StatusLine::None;
                self.tx
                    .send(AppEvent::DeleteBranchRequested { name, force: true });
            }
            _ => {}
        }
    }

    fn handle_merge_pr_prompt_key(&mut self, key: KeyEvent) {
        let StatusLine::MergePrPrompt {
            number,
            ref head_ref,
            state,
            stage,
        } = self.line
        else {
            return;
        };
        let head_ref = head_ref.clone();

        // 每個 stage 先消化自己的答案鍵（優先於全域 cancel，因為 cancel 預設含 'n'，
        // 會與 AskDeleteBranch 的「no」撞鍵），回傳「下一個 stage」；
        // Confirm 是終點（執行/取消），自行早退。
        let next = match stage {
            MergePrStage::PickMethod => match key.code {
                KeyCode::Char('m') | KeyCode::Char('M') => Some(MergeMethod::Merge),
                KeyCode::Char('s') | KeyCode::Char('S') => Some(MergeMethod::Squash),
                KeyCode::Char('r') | KeyCode::Char('R') => Some(MergeMethod::Rebase),
                _ => None,
            }
            .map(|method| MergePrStage::AskDeleteBranch { method }),

            MergePrStage::AskDeleteBranch { method } => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => Some(true),
                KeyCode::Char('n') | KeyCode::Char('N') => Some(false),
                _ => None,
            }
            .map(|delete_branch| MergePrStage::Confirm {
                method,
                delete_branch,
            }),

            MergePrStage::Confirm {
                method,
                delete_branch,
            } => {
                let is_confirm = matches!(self.ctx.keybind.get(&key), Some(UserEvent::Confirm))
                    || matches!(key.code, KeyCode::Enter);
                if is_confirm {
                    self.line = StatusLine::None;
                    self.tx.send(AppEvent::MergePrRequested {
                        number,
                        state,
                        method,
                        delete_branch,
                    });
                } else if matches!(self.ctx.keybind.get(&key), Some(UserEvent::Cancel)) {
                    self.line = StatusLine::None;
                }
                return;
            }
        };

        if let Some(stage) = next {
            self.line = StatusLine::MergePrPrompt {
                number,
                head_ref,
                state,
                stage,
            };
        } else if matches!(self.ctx.keybind.get(&key), Some(UserEvent::Cancel)) {
            // 這顆鍵不是本階段的答案 → 才輪到全域 cancel
            self.line = StatusLine::None;
        }
    }

    /// 解讀 y/n 確認鍵。cancel 必須先判 — 預設 `cancel = ["esc", "n"]` 含 `n`，
    /// 順序反了「no」就會被當成確認。把這個順序依賴收在這裡，呼叫端不必再記。
    fn yes_no_answer(&self, key: KeyEvent) -> Answer {
        if matches!(self.ctx.keybind.get(&key), Some(UserEvent::Cancel))
            || matches!(key.code, KeyCode::Char('n') | KeyCode::Char('N'))
        {
            return Answer::Cancel;
        }
        if matches!(self.ctx.keybind.get(&key), Some(UserEvent::Confirm))
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
                self.line = StatusLine::NotificationSuccess(UPDATE_INSTALLED_HINT.to_string());
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

    pub(super) fn render(&self, f: &mut Frame, area: Rect, view: &View, numeric_prefix: &str) {
        let mut text: Line = match &self.line {
            StatusLine::None => {
                if numeric_prefix.is_empty() {
                    build_hotkey_hints(view, &self.ctx)
                } else {
                    Line::raw(numeric_prefix).fg(self.ctx.color_theme.status_input_transient_fg)
                }
            }
            StatusLine::Input(msg, _, transient_msg) => {
                let msg_w = console::measure_text_width(msg.as_str());
                if let Some(t_msg) = transient_msg {
                    let t_msg_w = console::measure_text_width(t_msg.as_str());
                    // saturating：窄終端 + 長 transient message 會讓這串減法
                    // underflow，接著 `" ".repeat(天文數字)` 直接吃光記憶體。
                    let pad_w = (area.width as usize)
                        .saturating_sub(msg_w)
                        .saturating_sub(t_msg_w)
                        .saturating_sub(2 /* pad */);
                    Line::from(vec![
                        msg.as_str().fg(self.ctx.color_theme.status_input_fg),
                        " ".repeat(pad_w).into(),
                        t_msg
                            .as_str()
                            .fg(self.ctx.color_theme.status_input_transient_fg),
                    ])
                } else {
                    Line::raw(msg).fg(self.ctx.color_theme.status_input_fg)
                }
            }
            StatusLine::RefPicker { options, kind } => self.render_picker_line(
                kind.picker_prompt(),
                options.iter().map(String::as_str),
                ESC_CANCEL.into(),
            ),
            StatusLine::CheckoutPicker { options, kind } => self.render_picker_line(
                kind.picker_prompt(),
                options.iter().map(String::as_str),
                ESC_CANCEL.into(),
            ),
            StatusLine::ChildPicker { options, total } => {
                let tail = if *total > options.len() {
                    format!("(+{} more)", total - options.len())
                } else {
                    ESC_CANCEL.into()
                };
                self.render_picker_line(
                    "Go to child: ",
                    options.iter().map(|o| o.label.as_str()),
                    tail,
                )
            }
            StatusLine::RelatedPicker { items } => self.render_related_picker_line(items),
            StatusLine::DeleteBranchPicker { options, total } => {
                let tail = if *total > options.len() {
                    format!("(+{} more, use tab view)", total - options.len())
                } else {
                    ESC_CANCEL.into()
                };
                self.render_picker_line("Delete branch: ", options.iter().map(String::as_str), tail)
            }
            StatusLine::DeleteBranchConfirm { name } => {
                let hint_fg = self.ctx.color_theme.status_interactive_fg;
                Line::from(vec![
                    format!("Delete '{name}'? ").into(),
                    "[y]es".fg(hint_fg),
                    " / ".into(),
                    "[n]o".fg(hint_fg),
                    " / ".into(),
                    "[f]orce".fg(hint_fg),
                ])
            }
            StatusLine::MergePrPrompt {
                number,
                head_ref,
                stage,
                ..
            } => self.render_merge_pr_line(*number, head_ref, *stage),
            StatusLine::ToggleStatePrompt {
                number,
                kind,
                action,
                ..
            } => confirm_line(action.prompt(*kind, *number), "confirm", &self.ctx),
            StatusLine::TogglePrDraftPrompt { number, action, .. } => {
                confirm_line(action.prompt(*number), "confirm", &self.ctx)
            }
            StatusLine::UpdatePrompt { tag } => confirm_line(
                format!("v{} → {tag}", env!("CARGO_PKG_VERSION")),
                "update",
                &self.ctx,
            ),
            StatusLine::RestartPrompt { tag, .. } => {
                confirm_line(format!("Updated to {tag}."), "restart", &self.ctx)
            }
            StatusLine::NotificationInfo(msg) => {
                Line::raw(msg).fg(self.ctx.color_theme.status_info_fg)
            }
            StatusLine::NotificationSuccess(msg) => Line::raw(msg)
                .add_modifier(Modifier::BOLD)
                .fg(self.ctx.color_theme.status_success_fg),
            StatusLine::NotificationWarn(msg) => Line::raw(msg)
                .add_modifier(Modifier::BOLD)
                .fg(self.ctx.color_theme.status_warn_fg),
            StatusLine::NotificationError(msg) => Line::raw(format!("ERROR: {msg}"))
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

        let block = Block::default()
            .borders(Borders::TOP)
            .style(Style::default().fg(self.ctx.color_theme.divider_fg))
            .padding(Padding::horizontal(1));

        // 截斷統一在這裡套一次，不分支 —— picker 那幾行才是真正沒有上限的
        // （`render_picker_line` 把每個 branch 名 inline 印出來），提示列反而是
        // 人工策劃過的。可用寬度從 `block.inner()` 拿，不要手算 padding。
        let max_width = block.inner(area).width as usize;
        let paragraph = Paragraph::new(truncate_line(text, max_width)).block(block);
        f.render_widget(paragraph, area);

        if let StatusLine::Input(_, Some(cursor_pos), _) = &self.line {
            let (x, y) = (area.x + cursor_pos + 1, area.y + 1);
            match &self.ctx.ui_config.cursor_type {
                CursorType::Native => {
                    f.set_cursor_position((x, y));
                }
                CursorType::Virtual(cursor) => {
                    let style = Style::default().fg(self.ctx.color_theme.virtual_cursor_fg);
                    f.buffer_mut().set_string(x, y, cursor, style);
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
                spans.push("、".into());
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
        spans.push(ESC_CANCEL.fg(self.ctx.color_theme.status_interactive_fg));
        Line::from(spans)
    }

    /// `labels` 收 `impl Iterator<Item = &'s str>` 而非 `&'s [String]`：
    /// `RefPicker`/`CheckoutPicker`/`DeleteBranchPicker` 的候選本來就是
    /// `String`，`ChildPicker` 的候選是 `ChildPickOption`，只借它的
    /// `label` 欄位——不用為了統一型別多 clone 一份 `Vec<String>`。
    fn render_picker_line<'s>(
        &self,
        prompt: &'s str,
        labels: impl Iterator<Item = &'s str>,
        tail: String,
    ) -> Line<'s> {
        let mut spans: Vec<Span<'s>> = vec![prompt.into()];
        for (i, name) in labels.enumerate() {
            spans.push(format!("[{}]", i + 1).fg(self.ctx.color_theme.status_interactive_fg));
            spans.push(name.into());
            spans.push("  ".into());
        }
        spans.push(tail.fg(self.ctx.color_theme.status_interactive_fg));
        Line::from(spans)
    }

    fn render_merge_pr_line(
        &self,
        number: u64,
        head_ref: &str,
        stage: MergePrStage,
    ) -> Line<'static> {
        let hint_fg = self.ctx.color_theme.status_interactive_fg;
        match stage {
            MergePrStage::PickMethod => Line::from(vec![
                format!("Merge PR #{number} ({head_ref}): ").into(),
                "[m]".fg(hint_fg),
                "erge  ".into(),
                "[s]".fg(hint_fg),
                "quash  ".into(),
                "[r]".fg(hint_fg),
                "ebase  ".into(),
                "(Esc cancel)".fg(hint_fg),
            ]),
            MergePrStage::AskDeleteBranch { method } => Line::from(vec![
                format!(
                    "Delete branch '{head_ref}' after {} merge? ",
                    method.display()
                )
                .into(),
                "[y]es".fg(hint_fg),
                " / ".into(),
                "[n]o".fg(hint_fg),
                "  (Esc cancel)".fg(hint_fg),
            ]),
            MergePrStage::Confirm {
                method,
                delete_branch,
            } => {
                let del = if delete_branch { "yes" } else { "no" };
                let prompt = format!(
                    "Merge #{number} with {}, delete branch: {del}  ",
                    method.display()
                );
                confirm_line(prompt, "execute", &self.ctx)
            }
        }
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
    /// 一張空 map，會讓 `self.ctx.keybind.get(&key)` 對所有鍵都回 `None`，
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
        StatusLine::MergePrPrompt {
            number: 42,
            head_ref: "feature/x".into(),
            state: StateFilter::Open,
            stage: MergePrStage::PickMethod,
        }
    }

    #[test]
    fn merge_pr_prompt_advances_through_all_three_stages_and_sends_request() {
        let (mut state, rx) = test_state();
        state.line = merge_pr_prompt();

        state.handle_merge_pr_prompt_key(char_key('s')); // squash
        assert!(matches!(
            state.line,
            StatusLine::MergePrPrompt {
                stage: MergePrStage::AskDeleteBranch {
                    method: MergeMethod::Squash
                },
                ..
            }
        ));

        // `n` 在這個階段的意思是「不刪分支、推進到下一階段」，不是取消——
        // `n` 也綁在 cancel，這條順序依賴是這組邏輯最容易改壞的地方。
        state.handle_merge_pr_prompt_key(char_key('n'));
        assert!(matches!(
            state.line,
            StatusLine::MergePrPrompt {
                stage: MergePrStage::Confirm {
                    method: MergeMethod::Squash,
                    delete_branch: false,
                },
                ..
            }
        ));

        state.handle_merge_pr_prompt_key(key(KeyCode::Enter));
        assert!(matches!(state.line, StatusLine::None));
        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::MergePrRequested {
                number: 42,
                method: MergeMethod::Squash,
                delete_branch: false,
                ..
            })
        ));
    }

    #[test]
    fn merge_pr_prompt_cancel_mid_flow_clears_without_sending() {
        let (mut state, rx) = test_state();
        state.line = merge_pr_prompt();

        state.handle_merge_pr_prompt_key(key(KeyCode::Esc));

        assert!(matches!(state.line, StatusLine::None));
        assert!(rx.try_recv().is_err());
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
            StatusLine::NotificationSuccess(msg) if msg == UPDATE_INSTALLED_HINT
        ));
        assert!(rx.try_recv().is_err());
    }

    /// `StatusLine` 全部 17 個變體各自對 `handle_intercepting_key`／
    /// `is_input_mode_variant` 的回傳值，把「兩者故意不同步」這件事從一句
    /// 註解變成有東西擋著的不變式（做法比照這個 session 的 `2fa4064` 先
    /// 例）。5 個 prompt 變體在 handle_intercepting_key 永遠攔截，但
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
                StatusLine::Input(String::new(), None, None),
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
            17,
            "StatusLine 有 17 個變體，表格漏列或多列了，先檢查表格本身"
        );

        for (label, line, want_intercept, want_input_mode) in cases {
            // handle_intercepting_key 在 11 個攔截變體上會呼叫對應 handler、
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
}

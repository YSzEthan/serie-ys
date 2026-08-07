use std::rc::Rc;

use ratatui::{
    crossterm::event::{KeyCode, KeyEvent},
    layout::Rect,
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Padding, Paragraph},
    Frame,
};

use crate::{
    config::CursorType,
    event::{AppEvent, CheckoutPickKind, RefCopyKind, RelatedItem, Sender, UserEvent},
    github::{GhItemKind, MergeMethod, PrDraftAction, StateAction, StateFilter},
    view::View,
};

use super::AppContext;

const ESC_CANCEL: &str = "(Esc to cancel)";

fn picker_digit_index(key: KeyEvent) -> Option<usize> {
    let KeyCode::Char(c) = key.code else {
        return None;
    };
    let digit = c.to_digit(10)?;
    (digit as usize).checked_sub(1)
}

/// 單階段 y/n 確認的狀態列，與 [`StatusLineState::yes_no_answer`] 接受的鍵一致。
fn confirm_line(prompt: String, hint_fg: Color) -> Line<'static> {
    Line::from(vec![
        prompt.into(),
        "[y/Enter]".fg(hint_fg),
        " confirm  ".into(),
        "[n/Esc]".fg(hint_fg),
        " cancel".into(),
    ])
}

fn build_hotkey_hints(view: &View, ctx: &AppContext) -> Line<'static> {
    let hints: Vec<(UserEvent, &str)> = match view {
        View::List(_) => vec![
            (UserEvent::Search, "search"),
            (UserEvent::Filter, "filter"),
            (UserEvent::IgnoreCaseToggle, "case"),
            (UserEvent::CreateTag, "tag"),
            (UserEvent::RefList, "refs"),
            (UserEvent::RemoteRefsToggle, "remote"),
            (UserEvent::GitHubToggle, "github"),
            (UserEvent::Refresh, "refresh"),
            (UserEvent::HelpToggle, "help"),
        ],
        View::Detail(_) => vec![
            (UserEvent::ShortCopy, "copy"),
            (UserEvent::Close, "close"),
            (UserEvent::HelpToggle, "help"),
        ],
        View::Refs(_) => vec![
            (UserEvent::Checkout, "checkout"),
            (UserEvent::DeleteRef, "delete"),
            (UserEvent::Cancel, "close"),
            (UserEvent::HelpToggle, "help"),
        ],
        View::CreateTag(_) | View::DeleteTag(_) | View::DeleteRef(_) => vec![
            (UserEvent::Confirm, "confirm"),
            (UserEvent::Cancel, "cancel"),
        ],
        View::Help(_) => vec![(UserEvent::Close, "close")],
        View::GitHub(ref view) => view.status_hints(),
        _ => vec![],
    };

    let key_fg = ctx.color_theme.help_key_fg;
    let desc_fg = ctx.color_theme.status_input_transient_fg;

    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, (event, desc)) in hints.iter().enumerate() {
        if let Some(key) = ctx.keybind.keys_for_event(*event).first() {
            if i > 0 {
                spans.push(Span::raw("  "));
            }
            spans.push(Span::styled(key.clone(), Style::default().fg(key_fg)));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                (*desc).to_string(),
                Style::default().fg(desc_fg),
            ));
        }
    }
    Line::from(spans)
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
    RelatedPicker {
        items: Vec<RelatedItem>,
    },
    NotificationInfo(String),
    NotificationSuccess(String),
    NotificationWarn(String),
    NotificationError(String),
    /// 尾端是 OSC 8 超連結的通知。渲染時必須繞過 `Line::raw`——
    /// 它會把跳脫序列逐 byte 拆到不同 cell——改成用
    /// `set_symbol` + 後續 cell 的 `set_skip` 把整包 payload 塞進單一 cell。
    NotificationHyperlink {
        prefix: &'static str,
        label: String,
        url: String,
    },
}

#[derive(Debug)]
pub(super) struct StatusLineState {
    line: StatusLine,
    ctx: Rc<AppContext>,
    tx: Sender,
}

impl StatusLineState {
    pub(super) fn new(ctx: Rc<AppContext>, tx: Sender) -> Self {
        Self {
            line: StatusLine::default(),
            ctx,
            tx,
        }
    }

    pub(super) fn open_ref_picker(&mut self, options: Vec<String>, kind: RefCopyKind) {
        self.line = StatusLine::RefPicker { options, kind };
    }

    pub(super) fn open_checkout_picker(&mut self, options: Vec<String>, kind: CheckoutPickKind) {
        self.line = StatusLine::CheckoutPicker { options, kind };
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

    pub(super) fn set_notification_hyperlink(
        &mut self,
        prefix: &'static str,
        label: String,
        url: String,
    ) {
        self.line = StatusLine::NotificationHyperlink { prefix, label, url };
    }

    /// 回傳 true 表示這是 8 個攔截變體之一（7 個 handler，`ToggleStatePrompt`
    /// 與 `TogglePrDraftPrompt` 共用 `handle_yes_no_prompt_key`）、鍵已被吃掉。
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
            StatusLine::ToggleStatePrompt { .. } | StatusLine::TogglePrDraftPrompt { .. } => {
                self.handle_yes_no_prompt_key(key);
                true
            }
            StatusLine::None
            | StatusLine::Input(_, _, _)
            | StatusLine::NotificationInfo(_)
            | StatusLine::NotificationSuccess(_)
            | StatusLine::NotificationWarn(_)
            | StatusLine::NotificationError(_)
            | StatusLine::NotificationHyperlink { .. } => false,
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
            | StatusLine::RelatedPicker { .. }
            | StatusLine::DeleteBranchPicker { .. }
            | StatusLine::DeleteBranchConfirm { .. }
            | StatusLine::MergePrPrompt { .. }
            | StatusLine::ToggleStatePrompt { .. }
            | StatusLine::TogglePrDraftPrompt { .. } => false,
            StatusLine::NotificationInfo(_)
            | StatusLine::NotificationSuccess(_)
            | StatusLine::NotificationWarn(_)
            | StatusLine::NotificationHyperlink { .. } => {
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
    /// `handle_intercepting_key` 涵蓋的變體不同步**：3 個 prompt
    /// （`MergePrPrompt`/`ToggleStatePrompt`/`TogglePrDraftPrompt`）不在這份
    /// 清單裡，因為它們在 `handle_intercepting_key` 永遠回傳 `true`，唯一
    /// 能繞過的只有 `ForceQuit`，而 `ForceQuit` 分支根本不查這個方法。動
    /// `ForceQuit` 分支或動 `handle_intercepting_key` 涵蓋範圍時，回來檢查
    /// 這裡。
    pub(super) fn is_input_mode_variant(&self) -> bool {
        match self.line {
            StatusLine::Input(_, _, _)
            | StatusLine::RefPicker { .. }
            | StatusLine::CheckoutPicker { .. }
            | StatusLine::RelatedPicker { .. }
            | StatusLine::DeleteBranchPicker { .. }
            | StatusLine::DeleteBranchConfirm { .. } => true,
            StatusLine::None
            | StatusLine::MergePrPrompt { .. }
            | StatusLine::ToggleStatePrompt { .. }
            | StatusLine::TogglePrDraftPrompt { .. }
            | StatusLine::NotificationInfo(_)
            | StatusLine::NotificationSuccess(_)
            | StatusLine::NotificationWarn(_)
            | StatusLine::NotificationError(_)
            | StatusLine::NotificationHyperlink { .. } => false,
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
            _ => {}
        }
    }

    pub(super) fn render(&self, f: &mut Frame, area: Rect, view: &View, numeric_prefix: &str) {
        let text: Line = match &self.line {
            StatusLine::NotificationHyperlink { prefix, label, url } => {
                self.render_hyperlink_notification(f, area, prefix, label, url);
                return;
            }
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
                    let pad_w = area.width as usize - msg_w - t_msg_w - 2 /* pad */;
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
            StatusLine::RefPicker { options, kind } => {
                self.render_picker_line(kind.picker_prompt(), options, ESC_CANCEL.into())
            }
            StatusLine::CheckoutPicker { options, kind } => {
                self.render_picker_line(kind.picker_prompt(), options, ESC_CANCEL.into())
            }
            StatusLine::RelatedPicker { items } => self.render_related_picker_line(items),
            StatusLine::DeleteBranchPicker { options, total } => {
                let tail = if *total > options.len() {
                    format!("(+{} more, use tab view)", total - options.len())
                } else {
                    ESC_CANCEL.into()
                };
                self.render_picker_line("Delete branch: ", options, tail)
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
            } => confirm_line(
                action.prompt(*kind, *number),
                self.ctx.color_theme.status_interactive_fg,
            ),
            StatusLine::TogglePrDraftPrompt { number, action, .. } => confirm_line(
                action.prompt(*number),
                self.ctx.color_theme.status_interactive_fg,
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
            StatusLine::NotificationError(msg) => Line::raw(format!("ERROR: {msg}"))
                .add_modifier(Modifier::BOLD)
                .fg(self.ctx.color_theme.status_error_fg),
        };
        let paragraph = Paragraph::new(text).block(
            Block::default()
                .borders(Borders::TOP)
                .style(Style::default().fg(self.ctx.color_theme.divider_fg))
                .padding(Padding::horizontal(1)),
        );
        f.render_widget(paragraph, area);

        if let StatusLine::Input(_, Some(cursor_pos), _) = &self.line {
            let (x, y) = (area.x + cursor_pos + 1, area.y + 1);
            match &self.ctx.ui_config.common.cursor_type {
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

    fn render_hyperlink_notification(
        &self,
        f: &mut Frame,
        area: Rect,
        prefix: &str,
        label: &str,
        url: &str,
    ) {
        let block = Block::default()
            .borders(Borders::TOP)
            .style(Style::default().fg(self.ctx.color_theme.divider_fg))
            .padding(Padding::horizontal(1));
        let inner = block.inner(area);
        f.render_widget(block, area);

        let buf = f.buffer_mut();
        let style = Style::default().fg(self.ctx.color_theme.status_info_fg);
        buf.set_string(inner.left(), inner.top(), prefix, style);
        let prefix_w = console::measure_text_width(prefix) as u16;
        let x0 = inner.left().saturating_add(prefix_w);
        if x0 >= inner.right() {
            return;
        }
        let payload = crate::external::format_osc8_hyperlink(url, label);
        let label_width = console::measure_text_width(label) as u16;
        buf[(x0, inner.top())].set_symbol(&payload).set_style(style);
        let remaining = inner.right().saturating_sub(x0);
        for i in 1..label_width.min(remaining) {
            buf[(x0 + i, inner.top())].set_skip(true);
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

    fn render_picker_line<'s>(
        &self,
        prompt: &'s str,
        options: &'s [String],
        tail: String,
    ) -> Line<'s> {
        let mut spans: Vec<Span<'s>> = vec![prompt.into()];
        for (i, name) in options.iter().enumerate() {
            spans.push(format!("[{}]", i + 1).fg(self.ctx.color_theme.status_interactive_fg));
            spans.push(name.as_str().into());
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
                Line::from(vec![
                    format!(
                        "Merge #{number} with {}, delete branch: {del}  ",
                        method.display()
                    )
                    .into(),
                    "[y/Enter]".fg(hint_fg),
                    " execute  ".into(),
                    "[Esc]".fg(hint_fg),
                    " cancel".into(),
                ])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use ratatui::crossterm::event::KeyModifiers;

    use crate::{
        color::ColorTheme,
        config::{CoreConfig, UiConfig},
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
        })
    }

    fn test_state() -> (StatusLineState, mpsc::Receiver<AppEvent>) {
        let (tx, rx) = Sender::channel_for_test();
        (StatusLineState::new(test_ctx(), tx), rx)
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

    /// `StatusLine` 全部 15 個變體各自對 `handle_intercepting_key`／
    /// `is_input_mode_variant` 的回傳值，把「兩者故意不同步」這件事從一句
    /// 註解變成有東西擋著的不變式（做法比照這個 session 的 `2fa4064` 先
    /// 例）。3 個 prompt 變體在 handle_intercepting_key 永遠攔截，但
    /// is_input_mode_variant 故意不含它們——見兩個方法各自的文件註解。
    #[test]
    fn intercept_and_input_mode_tables_stay_intentionally_out_of_sync() {
        fn make(line: StatusLine) -> StatusLineState {
            let (tx, _rx) = Sender::channel_for_test();
            StatusLineState {
                line,
                ctx: test_ctx(),
                tx,
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
            (
                "NotificationHyperlink",
                StatusLine::NotificationHyperlink {
                    prefix: "",
                    label: String::new(),
                    url: String::new(),
                },
                false,
                false,
            ),
        ];

        assert_eq!(
            cases.len(),
            15,
            "StatusLine 有 15 個變體，表格漏列或多列了，先檢查表格本身"
        );

        for (label, line, want_intercept, want_input_mode) in cases {
            // handle_intercepting_key 在 8 個攔截變體上會呼叫對應 handler、
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

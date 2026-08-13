use std::{borrow::Cow, collections::BTreeMap};

use ratatui::{
    crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    layout::{Constraint, Layout, Rect},
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, ListItem, ListState, Paragraph},
    DefaultTerminal, Frame,
};

use unicode_width::UnicodeWidthStr;

use crate::{
    color::ColorTheme,
    event::UserEvent,
    keybind::{key_event_to_config_string, KeyBind},
};

use super::{inline_string_array, ConfigKey, Draft, ResolvedDefaults, KEYBIND};

pub(crate) enum Flow {
    Continue,
    /// Esc／←／h：回主選單。跟 `color_editor::Flow::Back` 同一個位階。
    Back,
}

#[derive(Clone, Copy)]
enum Mode {
    Replace,
    Append,
}

enum CaptureState {
    Waiting,
    /// `key_event_to_config_string` 回 `None`：這顆鍵寫不進設定檔，留在
    /// 捕捉畫面顯示原因，不返回清單。
    Rejected(&'static str),
    /// 捕捉到的鍵目前歸別的 action。`y` 搶過來，其他鍵取消整次捕捉。
    Conflict {
        key: KeyEvent,
        victim: UserEvent,
    },
}

struct Capture {
    target: UserEvent,
    mode: Mode,
    state: CaptureState,
}

/// `list`（瀏覽 + 捕捉二選一）就是全部合法畫面，跟 `color_editor` 同一個
/// 「沒有 `Screen` enum」理由。
///
/// 唯一可變狀態是 `overrides`：本次 session 對 `[keybind]` 的覆寫，跟
/// `KeyBind::assign`／`unset` 同構（`Some(keys)` = 取代，`None` = 回到
/// 內建預設）。畫面上該顯示什麼鍵，每次 render 從 `file_patch` + `overrides`
/// 重算一次 `effective()`——不維護第二份「畫面上顯示的樣子」，也不必自己
/// 重寫一套搶鍵演算法：`KeyBind::assign` 本來就會把鍵從其他 action 身上
/// 拿掉，這裡只是它的呼叫端。
pub(crate) struct KeyBindEditorState {
    /// 進編輯器當下，設定檔 `[keybind]` 已經有的內容（未套用本次 session
    /// 的改動）。`current_patch()` 的起點。
    file_patch: KeyBind,
    /// 本次 session 對每個 action 的最終決定；只有出現在這裡的 action 才會
    /// 被寫回 `draft.edits`——沒動過的 action 即使因為「被搶鍵」而效果上
    /// 變了，也不需要在檔案裡留下痕跡（`effective()` 在下次啟動時會用
    /// 完全相同的 `assign` 邏輯重新算出同樣的結果）。
    overrides: BTreeMap<UserEvent, Option<Vec<KeyEvent>>>,
    /// 全部 action，宣告順序：`KeyBind` 內建的順序（＝
    /// `assets/default-keybind.toml` 的檔案順序，即 `UserEvent` 宣告順序）
    /// 後面接設定檔裡實際存在的 `user_command_N`。
    actions: Vec<UserEvent>,
    user_commands: BTreeMap<usize, String>,
    list: ListState,
    capture: Option<Capture>,
    config_is_fallback: bool,
}

fn build_actions(user_commands: &BTreeMap<usize, String>) -> Vec<UserEvent> {
    let mut actions: Vec<UserEvent> = KeyBind::new(None)
        .bindings()
        .iter()
        .map(|(e, _)| *e)
        .collect();
    actions.extend(user_commands.keys().map(|n| UserEvent::UserCommand(*n)));
    actions
}

/// 跟畫面無關的單一描述。`UserEvent::description()` 涵蓋固定文字的 action；
/// `UserCommand(n)` 沒有固定文字，改查 `user_commands`（來自
/// `[user_command.commands]`）取使用者自己取的名稱。
fn describe(action: UserEvent, user_commands: &BTreeMap<usize, String>) -> Cow<'static, str> {
    match action {
        UserEvent::UserCommand(n) => match user_commands.get(&n) {
            Some(name) => Cow::Owned(format!("執行 user command：{name}")),
            None => Cow::Borrowed("（設定檔裡找不到這個 user command）"),
        },
        _ => Cow::Borrowed(action.description().unwrap_or_default()),
    }
}

impl KeyBindEditorState {
    pub fn new(defaults: &ResolvedDefaults) -> Self {
        let mut list = ListState::default();
        list.select(Some(0));
        Self {
            file_patch: defaults.keybind_patch.clone(),
            overrides: BTreeMap::new(),
            actions: build_actions(&defaults.user_commands),
            user_commands: defaults.user_commands.clone(),
            list,
            capture: None,
            config_is_fallback: defaults.config_is_fallback,
        }
    }

    /// 本次 session 到目前為止「會被寫進檔案」的 `[keybind]` 內容：檔案原有
    /// 的 patch 疊上 `overrides`。只包含明確設定過的 action，不含純粹沿用
    /// 內建預設的 action——這正是它跟 `effective()` 的差別。
    fn current_patch(&self) -> KeyBind {
        let mut patch = self.file_patch.clone();
        for (event, keys) in &self.overrides {
            match keys {
                Some(k) => patch.assign(*event, k.clone()),
                None => patch.unset(*event),
            }
        }
        patch
    }

    /// 畫面上、以及套用後實際生效的完整鍵位（內建預設 + `current_patch`）。
    fn effective(&self) -> KeyBind {
        KeyBind::new(Some(self.current_patch()))
    }

    fn selected(&self) -> UserEvent {
        self.actions[self.list.selected().unwrap_or(0)]
    }

    /// 零 I/O 的純狀態轉移，可以直接餵 `KeyEvent` 測試——跟
    /// `color_editor::ColorEditorState::on_key` 同一個模式。
    pub fn on_key(&mut self, key: KeyEvent, draft: &mut Draft) -> Flow {
        if key.kind != KeyEventKind::Press {
            return Flow::Continue;
        }

        if self.capture.is_some() {
            return self.on_capture_key(key, draft);
        }

        if super::is_abort_key(&key) {
            return Flow::Back;
        }

        match key.code {
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => Flow::Back,
            KeyCode::Up | KeyCode::Char('k') => {
                super::clamped_move(&mut self.list, -1, self.actions.len());
                Flow::Continue
            }
            KeyCode::Down | KeyCode::Char('j') => {
                super::clamped_move(&mut self.list, 1, self.actions.len());
                Flow::Continue
            }
            KeyCode::PageUp => {
                super::clamped_move(&mut self.list, -10, self.actions.len());
                Flow::Continue
            }
            KeyCode::PageDown => {
                super::clamped_move(&mut self.list, 10, self.actions.len());
                Flow::Continue
            }
            KeyCode::Home => {
                self.list.select(Some(0));
                Flow::Continue
            }
            KeyCode::End => {
                self.list.select(Some(self.actions.len() - 1));
                Flow::Continue
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                self.capture = Some(Capture {
                    target: self.selected(),
                    mode: Mode::Replace,
                    state: CaptureState::Waiting,
                });
                Flow::Continue
            }
            KeyCode::Char('a') => {
                self.capture = Some(Capture {
                    target: self.selected(),
                    mode: Mode::Append,
                    state: CaptureState::Waiting,
                });
                Flow::Continue
            }
            KeyCode::Char('x') => {
                let target = self.selected();
                self.overrides.insert(target, Some(Vec::new()));
                self.sync(target, draft);
                Flow::Continue
            }
            KeyCode::Char('r') => {
                let target = self.selected();
                self.overrides.insert(target, None);
                self.sync(target, draft);
                Flow::Continue
            }
            _ => Flow::Continue,
        }
    }

    fn on_capture_key(&mut self, key: KeyEvent, draft: &mut Draft) -> Flow {
        let Some(capture) = &self.capture else {
            return Flow::Continue;
        };
        let target = capture.target;
        let mode = capture.mode;

        if let CaptureState::Conflict { key: pending, .. } = &capture.state {
            let pending = *pending;
            if key.code == KeyCode::Char('y') && key.modifiers.is_empty() {
                let effective = self.effective();
                self.commit_capture(target, mode, pending, &effective, draft);
            } else {
                self.capture = None;
            }
            return Flow::Continue;
        }

        // 取消鍵只有 ctrl-c，不能複用 `is_abort_key`——它把 ctrl-d 也算
        // 進去，而 `half_page_down = ["ctrl-d"]` 是預設綁定，會變成
        // 「唯一綁不到 ctrl-d 的時候就是要重綁 ctrl-d 的時候」。
        if key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL {
            self.capture = None;
            return Flow::Continue;
        }

        // 只留 code／modifiers：捕捉到的原始事件可能帶 kitty protocol 的
        // `state` 旗標，那些不影響「這是哪顆鍵」，留著只會讓同一顆鍵在
        // 不同終端下 round-trip 不回來。
        let normalized = KeyEvent::new(key.code, key.modifiers);

        if key_event_to_config_string(normalized).is_none() {
            if let Some(c) = &mut self.capture {
                c.state = CaptureState::Rejected("這個按鍵無法寫進設定檔");
            }
            return Flow::Continue;
        }

        let effective = self.effective();

        // 追加模式下鍵已經是這個 action 自己的：不是衝突，也不用再寫一次
        // （寫了會在陣列裡產生重複項目，下次啟動時會被當成同一顆鍵綁給
        // 同一個 event 兩次，載入直接失敗）。當作已完成，退出捕捉。
        if matches!(mode, Mode::Append) && effective.key_events(target).contains(&normalized) {
            self.capture = None;
            return Flow::Continue;
        }

        match effective.get(&normalized).copied() {
            Some(owner) if owner != target => {
                if let Some(c) = &mut self.capture {
                    c.state = CaptureState::Conflict {
                        key: normalized,
                        victim: owner,
                    };
                }
            }
            _ => self.commit_capture(target, mode, normalized, &effective, draft),
        }
        Flow::Continue
    }

    /// 捕捉完成：算出 `target` 的新陣列、指派、寫回 `overrides`／`draft`。
    ///
    /// 這顆鍵如果搶自另一個「已經在 `current_patch` 裡有明確 entry」的
    /// action（使用者自己的檔案或這個 session 設過的），那個 action 的
    /// 陣列也要一起更新——不寫的話同一顆鍵在檔案裡會出現兩次，下次啟動
    /// 直接載入失敗。搶自純內建預設的 action 不用寫：`current_patch` 本來
    /// 就不含它的 entry，`assign` 的效果在下次啟動時会由同一套邏輯自動
    /// 重算出來。受害者最多一個——`current_patch` 內部無衝突（不變式），
    /// 一顆鍵在裡面只可能有一個 owner，`current.get(&key)` 直接查得到，
    /// 不需要整份 `bindings()` 快照再逐條 diff。
    fn commit_capture(
        &mut self,
        target: UserEvent,
        mode: Mode,
        key: KeyEvent,
        effective: &KeyBind,
        draft: &mut Draft,
    ) {
        let new_keys = match mode {
            Mode::Replace => vec![key],
            Mode::Append => {
                let mut keys = effective.key_events(target).to_vec();
                keys.push(key);
                keys
            }
        };

        let mut current = self.current_patch();
        let victim = current.get(&key).copied().filter(|v| *v != target);
        current.assign(target, new_keys.clone());

        self.overrides.insert(target, Some(new_keys));
        self.sync(target, draft);

        if let Some(victim) = victim {
            self.overrides
                .insert(victim, Some(current.key_events(victim).to_vec()));
            self.sync(victim, draft);
        }

        self.capture = None;
    }

    /// 把 `overrides[event]` 目前的決定同步進 `draft.edits`——跟
    /// `NumberField::commit` 同一個三態寫法：`Some(keys)` 寫陣列、`None`
    /// 移除該鍵（回到內建預設）。
    fn sync(&mut self, event: UserEvent, draft: &mut Draft) {
        let Some(name) = event.config_name() else {
            return;
        };
        let config_key = ConfigKey {
            table: KEYBIND,
            key: name.into(),
        };
        match self.overrides.get(&event) {
            Some(Some(keys)) => {
                let strings: Vec<String> = keys
                    .iter()
                    .filter_map(|k| key_event_to_config_string(*k))
                    .collect();
                draft
                    .edits
                    .insert(config_key, Some(inline_string_array(&strings)));
            }
            _ => {
                draft.edits.insert(config_key, None);
            }
        }
    }

    fn exit_warning(&self, effective: &KeyBind) -> Option<&'static str> {
        let quit_disabled = effective.key_events(UserEvent::Quit).is_empty();
        let force_quit_disabled = effective.key_events(UserEvent::ForceQuit).is_empty();
        (quit_disabled && force_quit_disabled)
            .then_some("quit 與 force_quit 都沒有綁定鍵，套用後將無法用快捷鍵離開程式")
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect, chrome: &ColorTheme) {
        let effective = self.effective();

        let [list_area, warn_area, hint_area] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);

        let selected = self.list.selected().unwrap_or(0);
        let total = self.actions.len();

        let items: Vec<ListItem> = self
            .actions
            .iter()
            .map(|&action| {
                let touched = self.overrides.contains_key(&action);
                let marker = if touched { "✓ " } else { "  " };
                let config_name = action.config_name().unwrap_or_default();
                let keys = effective.key_events(action);
                let keys_display = if keys.is_empty() {
                    "(未綁定)".to_string()
                } else {
                    keys.iter()
                        .filter_map(|k| key_event_to_config_string(*k))
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                let desc = describe(action, &self.user_commands);
                ListItem::new(Line::from(vec![
                    Span::raw(marker),
                    Span::raw(format!("{config_name:<28}")),
                    Span::styled(
                        format!("{keys_display:<22}"),
                        Style::default().fg(chrome.help_key_fg),
                    ),
                    Span::raw(desc),
                ]))
            })
            .collect();

        let list = super::styled_list(items, chrome).block(
            Block::default()
                .title(format!(" 快捷鍵 [{}/{}] ", selected + 1, total))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(chrome.divider_fg)),
        );
        f.render_stateful_widget(list, list_area, &mut self.list);

        if self.config_is_fallback {
            f.render_widget(
                Paragraph::new(
                    Line::raw("設定檔載入失敗，以下是內建預設").fg(chrome.status_warn_fg),
                ),
                warn_area,
            );
        } else if let Some(msg) = self.exit_warning(&effective) {
            f.render_widget(
                Paragraph::new(Line::raw(msg).fg(chrome.status_warn_fg)),
                warn_area,
            );
        }

        let hint = crate::widget::hint_line(
            chrome,
            &[
                ("↑↓/kj".into(), "移動"),
                ("Enter/l".into(), "取代"),
                ("a".into(), "追加"),
                ("x".into(), "停用"),
                ("r".into(), "還原預設"),
                ("Esc/h".into(), "返回"),
            ],
            chrome.help_key_fg,
        );
        f.render_widget(Paragraph::new(hint), hint_area);

        if let Some(capture) = &self.capture {
            render_capture_dialog(f, area, capture, &effective, &self.user_commands, chrome);
        }
    }
}

fn render_capture_dialog(
    f: &mut Frame,
    area: Rect,
    capture: &Capture,
    effective: &KeyBind,
    user_commands: &BTreeMap<usize, String>,
    chrome: &ColorTheme,
) {
    let config_name = capture.target.config_name().unwrap_or_default();
    let mode_label = match capture.mode {
        Mode::Replace => "取代全部綁定",
        Mode::Append => "追加一顆鍵",
    };
    let current = effective.keys_for_event(capture.target).join(", ");
    let current_line = if current.is_empty() {
        "目前：（未綁定）".to_string()
    } else {
        format!("目前：{current}")
    };

    let message = match &capture.state {
        CaptureState::Waiting => "請按下要綁定的鍵…".to_string(),
        CaptureState::Rejected(msg) => (*msg).to_string(),
        CaptureState::Conflict { key, victim } => {
            let key_str = key_event_to_config_string(*key).unwrap_or_default();
            let victim_name = victim.config_name().unwrap_or_default();
            let victim_desc = describe(*victim, user_commands);
            format!("{key_str} 目前綁給 {victim_name}（{victim_desc}）。y = 搶過來，其他鍵 = 取消")
        }
    };

    let hint = crate::widget::hint_line(
        chrome,
        &[("Ctrl-C".into(), "取消（因此無法在此綁定 Ctrl-C）")],
        chrome.help_key_fg,
    );

    // `chars().count()` 低估中文字寬——CJK 字元佔 2 個終端格，卻只算 1 個
    // char。`message` 幾乎全是中文（衝突提示尤其），量錯的話對話框會窄到
    // 把字擠出邊框外。`UnicodeWidthStr::width()` 照終端實際佔用的格數算，
    // 跟 `hint.width()`（ratatui 自己也是這樣量）用同一套標準。
    let dialog_width = (message.width() as u16 + 4)
        .max(mode_label.width() as u16 + 4)
        .max(current_line.width() as u16 + 4)
        .max(hint.width() as u16 + 2)
        .max(30)
        .min(area.width.saturating_sub(4));
    let dialog_height = 6u16.min(area.height.saturating_sub(2));
    let dialog_area = super::centered_rect(area, dialog_width, dialog_height);

    f.render_widget(Clear, dialog_area);
    let block = Block::default()
        .title(format!(" 綁定 {config_name} "))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(chrome.divider_fg))
        .style(Style::default().bg(chrome.bg).fg(chrome.fg));
    let inner = block.inner(dialog_area);
    f.render_widget(block, dialog_area);

    let [mode_area, current_area, message_area, hint_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    f.render_widget(Paragraph::new(Line::raw(mode_label)), mode_area);
    f.render_widget(Paragraph::new(Line::raw(current_line)), current_area);
    f.render_widget(
        Paragraph::new(Line::raw(message)).fg(chrome.status_warn_fg),
        message_area,
    );
    f.render_widget(Paragraph::new(hint), hint_area);
}

/// 接收「已經在跑」的 terminal，跟 `color_editor::run`／`path_browser::run`
/// 同一個約定。
pub(crate) fn run(
    terminal: &mut DefaultTerminal,
    draft: &mut Draft,
    defaults: &ResolvedDefaults,
    chrome: &ColorTheme,
) -> crate::Result<()> {
    let mut state = KeyBindEditorState::new(defaults);
    loop {
        terminal.draw(|f| state.render(f, f.area(), chrome))?;
        let Event::Key(key) = ratatui::crossterm::event::read()? else {
            continue;
        };
        match state.on_key(key, draft) {
            Flow::Continue => {}
            Flow::Back => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::{KeyEventKind, KeyModifiers};

    use super::*;

    fn test_defaults() -> ResolvedDefaults {
        ResolvedDefaults::from_core(&crate::config::CoreConfig::default())
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn char_key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn ctrl_key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn test_draft() -> Draft {
        Draft::new()
    }

    /// `toml_edit::Value` 沒有 `PartialEq`，測試改比字串化結果——跟
    /// `apply_touched_settings` 最終寫進檔案的內容是同一件事。
    fn edited_value(draft: &Draft, key: &ConfigKey) -> Option<Option<String>> {
        draft
            .edits
            .get(key)
            .map(|v| v.as_ref().map(ToString::to_string))
    }

    fn navigate_down_row(state: &KeyBindEditorState) -> usize {
        state
            .actions
            .iter()
            .position(|&e| e == UserEvent::NavigateDown)
            .unwrap()
    }

    fn refresh_row(state: &KeyBindEditorState) -> usize {
        state
            .actions
            .iter()
            .position(|&e| e == UserEvent::Refresh)
            .unwrap()
    }

    fn quit_row(state: &KeyBindEditorState) -> usize {
        state
            .actions
            .iter()
            .position(|&e| e == UserEvent::Quit)
            .unwrap()
    }

    #[test]
    fn browsing_without_committing_touches_nothing() {
        let mut state = KeyBindEditorState::new(&test_defaults());
        let mut draft = test_draft();
        state.on_key(key(KeyCode::Down), &mut draft);
        state.on_key(key(KeyCode::Up), &mut draft);
        assert!(draft.edits.is_empty());
        assert!(state.overrides.is_empty());
    }

    #[test]
    fn enter_replaces_all_bindings() {
        let mut state = KeyBindEditorState::new(&test_defaults());
        let mut draft = test_draft();
        state.list.select(Some(navigate_down_row(&state)));

        state.on_key(key(KeyCode::Enter), &mut draft);
        state.on_key(ctrl_key('n'), &mut draft);

        let effective = state.effective();
        assert_eq!(
            effective.get(&ctrl_key('n')),
            Some(&UserEvent::NavigateDown)
        );
        assert_eq!(effective.get(&char_key('j')), None);
        assert_eq!(effective.get(&key(KeyCode::Down)), None);

        let config_key = ConfigKey {
            table: KEYBIND,
            key: "navigate_down".into(),
        };
        assert!(draft.edits.contains_key(&config_key));
    }

    #[test]
    fn append_keeps_existing_keys_and_adds_one() {
        let mut state = KeyBindEditorState::new(&test_defaults());
        let mut draft = test_draft();
        state.list.select(Some(navigate_down_row(&state)));

        state.on_key(char_key('a'), &mut draft);
        state.on_key(ctrl_key('n'), &mut draft);

        let effective = state.effective();
        assert_eq!(
            effective.get(&char_key('j')),
            Some(&UserEvent::NavigateDown)
        );
        assert_eq!(
            effective.get(&key(KeyCode::Down)),
            Some(&UserEvent::NavigateDown)
        );
        assert_eq!(
            effective.get(&ctrl_key('n')),
            Some(&UserEvent::NavigateDown)
        );
    }

    #[test]
    fn append_an_already_bound_key_is_a_no_op_not_a_duplicate() {
        let mut state = KeyBindEditorState::new(&test_defaults());
        let mut draft = test_draft();
        state.list.select(Some(navigate_down_row(&state)));

        state.on_key(char_key('a'), &mut draft);
        state.on_key(char_key('j'), &mut draft); // j 已經是 navigate_down 的鍵

        assert!(state.capture.is_none());
        assert!(draft.edits.is_empty());
        assert!(state.overrides.is_empty());
    }

    #[test]
    fn x_disables_the_action() {
        let mut state = KeyBindEditorState::new(&test_defaults());
        let mut draft = test_draft();
        state.list.select(Some(refresh_row(&state)));

        state.on_key(char_key('x'), &mut draft);

        assert_eq!(
            state.effective().keys_for_event(UserEvent::Refresh),
            Vec::<String>::new()
        );
        let config_key = ConfigKey {
            table: KEYBIND,
            key: "refresh".into(),
        };
        assert_eq!(
            edited_value(&draft, &config_key),
            Some(Some(inline_string_array(&[]).to_string()))
        );
    }

    #[test]
    fn r_reverts_to_default_and_removes_the_key() {
        let mut state = KeyBindEditorState::new(&test_defaults());
        let mut draft = test_draft();
        state.list.select(Some(refresh_row(&state)));

        state.on_key(char_key('x'), &mut draft);
        state.on_key(char_key('r'), &mut draft);

        assert_eq!(
            state.effective().display_key(UserEvent::Refresh).as_deref(),
            Some("r")
        );
        let config_key = ConfigKey {
            table: KEYBIND,
            key: "refresh".into(),
        };
        assert_eq!(edited_value(&draft, &config_key), Some(None));
    }

    /// `r` 之後被搶走的鍵要自動歸還——不需要任何「victim 復原」的額外邏輯，
    /// 因為 `overrides` 被清空後 `effective()` 從頭重算，被搶的 action
    /// 自然拿回內建預設。
    #[test]
    fn reverting_a_steal_gives_the_key_back_to_its_original_owner() {
        let mut state = KeyBindEditorState::new(&test_defaults());
        let mut draft = test_draft();
        state.list.select(Some(quit_row(&state)));
        state.on_key(key(KeyCode::Enter), &mut draft);
        state.on_key(char_key('r'), &mut draft); // r 鍵，refresh 的預設鍵：會撞見衝突
        state.on_key(char_key('y'), &mut draft); // 搶過來

        assert_eq!(
            state.effective().get(&char_key('r')),
            Some(&UserEvent::Quit)
        );

        state.list.select(Some(quit_row(&state)));
        state.on_key(char_key('r'), &mut draft); // 還原 quit

        assert_eq!(
            state.effective().get(&char_key('r')),
            Some(&UserEvent::Refresh)
        );
    }

    /// 搶自「使用者檔案已經自訂過」的 action：受害者的陣列要一起改寫，
    /// 檔案自身才會保持一致（否則同一顆鍵在檔案裡出現兩次，下次啟動整份
    /// 設定檔會載入失敗）。
    #[test]
    fn stealing_from_a_customized_action_rewrites_its_array_too() {
        let mut defaults = test_defaults();
        defaults
            .keybind_patch
            .assign(UserEvent::Quit, vec![char_key('z')]);
        // quit 目前的自訂鍵是 z；接下來把 z 搶給 fetch。
        let mut state = KeyBindEditorState::new(&defaults);
        let mut draft = test_draft();
        let fetch_row = state
            .actions
            .iter()
            .position(|&e| e == UserEvent::Fetch)
            .unwrap();
        state.list.select(Some(fetch_row));
        state.on_key(key(KeyCode::Enter), &mut draft);
        state.on_key(char_key('z'), &mut draft); // z 目前歸 quit：會撞見衝突
        state.on_key(char_key('y'), &mut draft); // 搶過來

        let quit_config_key = ConfigKey {
            table: KEYBIND,
            key: "quit".into(),
        };
        assert_eq!(
            edited_value(&draft, &quit_config_key),
            Some(Some(inline_string_array(&[]).to_string()))
        );
    }

    /// 搶自純內建預設的 action：不用改寫它的陣列——`current_patch` 本來就
    /// 沒有它的 entry，下次啟動時同一套 `assign` 邏輯會自動算出同樣結果。
    #[test]
    fn stealing_from_a_default_only_action_does_not_touch_its_file_entry() {
        let mut state = KeyBindEditorState::new(&test_defaults());
        let mut draft = test_draft();
        state.list.select(Some(quit_row(&state)));
        state.on_key(key(KeyCode::Enter), &mut draft);
        state.on_key(char_key('r'), &mut draft); // r，refresh 的預設鍵，refresh 從未被使用者自訂

        let refresh_config_key = ConfigKey {
            table: KEYBIND,
            key: "refresh".into(),
        };
        assert!(!draft.edits.contains_key(&refresh_config_key));
    }

    #[test]
    fn conflict_prompts_and_y_commits_the_steal() {
        let mut state = KeyBindEditorState::new(&test_defaults());
        let mut draft = test_draft();
        state.list.select(Some(quit_row(&state)));
        state.on_key(key(KeyCode::Enter), &mut draft);
        state.on_key(char_key('r'), &mut draft);

        assert!(matches!(
            state.capture.as_ref().map(|c| &c.state),
            Some(CaptureState::Conflict { .. })
        ));

        state.on_key(char_key('y'), &mut draft);

        assert!(state.capture.is_none());
        assert_eq!(
            state.effective().get(&char_key('r')),
            Some(&UserEvent::Quit)
        );
    }

    #[test]
    fn conflict_any_other_key_cancels_the_whole_capture() {
        let mut state = KeyBindEditorState::new(&test_defaults());
        let mut draft = test_draft();
        state.list.select(Some(quit_row(&state)));
        state.on_key(key(KeyCode::Enter), &mut draft);
        state.on_key(char_key('r'), &mut draft);

        state.on_key(char_key('n'), &mut draft);

        assert!(state.capture.is_none());
        assert!(draft.edits.is_empty());
        assert_eq!(
            state.effective().get(&char_key('r')),
            Some(&UserEvent::Refresh)
        );
    }

    #[test]
    fn ctrl_d_is_bindable_because_cancel_is_only_ctrl_c() {
        // ctrl-d 是 half_page_down 的預設鍵，所以搶過來一定會先撞見衝突
        // 提示；這條測試要驗的是「ctrl-d 能被捕捉到、不會被當成取消鍵吃掉」，
        // 不是零衝突。
        let mut state = KeyBindEditorState::new(&test_defaults());
        let mut draft = test_draft();
        state.list.select(Some(navigate_down_row(&state)));
        state.on_key(key(KeyCode::Enter), &mut draft);
        state.on_key(ctrl_key('d'), &mut draft);

        assert!(matches!(
            state.capture.as_ref().map(|c| &c.state),
            Some(CaptureState::Conflict { key, victim })
                if *key == ctrl_key('d') && *victim == UserEvent::HalfPageDown
        ));

        state.on_key(char_key('y'), &mut draft);

        assert_eq!(
            state.effective().get(&ctrl_key('d')),
            Some(&UserEvent::NavigateDown)
        );
    }

    #[test]
    fn ctrl_c_cancels_the_capture() {
        let mut state = KeyBindEditorState::new(&test_defaults());
        let mut draft = test_draft();
        state.list.select(Some(navigate_down_row(&state)));
        state.on_key(key(KeyCode::Enter), &mut draft);
        state.on_key(ctrl_key('c'), &mut draft);

        assert!(state.capture.is_none());
        assert!(draft.edits.is_empty());
    }

    #[test]
    fn release_and_repeat_events_are_ignored_during_capture() {
        let mut state = KeyBindEditorState::new(&test_defaults());
        let mut draft = test_draft();
        state.list.select(Some(navigate_down_row(&state)));
        state.on_key(key(KeyCode::Enter), &mut draft);

        let mut release = ctrl_key('n');
        release.kind = KeyEventKind::Release;
        state.on_key(release, &mut draft);

        // 捕捉還沒被吃掉：接下來真正的 Press 才會生效。
        assert!(state.capture.is_some());
        state.on_key(ctrl_key('n'), &mut draft);
        assert_eq!(
            state.effective().get(&ctrl_key('n')),
            Some(&UserEvent::NavigateDown)
        );
    }

    #[test]
    fn unwritable_key_is_rejected_and_stays_in_capture() {
        let mut state = KeyBindEditorState::new(&test_defaults());
        let mut draft = test_draft();
        state.list.select(Some(navigate_down_row(&state)));
        state.on_key(key(KeyCode::Enter), &mut draft);

        state.on_key(key(KeyCode::Null), &mut draft);

        assert!(matches!(
            state.capture.as_ref().map(|c| &c.state),
            Some(CaptureState::Rejected(_))
        ));
    }
}

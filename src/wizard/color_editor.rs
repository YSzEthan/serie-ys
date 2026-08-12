use ratatui::{
    crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    layout::{Constraint, Layout, Rect},
    style::{Color as RatatuiColor, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, ListItem, ListState, Paragraph},
    DefaultTerminal, Frame,
};
use tui_input::backend::crossterm::EventHandler;

use crate::color::{
    color_to_config_string, preview_block, ColorTheme, FlatColors, PreviewBlock, COLOR_KEYS,
    NAMED_COLORS,
};

use super::{ConfigKey, Draft, ResolvedDefaults, COLOR};

pub(crate) enum Flow {
    Continue,
    /// Esc／←／h 在清單畫面按下：回主選單。跟 `path_browser::Flow::Abort`
    /// 是同一個位階——這裡沒有「取消整個 wizard」的路，那是外層 `on_key`
    /// 的事。
    Back,
}

/// 三種表示法是三個變體，不是「一個 mode 欄位 + 三份可能是死的暫存」。
/// 永遠只有一份活的表示法；Tab 換變體並從現值重算。
enum ColorEdit {
    /// `NAMED_COLORS` 的索引。
    Named(usize),
    Indexed(tui_input::Input),
    Hex(tui_input::Input),
}

impl ColorEdit {
    /// 依現值挑起始變體：Rgb → Hex、Indexed → Indexed、其餘 → Named。
    /// 使用者十之八九只是要微調現值，不該每次都從色名重打。
    fn from_current(current: RatatuiColor) -> Self {
        match current {
            RatatuiColor::Rgb(r, g, b) => {
                ColorEdit::Hex(tui_input::Input::new(format!("{r:02X}{g:02X}{b:02X}")))
            }
            RatatuiColor::Indexed(i) => ColorEdit::Indexed(tui_input::Input::new(i.to_string())),
            _ => ColorEdit::Named(named_index_of(current)),
        }
    }

    /// Tab 換到下一個表示法。傳入的 `current` 一律是
    /// `colors.values[list.selected()]`（已 commit 的草稿值），不是
    /// `self.value()`——後者在 hex 打一半（`#E0`）時是 `None`，為它生一份
    /// 「最後一個合法值」的暫存，就是把好不容易砍掉的東西加回去。
    fn next_mode(&self, current: RatatuiColor) -> Self {
        match self {
            ColorEdit::Named(_) => ColorEdit::Indexed(tui_input::Input::new(match current {
                RatatuiColor::Indexed(i) => i.to_string(),
                _ => String::new(),
            })),
            ColorEdit::Indexed(_) => ColorEdit::Hex(tui_input::Input::new(match current {
                RatatuiColor::Rgb(r, g, b) => format!("{r:02X}{g:02X}{b:02X}"),
                _ => String::new(),
            })),
            ColorEdit::Hex(_) => ColorEdit::Named(named_index_of(current)),
        }
    }

    /// `None` = 輸入還不完整（hex 不足 6 碼、索引不是合法數字），Enter 要拒絕。
    fn value(&self) -> Option<RatatuiColor> {
        match self {
            ColorEdit::Named(idx) => NAMED_COLORS.get(*idx).map(|(_, c)| *c),
            ColorEdit::Indexed(input) => {
                input.value().parse::<u8>().ok().map(RatatuiColor::Indexed)
            }
            ColorEdit::Hex(input) => {
                let s = input.value();
                if s.len() != 6 {
                    return None;
                }
                let r = u8::from_str_radix(&s[0..2], 16).ok()?;
                let g = u8::from_str_radix(&s[2..4], 16).ok()?;
                let b = u8::from_str_radix(&s[4..6], 16).ok()?;
                Some(RatatuiColor::Rgb(r, g, b))
            }
        }
    }

    /// `value()` 是 `None` 時，缺什麼——Enter 被拒絕時顯示在對話框裡。
    /// `Named` 一定有值（`NAMED_COLORS` 非空），不會走到這裡。
    fn hint(&self) -> Option<String> {
        if self.value().is_some() {
            return None;
        }
        match self {
            ColorEdit::Hex(input) => {
                Some(format!("HEX 需要 6 碼，目前 {} 碼", input.value().len()))
            }
            ColorEdit::Indexed(_) => Some("索引需要 0-255 的數字".to_string()),
            ColorEdit::Named(_) => None,
        }
    }

    /// `Esc`／`Tab`／`Enter` 由外層 `on_edit_key` 攔截；這裡只處理各表示法
    /// 自己的輸入。
    fn on_key(&mut self, key: KeyEvent) {
        match self {
            ColorEdit::Named(idx) => match key.code {
                KeyCode::Left | KeyCode::Char('h') => {
                    *idx = (*idx + NAMED_COLORS.len() - 1) % NAMED_COLORS.len();
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    *idx = (*idx + 1) % NAMED_COLORS.len();
                }
                _ => {}
            },
            ColorEdit::Indexed(input) => match key.code {
                KeyCode::Left | KeyCode::Down | KeyCode::Char('h') | KeyCode::Char('j') => {
                    super::adjust_number(input, false, 0, 255);
                }
                KeyCode::Right | KeyCode::Up | KeyCode::Char('l') | KeyCode::Char('k') => {
                    super::adjust_number(input, true, 0, 255);
                }
                KeyCode::Char(c) if c.is_ascii_digit() => {
                    // 先在副本上驗證能不能被解析成 u8，能才真的寫回去——跟
                    // `on_number_key` 同一個模式，只是上限固定 255。
                    let mut probe = input.clone();
                    probe.handle_event(&Event::Key(key));
                    if probe.value().parse::<u8>().is_ok() {
                        *input = probe;
                    }
                }
                KeyCode::Backspace => {
                    input.handle_event(&Event::Key(key));
                }
                _ => {}
            },
            ColorEdit::Hex(input) => {
                // Hex 模式下 a-f 是合法輸入字元，不能當導覽鍵（這個變體本來
                // 就沒有定義任何導覽語意）；也不能被 ctrl 修飾字誤觸——
                // `ctrl-f` 不該被當成輸入 'f'。
                let plain = key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;
                match key.code {
                    KeyCode::Char(c) if plain && c.is_ascii_hexdigit() => {
                        if input.value().len() < 6 {
                            input.handle_event(&Event::Key(key));
                        }
                    }
                    KeyCode::Backspace => {
                        input.handle_event(&Event::Key(key));
                    }
                    _ => {}
                }
            }
        }
    }
}

fn named_index_of(color: RatatuiColor) -> usize {
    NAMED_COLORS
        .iter()
        .position(|(_, c)| *c == color)
        .unwrap_or(0)
}

/// `list` + `edit` 的組合就是全部合法畫面，沒有不可表示的狀態，也不需要
/// `Screen` enum 或畫面堆疊——後者要靠註解保證「[0] 一定是 Fields」，而
/// 註解保證的不變式就是沒有保證。Esc 就是把 `edit` 打成 `None`。
pub(crate) struct ColorEditorState {
    /// 進編輯器當下的原值。`r` 的還原目標，也讓「這欄改過了」直接由
    /// `base.values[i] != colors.values[i]` 得出，不必去問 `draft.edits`。
    base: FlatColors,
    /// 編輯中的草稿，起點＝`base`。
    colors: FlatColors,
    list: ListState,
    /// `Some` = 正在編輯 `list.selected()` 那一欄。
    edit: Option<ColorEdit>,
    /// `true` = 設定檔載入失敗，`base`/`colors` 是內建硬預設，不是使用者的
    /// 真實設定——`render` 要據此在預覽區上方講清楚。
    theme_is_fallback: bool,
}

impl ColorEditorState {
    pub fn new(defaults: &ResolvedDefaults) -> Self {
        let mut list = ListState::default();
        list.select(Some(0));
        Self {
            base: FlatColors::from(&defaults.theme),
            colors: FlatColors::from(&defaults.theme),
            list,
            edit: None,
            theme_is_fallback: defaults.theme_is_fallback,
        }
    }

    /// 零 I/O 的純狀態轉移，可以直接餵 `KeyEvent` 測試——跟
    /// `WizardState::on_key` 同一個模式。
    pub fn on_key(&mut self, key: KeyEvent, draft: &mut Draft) -> Flow {
        if key.kind != KeyEventKind::Press {
            return Flow::Continue;
        }

        if self.edit.is_some() {
            self.on_edit_key(key, draft);
            return Flow::Continue;
        }

        if super::is_abort_key(&key) {
            return Flow::Back;
        }

        match key.code {
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => Flow::Back,
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                Flow::Continue
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                Flow::Continue
            }
            // `on_key` 是零 I/O 純函式，拿不到 viewport 高度；43 列分四頁
            // 多，固定步長夠用，不值得為它在 render 時回存高度。
            KeyCode::PageUp => {
                self.move_selection(-10);
                Flow::Continue
            }
            KeyCode::PageDown => {
                self.move_selection(10);
                Flow::Continue
            }
            KeyCode::Home => {
                self.list.select(Some(0));
                Flow::Continue
            }
            KeyCode::End => {
                self.list.select(Some(COLOR_KEYS.len() - 1));
                Flow::Continue
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                let idx = self.list.selected().unwrap_or(0);
                self.edit = Some(ColorEdit::from_current(self.colors.values[idx]));
                Flow::Continue
            }
            // 43 個欄位配一個手打 hex 的輸入框，誤按是必然的，而 `edits`
            // 對顏色沒有其他移除路徑。
            KeyCode::Char('r') => {
                self.revert_selected(draft);
                Flow::Continue
            }
            _ => Flow::Continue,
        }
    }

    fn on_edit_key(&mut self, key: KeyEvent, draft: &mut Draft) {
        // ctrl-c / ctrl-d 照 path_browser 的慣例：退一層（取消這次編輯），
        // 不是離開整個顏色編輯器。
        if super::is_abort_key(&key) {
            self.edit = None;
            return;
        }

        let idx = self.list.selected().unwrap_or(0);

        match key.code {
            KeyCode::Esc => {
                self.edit = None;
            }
            KeyCode::Tab => {
                let current = self.colors.values[idx];
                if let Some(edit) = &mut self.edit {
                    *edit = edit.next_mode(current);
                }
            }
            KeyCode::Enter => {
                let committed = self.edit.as_ref().and_then(ColorEdit::value);
                if let Some(color) = committed {
                    self.colors.values[idx] = color;
                    self.commit_field(draft, idx);
                    self.edit = None;
                }
                // 缺什麼由 `ColorEdit::hint()` 在畫面上顯示，留在原地。
            }
            _ => {
                if let Some(edit) = &mut self.edit {
                    edit.on_key(key);
                }
            }
        }
    }

    /// 目前該顯示的值：選到的那一列且正在編輯時，是輸入到一半的即時值
    /// （不完整就沿用編輯前的值）；其餘情況是已 commit 的草稿值。
    ///
    /// 不直接寫回 `colors.values[idx]`——那是 `ColorEdit::next_mode` 的
    /// 「現值」基準，Tab 在 hex 打一半時要落在「最後一個合法值」，不是
    /// 半成品；把即時輸入寫回去會讓那個基準變得不穩定。
    fn live_value(&self, idx: usize) -> RatatuiColor {
        if self.list.selected() == Some(idx) {
            if let Some(color) = self.edit.as_ref().and_then(ColorEdit::value) {
                return color;
            }
        }
        self.colors.values[idx]
    }

    fn move_selection(&mut self, delta: i32) {
        let len = COLOR_KEYS.len() as i32;
        let current = self.list.selected().unwrap_or(0) as i32;
        let next = (current + delta).clamp(0, len - 1);
        self.list.select(Some(next as usize));
    }

    fn commit_field(&mut self, draft: &mut Draft, idx: usize) {
        let key = ConfigKey {
            table: COLOR,
            key: COLOR_KEYS[idx],
        };
        let value = color_to_config_string(self.colors.values[idx]);
        draft.edits.insert(key, Some(value.into()));
    }

    /// `r`：回到 `base` 的原值，並把 `edits` 裡這一欄的紀錄整個移除——
    /// 不是寫 `Some(None)`。後者會把範本裡那一行整個刪掉，而使用者要的是
    /// 「取消我這次的改動」，不是「刪掉這個設定」。
    fn revert_selected(&mut self, draft: &mut Draft) {
        let idx = self.list.selected().unwrap_or(0);
        let key = ConfigKey {
            table: COLOR,
            key: COLOR_KEYS[idx],
        };
        draft.edits.remove(&key);
        self.colors.values[idx] = self.base.values[idx];
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect, chrome: &ColorTheme) {
        let [main_area, hint_area] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);
        let [list_area, preview_area] =
            Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
                .areas(main_area);

        let selected = self.list.selected().unwrap_or(0);

        let items: Vec<ListItem> = COLOR_KEYS
            .iter()
            .enumerate()
            .map(|(i, key)| {
                let color = self.live_value(i);
                let touched = self.colors.values[i] != self.base.values[i];
                let marker = if touched { "✓ " } else { "  " };
                ListItem::new(Line::from(vec![
                    Span::raw(marker),
                    Span::styled("■ ", Style::default().fg(color)),
                    Span::raw(format!("{key:<30}")),
                    Span::raw(color_to_config_string(color)),
                ]))
            })
            .collect();
        let list = super::styled_list(items, chrome).block(
            Block::default()
                .title(format!(" 顏色 [{}/{}] ", selected + 1, COLOR_KEYS.len()))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(chrome.divider_fg)),
        );
        f.render_stateful_widget(list, list_area, &mut self.list);

        // 選中欄位若正在編輯，預覽要即時反映輸入到一半的值——`self.colors`
        // 本身直到 Enter 才會被寫入，這裡只在預覽用的副本上覆蓋，不動草稿。
        let mut preview_values = self.colors.values.clone();
        preview_values[selected] = self.live_value(selected);
        let preview_colors = FlatColors {
            values: preview_values,
            graph: self.colors.graph.clone(),
        };

        let focus = preview_block(COLOR_KEYS[selected]);
        let mut preview_text: Vec<Line> = Vec::new();
        if self.theme_is_fallback {
            preview_text
                .push(Line::raw("設定檔載入失敗，以下是內建預設").fg(chrome.status_warn_fg));
        }
        preview_text.extend(preview_lines(&preview_colors, focus));
        f.render_widget(
            Paragraph::new(preview_text).block(
                Block::default()
                    .title(" 預覽 ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(chrome.divider_fg)),
            ),
            preview_area,
        );

        let hint = crate::widget::hint_line(
            chrome,
            &[
                ("↑↓/kj".into(), "移動"),
                ("PgUp/PgDn".into(), "翻頁"),
                ("Enter/l".into(), "編輯"),
                ("r".into(), "還原"),
                ("Esc/h".into(), "返回"),
            ],
            chrome.help_key_fg,
        );
        f.render_widget(Paragraph::new(hint), hint_area);

        if let Some(edit) = &self.edit {
            render_edit_dialog(f, area, COLOR_KEYS[selected], edit, chrome);
        }
    }
}

fn preview_lines(colors: &FlatColors, focus: PreviewBlock) -> Vec<Line<'static>> {
    let c = |key: &str| color_at(colors, key);
    let mark = |block: PreviewBlock| if block == focus { "▏" } else { " " };

    vec![
        Line::raw(format!("{}[Commit list]", mark(PreviewBlock::List))),
        Line::from(vec![
            Span::styled("● ", Style::default().fg(c("list_ref_branch_fg"))),
            Span::styled("a1b2c3d ", Style::default().fg(c("list_hash_fg"))),
            Span::styled("(main) ", Style::default().fg(c("list_ref_branch_fg"))),
            Span::styled("feat: 新增功能 ", Style::default().fg(c("list_subject_fg"))),
            Span::styled("Ethan ", Style::default().fg(c("list_name_fg"))),
            Span::styled("2 天前", Style::default().fg(c("list_date_fg"))),
        ]),
        Line::from(vec![
            Span::styled(
                "  d4e5f6a ",
                Style::default()
                    .bg(c("list_selected_bg"))
                    .fg(c("list_selected_fg")),
            ),
            Span::styled("(HEAD -> main) ", Style::default().fg(c("list_head_fg"))),
            Span::styled(
                "fix: 修正邊界條件",
                Style::default().fg(c("list_subject_fg")),
            ),
        ]),
        Line::raw(""),
        Line::raw(format!("{}[Detail]", mark(PreviewBlock::Detail))),
        Line::from(vec![
            Span::styled("Author: ", Style::default().fg(c("detail_label_fg"))),
            Span::styled("Ethan ", Style::default().fg(c("detail_name_fg"))),
            Span::styled(
                "<it@scanoo.com.tw>",
                Style::default().fg(c("detail_email_fg")),
            ),
        ]),
        Line::from(vec![
            Span::styled("M ", Style::default().fg(c("detail_file_change_modify_fg"))),
            Span::styled("src/app.rs", Style::default().fg(c("detail_name_fg"))),
        ]),
        Line::from(vec![
            Span::styled("src/app.rs ", Style::default().fg(c("diff_title_path_fg"))),
            Span::styled(
                "@@ -1,3 +1,4 @@",
                Style::default().fg(c("diff_title_hunk_fg")),
            ),
        ]),
        Line::raw(""),
        Line::raw(format!("{}[狀態列]", mark(PreviewBlock::Status))),
        Line::from(vec![
            Span::styled("Enter", Style::default().fg(c("help_key_fg"))),
            Span::styled(":確認", Style::default().fg(c("status_input_transient_fg"))),
        ]),
        Line::raw("已複製 commit hash").fg(c("status_success_fg")),
        Line::raw("──────────").fg(c("divider_fg")),
    ]
}

fn color_at(colors: &FlatColors, key: &str) -> RatatuiColor {
    let idx = COLOR_KEYS
        .iter()
        .position(|k| *k == key)
        .expect("鍵一定在 COLOR_KEYS 裡");
    colors.values[idx]
}

fn render_edit_dialog(
    f: &mut Frame,
    area: Rect,
    key_name: &str,
    edit: &ColorEdit,
    chrome: &ColorTheme,
) {
    let mode_label = match edit {
        ColorEdit::Named(_) => "[色名]  256色   HEX",
        ColorEdit::Indexed(_) => " 色名  [256色]  HEX",
        ColorEdit::Hex(_) => " 色名   256色  [HEX]",
    };
    let value_line = match edit {
        ColorEdit::Named(idx) => format!("< {} >", NAMED_COLORS[*idx].0),
        ColorEdit::Indexed(input) => format!("> {}", input.value()),
        ColorEdit::Hex(input) => format!("> #{}", input.value()),
    };
    let message = edit.hint().unwrap_or_default();

    let hint = crate::widget::hint_line(
        chrome,
        &[
            ("Tab".into(), "切換表示法"),
            ("Enter".into(), "確認"),
            ("Esc".into(), "取消"),
        ],
        chrome.help_key_fg,
    );

    let dialog_width = (hint.width() as u16 + 2)
        .max(30)
        .min(area.width.saturating_sub(4));
    let dialog_height = 6u16.min(area.height.saturating_sub(2));
    let dialog_area = super::centered_rect(area, dialog_width, dialog_height);

    f.render_widget(Clear, dialog_area);
    let block = Block::default()
        .title(format!(" color.{key_name} "))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(chrome.divider_fg))
        .style(Style::default().bg(chrome.bg).fg(chrome.fg));
    let inner = block.inner(dialog_area);
    f.render_widget(block, dialog_area);

    let [mode_area, value_area, message_area, hint_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    f.render_widget(Paragraph::new(Line::raw(mode_label)), mode_area);
    f.render_widget(Paragraph::new(Line::raw(value_line)), value_area);
    f.render_widget(
        Paragraph::new(Line::raw(message)).fg(chrome.status_warn_fg),
        message_area,
    );
    f.render_widget(Paragraph::new(hint), hint_area);

    match edit {
        ColorEdit::Indexed(input) | ColorEdit::Hex(input) => {
            f.set_cursor_position((
                value_area.x + 2 + input.visual_cursor() as u16,
                value_area.y,
            ));
        }
        ColorEdit::Named(_) => {}
    }
}

/// 接收「已經在跑」的 terminal，跟 `path_browser::run` 同一個約定。
pub(crate) fn run(
    terminal: &mut DefaultTerminal,
    draft: &mut Draft,
    defaults: &ResolvedDefaults,
    chrome: &ColorTheme,
) -> crate::Result<()> {
    let mut state = ColorEditorState::new(defaults);
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
    use ratatui::crossterm::event::KeyModifiers;

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

    fn field_key(idx: usize) -> ConfigKey {
        ConfigKey {
            table: COLOR,
            key: COLOR_KEYS[idx],
        }
    }

    /// `toml_edit::Value` 沒有 `PartialEq`，測試只能比它包的字串。
    fn committed_string(draft: &Draft, idx: usize) -> Option<String> {
        draft
            .edits
            .get(&field_key(idx))?
            .as_ref()?
            .as_str()
            .map(str::to_string)
    }

    fn enter_edit(state: &mut ColorEditorState, draft: &mut Draft) {
        assert!(matches!(
            state.on_key(key(KeyCode::Enter), draft),
            Flow::Continue
        ));
        assert!(state.edit.is_some());
    }

    #[test]
    fn browsing_without_committing_touches_nothing() {
        let mut state = ColorEditorState::new(&test_defaults());
        let mut draft = Draft::new();
        state.on_key(key(KeyCode::Down), &mut draft);
        state.on_key(key(KeyCode::Down), &mut draft);
        state.on_key(key(KeyCode::PageDown), &mut draft);
        state.on_key(key(KeyCode::Home), &mut draft);
        state.on_key(key(KeyCode::End), &mut draft);
        assert!(draft.edits.is_empty());
    }

    #[test]
    fn each_notation_commits_the_right_color() {
        // Indexed
        let mut state = ColorEditorState::new(&test_defaults());
        let mut draft = Draft::new();
        enter_edit(&mut state, &mut draft);
        state.on_key(key(KeyCode::Tab), &mut draft); // Named -> Indexed
        for c in "208".chars() {
            state.on_key(char_key(c), &mut draft);
        }
        state.on_key(key(KeyCode::Enter), &mut draft);
        assert_eq!(
            committed_string(&draft, 0),
            Some(color_to_config_string(RatatuiColor::Indexed(208)))
        );

        // Hex
        let mut state = ColorEditorState::new(&test_defaults());
        let mut draft = Draft::new();
        enter_edit(&mut state, &mut draft);
        state.on_key(key(KeyCode::Tab), &mut draft); // Named -> Indexed
        state.on_key(key(KeyCode::Tab), &mut draft); // Indexed -> Hex
        for c in "E06C76".chars() {
            state.on_key(char_key(c), &mut draft);
        }
        state.on_key(key(KeyCode::Enter), &mut draft);
        assert_eq!(
            committed_string(&draft, 0),
            Some(color_to_config_string(RatatuiColor::Rgb(0xE0, 0x6C, 0x76)))
        );

        // Named：起點是 fg 的預設值 reset，右移一格换到下一個具名色即可送出。
        let mut state = ColorEditorState::new(&test_defaults());
        let mut draft = Draft::new();
        enter_edit(&mut state, &mut draft);
        state.on_key(key(KeyCode::Right), &mut draft);
        state.on_key(key(KeyCode::Enter), &mut draft);
        assert!(draft.edits.contains_key(&field_key(0)));
    }

    #[test]
    fn editing_updates_the_draft_colors_immediately() {
        let mut state = ColorEditorState::new(&test_defaults());
        let mut draft = Draft::new();
        enter_edit(&mut state, &mut draft);
        state.on_key(key(KeyCode::Tab), &mut draft);
        for c in "208".chars() {
            state.on_key(char_key(c), &mut draft);
        }
        // 還沒按 Enter，`edits` 跟 `colors`（Tab 的現值基準）都不該提早
        // 被寫入——即時反映的是 `live_value`（預覽用），不是提早 commit。
        assert!(draft.edits.is_empty());
        assert_eq!(state.colors.values[0], state.base.values[0]);
        assert_eq!(state.live_value(0), RatatuiColor::Indexed(208));
    }

    #[test]
    fn hex_mode_sends_abcdef_to_the_input_instead_of_navigating() {
        let mut state = ColorEditorState::new(&test_defaults());
        let mut draft = Draft::new();
        enter_edit(&mut state, &mut draft);
        state.on_key(key(KeyCode::Tab), &mut draft);
        state.on_key(key(KeyCode::Tab), &mut draft);
        for c in "abcdef".chars() {
            state.on_key(char_key(c), &mut draft);
        }
        let Some(ColorEdit::Hex(input)) = &state.edit else {
            panic!("應該還在 Hex 模式");
        };
        assert_eq!(
            input.value(),
            "abcdef",
            "a-f 要進輸入框，不是被當成導覽鍵吃掉"
        );
    }

    #[test]
    fn hex_mode_ignores_ctrl_modified_hex_letters() {
        let mut state = ColorEditorState::new(&test_defaults());
        let mut draft = Draft::new();
        enter_edit(&mut state, &mut draft);
        state.on_key(key(KeyCode::Tab), &mut draft);
        state.on_key(key(KeyCode::Tab), &mut draft);
        state.on_key(ctrl_key('f'), &mut draft);
        let Some(ColorEdit::Hex(input)) = &state.edit else {
            panic!("應該還在 Hex 模式");
        };
        assert_eq!(input.value(), "", "ctrl-f 不該被當成 hex 輸入");
    }

    #[test]
    fn hex_mode_rejects_incomplete_input_on_enter() {
        let mut state = ColorEditorState::new(&test_defaults());
        let mut draft = Draft::new();
        enter_edit(&mut state, &mut draft);
        state.on_key(key(KeyCode::Tab), &mut draft);
        state.on_key(key(KeyCode::Tab), &mut draft);
        for c in "E0".chars() {
            state.on_key(char_key(c), &mut draft);
        }
        state.on_key(key(KeyCode::Enter), &mut draft);
        assert!(state.edit.is_some(), "輸入不完整，Enter 要被拒絕、留在原地");
        assert!(draft.edits.is_empty());
    }

    #[test]
    fn esc_from_edit_keeps_the_selected_row() {
        let mut state = ColorEditorState::new(&test_defaults());
        let mut draft = Draft::new();
        state.on_key(key(KeyCode::Down), &mut draft);
        state.on_key(key(KeyCode::Down), &mut draft);
        assert_eq!(state.list.selected(), Some(2));

        enter_edit(&mut state, &mut draft);
        assert!(matches!(
            state.on_key(key(KeyCode::Esc), &mut draft),
            Flow::Continue
        ));
        assert!(state.edit.is_none());
        assert_eq!(state.list.selected(), Some(2), "取消編輯不該移動選取列");
    }

    #[test]
    fn revert_removes_the_edit_instead_of_writing_null() {
        let mut state = ColorEditorState::new(&test_defaults());
        let mut draft = Draft::new();
        enter_edit(&mut state, &mut draft);
        state.on_key(key(KeyCode::Right), &mut draft);
        state.on_key(key(KeyCode::Enter), &mut draft);
        assert!(draft.edits.contains_key(&field_key(0)));

        state.on_key(char_key('r'), &mut draft);
        assert!(
            !draft.edits.contains_key(&field_key(0)),
            "r 是移除這個鍵，不是寫 Some(None)"
        );
        assert_eq!(state.colors.values[0], state.base.values[0]);
    }
}

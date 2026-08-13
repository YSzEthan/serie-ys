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
    color_to_config_string, parse_rgba_color, preview_block, ColorTheme, FlatColors, GraphColor,
    PreviewBlock, COLOR_KEYS, NAMED_COLORS,
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
            ColorEdit::Hex(input) => match key.code {
                KeyCode::Backspace => {
                    input.handle_event(&Event::Key(key));
                }
                // a-f 是合法輸入字元，不能當導覽鍵（這個變體本來就沒有定義
                // 任何導覽語意）。
                _ => push_hex_digit(input, key, 6),
            },
        }
    }
}

/// Hex 輸入：a-f 是資料不是導覽鍵，且不能被 ctrl 修飾字誤觸——`ctrl-f`
/// 不該被當成輸入 'f'。`max_len` 讓平面色（6 碼）與分支色（含 alpha，
/// 8 碼）共用同一條規則。
fn push_hex_digit(input: &mut tui_input::Input, key: KeyEvent, max_len: usize) {
    let plain = key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;
    let KeyCode::Char(c) = key.code else {
        return;
    };
    if plain && c.is_ascii_hexdigit() && input.value().len() < max_len {
        input.handle_event(&Event::Key(key));
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
    /// Fields 清單，43 個平面色鍵 + 第 44 列（`[color.graph].branches` 入口）。
    list: ListState,
    /// `Some` = 正在編輯 `list.selected()` 那一欄（只在 `list` 選到 0..43
    /// 範圍內的平面色鍵時才會是 `Some`；第 44 列按 Enter 開的是
    /// `branches`，不是這個）。
    edit: Option<ColorEdit>,
    /// `Some` = 在 `[color.graph].branches` 子畫面。分支色是純字串（8 位
    /// hex 的 alpha 位元組 `RatatuiColor` 表示不了），跟 `ColorEdit` 的
    /// Named/Indexed/Hex 三態不是同一種東西，所以自成一套獨立狀態，不跟
    /// `list`／`edit` 混用。
    branches: Option<BranchesEditor>,
    /// `true` = 設定檔載入失敗，`base`/`colors` 是內建硬預設，不是使用者的
    /// 真實設定——`render` 要據此在預覽區上方講清楚。
    theme_is_fallback: bool,
}

/// `[color.graph].branches` 子畫面自己的清單與編輯狀態。
struct BranchesEditor {
    /// 編輯中的草稿。每次 `a`／`d`／cell 編輯確認後立刻同步進
    /// `ColorEditorState.colors.graph.branches` 與 `draft.edits`——不像
    /// 平面色欄位那樣要按 Enter 才落地，因為這裡沒有「打到一半」的整陣列
    /// 概念，`a`／`d` 本身就是離散、已完成的動作。
    values: Vec<String>,
    list: ListState,
    /// `Some` = 正在編輯選到的那一格：原始 hex 字串，不經過 `RatatuiColor`
    /// （8 位 alpha `RatatuiColor` 表示不了，見必讀事實 5）。
    edit: Option<tui_input::Input>,
}

impl BranchesEditor {
    fn move_selection(&mut self, delta: i32) {
        super::clamped_move(&mut self.list, delta, self.values.len());
    }
}

/// garde 的 pattern 是 `^#([0-9a-fA-F]{6}|[0-9a-fA-F]{8})$`；這裡只驗語法，
/// 不經過 `RatatuiColor`（alpha 會被砍掉）。回傳統一成大寫的 `#XXXXXX`／
/// `#XXXXXXXX`，跟範本的拼法一致。
fn valid_branch_hex(s: &str) -> Option<String> {
    let len = s.len();
    if (len == 6 || len == 8) && s.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(format!("#{}", s.to_uppercase()))
    } else {
        None
    }
}

/// 分支色清單裡每一格的色塊。解析失敗（理論上不會，garde 已經擋在前面）
/// 就顯示 `DarkGray`，不 panic。
fn branch_swatch_color(hex: &str) -> RatatuiColor {
    parse_rgba_color(hex)
        .map(GraphColor::to_ratatui_color)
        .unwrap_or(RatatuiColor::DarkGray)
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
            branches: None,
            theme_is_fallback: defaults.config_is_fallback,
        }
    }

    /// 零 I/O 的純狀態轉移，可以直接餵 `KeyEvent` 測試——跟
    /// `WizardState::on_key` 同一個模式。
    pub fn on_key(&mut self, key: KeyEvent, draft: &mut Draft) -> Flow {
        if key.kind != KeyEventKind::Press {
            return Flow::Continue;
        }

        if self.branches.is_some() {
            return self.on_branches_key(key, draft);
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
            // `on_key` 是零 I/O 純函式，拿不到 viewport 高度；43+1 列分
            // 四頁多，固定步長夠用，不值得為它在 render 時回存高度。
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
                self.list.select(Some(COLOR_KEYS.len()));
                Flow::Continue
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                let idx = self.list.selected().unwrap_or(0);
                if idx == COLOR_KEYS.len() {
                    self.open_branches();
                } else {
                    self.edit = Some(ColorEdit::from_current(self.colors.values[idx]));
                }
                Flow::Continue
            }
            // 43 個欄位配一個手打 hex 的輸入框，誤按是必然的，而 `edits`
            // 對顏色沒有其他移除路徑。第 44 列（branches 入口）沒有單一
            // 「原值」可還原，這裡直接跳過。
            KeyCode::Char('r') => {
                let idx = self.list.selected().unwrap_or(0);
                if idx < COLOR_KEYS.len() {
                    self.revert_selected(draft);
                }
                Flow::Continue
            }
            _ => Flow::Continue,
        }
    }

    fn open_branches(&mut self) {
        let mut list = ListState::default();
        list.select(Some(0));
        self.branches = Some(BranchesEditor {
            values: self.colors.graph.branches.clone(),
            list,
            edit: None,
        });
    }

    /// `[color.graph].branches` 子畫面的按鍵處理。巢狀的「正在編輯某一格」
    /// 交給 `on_branch_cell_key`，這裡只管清單層級的導覽與 a/d。
    fn on_branches_key(&mut self, key: KeyEvent, draft: &mut Draft) -> Flow {
        let editing_cell = self.branches.as_ref().is_some_and(|b| b.edit.is_some());
        if editing_cell {
            self.on_branch_cell_key(key, draft);
            return Flow::Continue;
        }

        // ctrl-c / ctrl-d / Esc / h / ←：都是退一層（離開 branches 子畫面），
        // 不是離開整個顏色編輯器。
        if super::is_abort_key(&key)
            || matches!(key.code, KeyCode::Esc | KeyCode::Left | KeyCode::Char('h'))
        {
            self.branches = None;
            return Flow::Continue;
        }

        let Some(b) = &mut self.branches else {
            return Flow::Continue;
        };
        let mut changed = false;
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => b.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => b.move_selection(1),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                let idx = b.list.selected().unwrap_or(0);
                let current = b.values[idx].trim_start_matches('#').to_string();
                b.edit = Some(tui_input::Input::new(current));
            }
            KeyCode::Char('a') => {
                let idx = b.list.selected().unwrap_or(0);
                let clone = b.values[idx].clone();
                b.values.insert(idx + 1, clone);
                b.list.select(Some(idx + 1));
                changed = true;
            }
            // 刪到剩 1 格要拒絕：garde 是 `length(min = 1)`，空陣列會讓
            // 整份設定檔載入失敗，`GraphColorSet::get()` 對空 Vec 直接
            // panic。這條路徑不跑 garde（`set_color` 那條 serde 路徑只在
            // `From` 之後才 validate），wizard 必須自己擋。
            KeyCode::Char('d') if b.values.len() > 1 => {
                let idx = b.list.selected().unwrap_or(0);
                b.values.remove(idx);
                b.list.select(Some(idx.min(b.values.len() - 1)));
                changed = true;
            }
            _ => {}
        }
        if changed {
            self.sync_branches(draft);
        }
        Flow::Continue
    }

    fn on_branch_cell_key(&mut self, key: KeyEvent, draft: &mut Draft) {
        if super::is_abort_key(&key) || key.code == KeyCode::Esc {
            if let Some(b) = &mut self.branches {
                b.edit = None;
            }
            return;
        }

        let Some(b) = &mut self.branches else {
            return;
        };
        let Some(input) = &mut b.edit else {
            return;
        };

        match key.code {
            KeyCode::Enter => {
                // 缺什麼由對話框的訊息行顯示，留在原地。
                let Some(hex) = valid_branch_hex(input.value()) else {
                    return;
                };
                let idx = b.list.selected().unwrap_or(0);
                b.values[idx] = hex;
                b.edit = None;
                self.sync_branches(draft);
            }
            KeyCode::Backspace => {
                input.handle_event(&Event::Key(key));
            }
            // Hex 模式下 a-f 是合法輸入字元，上限含 alpha 的 8 碼。
            _ => push_hex_digit(input, key, 8),
        }
    }

    /// 把 branches 子畫面的草稿同步進 `colors.graph.branches`（清單頁與
    /// 預覽讀的是這份）與 `draft.edits`（寫回設定檔）。`a`／`d`／cell 編輯
    /// 確認後都要呼叫——每個動作本身就是已完成的變更，沒有「打到一半」
    /// 的整陣列概念，不需要像平面色欄位那樣另外等一個 Enter。
    fn sync_branches(&mut self, draft: &mut Draft) {
        let Some(branches) = &self.branches else {
            return;
        };
        let values = branches.values.clone();
        self.colors.graph.branches = values.clone();
        let key = ConfigKey {
            table: super::COLOR_GRAPH,
            key: "branches".into(),
        };
        draft
            .edits
            .insert(key, Some(super::multiline_string_array(&values)));
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
        // +1 為第 44 列（`[color.graph].branches` 入口）。
        super::clamped_move(&mut self.list, delta, COLOR_KEYS.len() + 1);
    }

    fn commit_field(&mut self, draft: &mut Draft, idx: usize) {
        let key = ConfigKey {
            table: COLOR,
            key: COLOR_KEYS[idx].into(),
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
            key: COLOR_KEYS[idx].into(),
        };
        draft.edits.remove(&key);
        self.colors.values[idx] = self.base.values[idx];
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect, chrome: &ColorTheme) {
        if let Some(branches) = &mut self.branches {
            render_branches(f, area, branches, chrome);
            return;
        }

        let [main_area, hint_area] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);
        let [list_area, preview_area] =
            Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
                .areas(main_area);

        let selected = self.list.selected().unwrap_or(0);
        let total = COLOR_KEYS.len() + 1;

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
            .chain(std::iter::once(branches_row_item(
                &self.colors.graph.branches,
                &self.base.graph.branches,
            )))
            .collect();
        let list = super::styled_list(items, chrome).block(
            Block::default()
                .title(format!(" 顏色 [{}/{}] ", selected + 1, total))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(chrome.divider_fg)),
        );
        f.render_stateful_widget(list, list_area, &mut self.list);

        let mut preview_text: Vec<Line> = Vec::new();
        if self.theme_is_fallback {
            preview_text
                .push(Line::raw("設定檔載入失敗，以下是內建預設").fg(chrome.status_warn_fg));
        }
        if selected < COLOR_KEYS.len() {
            // 選中欄位若正在編輯，預覽要即時反映輸入到一半的值——
            // `self.colors` 本身直到 Enter 才會被寫入，這裡只在預覽用的
            // 副本上覆蓋，不動草稿。
            let mut preview_values = self.colors.values.clone();
            preview_values[selected] = self.live_value(selected);
            let preview_colors = FlatColors {
                values: preview_values,
                graph: self.colors.graph.clone(),
            };
            let focus = preview_block(COLOR_KEYS[selected]);
            preview_text.extend(preview_lines(&preview_colors, focus));
        } else {
            preview_text.push(Line::raw("[color.graph].branches"));
            preview_text.push(Line::from(
                self.colors
                    .graph
                    .branches
                    .iter()
                    .map(|hex| Span::styled("■ ", Style::default().fg(branch_swatch_color(hex))))
                    .collect::<Vec<_>>(),
            ));
            preview_text.push(Line::raw("Enter 進入編輯"));
        }
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

fn branches_row_item(current: &[String], base: &[String]) -> ListItem<'static> {
    let touched = current != base;
    let marker = if touched { "✓ " } else { "  " };
    ListItem::new(Line::raw(format!(
        "{marker}[color.graph].branches  ({} 色)",
        current.len()
    )))
}

fn render_branches(f: &mut Frame, area: Rect, branches: &mut BranchesEditor, chrome: &ColorTheme) {
    let [list_area, hint_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);

    let items: Vec<ListItem> = branches
        .values
        .iter()
        .enumerate()
        .map(|(i, hex)| {
            ListItem::new(Line::from(vec![
                Span::raw(format!("{:>2}  ", i + 1)),
                Span::styled("■ ", Style::default().fg(branch_swatch_color(hex))),
                Span::raw(hex.clone()),
            ]))
        })
        .collect();
    let title = format!(
        " [color.graph].branches [{}/{}] ",
        branches.list.selected().unwrap_or(0) + 1,
        branches.values.len()
    );
    let list = super::styled_list(items, chrome).block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(chrome.divider_fg)),
    );
    f.render_stateful_widget(list, list_area, &mut branches.list);

    // 常駐約束，不是事件訊息：刪到剩最後一格時提示會自動變，不需要另外
    // 存一個「剛才被拒絕」的旗標、也不需要清除規則。
    let delete_hint: &str = if branches.values.len() == 1 {
        "至少保留 1 色"
    } else {
        "刪除"
    };
    let hint = crate::widget::hint_line(
        chrome,
        &[
            ("Enter/l".into(), "編輯"),
            ("a".into(), "新增"),
            ("d".into(), delete_hint),
            ("Esc/h".into(), "返回"),
        ],
        chrome.help_key_fg,
    );
    f.render_widget(Paragraph::new(hint), hint_area);

    if let Some(edit) = &branches.edit {
        let idx = branches.list.selected().unwrap_or(0);
        render_branch_edit_dialog(f, area, idx, edit, chrome);
    }
}

fn render_branch_edit_dialog(
    f: &mut Frame,
    area: Rect,
    index: usize,
    edit: &tui_input::Input,
    chrome: &ColorTheme,
) {
    let value_line = format!("> #{}", edit.value());
    let message = if valid_branch_hex(edit.value()).is_some() {
        String::new()
    } else {
        format!("需要 6 或 8 碼 hex，目前 {} 碼", edit.value().len())
    };

    let hint = crate::widget::hint_line(
        chrome,
        &[("Enter".into(), "確認"), ("Esc".into(), "取消")],
        chrome.help_key_fg,
    );

    let dialog_width = (hint.width() as u16 + 2)
        .max(30)
        .min(area.width.saturating_sub(4));
    let dialog_height = 5u16.min(area.height.saturating_sub(2));
    let dialog_area = super::centered_rect(area, dialog_width, dialog_height);

    f.render_widget(Clear, dialog_area);
    let block = Block::default()
        .title(format!(" branches[{}] ", index + 1))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(chrome.divider_fg))
        .style(Style::default().bg(chrome.bg).fg(chrome.fg));
    let inner = block.inner(dialog_area);
    f.render_widget(block, dialog_area);

    let [value_area, message_area, hint_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    f.render_widget(Paragraph::new(Line::raw(value_line)), value_area);
    f.render_widget(
        Paragraph::new(Line::raw(message)).fg(chrome.status_warn_fg),
        message_area,
    );
    f.render_widget(Paragraph::new(hint), hint_area);

    f.set_cursor_position((value_area.x + 3 + edit.visual_cursor() as u16, value_area.y));
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
            key: COLOR_KEYS[idx].into(),
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

    // ── [color.graph].branches（commit 2） ──

    fn branches_key() -> ConfigKey {
        ConfigKey {
            table: super::super::COLOR_GRAPH,
            key: "branches".into(),
        }
    }

    fn enter_branches(state: &mut ColorEditorState, draft: &mut Draft) {
        state.list.select(Some(COLOR_KEYS.len()));
        assert!(matches!(
            state.on_key(key(KeyCode::Enter), draft),
            Flow::Continue
        ));
        assert!(state.branches.is_some(), "應該進了 branches 子畫面");
    }

    #[test]
    fn graph_branches_round_trips_and_stays_multiline() {
        let mut state = ColorEditorState::new(&test_defaults());
        let mut draft = Draft::new();
        enter_branches(&mut state, &mut draft);

        state.on_key(key(KeyCode::Enter), &mut draft); // 編輯第 1 格
        assert!(state.branches.as_ref().unwrap().edit.is_some());
        // 跟平面色的 HEX 模式一樣，輸入框預填現值（方便微調）——先清空
        // 再打新值，不是接在後面。
        for _ in 0..8 {
            state.on_key(key(KeyCode::Backspace), &mut draft);
        }
        for c in "AABBCC".chars() {
            state.on_key(char_key(c), &mut draft);
        }
        state.on_key(key(KeyCode::Enter), &mut draft); // 確認

        assert_eq!(state.branches.as_ref().unwrap().values[0], "#AABBCC");

        let updated = super::super::apply_touched_settings(&draft, "").unwrap();
        assert!(updated.contains("#AABBCC"), "{updated}");
        // 每個元素自己一行，不是壓成單行——跟範本的排版一致。
        assert!(updated.contains("branches = [\n"), "{updated}");

        let theme = crate::config::parse_color(&updated).unwrap();
        assert_eq!(theme.graph.branches[0], "#AABBCC");
        assert_eq!(theme.graph.branches.len(), 6);
    }

    #[test]
    fn branch_delete_refuses_to_empty_the_list() {
        let mut state = ColorEditorState::new(&test_defaults());
        let mut draft = Draft::new();
        enter_branches(&mut state, &mut draft);

        let default_len = state.branches.as_ref().unwrap().values.len();
        for _ in 0..default_len + 3 {
            state.on_key(char_key('d'), &mut draft);
        }
        assert_eq!(
            state.branches.as_ref().unwrap().values.len(),
            1,
            "刪到剩 1 格後再按 d 不該繼續刪"
        );
        // 空陣列一旦寫回，garde `length(min=1)` 會讓整份設定檔載入失敗——
        // 這裡順便確認最後一次成功的刪除有同步進 edits。
        assert!(draft.edits.contains_key(&branches_key()));
        let updated = super::super::apply_touched_settings(&draft, "").unwrap();
        let theme = crate::config::parse_color(&updated).unwrap();
        assert_eq!(theme.graph.branches.len(), 1);
    }

    #[test]
    fn branch_edit_is_locked_to_hex() {
        let mut state = ColorEditorState::new(&test_defaults());
        let mut draft = Draft::new();
        enter_branches(&mut state, &mut draft);
        state.on_key(key(KeyCode::Enter), &mut draft);

        // 輸入框預填現值（"E06C76"）；非 hex 字元（g、z）不該改動它——
        // 分支色從頭到尾只有一種表示法，沒有 Tab 可以切走。
        let seeded = state
            .branches
            .as_ref()
            .unwrap()
            .edit
            .as_ref()
            .unwrap()
            .value()
            .to_string();
        state.on_key(char_key('g'), &mut draft);
        state.on_key(char_key('z'), &mut draft);
        let input = state.branches.as_ref().unwrap().edit.as_ref().unwrap();
        assert_eq!(input.value(), seeded, "非 hex 字元不該被接受");

        for _ in 0..8 {
            state.on_key(key(KeyCode::Backspace), &mut draft);
        }
        for c in "aabbcc".chars() {
            state.on_key(char_key(c), &mut draft);
        }
        let input = state.branches.as_ref().unwrap().edit.as_ref().unwrap();
        assert_eq!(input.value(), "aabbcc");
    }

    #[test]
    fn branch_edit_preserves_the_alpha_channel() {
        let mut state = ColorEditorState::new(&test_defaults());
        let mut draft = Draft::new();
        enter_branches(&mut state, &mut draft);
        state.on_key(key(KeyCode::Enter), &mut draft);
        // 輸入框預填現值（6 碼），先清空再打 8 碼含 alpha 的新值。
        for _ in 0..8 {
            state.on_key(key(KeyCode::Backspace), &mut draft);
        }
        for c in "AABBCCDD".chars() {
            // 8 位含 alpha
            state.on_key(char_key(c), &mut draft);
        }
        state.on_key(key(KeyCode::Enter), &mut draft);

        assert_eq!(
            state.branches.as_ref().unwrap().values[0],
            "#AABBCCDD",
            "8 位 hex 的 alpha 位元組要保留，不能被砍成 6 位——這條路徑不經過 \
             RatatuiColor（parse_hex_color 只認 7 字元的 #RRGGBB）"
        );
    }

    #[test]
    fn esc_pops_one_screen_at_a_time() {
        let mut state = ColorEditorState::new(&test_defaults());
        let mut draft = Draft::new();
        enter_branches(&mut state, &mut draft);
        state.on_key(key(KeyCode::Enter), &mut draft); // 進 cell edit

        assert!(matches!(
            state.on_key(key(KeyCode::Esc), &mut draft),
            Flow::Continue
        ));
        assert!(state.branches.as_ref().unwrap().edit.is_none());
        assert!(state.branches.is_some(), "第一次 Esc 只離開 cell edit");

        assert!(matches!(
            state.on_key(key(KeyCode::Esc), &mut draft),
            Flow::Continue
        ));
        assert!(state.branches.is_none(), "第二次 Esc 離開 branches 子畫面");

        assert!(matches!(
            state.on_key(key(KeyCode::Esc), &mut draft),
            Flow::Back
        ));
    }

    /// `preview_lines` 用一串字串字面值查 `COLOR_KEYS`（`color_at` 找不到
    /// 就 `expect` panic）。這條測試把整個函式跑過一遍——任何一個鍵名
    /// 打錯字，這裡就會 panic，不必等到真的打開顏色編輯器才發現。三種
    /// `PreviewBlock` 都跑一次：目前 `focus` 只影響標記前綴、不影響用到
    /// 哪些鍵，但這樣測就算未來有人改成 focus 相依的內容，覆蓋率也不會
    /// 悄悄漏掉。
    #[test]
    fn preview_lines_uses_only_real_color_keys() {
        let colors = FlatColors::from(&ColorTheme::default());
        for focus in [
            PreviewBlock::List,
            PreviewBlock::Detail,
            PreviewBlock::Status,
        ] {
            let lines = preview_lines(&colors, focus);
            assert!(!lines.is_empty());
        }
    }
}

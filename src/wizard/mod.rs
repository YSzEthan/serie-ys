pub(crate) mod path_browser;

use std::io::Write;

use clap::{Parser, ValueEnum};
use ratatui::{
    crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::Line,
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    DefaultTerminal, Frame,
};
use tui_input::backend::crossterm::EventHandler;

use crate::{
    color::ColorTheme, Args, CommitOrderType, CompactType, GraphStyle, GraphWidthType,
    InitialSelection,
};

/// -h 在 TTY 下的入口。回傳 `None` = 使用者放棄（等同原本 `--help` 印完離開，
/// `run()` 收到後直接 `return Ok(())`）；`Some(args)` = 使用者選好了，直接接續
/// `run()` 274 行以後的邏輯。
///
/// 完全不依賴 `config::load()`：固定用 `ColorTheme::default()` 畫面，這樣 config
/// 檔壞掉也不會連 `-h` 都叫不出來 —— 那正是使用者最需要它的時候。
pub fn run() -> crate::Result<Option<Args>> {
    let theme = ColorTheme::default();
    let mut terminal = ratatui::init();
    let outcome = wizard_loop(&mut terminal, WizardState::new(), &theme);
    ratatui::restore();
    drop(terminal); // 游標要等這裡才會被叫回來，print 必須在這之後才做

    match outcome? {
        None => Ok(None),
        Some((draft, print)) => {
            if print {
                println!("{}", format_equivalent_command(&draft));
                print!("按 Enter 繼續啟動 ysgit... ");
                std::io::stdout().flush().ok();
                let mut discard = String::new();
                std::io::stdin().read_line(&mut discard).ok();
            }
            Ok(Some(draft))
        }
    }
}

fn wizard_loop(
    terminal: &mut DefaultTerminal,
    mut state: WizardState,
    theme: &ColorTheme,
) -> crate::Result<Option<(Args, bool)>> {
    loop {
        terminal.draw(|f| state.render(f, f.area(), theme))?;
        let Event::Key(key) = ratatui::crossterm::event::read()? else {
            continue;
        };
        match state.on_key(key) {
            Flow::Continue => {}
            Flow::Abort => return Ok(None),
            Flow::Launch { print } => return Ok(Some((state.draft, print))),
            Flow::OpenPath => {
                let start = path_browser::start_dir(&state.draft.path);
                if let Some(p) = path_browser::run(terminal, &start, theme)? {
                    state.draft.path = p.to_string_lossy().into_owned();
                }
                terminal.clear()?;
            }
            Flow::OpenMaxCount => {
                match run_number_input(terminal, state.draft.max_count, theme)? {
                    NumberFlow::Cancelled => {}
                    NumberFlow::Cleared => state.draft.max_count = None,
                    NumberFlow::Set(n) => state.draft.max_count = Some(n),
                }
                terminal.clear()?;
            }
        }
    }
}

fn is_abort_key(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d'))
        && key.modifiers.contains(KeyModifiers::CONTROL)
}

fn variant_name<T: ValueEnum>(v: &T) -> String {
    v.to_possible_value()
        .expect("wizard 用到的 ValueEnum 沒有任何變體用 #[value(skip)]")
        .get_name()
        .to_string()
}

fn order_desc(v: CommitOrderType) -> &'static str {
    match v {
        CommitOrderType::Chrono => "時間序",
        CommitOrderType::Topo => "拓撲序",
    }
}

fn graph_width_desc(v: GraphWidthType) -> &'static str {
    match v {
        GraphWidthType::Auto => "自動",
        GraphWidthType::Double => "寬版",
        GraphWidthType::Single => "窄版",
    }
}

fn compact_desc(v: CompactType) -> &'static str {
    match v {
        CompactType::Auto => "自動",
        CompactType::On => "開啟",
        CompactType::Off => "關閉",
    }
}

fn graph_style_desc(v: GraphStyle) -> &'static str {
    match v {
        GraphStyle::Rounded => "圓角",
        GraphStyle::Angular => "直角",
        GraphStyle::Ascii => "ASCII",
    }
}

fn initial_selection_desc(v: InitialSelection) -> &'static str {
    match v {
        InitialSelection::Latest => "最新",
        InitialSelection::Head => "HEAD",
    }
}

// ---------------------------------------------------------------------------
// 單層清單：四個 `<TYPE>` 欄位用 ←/→ 在原地輪迴切換值。循環只在該欄位的
// N 個合法值之間繞（不含「未設定」），還沒碰過的欄位一按 →／← 就直接落在
// 第一個／最後一個值 —— 循環站數等於真正的選項數，不會多一站看起來像
// 「N+1 個選項」。碰過之後就沒有回到「未設定」的路：選錯了就繼續循環到
// 想要的那個值，不是退回預設。
// ---------------------------------------------------------------------------

/// 在 `T::value_variants()` 上把 `*slot` 往 `delta` 方向移一站，寫回去的
/// 結果永遠是 `Some`。`*slot` 是 `None`（還沒碰過這個欄位）時，把它當成
/// 「已經站在 `default` 那一格」來算下一步 —— 不然第一次按 → 會落在跟畫面
/// 上顯示的預設值一模一樣的格子（因為預設值本身就是 `value_variants()` 的
/// 第一個），數值沒變、只是多了個勾，等於白按一次。這樣算，第一次按不管
/// 哪個方向都保證換到一個不一樣的值。
///
/// 四個 `<TYPE>` 欄位共用同一份算術，只是各自的 `T`／`default` 不同，所以
/// 抽成吃 `&mut Option<T>` 的自由函式而不是把 `FieldKind` 本身泛型化——後者
/// 才會讓型別設計變複雜，這裡型別完全由呼叫端推導。
fn cycle_value<T: ValueEnum + Copy + PartialEq>(slot: &mut Option<T>, default: T, delta: i32) {
    let variants = T::value_variants();
    let index_of = |v: T| {
        variants
            .iter()
            .position(|&x| x == v)
            .expect("值一定是合法變體之一")
    };
    let current = index_of(slot.unwrap_or(default)) as i32;
    let next = (current + delta).rem_euclid(variants.len() as i32);
    *slot = Some(variants[next as usize]);
}

/// (目前有效值的中文說明, 是不是使用者這次 session 主動選的)。未設定時
/// 顯示的仍是這個欄位真正的預設值（跟 `run()` 裡 `config::load()` 之後
/// `.or(core_config.option.*)` 鏈最終落地的那個值一致），不是「使用預設值」
/// 這種空話。
fn resolve_desc<T: Copy>(
    slot: Option<T>,
    default: T,
    desc: fn(T) -> &'static str,
) -> (&'static str, bool) {
    (desc(slot.unwrap_or(default)), slot.is_some())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FieldKind {
    Order,
    GraphWidth,
    Compact,
    GraphStyle,
    InitialSelection,
}

impl FieldKind {
    fn flags(self) -> &'static str {
        match self {
            FieldKind::Order => "-o, --order",
            FieldKind::GraphWidth => "-g, --graph-width",
            FieldKind::Compact => "-c, --compact",
            FieldKind::GraphStyle => "-s, --graph-style",
            FieldKind::InitialSelection => "-i, --initial-selection",
        }
    }

    fn help(self) -> &'static str {
        match self {
            FieldKind::Order => "Commit 排序演算法",
            FieldKind::GraphWidth => "Commit 圖形格子寬度",
            FieldKind::Compact => "緊湊模式",
            FieldKind::GraphStyle => "Commit 圖形邊線風格",
            FieldKind::InitialSelection => "初始選取的 commit",
        }
    }

    /// `delta = 1` 往前一站（→／Enter），`delta = -1` 往後一站（←）。
    fn cycle(self, draft: &mut Args, delta: i32) {
        match self {
            FieldKind::Order => cycle_value(&mut draft.order, CommitOrderType::Chrono, delta),
            FieldKind::GraphWidth => {
                cycle_value(&mut draft.graph_width, GraphWidthType::Auto, delta)
            }
            FieldKind::Compact => cycle_value(&mut draft.compact, CompactType::Auto, delta),
            FieldKind::GraphStyle => {
                cycle_value(&mut draft.graph_style, GraphStyle::Rounded, delta)
            }
            FieldKind::InitialSelection => cycle_value(
                &mut draft.initial_selection,
                InitialSelection::Latest,
                delta,
            ),
        }
    }

    fn current(self, draft: &Args) -> (&'static str, bool) {
        match self {
            FieldKind::Order => resolve_desc(draft.order, CommitOrderType::Chrono, order_desc),
            FieldKind::GraphWidth => {
                resolve_desc(draft.graph_width, GraphWidthType::Auto, graph_width_desc)
            }
            FieldKind::Compact => resolve_desc(draft.compact, CompactType::Auto, compact_desc),
            FieldKind::GraphStyle => {
                resolve_desc(draft.graph_style, GraphStyle::Rounded, graph_style_desc)
            }
            FieldKind::InitialSelection => resolve_desc(
                draft.initial_selection,
                InitialSelection::Latest,
                initial_selection_desc,
            ),
        }
    }
}

enum TopRowAction {
    OpenPath,
    OpenMaxCount,
    Field(FieldKind),
    Launch { print: bool },
}

struct TopRow {
    action: TopRowAction,
    flags: &'static str,
    help: &'static str,
}

fn build_top_rows() -> Vec<TopRow> {
    vec![
        TopRow {
            action: TopRowAction::OpenPath,
            flags: "[PATH]",
            help: "git 倉庫路徑",
        },
        TopRow {
            action: TopRowAction::OpenMaxCount,
            flags: "-n, --max-count <NUMBER>",
            help: "要渲染的最大 commit 數量",
        },
        TopRow {
            action: TopRowAction::Field(FieldKind::Order),
            flags: FieldKind::Order.flags(),
            help: FieldKind::Order.help(),
        },
        TopRow {
            action: TopRowAction::Field(FieldKind::GraphWidth),
            flags: FieldKind::GraphWidth.flags(),
            help: FieldKind::GraphWidth.help(),
        },
        TopRow {
            action: TopRowAction::Field(FieldKind::Compact),
            flags: FieldKind::Compact.flags(),
            help: FieldKind::Compact.help(),
        },
        TopRow {
            action: TopRowAction::Field(FieldKind::GraphStyle),
            flags: FieldKind::GraphStyle.flags(),
            help: FieldKind::GraphStyle.help(),
        },
        TopRow {
            action: TopRowAction::Field(FieldKind::InitialSelection),
            flags: FieldKind::InitialSelection.flags(),
            help: FieldKind::InitialSelection.help(),
        },
        TopRow {
            action: TopRowAction::Launch { print: false },
            flags: "▶ 啟動 ysgit",
            help: "",
        },
        TopRow {
            action: TopRowAction::Launch { print: true },
            flags: "▶ 啟動 ysgit（先印出等效指令字串）",
            help: "",
        },
    ]
}

/// 每一列的顯示文字：PATH／MaxCount／`<TYPE>` 欄位都附上目前有效值（明確的
/// 內容，不是「使用預設值」這種空話）；`<TYPE>` 欄位額外用打勾標示「這是
/// 使用者主動選的」。
fn top_row_label(row: &TopRow, draft: &Args) -> String {
    let (checked, body) = match row.action {
        TopRowAction::OpenPath => (
            false,
            format!("{}  {}（目前：{}）", row.flags, row.help, draft.path),
        ),
        TopRowAction::OpenMaxCount => (
            false,
            format!(
                "{}  {}（目前：{}）",
                row.flags,
                row.help,
                draft
                    .max_count
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "不限制".to_string())
            ),
        ),
        TopRowAction::Field(field) => {
            let (desc, explicit) = field.current(draft);
            (
                explicit,
                format!("{}  {}（目前：{}）", row.flags, row.help, desc),
            )
        }
        TopRowAction::Launch { .. } => (false, row.flags.to_string()),
    };
    let prefix = if checked { "✓ " } else { "  " };
    format!("{prefix}{body}")
}

enum Flow {
    Continue,
    Abort,
    Launch { print: bool },
    OpenPath,
    OpenMaxCount,
}

struct WizardState {
    draft: Args,
    rows: Vec<TopRow>,
    list: ListState,
}

impl WizardState {
    fn new() -> Self {
        let draft = Args::try_parse_from(["ysgit"]).expect("無參數的 parse 一定要成功");
        let mut list = ListState::default();
        list.select(Some(0));
        Self {
            draft,
            rows: build_top_rows(),
            list,
        }
    }

    /// 零 I/O 的純狀態轉移，可以直接餵 `KeyEvent` 測試 —— 純粹的清單瀏覽，
    /// ↑↓ 跟 vim 的 k/j 等價、←→ 跟 h/l 等價。
    fn on_key(&mut self, key: KeyEvent) -> Flow {
        if key.kind != KeyEventKind::Press {
            return Flow::Continue;
        }
        if is_abort_key(&key) {
            return Flow::Abort;
        }
        match key.code {
            KeyCode::Esc => Flow::Abort,
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                Flow::Continue
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                Flow::Continue
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.cycle_selected(1);
                self.activate_selected()
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.cycle_selected(-1);
                Flow::Continue
            }
            KeyCode::Enter => self.activate_selected(),
            _ => Flow::Continue,
        }
    }

    fn move_selection(&mut self, delta: i32) {
        let len = self.rows.len() as i32;
        let current = self.list.selected().unwrap_or(0) as i32;
        let next = (current + delta).clamp(0, len - 1);
        self.list.select(Some(next as usize));
    }

    /// ← 或「→ 之前」呼叫：如果選中的是 `<TYPE>` 欄位，往 `delta` 方向輪迴
    /// 切換一站；其餘欄位（PATH／MaxCount／Launch）不受影響。
    fn cycle_selected(&mut self, delta: i32) {
        let Some(row_idx) = self.list.selected() else {
            return;
        };
        if let TopRowAction::Field(field) = self.rows[row_idx].action {
            field.cycle(&mut self.draft, delta);
        }
    }

    /// Enter 與 →／l 都會呼叫（→／l 先呼叫 `cycle_selected` 切換值，這裡
    /// 再處理「有明確終點動作」的列）。PATH／MaxCount 開對應的子畫面；
    /// Launch 直接啟動——兩個觸發鍵沒有差別待遇。`<TYPE>` 欄位在這裡是
    /// no-op：切換已經在 `cycle_selected` 做完了，這裡不用再做事。
    fn activate_selected(&mut self) -> Flow {
        let Some(row_idx) = self.list.selected() else {
            return Flow::Continue;
        };
        match self.rows[row_idx].action {
            TopRowAction::OpenPath => Flow::OpenPath,
            TopRowAction::OpenMaxCount => Flow::OpenMaxCount,
            TopRowAction::Field(_) => Flow::Continue,
            TopRowAction::Launch { print } => Flow::Launch { print },
        }
    }

    fn render(&mut self, f: &mut Frame, area: Rect, theme: &ColorTheme) {
        let [list_area, hint_area] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);

        let items: Vec<ListItem> = self
            .rows
            .iter()
            .map(|row| ListItem::new(top_row_label(row, &self.draft)))
            .collect();
        f.render_stateful_widget(styled_list(items, theme), list_area, &mut self.list);

        let hint = crate::widget::build_hint_line(
            theme,
            &[
                ("↑↓/kj", "選擇"),
                ("←→/hl", "切換選項"),
                ("Enter", "開啟/啟動"),
                ("Esc/Ctrl-C", "離開"),
            ],
        );
        f.render_widget(Paragraph::new(hint), hint_area);
    }
}

fn styled_list<'a>(items: Vec<ListItem<'a>>, theme: &ColorTheme) -> List<'a> {
    List::new(items).highlight_style(
        Style::default()
            .bg(theme.list_selected_bg)
            .fg(theme.list_selected_fg),
    )
}

// ---------------------------------------------------------------------------
// -n/--max-count 的數字輸入彈窗。←/↓（含 vim 的 h/j）減一，→/↑（含 vim 的
// l/k）加一；打字仍然可以直接輸入精確數字，但只收數字字元。
// ---------------------------------------------------------------------------

enum NumberFlow {
    Cancelled,
    Cleared,
    Set(usize),
}

/// 純函式，可測。空字串當 0 處理；減到 0 就不再往下（`max_count` 是
/// `usize`，沒有負數）。
fn adjust_number(input: &mut tui_input::Input, increase: bool) {
    let current: usize = input.value().parse().unwrap_or(0);
    let next = if increase {
        current.saturating_add(1)
    } else {
        current.saturating_sub(1)
    };
    *input = tui_input::Input::new(next.to_string());
}

/// 零 I/O 的純狀態轉移，可以直接餵 `KeyEvent` 測試 —— 跟 `WizardState::on_key`
/// 同一個模式。回傳 `Some(flow)` 表示這一鍵該結束對話框，`None` 表示繼續編輯。
fn on_number_key(input: &mut tui_input::Input, key: KeyEvent) -> Option<NumberFlow> {
    if key.kind != KeyEventKind::Press {
        return None;
    }
    if is_abort_key(&key) {
        return Some(NumberFlow::Cancelled);
    }
    match key.code {
        KeyCode::Esc => Some(NumberFlow::Cancelled),
        KeyCode::Enter => Some(match input.value().parse::<usize>() {
            Ok(n) => NumberFlow::Set(n),
            Err(_) => NumberFlow::Cleared, // 空字串
        }),
        KeyCode::Left | KeyCode::Down | KeyCode::Char('h') | KeyCode::Char('j') => {
            adjust_number(input, false);
            None
        }
        KeyCode::Right | KeyCode::Up | KeyCode::Char('l') | KeyCode::Char('k') => {
            adjust_number(input, true);
            None
        }
        KeyCode::Char(c) if c.is_ascii_digit() => {
            // 先在副本上驗證能不能被解析成 usize，能才真的寫回去 —— 溢位
            // （例如打滿 20 位數）就吞掉這一鍵，讓「打得進去的字串」跟
            // 「Enter 解析得出來的數字」永遠是同一件事。
            let mut probe = input.clone();
            probe.handle_event(&Event::Key(key));
            if probe.value().parse::<usize>().is_ok() {
                *input = probe;
            }
            None
        }
        // 沒有 Delete：游標經由 `Input::new` 永遠停在字串尾端（←→ 已經被
        // ±1 徵用，沒有游標移動的路），`DeleteNextChar` 在游標==長度時是
        // no-op，列了也用不到。
        KeyCode::Backspace => {
            input.handle_event(&Event::Key(key));
            None
        }
        _ => None,
    }
}

fn run_number_input(
    terminal: &mut DefaultTerminal,
    current: Option<usize>,
    theme: &ColorTheme,
) -> crate::Result<NumberFlow> {
    let mut input = tui_input::Input::new(current.map(|n| n.to_string()).unwrap_or_default());
    loop {
        terminal.draw(|f| render_number_input(f, f.area(), &input, theme))?;
        let Event::Key(key) = ratatui::crossterm::event::read()? else {
            continue;
        };
        if let Some(flow) = on_number_key(&mut input, key) {
            return Ok(flow);
        }
    }
}

fn render_number_input(f: &mut Frame, area: Rect, input: &tui_input::Input, theme: &ColorTheme) {
    let hint = crate::widget::build_hint_line(
        theme,
        &[
            ("←↓/hj", "-1"),
            ("→↑/lk", "+1"),
            ("Enter", "確認"),
            ("Esc", "取消"), // Esc 只是放棄這次編輯，不會清空已有的值
        ],
    );

    // 寬度跟著提示列的實際渲染寬度量，不是憑印象數 CJK 格數寫死 —— 提示文字
    // 一改，這裡自動跟著對，不會又裁字。+2 是左右邊框各一格。
    let dialog_width = (hint.width() as u16 + 2).min(area.width.saturating_sub(4));
    let dialog_height = 5u16.min(area.height.saturating_sub(2));
    let dialog_area = centered_rect(area, dialog_width, dialog_height);

    f.render_widget(Clear, dialog_area);
    let block = Block::default()
        .title(" 要渲染的最大 commit 數量 ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.divider_fg))
        .style(Style::default().bg(theme.bg).fg(theme.fg));
    let inner = block.inner(dialog_area);
    f.render_widget(block, dialog_area);

    let [input_area, hint_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(inner);

    f.render_widget(
        Paragraph::new(Line::raw(format!("> {}", input.value()))),
        input_area,
    );
    f.render_widget(Paragraph::new(hint), hint_area);

    f.set_cursor_position((
        input_area.x + 2 + input.visual_cursor() as u16,
        input_area.y,
    ));
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width, height)
}

// ---------------------------------------------------------------------------
// 等效指令字串
// ---------------------------------------------------------------------------

/// 純函式，可測。路徑含空白時加單引號，這行是給人複製貼上重跑用的，不是給
/// shell 直接吃的，不需要完整 shell-escape。
fn quote_if_needed(s: &str) -> String {
    if s.chars().any(char::is_whitespace) {
        format!("'{s}'")
    } else {
        s.to_string()
    }
}

pub(crate) fn format_equivalent_command(args: &Args) -> String {
    let mut parts = vec!["ysgit".to_string()];
    if let Some(n) = args.max_count {
        parts.push(format!("-n {n}"));
    }
    if let Some(v) = args.order {
        parts.push(format!("-o {}", variant_name(&v)));
    }
    if let Some(v) = args.graph_width {
        parts.push(format!("-g {}", variant_name(&v)));
    }
    if let Some(v) = args.compact {
        parts.push(format!("-c {}", variant_name(&v)));
    }
    if let Some(v) = args.graph_style {
        parts.push(format!("-s {}", variant_name(&v)));
    }
    if let Some(v) = args.initial_selection {
        parts.push(format!("-i {}", variant_name(&v)));
    }
    if args.path != "." {
        parts.push(quote_if_needed(&args.path));
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Row 索引固定：0=PATH 1=MaxCount 2=Order 3=GraphWidth 4=Compact
    // 5=GraphStyle 6=InitialSelection 7=Launch(靜默) 8=Launch(印字串)。
    // 用按幾次 Down 移動來定位。
    const ROW_ORDER: usize = 2;
    const ROW_GRAPH_STYLE: usize = 5;
    const ROW_MAX_COUNT: usize = 1;
    const ROW_LAUNCH: usize = 7;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn char_key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn ctrl_key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn move_to_row(s: &mut WizardState, idx: usize) {
        for _ in 0..idx {
            s.on_key(key(KeyCode::Down));
        }
        assert_eq!(s.list.selected(), Some(idx));
    }

    #[test]
    fn right_cycles_forward_through_each_variant_and_wraps() {
        let mut s = WizardState::new();
        move_to_row(&mut s, ROW_ORDER);
        assert_eq!(s.draft.order, None, "還沒碰過，維持未設定");

        s.on_key(key(KeyCode::Right));
        assert_eq!(
            s.draft.order,
            Some(CommitOrderType::Topo),
            "chrono 是預設值，第一次按 → 要跳過它，直接切到 topo"
        );

        s.on_key(key(KeyCode::Right));
        assert_eq!(s.draft.order, Some(CommitOrderType::Chrono));

        s.on_key(key(KeyCode::Right));
        assert_eq!(
            s.draft.order,
            Some(CommitOrderType::Topo),
            "只在兩個真實值之間繞，不會繞回未設定"
        );
    }

    #[test]
    // `-o` 只有兩個值，←/→ 的完整序列剛好逐字相同（模 2 下 +1 跟 -1 是同一件
    // 事），沒辦法測出方向真的接對了——把 ← 誤接成 cycle(1) 照樣能讓
    // `right_cycles_forward_through_each_variant_and_wraps` 跟這條測試同時綠燈。
    // 換三個值的 `-s`，方向錯了序列才會真的不一樣。
    fn left_cycles_backward_through_each_variant_and_wraps() {
        let mut s = WizardState::new();
        move_to_row(&mut s, ROW_GRAPH_STYLE);
        assert_eq!(s.draft.graph_style, None);

        s.on_key(key(KeyCode::Left));
        assert_eq!(
            s.draft.graph_style,
            Some(GraphStyle::Ascii),
            "第一次按 ← 要跳過預設值 rounded，往後繞到最後一個值 ascii"
        );

        s.on_key(key(KeyCode::Left));
        assert_eq!(
            s.draft.graph_style,
            Some(GraphStyle::Angular),
            "← 是往後退，不是又往前"
        );

        s.on_key(key(KeyCode::Left));
        assert_eq!(s.draft.graph_style, Some(GraphStyle::Rounded));

        s.on_key(key(KeyCode::Left));
        assert_eq!(
            s.draft.graph_style,
            Some(GraphStyle::Ascii),
            "只在三個真實值之間繞，不會繞回未設定"
        );
    }

    /// `-o` 只有兩個值，←/→ 從未設定出發的第一步剛好會落在同一個地方，看不出
    /// 方向的差異。換一個三個值的欄位，才能證明「跳過預設值」這件事對兩個
    /// 方向都成立，而且方向真的不同。
    #[test]
    fn first_press_skips_the_default_variant_in_either_direction() {
        let mut s = WizardState::new();
        move_to_row(&mut s, ROW_GRAPH_STYLE);
        assert_eq!(s.draft.graph_style, None);

        s.on_key(key(KeyCode::Right));
        assert_eq!(
            s.draft.graph_style,
            Some(GraphStyle::Angular),
            "rounded 是預設值，→ 要跳過它，切到 angular"
        );

        let mut s = WizardState::new();
        move_to_row(&mut s, ROW_GRAPH_STYLE);
        s.on_key(key(KeyCode::Left));
        assert_eq!(
            s.draft.graph_style,
            Some(GraphStyle::Ascii),
            "← 也要跳過 rounded，往另一個方向切到 ascii"
        );
    }

    #[test]
    fn cycling_a_type_field_updates_the_row_label_immediately() {
        let mut s = WizardState::new();
        move_to_row(&mut s, ROW_ORDER);
        s.on_key(key(KeyCode::Right)); // 跳過預設值，直接切到 topo

        let row = &s.rows[ROW_ORDER];
        assert!(
            top_row_label(row, &s.draft).contains("拓撲序"),
            "切換完不用離開這一列就看得到新值"
        );
    }

    /// 切換已經在 `cycle_selected`（←/→ 那一步）做完了，`activate_selected`
    /// 對 `<TYPE>` 欄位刻意什麼都不做——這裡釘住這個行為，避免以後改動
    /// `activate_selected` 時不小心讓 Enter 在 Field 列上又多做一次切換。
    #[test]
    fn enter_on_a_type_field_row_is_a_no_op() {
        let mut s = WizardState::new();
        move_to_row(&mut s, ROW_ORDER);
        s.on_key(key(KeyCode::Right)); // 先切到 topo
        assert_eq!(s.draft.order, Some(CommitOrderType::Topo));

        assert!(matches!(s.on_key(key(KeyCode::Enter)), Flow::Continue));
        assert_eq!(
            s.draft.order,
            Some(CommitOrderType::Topo),
            "Enter 在 <TYPE> 列上不觸發任何動作，值維持不變"
        );
    }

    #[test]
    fn left_is_a_no_op_on_the_path_row() {
        let mut s = WizardState::new();
        assert!(matches!(s.on_key(key(KeyCode::Left)), Flow::Continue));
        assert_eq!(s.draft.path, ".", "PATH 不是可循環的欄位，← 不動它");
    }

    #[test]
    fn enter_on_path_row_requests_path_browser() {
        let mut s = WizardState::new();
        assert!(matches!(s.on_key(key(KeyCode::Enter)), Flow::OpenPath));
    }

    #[test]
    fn enter_on_max_count_row_requests_number_input() {
        let mut s = WizardState::new();
        move_to_row(&mut s, ROW_MAX_COUNT);
        assert!(matches!(s.on_key(key(KeyCode::Enter)), Flow::OpenMaxCount));
    }

    #[test]
    fn right_on_path_row_also_opens_the_browser() {
        let mut s = WizardState::new();
        assert!(matches!(s.on_key(key(KeyCode::Right)), Flow::OpenPath));
    }

    #[test]
    fn right_l_and_enter_all_trigger_launch() {
        for trigger in [key(KeyCode::Right), char_key('l'), key(KeyCode::Enter)] {
            let mut s = WizardState::new();
            move_to_row(&mut s, ROW_LAUNCH);
            assert!(
                matches!(s.on_key(trigger), Flow::Launch { print: false }),
                "{trigger:?} 在 Launch 列上都要能直接觸發啟動"
            );
        }
    }

    #[test]
    fn esc_and_ctrl_c_abort() {
        let mut s = WizardState::new();
        assert!(matches!(s.on_key(key(KeyCode::Esc)), Flow::Abort));
        assert!(matches!(s.on_key(ctrl_key('c')), Flow::Abort));
        assert!(matches!(s.on_key(ctrl_key('d')), Flow::Abort));
    }

    #[test]
    fn plain_c_without_control_does_not_abort_or_move() {
        let mut s = WizardState::new();
        assert!(matches!(s.on_key(char_key('c')), Flow::Continue));
        assert_eq!(
            s.list.selected(),
            Some(0),
            "非 vim 鍵、非 Ctrl+c，什麼都不該發生"
        );
    }

    #[test]
    fn vim_jk_move_selection_same_as_arrow_keys() {
        let mut s = WizardState::new();
        s.on_key(char_key('j'));
        assert_eq!(s.list.selected(), Some(1));
        s.on_key(char_key('j'));
        assert_eq!(s.list.selected(), Some(2));
        s.on_key(char_key('k'));
        assert_eq!(s.list.selected(), Some(1));
    }

    #[test]
    // 一樣換三個值的 graph_style：h 若誤接成 +1，會從 Angular 再往前跳到
    // Ascii 而不是退回 Rounded，斷言才抓得到方向錯誤。
    fn vim_hl_cycle_type_field_same_as_arrow_keys() {
        let mut s = WizardState::new();
        move_to_row(&mut s, ROW_GRAPH_STYLE);
        s.on_key(char_key('l'));
        assert_eq!(s.draft.graph_style, Some(GraphStyle::Angular));
        s.on_key(char_key('h'));
        assert_eq!(
            s.draft.graph_style,
            Some(GraphStyle::Rounded),
            "h 要往回退，不是又往前"
        );
    }

    #[test]
    fn move_selection_clamps_at_both_ends() {
        let mut s = WizardState::new();
        s.on_key(key(KeyCode::Up));
        assert_eq!(s.list.selected(), Some(0), "已在第 0 項，不會變成負數");

        let last = s.rows.len() - 1;
        for _ in 0..last + 5 {
            s.on_key(key(KeyCode::Down));
        }
        assert_eq!(s.list.selected(), Some(last), "已是最後一項，不會再往下");
    }

    #[test]
    fn top_row_shows_the_real_default_not_a_vague_placeholder() {
        let s = WizardState::new();
        assert!(
            top_row_label(&s.rows[ROW_ORDER], &s.draft).contains("時間序"),
            "chrono 是真正的預設值"
        );
        assert!(top_row_label(&s.rows[ROW_MAX_COUNT], &s.draft).contains("不限制"));
    }

    #[test]
    fn adjust_number_increments_and_decrements() {
        let mut input = tui_input::Input::new("5".to_string());
        adjust_number(&mut input, true);
        assert_eq!(input.value(), "6");
        adjust_number(&mut input, false);
        assert_eq!(input.value(), "5");
    }

    #[test]
    fn adjust_number_does_not_go_below_zero() {
        let mut input = tui_input::Input::new("0".to_string());
        adjust_number(&mut input, false);
        assert_eq!(input.value(), "0", "usize 沒有負數，減到底就停在 0");
    }

    #[test]
    fn adjust_number_treats_empty_input_as_zero() {
        let mut input = tui_input::Input::default();
        adjust_number(&mut input, true);
        assert_eq!(input.value(), "1");
    }

    /// `on_number_key` 一次收四個「-1」鍵跟四個「+1」鍵，直接鎖住每一個鍵
    /// 對到的方向——之前只有 `adjust_number` 本身的加/減測試，沒有測到
    /// 「哪個 KeyCode 該對應哪個方向」這件事。
    #[test]
    fn on_number_key_maps_every_key_to_the_correct_direction() {
        for decrease_key in [
            key(KeyCode::Left),
            key(KeyCode::Down),
            char_key('h'),
            char_key('j'),
        ] {
            let mut input = tui_input::Input::new("5".to_string());
            assert!(on_number_key(&mut input, decrease_key).is_none());
            assert_eq!(input.value(), "4", "{decrease_key:?} 應該是 -1");
        }

        for increase_key in [
            key(KeyCode::Right),
            key(KeyCode::Up),
            char_key('l'),
            char_key('k'),
        ] {
            let mut input = tui_input::Input::new("5".to_string());
            assert!(on_number_key(&mut input, increase_key).is_none());
            assert_eq!(input.value(), "6", "{increase_key:?} 應該是 +1");
        }
    }

    #[test]
    fn format_equivalent_command_only_includes_touched_fields() {
        let default_args = Args::try_parse_from(["ysgit"]).unwrap();
        assert_eq!(format_equivalent_command(&default_args), "ysgit");

        let mut args = Args::try_parse_from(["ysgit"]).unwrap();
        args.max_count = Some(50);
        args.order = Some(CommitOrderType::Topo);
        assert_eq!(format_equivalent_command(&args), "ysgit -n 50 -o topo");
    }

    #[test]
    fn format_equivalent_command_quotes_paths_with_whitespace() {
        let mut args = Args::try_parse_from(["ysgit"]).unwrap();
        args.path = "/Users/a b/repo".to_string();
        assert_eq!(format_equivalent_command(&args), "ysgit '/Users/a b/repo'");
    }

    #[test]
    fn format_equivalent_command_does_not_quote_plain_paths() {
        let mut args = Args::try_parse_from(["ysgit"]).unwrap();
        args.path = "/Users/a/repo".to_string();
        assert_eq!(format_equivalent_command(&args), "ysgit /Users/a/repo");
    }
}

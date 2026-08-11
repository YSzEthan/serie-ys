pub(crate) mod path_browser;

use clap::{Parser, ValueEnum};
use ratatui::{
    crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    layout::{Constraint, Layout, Rect},
    style::{Style, Stylize},
    text::Line,
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    DefaultTerminal, Frame,
};
use tui_input::backend::crossterm::EventHandler;

use crate::{
    color::ColorTheme,
    config,
    update::{self, AutoRestart, UpdateMode},
    Args, CommitOrderType, CompactType, GraphStyle, GraphWidthType, InitialSelection,
};

/// -h 在 TTY 下的入口。回傳 `None` = 使用者放棄（等同原本 `--help` 印完離開，
/// `run()` 收到後直接 `return Ok(())`）；`Some(args)` = 使用者選好了，直接接續
/// `src/lib.rs::run()` 裡 `Args::try_parse()` 之後的邏輯。
///
/// 精靈畫面固定用 `ColorTheme::default()`：設定檔壞掉時 `-h` 還要能開得起來，
/// 不能依賴 `config::load()` 成功。但「目前值」的顯示與循環切換的起點需要
/// 真正的設定檔內容——這兩件事分開處理，`ResolvedDefaults::load()` 自己
/// 容錯（載入失敗就退回內建硬預設），不影響這裡固定選用的主題。
pub fn run() -> crate::Result<Option<Args>> {
    let theme = ColorTheme::default();
    let mut terminal = ratatui::init();
    let outcome = wizard_loop(&mut terminal, WizardState::new(), &theme);
    ratatui::restore();
    drop(terminal); // 游標要等這裡才會被叫回來

    outcome
}

fn wizard_loop(
    terminal: &mut DefaultTerminal,
    mut state: WizardState,
    theme: &ColorTheme,
) -> crate::Result<Option<Args>> {
    loop {
        terminal.draw(|f| state.render(f, f.area(), theme))?;
        let Event::Key(key) = ratatui::crossterm::event::read()? else {
            continue;
        };
        match state.on_key(key) {
            Flow::Continue => {}
            Flow::Abort => return Ok(None),
            Flow::Launch => {
                // 每次按 Launch 都是新的嘗試，先清掉上一次的錯誤——寫成功
                // 就直接離開；寫失敗把原因留在畫面上，continue 迴圈讓使用者
                // 看得到，不強行啟動一個「以為存了、其實沒存」的 session。
                state.write_error = None;
                match write_touched_settings(&state) {
                    Ok(()) => return Ok(Some(state.draft)),
                    Err(e) => state.write_error = Some(e),
                }
            }
            Flow::OpenPath => {
                let start = path_browser::start_dir(&state.draft.path);
                if let Some(p) = path_browser::run(terminal, &start, theme)? {
                    state.draft.path = p.to_string_lossy().into_owned();
                }
                terminal.clear()?;
            }
            Flow::OpenMaxCount => {
                let current = state.draft.max_count.or(state.defaults.max_count);
                if let NumberFlow::Committed(v) = run_number_input(
                    terminal,
                    current,
                    theme,
                    "要渲染的最大 commit 數量",
                    0,
                    usize::MAX,
                )? {
                    state.draft.max_count = v;
                    state.max_count_touched = true;
                }
                terminal.clear()?;
            }
            Flow::OpenUpdateInterval => {
                let current = Some(
                    state
                        .draft
                        .update_interval
                        .unwrap_or(state.defaults.update_interval) as usize,
                );
                if let NumberFlow::Committed(v) = run_number_input(
                    terminal,
                    current,
                    theme,
                    "自動更新的檢查間隔（小時，1–48）",
                    update::MIN_INTERVAL_HOURS as usize,
                    update::MAX_INTERVAL_HOURS as usize,
                )? {
                    state.draft.update_interval = v.map(|n| n as u64);
                    state.update_interval_touched = true;
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

pub(crate) fn variant_name<T: ValueEnum>(v: &T) -> String {
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

fn update_mode_desc(v: UpdateMode) -> &'static str {
    match v {
        UpdateMode::Off => "關閉",
        UpdateMode::Check => "檢查後詢問",
        UpdateMode::Auto => "自動安裝",
    }
}

fn auto_restart_desc(v: AutoRestart) -> &'static str {
    match v {
        AutoRestart::Off => "關閉",
        AutoRestart::On => "開啟",
    }
}

/// 精靈顯示「目前值」與循環切換起點用的參考點。跟 `src/lib.rs` 的 `run()`
/// 裡 `args.field.or(core_config.option.field)` 那條合併鏈讀的是同一份
/// 設定檔，語意也一樣（沒被使用者這次 session 動過的欄位，最終生效的值
/// 就是設定檔裡的值）——差別只在這裡要先解出來給畫面顯示與 `cycle_value`
/// 當起點用，`run()` 那條合併鏈則是留給 CLI／設定檔的合併結果。
///
/// `from_core` 是純函式（不碰檔案系統），`load` 才是會呼叫
/// `config::load()` 的入口，兩者分開是為了讓測試能繞過真實檔案系統直接
/// 建構，不會因為開發機上 `target/debug/` 底下剛好有沒有一份設定檔而
/// 測出不一樣的結果。
struct ResolvedDefaults {
    order: CommitOrderType,
    graph_width: GraphWidthType,
    compact: CompactType,
    graph_style: GraphStyle,
    initial_selection: InitialSelection,
    max_count: Option<usize>,
    update_mode: UpdateMode,
    update_interval: u64,
    auto_restart: AutoRestart,
}

impl ResolvedDefaults {
    fn from_core(core: &config::CoreConfig) -> Self {
        Self {
            order: core.option.order.unwrap_or(CommitOrderType::Chrono),
            graph_width: core.option.graph_width.unwrap_or(GraphWidthType::Auto),
            compact: core.option.compact.unwrap_or(CompactType::Auto),
            graph_style: core.option.graph_style.unwrap_or_default(),
            initial_selection: core
                .option
                .initial_selection
                .unwrap_or(InitialSelection::Latest),
            max_count: core.option.max_count,
            update_mode: core.update.mode.unwrap_or_default(),
            update_interval: core
                .update
                .interval_hours
                .unwrap_or(update::DEFAULT_INTERVAL_HOURS),
            auto_restart: core.update.auto_restart.unwrap_or_default(),
        }
    }

    /// 設定檔壞掉、讀不到、解析失敗——任何 `config::load()` 失敗的原因都一律
    /// 退回內建硬預設，精靈仍然開得起來。這條路徑不能用 `?`。
    fn load() -> Self {
        let core = config::load().map(|(core, ..)| core).unwrap_or_default();
        Self::from_core(&core)
    }
}

// ---------------------------------------------------------------------------
// 單層清單：循環選擇欄位用 ←/→ 在原地輪迴切換值。循環只在該欄位的 N 個
// 合法值之間繞（不含「未設定」），還沒碰過的欄位一按 →／← 就直接落在
// 「目前有效值」的下一站／上一站 —— 循環站數等於真正的選項數，不會多一站
// 看起來像「N+1 個選項」。碰過之後就沒有回到「未設定」的路：選錯了就繼續
// 循環到想要的那個值，不是退回預設。
// ---------------------------------------------------------------------------

/// 在 `T::value_variants()` 上把 `*slot` 往 `delta` 方向移一站，寫回去的
/// 結果永遠是 `Some`。`*slot` 是 `None`（還沒碰過這個欄位）時，把它當成
/// 「已經站在 `current` 那一格」來算下一步（`current` 是這個欄位目前真正
/// 生效的值，見 `ResolvedDefaults`）—— 不然第一次按 → 會落在跟畫面上顯示
/// 的目前值一模一樣的格子，數值沒變、只是多了個勾，等於白按一次。這樣算，
/// 第一次按不管哪個方向都保證換到一個不一樣的值。
///
/// 七個循環選擇欄位共用同一份算術，只是各自的 `T`／`current` 不同，所以
/// 抽成吃 `&mut Option<T>` 的自由函式而不是把 `FieldKind` 本身泛型化——後者
/// 才會讓型別設計變複雜，這裡型別完全由呼叫端推導。
fn cycle_value<T: ValueEnum + Copy + PartialEq>(slot: &mut Option<T>, current: T, delta: i32) {
    let variants = T::value_variants();
    let index_of = |v: T| {
        variants
            .iter()
            .position(|&x| x == v)
            .expect("值一定是合法變體之一")
    };
    let idx = index_of(slot.unwrap_or(current)) as i32;
    let next = (idx + delta).rem_euclid(variants.len() as i32);
    *slot = Some(variants[next as usize]);
}

/// (目前有效值的中文說明, 是不是使用者這次 session 主動選的)。未設定時
/// 顯示的是這個欄位真正的目前值（`ResolvedDefaults`，讀自設定檔），不是
/// 「使用預設值」這種空話。
fn resolve_desc<T: Copy>(
    slot: Option<T>,
    current: T,
    desc: fn(T) -> &'static str,
) -> (&'static str, bool) {
    (desc(slot.unwrap_or(current)), slot.is_some())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FieldKind {
    Order,
    GraphWidth,
    Compact,
    GraphStyle,
    InitialSelection,
    UpdateMode,
    AutoRestart,
}

impl FieldKind {
    fn flags(self) -> &'static str {
        match self {
            FieldKind::Order => "-o, --order",
            FieldKind::GraphWidth => "-g, --graph-width",
            FieldKind::Compact => "-c, --compact",
            FieldKind::GraphStyle => "-s, --graph-style",
            FieldKind::InitialSelection => "-i, --initial-selection",
            FieldKind::UpdateMode => "--update-mode",
            FieldKind::AutoRestart => "--auto-restart",
        }
    }

    fn help(self) -> &'static str {
        match self {
            FieldKind::Order => "Commit 排序演算法",
            FieldKind::GraphWidth => "Commit 圖形格子寬度",
            FieldKind::Compact => "緊湊模式",
            FieldKind::GraphStyle => "Commit 圖形邊線風格",
            FieldKind::InitialSelection => "初始選取的 commit",
            FieldKind::UpdateMode => "自動更新檢查模式",
            FieldKind::AutoRestart => "更新後自動重啟／開啟新版",
        }
    }

    /// `delta = 1` 往前一站（→／Enter），`delta = -1` 往後一站（←）。
    fn cycle(self, draft: &mut Args, defaults: &ResolvedDefaults, delta: i32) {
        match self {
            FieldKind::Order => cycle_value(&mut draft.order, defaults.order, delta),
            FieldKind::GraphWidth => {
                cycle_value(&mut draft.graph_width, defaults.graph_width, delta)
            }
            FieldKind::Compact => cycle_value(&mut draft.compact, defaults.compact, delta),
            FieldKind::GraphStyle => {
                cycle_value(&mut draft.graph_style, defaults.graph_style, delta)
            }
            FieldKind::InitialSelection => cycle_value(
                &mut draft.initial_selection,
                defaults.initial_selection,
                delta,
            ),
            FieldKind::UpdateMode => {
                cycle_value(&mut draft.update_mode, defaults.update_mode, delta)
            }
            FieldKind::AutoRestart => {
                cycle_value(&mut draft.auto_restart, defaults.auto_restart, delta)
            }
        }
    }

    fn current(self, draft: &Args, defaults: &ResolvedDefaults) -> (&'static str, bool) {
        match self {
            FieldKind::Order => resolve_desc(draft.order, defaults.order, order_desc),
            FieldKind::GraphWidth => {
                resolve_desc(draft.graph_width, defaults.graph_width, graph_width_desc)
            }
            FieldKind::Compact => resolve_desc(draft.compact, defaults.compact, compact_desc),
            FieldKind::GraphStyle => {
                resolve_desc(draft.graph_style, defaults.graph_style, graph_style_desc)
            }
            FieldKind::InitialSelection => resolve_desc(
                draft.initial_selection,
                defaults.initial_selection,
                initial_selection_desc,
            ),
            FieldKind::UpdateMode => {
                resolve_desc(draft.update_mode, defaults.update_mode, update_mode_desc)
            }
            FieldKind::AutoRestart => {
                resolve_desc(draft.auto_restart, defaults.auto_restart, auto_restart_desc)
            }
        }
    }
}

enum TopRowAction {
    OpenPath,
    OpenMaxCount,
    OpenUpdateInterval,
    Field(FieldKind),
    Launch,
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
            help: "git 倉庫路徑（僅本次，不存檔）",
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
            action: TopRowAction::Field(FieldKind::UpdateMode),
            flags: FieldKind::UpdateMode.flags(),
            help: FieldKind::UpdateMode.help(),
        },
        TopRow {
            action: TopRowAction::OpenUpdateInterval,
            flags: "--update-interval <HOURS>",
            help: "自動更新的檢查間隔",
        },
        TopRow {
            action: TopRowAction::Field(FieldKind::AutoRestart),
            flags: FieldKind::AutoRestart.flags(),
            help: FieldKind::AutoRestart.help(),
        },
        TopRow {
            action: TopRowAction::Launch,
            flags: "▶ 啟動 ysgit",
            help: "",
        },
    ]
}

/// 每一列的顯示文字：所有欄位都附上目前有效值（讀設定檔得到的真實值，不是
/// 硬預設）。循環選擇的欄位（`Field`）額外用 `< >` 包住值，標示「這欄按
/// 左右鍵會變」；數字輸入的欄位（`OpenMaxCount`／`OpenUpdateInterval`）
/// 不用 `< >`——那是另一種互動（開子畫面打字），混用會誤導使用者以為也能
/// 直接左右切換。`✓` 標示這欄本次 session 有被使用者主動改過（Launch 時
/// 會寫回設定檔）。
fn top_row_label(row: &TopRow, state: &WizardState) -> String {
    let draft = &state.draft;
    let defaults = &state.defaults;
    let (checked, body) = match row.action {
        TopRowAction::OpenPath => (
            false,
            format!("{}  {}（目前：{}）", row.flags, row.help, draft.path),
        ),
        TopRowAction::OpenMaxCount => {
            let effective = draft.max_count.or(defaults.max_count);
            (
                state.max_count_touched,
                format!(
                    "{}  {}（目前：{}）",
                    row.flags,
                    row.help,
                    effective
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "不限制".to_string())
                ),
            )
        }
        TopRowAction::OpenUpdateInterval => (
            state.update_interval_touched,
            format!(
                "{}  {}（目前：{} 小時）",
                row.flags,
                row.help,
                draft.update_interval.unwrap_or(defaults.update_interval)
            ),
        ),
        TopRowAction::Field(field) => {
            let (desc, touched) = field.current(draft, defaults);
            // YSGIT_NO_UPDATE_CHECK 會在 `update::resolve()` 把 mode 壓成
            // Off——這裡只是提醒使用者，不是這一列本身的邏輯改變。
            let note = if field == FieldKind::UpdateMode
                && std::env::var_os("YSGIT_NO_UPDATE_CHECK").is_some()
            {
                "（目前被 YSGIT_NO_UPDATE_CHECK 壓成 off）"
            } else {
                ""
            };
            (
                touched,
                format!("{}  {}（目前：< {desc} >）{note}", row.flags, row.help),
            )
        }
        TopRowAction::Launch => (false, row.flags.to_string()),
    };
    let prefix = if checked { "✓ " } else { "  " };
    format!("{prefix}{body}")
}

enum Flow {
    Continue,
    Abort,
    Launch,
    OpenPath,
    OpenMaxCount,
    OpenUpdateInterval,
}

struct WizardState {
    draft: Args,
    defaults: ResolvedDefaults,
    rows: Vec<TopRow>,
    list: ListState,
    /// `max_count`／`update_interval` 是數字輸入，不像循環選擇欄位那樣
    /// `Some` 就代表「有碰過」——它們的清空手勢（`NumberFlow::Committed(None)`）
    /// 結果也是 `None`，跟「從沒開過這個對話框」在型別上分不出來。這兩個
    /// 旗標補上那個區別，Launch 寫回時才知道「要不要把 config 裡原本的
    /// 值刪掉」跟「乾脆不動這個鍵」是兩件不同的事。
    max_count_touched: bool,
    update_interval_touched: bool,
    /// 上一次 Launch 寫回設定檔失敗的原因，顯示在畫面上；成功或還沒按過
    /// Launch 都是 `None`。
    write_error: Option<String>,
}

impl WizardState {
    fn new() -> Self {
        Self::with_defaults(ResolvedDefaults::load())
    }

    fn with_defaults(defaults: ResolvedDefaults) -> Self {
        let draft = Args::try_parse_from(["ysgit"]).expect("無參數的 parse 一定要成功");
        let mut list = ListState::default();
        list.select(Some(0));
        Self {
            draft,
            defaults,
            rows: build_top_rows(),
            list,
            max_count_touched: false,
            update_interval_touched: false,
            write_error: None,
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

    /// ← 或「→ 之前」呼叫：如果選中的是循環選擇欄位，往 `delta` 方向輪迴
    /// 切換一站；其餘欄位（PATH／數字輸入／Launch）不受影響。
    fn cycle_selected(&mut self, delta: i32) {
        let Some(row_idx) = self.list.selected() else {
            return;
        };
        if let TopRowAction::Field(field) = self.rows[row_idx].action {
            field.cycle(&mut self.draft, &self.defaults, delta);
        }
    }

    /// Enter 與 →／l 都會呼叫（→／l 先呼叫 `cycle_selected` 切換值，這裡
    /// 再處理「有明確終點動作」的列）。PATH／數字輸入開對應的子畫面；
    /// Launch 直接啟動——兩個觸發鍵沒有差別待遇。循環選擇欄位在這裡是
    /// no-op：切換已經在 `cycle_selected` 做完了，這裡不用再做事。
    fn activate_selected(&mut self) -> Flow {
        let Some(row_idx) = self.list.selected() else {
            return Flow::Continue;
        };
        match self.rows[row_idx].action {
            TopRowAction::OpenPath => Flow::OpenPath,
            TopRowAction::OpenMaxCount => Flow::OpenMaxCount,
            TopRowAction::OpenUpdateInterval => Flow::OpenUpdateInterval,
            TopRowAction::Field(_) => Flow::Continue,
            TopRowAction::Launch => Flow::Launch,
        }
    }

    fn render(&mut self, f: &mut Frame, area: Rect, theme: &ColorTheme) {
        let [list_area, error_area, hint_area] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);

        let items: Vec<ListItem> = self
            .rows
            .iter()
            .map(|row| ListItem::new(top_row_label(row, self)))
            .collect();
        f.render_stateful_widget(styled_list(items, theme), list_area, &mut self.list);

        if let Some(err) = &self.write_error {
            f.render_widget(
                Paragraph::new(Line::raw(err.as_str()).fg(theme.status_error_fg)),
                error_area,
            );
        }

        // 精靈的按鍵不走 keybind 設定（它在主 TUI 啟動前就跑完了），所以提示
        // 直接給字串；格式仍與主畫面統一。
        let hint = crate::widget::hint_line(
            theme,
            &[
                ("↑↓/kj".into(), "選擇"),
                ("←→/hl".into(), "切換選項"),
                ("Enter".into(), "開啟/啟動"),
                ("Esc/Ctrl-C".into(), "放棄（不啟動、不存檔）"),
            ],
            theme.help_key_fg,
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
// 數字輸入彈窗（-n/--max-count、--update-interval 共用）。←/↓（含 vim 的
// h/j）減一，→/↑（含 vim 的 l/k）加一；打字仍然可以直接輸入精確數字，但
// 只收數字字元。
// ---------------------------------------------------------------------------

enum NumberFlow {
    Cancelled,
    /// `None` = 明確清空（空字串按 Enter），`Some(n)` = 確定成這個值——
    /// 兩者都代表「使用者確認了」，呼叫端要做的事（標記 touched、寫回
    /// `draft`）完全相同，只有要寫的值不同，因此不拆成兩個變體。
    Committed(Option<usize>),
}

/// 純函式，可測。空字串當 0 處理。夾在 `[min, max]` 之間——
/// `max_count` 沒有上限（`min = 0, max = usize::MAX`），`update_interval`
/// 是 `[1, 48]`。
fn adjust_number(input: &mut tui_input::Input, increase: bool, min: usize, max: usize) {
    let current: usize = input.value().parse().unwrap_or(0);
    let next = if increase {
        current.saturating_add(1).min(max)
    } else {
        current.saturating_sub(1).max(min)
    };
    *input = tui_input::Input::new(next.to_string());
}

/// 零 I/O 的純狀態轉移，可以直接餵 `KeyEvent` 測試 —— 跟 `WizardState::on_key`
/// 同一個模式。回傳 `Some(flow)` 表示這一鍵該結束對話框，`None` 表示繼續編輯。
///
/// `min`／`max` 只用來夾住方向鍵的 ±1 與 Enter 確認時的最終值，不擋輸入
/// 過程中的中間狀態——邊打字邊驗證範圍會讓「先打 4 再打 8 湊出 48」這種
/// 多位數輸入在中途卡住，直接輸入完再夾比較不擾民。
fn on_number_key(
    input: &mut tui_input::Input,
    key: KeyEvent,
    min: usize,
    max: usize,
) -> Option<NumberFlow> {
    if key.kind != KeyEventKind::Press {
        return None;
    }
    if is_abort_key(&key) {
        return Some(NumberFlow::Cancelled);
    }
    match key.code {
        KeyCode::Esc => Some(NumberFlow::Cancelled),
        KeyCode::Enter => Some(NumberFlow::Committed(
            input
                .value()
                .parse::<usize>()
                .ok()
                .map(|n| n.clamp(min, max)),
        )),
        KeyCode::Left | KeyCode::Down | KeyCode::Char('h') | KeyCode::Char('j') => {
            adjust_number(input, false, min, max);
            None
        }
        KeyCode::Right | KeyCode::Up | KeyCode::Char('l') | KeyCode::Char('k') => {
            adjust_number(input, true, min, max);
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
    title: &str,
    min: usize,
    max: usize,
) -> crate::Result<NumberFlow> {
    let mut input = tui_input::Input::new(current.map(|n| n.to_string()).unwrap_or_default());
    loop {
        terminal.draw(|f| render_number_input(f, f.area(), &input, theme, title))?;
        let Event::Key(key) = ratatui::crossterm::event::read()? else {
            continue;
        };
        if let Some(flow) = on_number_key(&mut input, key, min, max) {
            return Ok(flow);
        }
    }
}

fn render_number_input(
    f: &mut Frame,
    area: Rect,
    input: &tui_input::Input,
    theme: &ColorTheme,
    title: &str,
) {
    let hint = crate::widget::hint_line(
        theme,
        &[
            ("←↓/hj".into(), "-1"),
            ("→↑/lk".into(), "+1"),
            ("Enter".into(), "確認"),
            ("Esc".into(), "取消"), // Esc 只是放棄這次編輯，不會清空已有的值
        ],
        theme.help_key_fg,
    );

    // 寬度跟著提示列的實際渲染寬度量，不是憑印象數 CJK 格數寫死 —— 提示文字
    // 一改，這裡自動跟著對，不會又裁字。+2 是左右邊框各一格。
    let dialog_width = (hint.width() as u16 + 2).min(area.width.saturating_sub(4));
    let dialog_height = 5u16.min(area.height.saturating_sub(2));
    let dialog_area = centered_rect(area, dialog_width, dialog_height);

    f.render_widget(Clear, dialog_area);
    let block = Block::default()
        .title(format!(" {title} "))
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
// Launch 寫回設定檔。只動這次 session 被使用者改過的鍵，其餘內容（含使用者
// 手寫的註解、排版、其他區塊）原封不動——這是用 `toml_edit` 做部分更新，
// 而不是整份 `toml::to_string` 重寫的唯一理由。
// ---------------------------------------------------------------------------

fn write_touched_settings(state: &WizardState) -> Result<(), String> {
    let Some(path) = config::effective_path() else {
        // 無法決定要寫到哪（`exe_dir()` 解析不出來），安靜放棄，不擋 Launch
        // ——跟 `ensure_config_file()` 同一個哲學：存檔是附加價值，不是
        // 啟動的必要條件。
        return Ok(());
    };
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let updated = apply_touched_settings(state, &existing)?;
    std::fs::write(&path, updated).map_err(|e| format!("寫入設定檔失敗：{e}"))
}

/// 純函式：把 `state` 裡本次 session 被使用者改過的欄位套進既有的 TOML
/// 內容，回傳新的檔案內容字串。不碰檔案系統，方便直接測。
///
/// `Err` 只有一種原因：`existing` 語法本身就壞了（`toml_edit` 也 parse
/// 不動）。這裡直接不寫，絕不能 fallback 成「重建一份新文件」——那就是
/// 把使用者的檔案洗掉。語法合法但值不合法（例如 `graph_style = "asci"`）
/// 不會走到這條錯誤：`toml_edit` 只在乎語法，不驗語意，外科手術式改寫
/// 對這種情況是安全的，順便把壞鍵修掉。
fn apply_touched_settings(state: &WizardState, existing: &str) -> Result<String, String> {
    let mut doc = existing
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("設定檔語法錯誤，這次改動不會存檔：{e}"))?;

    let core = table_entry(doc.as_table_mut(), "core")?;
    let option = table_entry(core, "option")?;
    set_enum(option, "order", state.draft.order);
    set_enum(option, "graph_width", state.draft.graph_width);
    set_enum(option, "compact", state.draft.compact);
    set_enum(option, "graph_style", state.draft.graph_style);
    set_enum(option, "initial_selection", state.draft.initial_selection);
    if state.max_count_touched {
        set_number(option, "max_count", state.draft.max_count.map(|n| n as i64));
    }

    // 重新從 `doc` 借一次：上面 `option` 借用的生命週期在這裡已經結束
    // （最後一次使用是前面的 `set_number`），才能再借出 `core.update`。
    let core = table_entry(doc.as_table_mut(), "core")?;
    let update = table_entry(core, "update")?;
    set_enum(update, "mode", state.draft.update_mode);
    if state.update_interval_touched {
        set_number(
            update,
            "interval_hours",
            state.draft.update_interval.map(|n| n as i64),
        );
    }
    set_enum(update, "auto_restart", state.draft.auto_restart);

    Ok(doc.to_string())
}

/// `Table::entry(key).or_insert(...)` 而不是 `doc["a"]["b"]` 那種鏈式索引：
/// 後者在 `a` 還不存在時，`Item` 的 `IndexMut` 實作會把它自動生成成
/// **inline table**（`a = { b = ... }`）而不是一般的 `[a]` 表格區塊——這是
/// `toml_edit` 為了支援 `doc["a"]["b"]["c"] = value(1)` 這種寫法的既有
/// 行為（見 `Index for str` 的 `index_mut`：遇到 `Item::None` 直接包一層
/// `InlineTable` 再繼續索引），不是這裡的臭蟲，但拿來寫巢狀 `[core.option]`
/// 這種一定要是標準表格區塊的鍵就會生成使用者看不懂的格式。改用
/// `Table::entry()` 直接操作 `Table`，不經過 `Item` 索引，就不會踩到這個
/// 自動生成規則。
///
/// 鍵已存在但不是表格（例如使用者手寫了 `core = "oops"`）時回錯誤而不是
/// panic——語法合法但結構跟預期不符，跟語法本身壞掉是同一類「不強行動
/// 使用者檔案」的情況。
fn table_entry<'a>(
    table: &'a mut toml_edit::Table,
    key: &'static str,
) -> Result<&'a mut toml_edit::Table, String> {
    table
        .entry(key)
        .or_insert(toml_edit::table())
        .as_table_mut()
        .ok_or_else(|| format!("設定檔的 `{key}` 不是表格，這次改動不會存檔"))
}

/// 循環選擇欄位沒有「清空」手勢，`None` 只代表「沒碰過」，所以這裡永遠是
/// 「有值才寫，沒值就完全不動這個鍵」——不像 `set_number` 還要分辨「沒碰過」
/// 跟「明確清空」。
///
/// 字串轉換直接重用 `variant_name`（clap 的 kebab-case 名稱）而不是走
/// `serde` 序列化：這幾個 enum 都是單字變體，clap 的 kebab-case 跟 serde
/// 的 `rename_all = "lowercase"` 逐字相同（`config.rs` 的
/// `*_schema_enum_matches_every_accepted_cli_value` 系列測試釘住這個等式），
/// 沒必要為了同一個字串再繞 `serde_json` 一圈。
fn set_enum<T: ValueEnum>(table: &mut toml_edit::Table, key: &str, value: Option<T>) {
    if let Some(v) = value {
        set_preserving_decor(table, key, variant_name(&v).into());
    }
}

/// 呼叫端只在對應的 `*_touched` 旗標為真時才呼叫這個函式（`Some` = 設成
/// 這個值，`None` = 明確清空，移除該鍵，讓它退回 `config::load()` 的預設
/// 解析路徑）——「沒碰過」的情況呼叫端根本不會呼叫，不需要在這裡再分支。
fn set_number(table: &mut toml_edit::Table, key: &str, value: Option<i64>) {
    match value {
        Some(n) => set_preserving_decor(table, key, n.into()),
        None => {
            table.remove(key);
        }
    }
}

/// `table[key] = toml_edit::value(v)` 會整個換掉舊的 `Item`，連著它的
/// decor 一起丟掉——`assets/default-config.toml` 每一個旋鈕的說明都寫成
/// 該鍵值後面的行內註解（例如 `order = "chrono"  # commit 排序：...`），
/// 直接指派會讓使用者調一次那個值、說明就跟著消失，正好抵銷「含說明的
/// 預設設定檔」這個賣點。鍵已存在時改成原地覆寫值、保留舊 decor；鍵是
/// 新建的才用一般的 `insert`（沒有舊 decor 可留）。
fn set_preserving_decor(table: &mut toml_edit::Table, key: &str, new_value: toml_edit::Value) {
    if let Some(old) = table.get_mut(key).and_then(toml_edit::Item::as_value_mut) {
        let decor = old.decor().clone();
        *old = new_value;
        *old.decor_mut() = decor;
    } else {
        table.insert(key, toml_edit::Item::Value(new_value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_state() -> WizardState {
        WizardState::with_defaults(ResolvedDefaults::from_core(&config::CoreConfig::default()))
    }

    /// 依動作找列索引，取代寫死的數字常數——新增／刪除列時不用逐一改測試。
    fn row_of(rows: &[TopRow], pred: impl Fn(&TopRowAction) -> bool) -> usize {
        rows.iter()
            .position(|r| pred(&r.action))
            .expect("找不到符合條件的列")
    }

    fn row_of_field(rows: &[TopRow], field: FieldKind) -> usize {
        row_of(
            rows,
            move |a| matches!(a, TopRowAction::Field(f) if *f == field),
        )
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

    fn move_to_row(s: &mut WizardState, idx: usize) {
        for _ in 0..idx {
            s.on_key(key(KeyCode::Down));
        }
        assert_eq!(s.list.selected(), Some(idx));
    }

    #[test]
    fn right_cycles_forward_through_each_variant_and_wraps() {
        let mut s = test_state();
        let idx = row_of_field(&s.rows, FieldKind::Order);
        move_to_row(&mut s, idx);
        assert_eq!(s.draft.order, None, "還沒碰過，維持未設定");

        s.on_key(key(KeyCode::Right));
        assert_eq!(
            s.draft.order,
            Some(CommitOrderType::Topo),
            "chrono 是目前值，第一次按 → 要跳過它，直接切到 topo"
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
        let mut s = test_state();
        let idx = row_of_field(&s.rows, FieldKind::GraphStyle);
        move_to_row(&mut s, idx);
        assert_eq!(s.draft.graph_style, None);

        s.on_key(key(KeyCode::Left));
        assert_eq!(
            s.draft.graph_style,
            Some(GraphStyle::Ascii),
            "第一次按 ← 要跳過目前值 rounded，往後繞到最後一個值 ascii"
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
    /// 方向的差異。換一個三個值的欄位，才能證明「跳過目前值」這件事對兩個
    /// 方向都成立，而且方向真的不同。
    #[test]
    fn first_press_skips_the_current_variant_in_either_direction() {
        let mut s = test_state();
        let idx = row_of_field(&s.rows, FieldKind::GraphStyle);
        move_to_row(&mut s, idx);
        assert_eq!(s.draft.graph_style, None);

        s.on_key(key(KeyCode::Right));
        assert_eq!(
            s.draft.graph_style,
            Some(GraphStyle::Angular),
            "rounded 是目前值，→ 要跳過它，切到 angular"
        );

        let mut s = test_state();
        let idx = row_of_field(&s.rows, FieldKind::GraphStyle);
        move_to_row(&mut s, idx);
        s.on_key(key(KeyCode::Left));
        assert_eq!(
            s.draft.graph_style,
            Some(GraphStyle::Ascii),
            "← 也要跳過 rounded，往另一個方向切到 ascii"
        );
    }

    #[test]
    fn cycling_a_type_field_updates_the_row_label_immediately() {
        let mut s = test_state();
        let idx = row_of_field(&s.rows, FieldKind::Order);
        move_to_row(&mut s, idx);
        s.on_key(key(KeyCode::Right)); // 跳過目前值，直接切到 topo

        let row = &s.rows[idx];
        assert!(
            top_row_label(row, &s).contains("拓撲序"),
            "切換完不用離開這一列就看得到新值"
        );
    }

    #[test]
    fn cycle_field_rows_wrap_the_value_in_angle_brackets() {
        let s = test_state();
        let idx = row_of_field(&s.rows, FieldKind::Order);
        let label = top_row_label(&s.rows[idx], &s);
        assert!(
            label.contains("< 時間序 >"),
            "循環選擇欄位要用 < > 標示可切換：{label}"
        );
    }

    #[test]
    fn number_input_rows_do_not_use_angle_brackets() {
        // `row.flags` 本身含 `<NUMBER>`（CLI 語法），這裡只檢查「目前：」後面
        // 的值沒有被包一層 `< >`，不是整條 label 零角括號。
        let s = test_state();
        let idx = row_of(&s.rows, |a| matches!(a, TopRowAction::OpenMaxCount));
        let label = top_row_label(&s.rows[idx], &s);
        let value_part = label.split("目前：").nth(1).expect("label 要含「目前：」");
        assert!(
            !value_part.starts_with('<'),
            "數字輸入不是循環選擇，值不該被包在 < > 裡：{label}"
        );
    }

    /// 切換已經在 `cycle_selected`（←/→ 那一步）做完了，`activate_selected`
    /// 對循環選擇欄位刻意什麼都不做——這裡釘住這個行為，避免以後改動
    /// `activate_selected` 時不小心讓 Enter 在 Field 列上又多做一次切換。
    #[test]
    fn enter_on_a_type_field_row_is_a_no_op() {
        let mut s = test_state();
        let idx = row_of_field(&s.rows, FieldKind::Order);
        move_to_row(&mut s, idx);
        s.on_key(key(KeyCode::Right)); // 先切到 topo
        assert_eq!(s.draft.order, Some(CommitOrderType::Topo));

        assert!(matches!(s.on_key(key(KeyCode::Enter)), Flow::Continue));
        assert_eq!(
            s.draft.order,
            Some(CommitOrderType::Topo),
            "Enter 在循環選擇列上不觸發任何動作，值維持不變"
        );
    }

    #[test]
    fn left_is_a_no_op_on_the_path_row() {
        let mut s = test_state();
        assert!(matches!(s.on_key(key(KeyCode::Left)), Flow::Continue));
        assert_eq!(s.draft.path, ".", "PATH 不是可循環的欄位，← 不動它");
    }

    #[test]
    fn enter_on_path_row_requests_path_browser() {
        let mut s = test_state();
        assert!(matches!(s.on_key(key(KeyCode::Enter)), Flow::OpenPath));
    }

    #[test]
    fn enter_on_max_count_row_requests_number_input() {
        let mut s = test_state();
        let idx = row_of(&s.rows, |a| matches!(a, TopRowAction::OpenMaxCount));
        move_to_row(&mut s, idx);
        assert!(matches!(s.on_key(key(KeyCode::Enter)), Flow::OpenMaxCount));
    }

    #[test]
    fn enter_on_update_interval_row_requests_number_input() {
        let mut s = test_state();
        let idx = row_of(&s.rows, |a| matches!(a, TopRowAction::OpenUpdateInterval));
        move_to_row(&mut s, idx);
        assert!(matches!(
            s.on_key(key(KeyCode::Enter)),
            Flow::OpenUpdateInterval
        ));
    }

    #[test]
    fn update_mode_row_cycles_and_skips_the_current_variant() {
        let mut s = test_state();
        let idx = row_of_field(&s.rows, FieldKind::UpdateMode);
        move_to_row(&mut s, idx);
        assert_eq!(s.draft.update_mode, None);

        s.on_key(key(KeyCode::Right));
        assert_eq!(
            s.draft.update_mode,
            Some(UpdateMode::Auto),
            "check 是目前值，第一次按 → 跳過它，切到下一個 auto"
        );
    }

    #[test]
    fn auto_restart_row_toggles() {
        let mut s = test_state();
        let idx = row_of_field(&s.rows, FieldKind::AutoRestart);
        move_to_row(&mut s, idx);
        assert_eq!(s.draft.auto_restart, None);

        s.on_key(key(KeyCode::Right));
        assert_eq!(s.draft.auto_restart, Some(AutoRestart::On));
    }

    #[test]
    fn right_on_path_row_also_opens_the_browser() {
        let mut s = test_state();
        assert!(matches!(s.on_key(key(KeyCode::Right)), Flow::OpenPath));
    }

    #[test]
    fn right_l_and_enter_all_trigger_launch() {
        for trigger in [key(KeyCode::Right), char_key('l'), key(KeyCode::Enter)] {
            let mut s = test_state();
            let idx = row_of(&s.rows, |a| matches!(a, TopRowAction::Launch));
            move_to_row(&mut s, idx);
            assert!(
                matches!(s.on_key(trigger), Flow::Launch),
                "{trigger:?} 在 Launch 列上都要能直接觸發啟動"
            );
        }
    }

    #[test]
    fn esc_and_ctrl_c_abort() {
        let mut s = test_state();
        assert!(matches!(s.on_key(key(KeyCode::Esc)), Flow::Abort));
        assert!(matches!(s.on_key(ctrl_key('c')), Flow::Abort));
        assert!(matches!(s.on_key(ctrl_key('d')), Flow::Abort));
    }

    #[test]
    fn plain_c_without_control_does_not_abort_or_move() {
        let mut s = test_state();
        assert!(matches!(s.on_key(char_key('c')), Flow::Continue));
        assert_eq!(
            s.list.selected(),
            Some(0),
            "非 vim 鍵、非 Ctrl+c，什麼都不該發生"
        );
    }

    #[test]
    fn vim_jk_move_selection_same_as_arrow_keys() {
        let mut s = test_state();
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
        let mut s = test_state();
        let idx = row_of_field(&s.rows, FieldKind::GraphStyle);
        move_to_row(&mut s, idx);
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
        let mut s = test_state();
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
        let s = test_state();
        let order_idx = row_of_field(&s.rows, FieldKind::Order);
        assert!(
            top_row_label(&s.rows[order_idx], &s).contains("時間序"),
            "chrono 是真正的目前值"
        );
        let max_count_idx = row_of(&s.rows, |a| matches!(a, TopRowAction::OpenMaxCount));
        assert!(top_row_label(&s.rows[max_count_idx], &s).contains("不限制"));
    }

    /// `ResolvedDefaults` 讀到設定檔真實值時，顯示與循環起點都要反映它，
    /// 不是精靈自己那套硬預設——這是這批改動要修的核心失真。
    #[test]
    fn resolved_defaults_from_config_drive_the_display_and_cycle_start() {
        let mut core = config::CoreConfig::default();
        core.option.order = Some(CommitOrderType::Topo);
        let mut s = WizardState::with_defaults(ResolvedDefaults::from_core(&core));

        let idx = row_of_field(&s.rows, FieldKind::Order);
        assert!(
            top_row_label(&s.rows[idx], &s).contains("拓撲序"),
            "設定檔寫的是 topo，精靈顯示的目前值要跟著是拓撲序，不是硬預設的時間序"
        );

        move_to_row(&mut s, idx);
        s.on_key(key(KeyCode::Right));
        assert_eq!(
            s.draft.order,
            Some(CommitOrderType::Chrono),
            "topo 才是目前值，第一次按 → 要跳過它切到 chrono，不是繞回 topo"
        );
    }

    #[test]
    fn adjust_number_increments_and_decrements() {
        let mut input = tui_input::Input::new("5".to_string());
        adjust_number(&mut input, true, 0, usize::MAX);
        assert_eq!(input.value(), "6");
        adjust_number(&mut input, false, 0, usize::MAX);
        assert_eq!(input.value(), "5");
    }

    #[test]
    fn adjust_number_does_not_go_below_zero() {
        let mut input = tui_input::Input::new("0".to_string());
        adjust_number(&mut input, false, 0, usize::MAX);
        assert_eq!(input.value(), "0", "usize 沒有負數，減到底就停在 0");
    }

    #[test]
    fn adjust_number_treats_empty_input_as_zero() {
        let mut input = tui_input::Input::default();
        adjust_number(&mut input, true, 0, usize::MAX);
        assert_eq!(input.value(), "1");
    }

    #[test]
    fn adjust_number_respects_custom_min_and_max() {
        let mut input = tui_input::Input::new("1".to_string());
        adjust_number(&mut input, false, 1, 48);
        assert_eq!(input.value(), "1", "已在下限，減不下去");

        let mut input = tui_input::Input::new("48".to_string());
        adjust_number(&mut input, true, 1, 48);
        assert_eq!(input.value(), "48", "已在上限，加不上去");
    }

    #[test]
    fn on_number_key_enter_clamps_the_typed_value() {
        let mut input = tui_input::Input::new("99".to_string());
        assert!(matches!(
            on_number_key(&mut input, key(KeyCode::Enter), 1, 48),
            Some(NumberFlow::Committed(Some(48)))
        ));
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
            assert!(on_number_key(&mut input, decrease_key, 0, usize::MAX).is_none());
            assert_eq!(input.value(), "4", "{decrease_key:?} 應該是 -1");
        }

        for increase_key in [
            key(KeyCode::Right),
            key(KeyCode::Up),
            char_key('l'),
            char_key('k'),
        ] {
            let mut input = tui_input::Input::new("5".to_string());
            assert!(on_number_key(&mut input, increase_key, 0, usize::MAX).is_none());
            assert_eq!(input.value(), "6", "{increase_key:?} 應該是 +1");
        }
    }

    // ── Launch 寫回：apply_touched_settings 是純函式，不碰檔案系統 ──

    #[test]
    fn apply_touched_settings_only_writes_touched_keys() {
        let mut s = test_state();
        let idx = row_of_field(&s.rows, FieldKind::Order);
        move_to_row(&mut s, idx);
        s.on_key(key(KeyCode::Right)); // order = Some(Topo)，其餘欄位沒碰過

        let updated = apply_touched_settings(&s, "").unwrap();
        assert!(updated.contains("order = \"topo\""), "{updated}");
        assert!(
            !updated.contains("graph_width"),
            "沒碰過的鍵不該出現：{updated}"
        );
        assert!(
            !updated.contains("max_count"),
            "沒碰過的鍵不該出現：{updated}"
        );
    }

    #[test]
    fn apply_touched_settings_preserves_existing_comments_and_unrelated_keys() {
        let mut s = test_state();
        let idx = row_of_field(&s.rows, FieldKind::Order);
        move_to_row(&mut s, idx);
        s.on_key(key(KeyCode::Right));

        let existing = "# 我的註解\n[core.search]\nfuzzy = true\n";
        let updated = apply_touched_settings(&s, existing).unwrap();
        assert!(updated.contains("# 我的註解"), "{updated}");
        assert!(updated.contains("fuzzy = true"), "{updated}");
        assert!(updated.contains("order = \"topo\""), "{updated}");
    }

    #[test]
    fn apply_touched_settings_keeps_inline_comment_on_the_key_it_overwrites() {
        // assets/default-config.toml 把每個旋鈕的說明都寫成該鍵值後面的行內
        // 註解——改一次那個值，說明不能跟著消失。
        let mut s = test_state();
        let idx = row_of_field(&s.rows, FieldKind::Order);
        move_to_row(&mut s, idx);
        s.on_key(key(KeyCode::Right));

        let existing = "[core.option]\norder = \"chrono\"  # commit 排序\n";
        let updated = apply_touched_settings(&s, existing).unwrap();
        assert!(
            updated.contains("order = \"topo\"  # commit 排序"),
            "{updated}"
        );
    }

    #[test]
    fn apply_touched_settings_rejects_syntax_broken_existing_file() {
        let s = test_state();
        let broken = "[core.option\norder = chrono"; // 缺右中括號
        assert!(apply_touched_settings(&s, broken).is_err());
    }

    #[test]
    fn apply_touched_settings_does_not_touch_file_when_syntax_is_broken() {
        // 語法壞掉時絕對不能 fallback 成重建新文件——那就是把使用者的檔案
        // 洗掉。這裡驗證錯誤發生前檔案內容完全沒被讀取進 doc 重寫。
        let s = test_state();
        let broken = "not valid = = toml [[[";
        let err = apply_touched_settings(&s, broken).unwrap_err();
        assert!(err.contains("語法錯誤"), "{err}");
    }

    #[test]
    fn apply_touched_settings_removes_max_count_key_when_cleared() {
        let mut s = test_state();
        s.draft.max_count = Some(100);
        s.max_count_touched = true;
        let existing = "[core.option]\nmax_count = 100\n";
        let updated = apply_touched_settings(&s, existing).unwrap();
        assert!(updated.contains("max_count = 100"), "{updated}");

        s.draft.max_count = None; // 明確清空（NumberFlow::Committed(None)）
        let updated2 = apply_touched_settings(&s, &updated).unwrap();
        assert!(
            !updated2.contains("max_count"),
            "清空要移除這個鍵，不是寫 0：{updated2}"
        );
    }

    #[test]
    fn apply_touched_settings_leaves_max_count_alone_when_never_opened() {
        // max_count_touched 為 false（對話框從沒開過）時，即使 draft.max_count
        // 剛好是 None，也不該去動設定檔裡原本的值——「沒碰過」跟「明確清空」
        // 型別上分不出來，touched 旗標就是為了補上這個區別。
        let s = test_state();
        assert!(!s.max_count_touched);
        let existing = "[core.option]\nmax_count = 100\n";
        let updated = apply_touched_settings(&s, existing).unwrap();
        assert!(
            updated.contains("max_count = 100"),
            "沒開過對話框，原本的值要原封不動：{updated}"
        );
    }

    #[test]
    fn apply_touched_settings_writes_update_settings_too() {
        let mut s = test_state();
        let idx = row_of_field(&s.rows, FieldKind::AutoRestart);
        move_to_row(&mut s, idx);
        s.on_key(key(KeyCode::Right)); // auto_restart = Some(On)

        let updated = apply_touched_settings(&s, "").unwrap();
        assert!(updated.contains("auto_restart = \"on\""), "{updated}");
    }
}

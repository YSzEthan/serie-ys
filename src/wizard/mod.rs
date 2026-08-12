mod color_editor;
pub(crate) mod path_browser;

use std::collections::BTreeMap;

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
                match write_touched_settings(&state.draft) {
                    Ok(()) => return Ok(Some(state.draft.args)),
                    Err(e) => state.write_error = Some(e),
                }
            }
            Flow::OpenEditor(Dialog::Path) => {
                let start = path_browser::start_dir(&state.draft.args.path);
                if let Some(p) = path_browser::run(terminal, &start, theme)? {
                    state.draft.args.path = p.to_string_lossy().into_owned();
                }
                terminal.clear()?;
            }
            Flow::OpenEditor(Dialog::Number(field)) => {
                let spec = field.spec();
                let current = field.current(&state.draft, &state.defaults);
                if let NumberFlow::Committed(v) =
                    run_number_input(terminal, current, theme, spec.title, spec.min, spec.max)?
                {
                    field.commit(&mut state.draft, v);
                }
                terminal.clear()?;
            }
            Flow::OpenEditor(Dialog::ColorMenu) => {
                color_editor::run(terminal, &mut state.draft, &state.defaults, theme)?;
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
    /// 顏色編輯器的預覽基準（使用者實際設定，不是 `wizard::run()` 固定用
    /// 的畫面 chrome）。
    theme: ColorTheme,
    /// `true` = 設定檔載入失敗（讀不到／解析失敗／garde 驗證不過），`theme`
    /// 是內建硬預設，不是使用者的真實設定——顏色編輯器要據此在畫面上講
    /// 清楚，不能默默顯示 43 個預設色並宣稱那是使用者的設定。
    theme_is_fallback: bool,
}

impl ResolvedDefaults {
    fn from_core(core: &config::CoreConfig) -> Self {
        Self::from_parts(core, ColorTheme::default())
    }

    fn from_parts(core: &config::CoreConfig, theme: ColorTheme) -> Self {
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
            theme,
            theme_is_fallback: false,
        }
    }

    /// 設定檔壞掉、讀不到、解析失敗——任何 `config::load()` 失敗的原因都一律
    /// 退回內建硬預設，精靈仍然開得起來。這條路徑不能用 `?`。
    fn load() -> Self {
        match config::load() {
            Ok((core, _ui, theme, _keybind)) => Self::from_parts(&core, theme),
            Err(_) => {
                let mut defaults = Self::from_core(&config::CoreConfig::default());
                defaults.theme_is_fallback = true;
                defaults
            }
        }
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
/// 抽成吃 `&mut Option<T>` 的自由函式而不是把 `CycleField` 本身泛型化——後者
/// 才會讓型別設計變複雜，這裡型別完全由呼叫端推導。
///
/// 回傳新值的 clap kebab 名稱，也就是要寫進設定檔的那個字串：切到哪個值、
/// 記什麼字串，由同一次計算產生，`draft.args` 跟 `draft.edits` 不可能漂移。
fn cycle_value<T: ValueEnum + Copy + PartialEq>(
    slot: &mut Option<T>,
    current: T,
    delta: i32,
) -> String {
    let variants = T::value_variants();
    let index_of = |v: T| {
        variants
            .iter()
            .position(|&x| x == v)
            .expect("值一定是合法變體之一")
    };
    let idx = index_of(slot.unwrap_or(current)) as i32;
    let next = (idx + delta).rem_euclid(variants.len() as i32);
    let value = variants[next as usize];
    *slot = Some(value);
    variant_name(&value)
}

// ---------------------------------------------------------------------------
// 可編輯項目。「值要寫到設定檔哪裡」是資料（`ConfigKey`），所以寫回路徑
// （`apply_touched_settings`）不認識任何欄位，新增項目不用動它，也不用動 `Flow`。
// ---------------------------------------------------------------------------

/// 設定檔裡的一個鍵：`{ table: &["core", "option"], key: "order" }`。
/// `table` 是 slice 而不是固定深度的 tuple——顏色要寫的是 `["color"]` 與
/// `["color", "graph"]`，深度本來就不一樣。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct ConfigKey {
    table: &'static [&'static str],
    key: &'static str,
}

const CORE_OPTION: &[&str] = &["core", "option"];
const CORE_UPDATE: &[&str] = &["core", "update"];
const COLOR: &[&str] = &["color"];
const COLOR_GRAPH: &[&str] = &["color", "graph"];

/// ←/→ 在合法值之間原地輪迴切換的欄位。
#[derive(Clone, Copy, PartialEq, Eq)]
enum CycleField {
    Order,
    GraphWidth,
    Compact,
    GraphStyle,
    InitialSelection,
    UpdateMode,
    AutoRestart,
}

impl CycleField {
    /// 鍵名跟欄位名並非一律相同（`update_mode` 寫的是 `mode`、
    /// `update_interval` 寫的是 `interval_hours`），這張表是唯一的真相。
    fn config_key(self) -> ConfigKey {
        let (table, key) = match self {
            CycleField::Order => (CORE_OPTION, "order"),
            CycleField::GraphWidth => (CORE_OPTION, "graph_width"),
            CycleField::Compact => (CORE_OPTION, "compact"),
            CycleField::GraphStyle => (CORE_OPTION, "graph_style"),
            CycleField::InitialSelection => (CORE_OPTION, "initial_selection"),
            CycleField::UpdateMode => (CORE_UPDATE, "mode"),
            CycleField::AutoRestart => (CORE_UPDATE, "auto_restart"),
        };
        ConfigKey { table, key }
    }

    fn flags(self) -> &'static str {
        match self {
            CycleField::Order => "-o, --order",
            CycleField::GraphWidth => "-g, --graph-width",
            CycleField::Compact => "-c, --compact",
            CycleField::GraphStyle => "-s, --graph-style",
            CycleField::InitialSelection => "-i, --initial-selection",
            CycleField::UpdateMode => "--update-mode",
            CycleField::AutoRestart => "--auto-restart",
        }
    }

    fn help(self) -> &'static str {
        match self {
            CycleField::Order => "Commit 排序演算法",
            CycleField::GraphWidth => "Commit 圖形格子寬度",
            CycleField::Compact => "緊湊模式",
            CycleField::GraphStyle => "Commit 圖形邊線風格",
            CycleField::InitialSelection => "初始選取的 commit",
            CycleField::UpdateMode => "自動更新檢查模式",
            CycleField::AutoRestart => "更新後自動重啟／開啟新版",
        }
    }

    /// `delta = 1` 往前一站（→／Enter），`delta = -1` 往後一站（←）。
    /// `args`（啟動用的型別化值）與 `edits`（存檔用的字串）在這裡一起更新，
    /// 這是唯一同時寫兩邊的兩個地方之一（另一個是 `NumberField::commit`）。
    fn cycle(self, draft: &mut Draft, defaults: &ResolvedDefaults, delta: i32) {
        let args = &mut draft.args;
        let name = match self {
            CycleField::Order => cycle_value(&mut args.order, defaults.order, delta),
            CycleField::GraphWidth => {
                cycle_value(&mut args.graph_width, defaults.graph_width, delta)
            }
            CycleField::Compact => cycle_value(&mut args.compact, defaults.compact, delta),
            CycleField::GraphStyle => {
                cycle_value(&mut args.graph_style, defaults.graph_style, delta)
            }
            CycleField::InitialSelection => cycle_value(
                &mut args.initial_selection,
                defaults.initial_selection,
                delta,
            ),
            CycleField::UpdateMode => {
                cycle_value(&mut args.update_mode, defaults.update_mode, delta)
            }
            CycleField::AutoRestart => {
                cycle_value(&mut args.auto_restart, defaults.auto_restart, delta)
            }
        };
        draft.edits.insert(self.config_key(), Some(name.into()));
    }

    /// 目前有效值的中文說明。未設定時顯示的是這個欄位真正的目前值
    /// （`ResolvedDefaults`，讀自設定檔），不是「使用預設值」這種空話。
    fn current_desc(self, draft: &Draft, defaults: &ResolvedDefaults) -> &'static str {
        let args = &draft.args;
        match self {
            CycleField::Order => order_desc(args.order.unwrap_or(defaults.order)),
            CycleField::GraphWidth => {
                graph_width_desc(args.graph_width.unwrap_or(defaults.graph_width))
            }
            CycleField::Compact => compact_desc(args.compact.unwrap_or(defaults.compact)),
            CycleField::GraphStyle => {
                graph_style_desc(args.graph_style.unwrap_or(defaults.graph_style))
            }
            CycleField::InitialSelection => {
                initial_selection_desc(args.initial_selection.unwrap_or(defaults.initial_selection))
            }
            CycleField::UpdateMode => {
                update_mode_desc(args.update_mode.unwrap_or(defaults.update_mode))
            }
            CycleField::AutoRestart => {
                auto_restart_desc(args.auto_restart.unwrap_or(defaults.auto_restart))
            }
        }
    }
}

/// 數字輸入彈窗的參數，跟著欄位走而不是寫死在 `wizard_loop` 裡。
struct NumberSpec {
    title: &'static str,
    min: usize,
    max: usize,
}

/// Enter／→ 開數字輸入彈窗的欄位。
#[derive(Clone, Copy, PartialEq, Eq)]
enum NumberField {
    MaxCount,
    UpdateInterval,
}

impl NumberField {
    fn config_key(self) -> ConfigKey {
        match self {
            NumberField::MaxCount => ConfigKey {
                table: CORE_OPTION,
                key: "max_count",
            },
            NumberField::UpdateInterval => ConfigKey {
                table: CORE_UPDATE,
                key: "interval_hours",
            },
        }
    }

    fn flags(self) -> &'static str {
        match self {
            NumberField::MaxCount => "-n, --max-count <NUMBER>",
            NumberField::UpdateInterval => "--update-interval <HOURS>",
        }
    }

    fn help(self) -> &'static str {
        match self {
            NumberField::MaxCount => "要渲染的最大 commit 數量",
            NumberField::UpdateInterval => "自動更新的檢查間隔",
        }
    }

    fn spec(self) -> NumberSpec {
        match self {
            NumberField::MaxCount => NumberSpec {
                title: "要渲染的最大 commit 數量",
                min: 0,
                max: usize::MAX,
            },
            NumberField::UpdateInterval => NumberSpec {
                title: "自動更新的檢查間隔（小時，1–48）",
                min: update::MIN_INTERVAL_HOURS as usize,
                max: update::MAX_INTERVAL_HOURS as usize,
            },
        }
    }

    /// 目前有效值，也是彈窗開啟時的起始內容。`None` 只有 `max_count` 會發生
    /// （「不限制」）——`update_interval` 一定解得出一個數字。
    fn current(self, draft: &Draft, defaults: &ResolvedDefaults) -> Option<usize> {
        match self {
            NumberField::MaxCount => draft.args.max_count.or(defaults.max_count),
            NumberField::UpdateInterval => Some(
                draft
                    .args
                    .update_interval
                    .unwrap_or(defaults.update_interval) as usize,
            ),
        }
    }

    fn current_label(self, draft: &Draft, defaults: &ResolvedDefaults) -> String {
        // `current()` 對 UpdateInterval 一定回 `Some`（見該函式），`None` 只有
        // MaxCount 會發生；不在這裡重算一次 `defaults.update_interval` 這個
        // 已經在 `current()` 算過的後備值，避免同一條規則兩處真值。
        let Some(n) = self.current(draft, defaults) else {
            return "不限制".to_string();
        };
        match self {
            NumberField::MaxCount => n.to_string(),
            NumberField::UpdateInterval => format!("{n} 小時"),
        }
    }

    /// 使用者在彈窗按下 Enter。`None` = 明確清空，寫回時要移除該鍵——這跟
    /// 「從沒開過這個彈窗」（`edits` 裡根本沒有這個鍵）是兩件不同的事。
    fn commit(self, draft: &mut Draft, value: Option<usize>) {
        match self {
            NumberField::MaxCount => draft.args.max_count = value,
            NumberField::UpdateInterval => draft.args.update_interval = value.map(|n| n as u64),
        }
        draft
            .edits
            .insert(self.config_key(), value.map(|n| (n as i64).into()));
    }
}

/// 開子畫面的編輯器。keybind 之後在這裡加變體，`Flow` 不用跟著長。
#[derive(Clone, Copy)]
enum Dialog {
    Path,
    Number(NumberField),
    ColorMenu,
}

/// 一個項目怎麼編輯：原地循環，或開子畫面。
#[derive(Clone, Copy)]
enum Editor {
    Cycle(CycleField),
    Dialog(Dialog),
}

impl Editor {
    fn flags(self) -> &'static str {
        match self {
            Editor::Cycle(f) => f.flags(),
            Editor::Dialog(Dialog::Path) => "[PATH]",
            Editor::Dialog(Dialog::Number(f)) => f.flags(),
            Editor::Dialog(Dialog::ColorMenu) => "[COLOR]",
        }
    }

    fn help(self) -> &'static str {
        match self {
            Editor::Cycle(f) => f.help(),
            Editor::Dialog(Dialog::Path) => "git 倉庫路徑（僅本次，不存檔）",
            Editor::Dialog(Dialog::Number(f)) => f.help(),
            Editor::Dialog(Dialog::ColorMenu) => "介面配色（43 色）",
        }
    }

    /// 這個 editor 目前有幾個鍵被使用者改過。單鍵項目（Cycle／Number）是
    /// 0 或 1；PATH 永遠是 0——「只影響本次 session、永遠不進設定檔」由
    /// 型別保證：沒有任何程式路徑會為 PATH 建構 `ConfigKey`（`path_browser`
    /// 只寫 `draft.args.path`），不是靠這裡回傳 0 保證的。COLOR 一列管
    /// `["color"]` 整張表（含巢狀的 `["color","graph"]`），可能是
    /// 0..44——用 `starts_with` 而不是相等比較，否則 `[color.graph].branches`
    /// 不會被算進去，使用者只改分支色盤時 `[COLOR]` 那列不會亮 `✓`。
    fn touched_count(self, draft: &Draft) -> usize {
        match self {
            Editor::Cycle(f) => usize::from(draft.edits.contains_key(&f.config_key())),
            Editor::Dialog(Dialog::Number(f)) => {
                usize::from(draft.edits.contains_key(&f.config_key()))
            }
            Editor::Dialog(Dialog::Path) => 0,
            Editor::Dialog(Dialog::ColorMenu) => draft
                .edits
                .keys()
                .filter(|ck| ck.table.starts_with(COLOR))
                .count(),
        }
    }

    /// 目前有效值的顯示字串。循環選擇的欄位用 `< >` 包住，標示「這欄按左右鍵
    /// 會變」；開子畫面的欄位不包——那是另一種互動，混用會誤導使用者以為也能
    /// 直接左右切換。
    fn current_label(self, draft: &Draft, defaults: &ResolvedDefaults) -> String {
        match self {
            Editor::Cycle(f) => format!("< {} >", f.current_desc(draft, defaults)),
            Editor::Dialog(Dialog::Path) => draft.args.path.clone(),
            Editor::Dialog(Dialog::Number(f)) => f.current_label(draft, defaults),
            Editor::Dialog(Dialog::ColorMenu) => match self.touched_count(draft) {
                0 => "未修改".to_string(),
                n => format!("{n} 項已改"),
            },
        }
    }
}

#[derive(Clone, Copy)]
enum RowAction {
    Edit(Editor),
    Launch,
}

/// 精靈主選單的完整清單。新增可編輯項目只需要在這裡加一行——`flags`／
/// `help`／存檔路徑都掛在 `Editor` 上往下委派，不用再手抄一次。
const ROWS: &[RowAction] = &[
    RowAction::Edit(Editor::Dialog(Dialog::Path)),
    RowAction::Edit(Editor::Dialog(Dialog::Number(NumberField::MaxCount))),
    RowAction::Edit(Editor::Cycle(CycleField::Order)),
    RowAction::Edit(Editor::Cycle(CycleField::GraphWidth)),
    RowAction::Edit(Editor::Cycle(CycleField::Compact)),
    RowAction::Edit(Editor::Cycle(CycleField::GraphStyle)),
    RowAction::Edit(Editor::Cycle(CycleField::InitialSelection)),
    RowAction::Edit(Editor::Cycle(CycleField::UpdateMode)),
    RowAction::Edit(Editor::Dialog(Dialog::Number(NumberField::UpdateInterval))),
    RowAction::Edit(Editor::Cycle(CycleField::AutoRestart)),
    RowAction::Edit(Editor::Dialog(Dialog::ColorMenu)),
    RowAction::Launch,
];

/// 每一列的顯示文字：所有欄位都附上目前有效值（讀設定檔得到的真實值，不是
/// 硬預設）。`✓` 標示這欄本次 session 有被使用者主動改過（Launch 時會寫回
/// 設定檔）。
fn top_row_label(row: RowAction, draft: &Draft, defaults: &ResolvedDefaults) -> String {
    let RowAction::Edit(editor) = row else {
        return "▶ 啟動 ysgit".to_string();
    };
    // YSGIT_NO_UPDATE_CHECK 會在 `update::resolve()` 把 mode 壓成 Off——這裡
    // 只是提醒使用者，不是這一列本身的邏輯改變。
    let note = if matches!(editor, Editor::Cycle(CycleField::UpdateMode))
        && std::env::var_os("YSGIT_NO_UPDATE_CHECK").is_some()
    {
        "（目前被 YSGIT_NO_UPDATE_CHECK 壓成 off）"
    } else {
        ""
    };
    let prefix = if draft.is_touched(editor) {
        "✓ "
    } else {
        "  "
    };
    format!(
        "{prefix}{}  {}（目前：{}）{note}",
        editor.flags(),
        editor.help(),
        editor.current_label(draft, defaults)
    )
}

enum Flow {
    Continue,
    Abort,
    Launch,
    OpenEditor(Dialog),
}

/// `Args`（啟動用的型別化真值）＋本次 session 的改動日誌。`edits` 取代
/// 逐欄位的 touched 旗標：鍵不存在＝沒碰過，`Some(None)`＝明確清空（移除
/// 該鍵），`Some(Some(v))`＝設成這個值——三態對應三種語意，不用再靠外部
/// bool 補區別。用 `BTreeMap` 不用 `HashSet`：寫入順序要穩定（`HashSet`
/// 每次執行的迭代順序不同，同樣的操作會產生不同的檔案 diff），而且值直接
/// 掛在鍵上，不用「集合 + 逐欄位取值」兩步。
struct Draft {
    args: Args,
    edits: BTreeMap<ConfigKey, Option<toml_edit::Value>>,
}

impl Draft {
    fn new() -> Self {
        Self {
            args: Args::try_parse_from(["ysgit"]).expect("無參數的 parse 一定要成功"),
            edits: BTreeMap::new(),
        }
    }

    fn is_touched(&self, editor: Editor) -> bool {
        editor.touched_count(self) > 0
    }
}

struct WizardState {
    draft: Draft,
    defaults: ResolvedDefaults,
    list: ListState,
    /// 上一次 Launch 寫回設定檔失敗的原因，顯示在畫面上；成功或還沒按過
    /// Launch 都是 `None`。
    write_error: Option<String>,
}

impl WizardState {
    fn new() -> Self {
        Self::with_defaults(ResolvedDefaults::load())
    }

    fn with_defaults(defaults: ResolvedDefaults) -> Self {
        let mut list = ListState::default();
        list.select(Some(0));
        Self {
            draft: Draft::new(),
            defaults,
            list,
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
        let len = ROWS.len() as i32;
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
        if let RowAction::Edit(Editor::Cycle(field)) = ROWS[row_idx] {
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
        match ROWS[row_idx] {
            RowAction::Edit(Editor::Dialog(d)) => Flow::OpenEditor(d),
            RowAction::Edit(Editor::Cycle(_)) => Flow::Continue,
            RowAction::Launch => Flow::Launch,
        }
    }

    fn render(&mut self, f: &mut Frame, area: Rect, theme: &ColorTheme) {
        let [list_area, error_area, hint_area] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);

        let items: Vec<ListItem> = ROWS
            .iter()
            .map(|&row| ListItem::new(top_row_label(row, &self.draft, &self.defaults)))
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

fn write_touched_settings(draft: &Draft) -> Result<(), String> {
    let Some(path) = config::effective_path() else {
        // 無法決定要寫到哪（`exe_dir()` 解析不出來），安靜放棄，不擋 Launch
        // ——跟 `ensure_config_file()` 同一個哲學：存檔是附加價值，不是
        // 啟動的必要條件。
        return Ok(());
    };
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let updated = migrate_and_apply_touched_settings(draft, &existing)?;
    std::fs::write(&path, updated).map_err(|e| format!("寫入設定檔失敗：{e}"))
}

/// 純函式：`migrate_legacy_toml` + `apply_touched_settings` 的組合，抽出來
/// 讓測試能直接呼叫這個組合本身，而不是自己重抄一份呼叫順序——重抄的話，
/// 這裡萬一漏接 `migrate_legacy_toml`，測試不會發現。這是唯一真的把舊格式
/// 寫回磁碟變成新結構的地方——`config::load()` 只在記憶體裡轉換，不碰檔案
/// （理由見 `config::migrate_legacy_toml` 的 doc comment）。`write_touched_settings`
/// 本來就要覆寫整份檔案，順手轉掉不會多一份風險。
fn migrate_and_apply_touched_settings(draft: &Draft, existing: &str) -> Result<String, String> {
    let migrated = config::migrate_legacy_toml(existing);
    apply_touched_settings(draft, &migrated)
}

/// 純函式：把 `draft.edits` 裡本次 session 被使用者改過的鍵套進既有的 TOML
/// 內容，回傳新的檔案內容字串。不碰檔案系統，方便直接測。這個迴圈不認識
/// 任何欄位——新增可編輯項目只需要讓 `Editor::config_key()` 多回一個
/// `ConfigKey`，這裡完全不用動。
///
/// `Err` 只有一種原因：`existing` 語法本身就壞了（`toml_edit` 也 parse
/// 不動）。這裡直接不寫，絕不能 fallback 成「重建一份新文件」——那就是
/// 把使用者的檔案洗掉。語法合法但值不合法（例如 `graph_style = "asci"`）
/// 不會走到這條錯誤：`toml_edit` 只在乎語法，不驗語意，外科手術式改寫
/// 對這種情況是安全的，順便把壞鍵修掉。
fn apply_touched_settings(draft: &Draft, existing: &str) -> Result<String, String> {
    let mut doc = existing
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("設定檔語法錯誤，這次改動不會存檔：{e}"))?;

    for (ck, value) in &draft.edits {
        let table = config::ensure_table(doc.as_table_mut(), ck.table).ok_or_else(|| {
            format!(
                "設定檔的 `{}` 不是表格，這次改動不會存檔",
                ck.table.join(".")
            )
        })?;
        match value {
            Some(v) => set_preserving_decor(table, ck.key, v.clone()),
            None => {
                table.remove(ck.key);
            }
        }
    }

    Ok(doc.to_string())
}

/// `table[key] = toml_edit::value(v)` 會整個換掉舊的 `Item`，連著它的
/// decor 一起丟掉——`assets/default-config.toml` 每一個旋鈕的說明都寫成
/// 該鍵值後面的行內註解（例如 `order = "chrono"  # commit 排序：...`），
/// 直接指派會讓使用者調一次那個值、說明就跟著消失，正好抵銷「含說明的
/// 預設設定檔」這個賣點。鍵已存在時改成原地覆寫值、保留舊 decor；鍵是
/// 新建的才用一般的 `insert`（沒有舊 decor 可留）。
/// TOML 字串陣列。`multiline` 時每個元素自己一行、留尾隨逗號——
/// `[color.graph].branches` 在範本裡就是這個排版，壓成單行會產生一個
/// 沒人想看的 diff。keybind（#70）的陣列短（1-3 顆鍵）用單行。
///
/// `set_preserving_decor` 保留的是 Item **外圍** decor，陣列**內部**排版
/// 是這裡新建的 `Array` 決定，兩者管的是不同層次，不會互相踩。
fn string_array(items: &[String], multiline: bool) -> toml_edit::Value {
    let mut array = toml_edit::Array::new();
    for item in items {
        let mut value: toml_edit::Value = item.clone().into();
        if multiline {
            *value.decor_mut() = toml_edit::Decor::new("\n  ", "");
        }
        array.push_formatted(value);
    }
    if multiline {
        array.set_trailing("\n");
        array.set_trailing_comma(true);
    }
    toml_edit::Value::Array(array)
}

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
    use ratatui::style::Color as RatatuiColor;

    use super::*;

    fn test_state() -> WizardState {
        WizardState::with_defaults(ResolvedDefaults::from_core(&config::CoreConfig::default()))
    }

    /// 依動作找列索引，取代寫死的數字常數——新增／刪除列時不用逐一改測試。
    fn row_of(pred: impl Fn(&RowAction) -> bool) -> usize {
        ROWS.iter().position(pred).expect("找不到符合條件的列")
    }

    fn row_of_field(field: CycleField) -> usize {
        row_of(move |a| matches!(a, RowAction::Edit(Editor::Cycle(f)) if *f == field))
    }

    fn row_of_number(field: NumberField) -> usize {
        row_of(
            move |a| matches!(a, RowAction::Edit(Editor::Dialog(Dialog::Number(f))) if *f == field),
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
        let idx = row_of_field(CycleField::Order);
        move_to_row(&mut s, idx);
        assert_eq!(s.draft.args.order, None, "還沒碰過，維持未設定");

        s.on_key(key(KeyCode::Right));
        assert_eq!(
            s.draft.args.order,
            Some(CommitOrderType::Topo),
            "chrono 是目前值，第一次按 → 要跳過它，直接切到 topo"
        );

        s.on_key(key(KeyCode::Right));
        assert_eq!(s.draft.args.order, Some(CommitOrderType::Chrono));

        s.on_key(key(KeyCode::Right));
        assert_eq!(
            s.draft.args.order,
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
        let idx = row_of_field(CycleField::GraphStyle);
        move_to_row(&mut s, idx);
        assert_eq!(s.draft.args.graph_style, None);

        s.on_key(key(KeyCode::Left));
        assert_eq!(
            s.draft.args.graph_style,
            Some(GraphStyle::Ascii),
            "第一次按 ← 要跳過目前值 rounded，往後繞到最後一個值 ascii"
        );

        s.on_key(key(KeyCode::Left));
        assert_eq!(
            s.draft.args.graph_style,
            Some(GraphStyle::Angular),
            "← 是往後退，不是又往前"
        );

        s.on_key(key(KeyCode::Left));
        assert_eq!(s.draft.args.graph_style, Some(GraphStyle::Rounded));

        s.on_key(key(KeyCode::Left));
        assert_eq!(
            s.draft.args.graph_style,
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
        let idx = row_of_field(CycleField::GraphStyle);
        move_to_row(&mut s, idx);
        assert_eq!(s.draft.args.graph_style, None);

        s.on_key(key(KeyCode::Right));
        assert_eq!(
            s.draft.args.graph_style,
            Some(GraphStyle::Angular),
            "rounded 是目前值，→ 要跳過它，切到 angular"
        );

        let mut s = test_state();
        let idx = row_of_field(CycleField::GraphStyle);
        move_to_row(&mut s, idx);
        s.on_key(key(KeyCode::Left));
        assert_eq!(
            s.draft.args.graph_style,
            Some(GraphStyle::Ascii),
            "← 也要跳過 rounded，往另一個方向切到 ascii"
        );
    }

    #[test]
    fn cycling_a_type_field_updates_the_row_label_immediately() {
        let mut s = test_state();
        let idx = row_of_field(CycleField::Order);
        move_to_row(&mut s, idx);
        s.on_key(key(KeyCode::Right)); // 跳過目前值，直接切到 topo

        let row = ROWS[idx];
        assert!(
            top_row_label(row, &s.draft, &s.defaults).contains("拓撲序"),
            "切換完不用離開這一列就看得到新值"
        );
    }

    #[test]
    fn cycle_field_rows_wrap_the_value_in_angle_brackets() {
        let s = test_state();
        let idx = row_of_field(CycleField::Order);
        let label = top_row_label(ROWS[idx], &s.draft, &s.defaults);
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
        let idx = row_of_number(NumberField::MaxCount);
        let label = top_row_label(ROWS[idx], &s.draft, &s.defaults);
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
        let idx = row_of_field(CycleField::Order);
        move_to_row(&mut s, idx);
        s.on_key(key(KeyCode::Right)); // 先切到 topo
        assert_eq!(s.draft.args.order, Some(CommitOrderType::Topo));

        assert!(matches!(s.on_key(key(KeyCode::Enter)), Flow::Continue));
        assert_eq!(
            s.draft.args.order,
            Some(CommitOrderType::Topo),
            "Enter 在循環選擇列上不觸發任何動作，值維持不變"
        );
    }

    #[test]
    fn left_is_a_no_op_on_the_path_row() {
        let mut s = test_state();
        assert!(matches!(s.on_key(key(KeyCode::Left)), Flow::Continue));
        assert_eq!(s.draft.args.path, ".", "PATH 不是可循環的欄位，← 不動它");
    }

    #[test]
    fn enter_on_path_row_requests_path_browser() {
        let mut s = test_state();
        assert!(matches!(
            s.on_key(key(KeyCode::Enter)),
            Flow::OpenEditor(Dialog::Path)
        ));
    }

    #[test]
    fn enter_on_max_count_row_requests_number_input() {
        let mut s = test_state();
        let idx = row_of_number(NumberField::MaxCount);
        move_to_row(&mut s, idx);
        assert!(matches!(
            s.on_key(key(KeyCode::Enter)),
            Flow::OpenEditor(Dialog::Number(NumberField::MaxCount))
        ));
    }

    #[test]
    fn enter_on_update_interval_row_requests_number_input() {
        let mut s = test_state();
        let idx = row_of_number(NumberField::UpdateInterval);
        move_to_row(&mut s, idx);
        assert!(matches!(
            s.on_key(key(KeyCode::Enter)),
            Flow::OpenEditor(Dialog::Number(NumberField::UpdateInterval))
        ));
    }

    #[test]
    fn update_mode_row_cycles_and_skips_the_current_variant() {
        let mut s = test_state();
        let idx = row_of_field(CycleField::UpdateMode);
        move_to_row(&mut s, idx);
        assert_eq!(s.draft.args.update_mode, None);

        s.on_key(key(KeyCode::Right));
        assert_eq!(
            s.draft.args.update_mode,
            Some(UpdateMode::Auto),
            "check 是目前值，第一次按 → 跳過它，切到下一個 auto"
        );
    }

    #[test]
    fn auto_restart_row_toggles() {
        let mut s = test_state();
        let idx = row_of_field(CycleField::AutoRestart);
        move_to_row(&mut s, idx);
        assert_eq!(s.draft.args.auto_restart, None);

        s.on_key(key(KeyCode::Right));
        assert_eq!(s.draft.args.auto_restart, Some(AutoRestart::On));
    }

    #[test]
    fn right_on_path_row_also_opens_the_browser() {
        let mut s = test_state();
        assert!(matches!(
            s.on_key(key(KeyCode::Right)),
            Flow::OpenEditor(Dialog::Path)
        ));
    }

    #[test]
    fn right_l_and_enter_all_trigger_launch() {
        for trigger in [key(KeyCode::Right), char_key('l'), key(KeyCode::Enter)] {
            let mut s = test_state();
            let idx = row_of(|a| matches!(a, RowAction::Launch));
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
        let idx = row_of_field(CycleField::GraphStyle);
        move_to_row(&mut s, idx);
        s.on_key(char_key('l'));
        assert_eq!(s.draft.args.graph_style, Some(GraphStyle::Angular));
        s.on_key(char_key('h'));
        assert_eq!(
            s.draft.args.graph_style,
            Some(GraphStyle::Rounded),
            "h 要往回退，不是又往前"
        );
    }

    #[test]
    fn move_selection_clamps_at_both_ends() {
        let mut s = test_state();
        s.on_key(key(KeyCode::Up));
        assert_eq!(s.list.selected(), Some(0), "已在第 0 項，不會變成負數");

        let last = ROWS.len() - 1;
        for _ in 0..last + 5 {
            s.on_key(key(KeyCode::Down));
        }
        assert_eq!(s.list.selected(), Some(last), "已是最後一項，不會再往下");
    }

    #[test]
    fn top_row_shows_the_real_default_not_a_vague_placeholder() {
        let s = test_state();
        let order_idx = row_of_field(CycleField::Order);
        assert!(
            top_row_label(ROWS[order_idx], &s.draft, &s.defaults).contains("時間序"),
            "chrono 是真正的目前值"
        );
        let max_count_idx = row_of_number(NumberField::MaxCount);
        assert!(top_row_label(ROWS[max_count_idx], &s.draft, &s.defaults).contains("不限制"));
    }

    /// `ResolvedDefaults` 讀到設定檔真實值時，顯示與循環起點都要反映它，
    /// 不是精靈自己那套硬預設——這是這批改動要修的核心失真。
    #[test]
    fn resolved_defaults_from_config_drive_the_display_and_cycle_start() {
        let mut core = config::CoreConfig::default();
        core.option.order = Some(CommitOrderType::Topo);
        let mut s = WizardState::with_defaults(ResolvedDefaults::from_core(&core));

        let idx = row_of_field(CycleField::Order);
        assert!(
            top_row_label(ROWS[idx], &s.draft, &s.defaults).contains("拓撲序"),
            "設定檔寫的是 topo，精靈顯示的目前值要跟著是拓撲序，不是硬預設的時間序"
        );

        move_to_row(&mut s, idx);
        s.on_key(key(KeyCode::Right));
        assert_eq!(
            s.draft.args.order,
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
        let idx = row_of_field(CycleField::Order);
        move_to_row(&mut s, idx);
        s.on_key(key(KeyCode::Right)); // order = Some(Topo)，其餘欄位沒碰過

        let updated = apply_touched_settings(&s.draft, "").unwrap();
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
    fn write_touched_settings_upgrades_legacy_toml_before_applying_touched_keys() {
        // 呼叫 `write_touched_settings()` 實際會用的那個組合函式本身，不是
        // 自己重抄一份呼叫順序——重抄的話，`migrate_and_apply_touched_settings`
        // 裡萬一漏接 `migrate_legacy_toml`，這條測試不會發現。
        let mut s = test_state();
        let idx = row_of_field(CycleField::Order);
        move_to_row(&mut s, idx);
        s.on_key(key(KeyCode::Right)); // order = Some(Topo)

        let legacy = "[ui.refs]\nwidth = 40\n";
        let updated = migrate_and_apply_touched_settings(&s.draft, legacy).unwrap();

        assert!(updated.contains("refs_width = 40"), "{updated}");
        assert!(!updated.contains("[ui.refs]"), "{updated}");
        assert!(updated.contains("order = \"topo\""), "{updated}");
    }

    #[test]
    fn apply_touched_settings_preserves_existing_comments_and_unrelated_keys() {
        let mut s = test_state();
        let idx = row_of_field(CycleField::Order);
        move_to_row(&mut s, idx);
        s.on_key(key(KeyCode::Right));

        let existing = "# 我的註解\n[core.search]\nfuzzy = true\n";
        let updated = apply_touched_settings(&s.draft, existing).unwrap();
        assert!(updated.contains("# 我的註解"), "{updated}");
        assert!(updated.contains("fuzzy = true"), "{updated}");
        assert!(updated.contains("order = \"topo\""), "{updated}");
    }

    #[test]
    fn apply_touched_settings_keeps_inline_comment_on_the_key_it_overwrites() {
        // assets/default-config.toml 把每個旋鈕的說明都寫成該鍵值後面的行內
        // 註解——改一次那個值，說明不能跟著消失。
        let mut s = test_state();
        let idx = row_of_field(CycleField::Order);
        move_to_row(&mut s, idx);
        s.on_key(key(KeyCode::Right));

        let existing = "[core.option]\norder = \"chrono\"  # commit 排序\n";
        let updated = apply_touched_settings(&s.draft, existing).unwrap();
        assert!(
            updated.contains("order = \"topo\"  # commit 排序"),
            "{updated}"
        );
    }

    #[test]
    fn apply_touched_settings_rejects_syntax_broken_existing_file() {
        let s = test_state();
        let broken = "[core.option\norder = chrono"; // 缺右中括號
        assert!(apply_touched_settings(&s.draft, broken).is_err());
    }

    #[test]
    fn apply_touched_settings_does_not_touch_file_when_syntax_is_broken() {
        // 語法壞掉時絕對不能 fallback 成重建新文件——那就是把使用者的檔案
        // 洗掉。這裡驗證錯誤發生前檔案內容完全沒被讀取進 doc 重寫。
        let s = test_state();
        let broken = "not valid = = toml [[[";
        let err = apply_touched_settings(&s.draft, broken).unwrap_err();
        assert!(err.contains("語法錯誤"), "{err}");
    }

    #[test]
    fn apply_touched_settings_removes_max_count_key_when_cleared() {
        let mut s = test_state();
        NumberField::MaxCount.commit(&mut s.draft, Some(100));
        let existing = "[core.option]\nmax_count = 100\n";
        let updated = apply_touched_settings(&s.draft, existing).unwrap();
        assert!(updated.contains("max_count = 100"), "{updated}");

        NumberField::MaxCount.commit(&mut s.draft, None); // 明確清空（NumberFlow::Committed(None)）
        let updated2 = apply_touched_settings(&s.draft, &updated).unwrap();
        assert!(
            !updated2.contains("max_count"),
            "清空要移除這個鍵，不是寫 0：{updated2}"
        );
    }

    #[test]
    fn apply_touched_settings_leaves_max_count_alone_when_never_opened() {
        // 這個鍵不在 `edits` 裡（對話框從沒開過）時，即使 `draft.args.max_count`
        // 剛好是 `None`，也不該去動設定檔裡原本的值——「沒碰過」跟「明確清空」
        // 型別上分不出來，`edits` 就是為了補上這個區別。
        let s = test_state();
        assert!(!s
            .draft
            .is_touched(Editor::Dialog(Dialog::Number(NumberField::MaxCount))));
        let existing = "[core.option]\nmax_count = 100\n";
        let updated = apply_touched_settings(&s.draft, existing).unwrap();
        assert!(
            updated.contains("max_count = 100"),
            "沒開過對話框，原本的值要原封不動：{updated}"
        );
    }

    #[test]
    fn apply_touched_settings_writes_update_settings_too() {
        let mut s = test_state();
        let idx = row_of_field(CycleField::AutoRestart);
        move_to_row(&mut s, idx);
        s.on_key(key(KeyCode::Right)); // auto_restart = Some(On)

        let updated = apply_touched_settings(&s.draft, "").unwrap();
        assert!(updated.contains("auto_restart = \"on\""), "{updated}");
    }

    // ── 新架構釘住的不變式：ConfigKey 對應表、空 edits、PATH 的隔離 ──

    /// 9 個項目全部碰過一遍，寫回、再用真正的設定檔 parser（不是自己重抄一份
    /// 反序列化邏輯）讀回來逐欄位比對。這條測試同時證明 9 條表路徑、9 個鍵名
    /// （`update_mode` 寫的是 `mode`、`update_interval` 寫的是 `interval_hours`，
    /// 兩個鍵名跟欄位名不同，最容易打錯）、9 個字串值全對——`toml_edit` 只認
    /// 語法不認語意，鍵名寫錯不會有任何編譯期或執行期警訊，只有真的讀回來
    /// 比對值才抓得到。
    #[test]
    fn every_field_round_trips_through_the_real_config_parser() {
        let mut s = test_state();
        for field in [
            CycleField::Order,
            CycleField::GraphWidth,
            CycleField::Compact,
            CycleField::GraphStyle,
            CycleField::InitialSelection,
            CycleField::UpdateMode,
            CycleField::AutoRestart,
        ] {
            field.cycle(&mut s.draft, &s.defaults, 1);
        }
        NumberField::MaxCount.commit(&mut s.draft, Some(123));
        NumberField::UpdateInterval.commit(&mut s.draft, Some(12));

        let updated = apply_touched_settings(&s.draft, "").unwrap();
        let core = config::parse_core(&updated).unwrap();

        assert_eq!(core.option.order, s.draft.args.order);
        assert_eq!(core.option.graph_width, s.draft.args.graph_width);
        assert_eq!(core.option.compact, s.draft.args.compact);
        assert_eq!(core.option.graph_style, s.draft.args.graph_style);
        assert_eq!(
            core.option.initial_selection,
            s.draft.args.initial_selection
        );
        assert_eq!(core.option.max_count, s.draft.args.max_count);
        assert_eq!(core.update.mode, s.draft.args.update_mode);
        assert_eq!(core.update.interval_hours, s.draft.args.update_interval);
        assert_eq!(core.update.auto_restart, s.draft.args.auto_restart);
    }

    /// 新架構才有的保證：`edits` 是空的，`apply_touched_settings` 一次
    /// `ensure_table` 都不跑。舊版無條件跑 `table_entry(core)/(option)/(update)`，
    /// 就算沒有任何欄位被 touched，也會在缺這些區塊的設定檔裡憑空印出
    /// `[core]`\n\n`[core.option]`\n\n`[core.update]`——這是本次重構刻意改變
    /// 的行為，不是意外。
    #[test]
    fn untouched_wizard_rewrites_the_file_verbatim() {
        let s = test_state();
        let existing = "# 使用者的檔案\n[core.search]\nfuzzy = true\n";
        let updated = apply_touched_settings(&s.draft, existing).unwrap();
        assert_eq!(updated, existing, "什麼都沒改，輸出要逐字等於輸入");
    }

    /// PATH 沒有 `ConfigKey` 可以被塞進 `edits`（`Editor::touched_count` 對
    /// `Dialog::Path` 固定回 `0`，因為沒有任何程式路徑會為它建構
    /// `ConfigKey`）——「只影響本次 session、永遠不進設定檔」由型別保證，
    /// 這裡釘住結果。
    #[test]
    fn path_never_reaches_the_config_file() {
        let mut s = test_state();
        s.draft.args.path = "/some/very/specific/repo".to_string();
        let updated = apply_touched_settings(&s.draft, "").unwrap();
        assert!(!updated.contains("some/very/specific/repo"), "{updated}");
    }

    // ── 顏色（#69）：43 個平面色鍵的寫回路徑 ──

    /// 43 欄各設互不相同的值，寫回、用真正的設定檔 parser（不是自己重抄
    /// 一份反序列化邏輯）讀回來，再透過 `FlatColors`（已經被
    /// `flat_colors_round_trips_through_color_theme` 證明無損）逐欄位比對。
    /// 一次證明 43 個鍵名全對，也順便釘住「值必須寫成 TOML String」——寫成
    /// 整數的話 `Color::deserialize` 的兩條 untagged 分支都會失敗。
    #[test]
    fn every_color_field_round_trips_through_the_real_config_parser() {
        let mut s = test_state();
        for (i, key) in crate::color::COLOR_KEYS.iter().enumerate() {
            let value = crate::color::color_to_config_string(RatatuiColor::Indexed(i as u8));
            s.draft
                .edits
                .insert(ConfigKey { table: COLOR, key }, Some(value.into()));
        }

        let updated = apply_touched_settings(&s.draft, "").unwrap();
        let theme = config::parse_color(&updated).unwrap();
        let flat = crate::color::FlatColors::from(&theme);

        for (i, value) in flat.values.iter().enumerate() {
            assert_eq!(*value, RatatuiColor::Indexed(i as u8), "index {i}");
        }
    }

    #[test]
    fn editing_one_color_leaves_the_other_42_lines_verbatim() {
        let mut s = test_state();
        let asset = include_str!("../../assets/default-config.toml");
        s.draft.edits.insert(
            ConfigKey {
                table: COLOR,
                key: "list_hash_fg",
            },
            Some(crate::color::color_to_config_string(RatatuiColor::Indexed(208)).into()),
        );

        let updated = apply_touched_settings(&s.draft, asset).unwrap();
        assert!(updated.contains("list_hash_fg = \"208\""), "{updated}");

        for line in asset.lines() {
            if line.starts_with("list_hash_fg") {
                continue;
            }
            assert!(updated.contains(line), "遺失了這一行：{line}");
        }
    }

    /// `[COLOR]` 那列的「N 項已改」直接反映 `edits` 裡屬於 `["color"]` 這棵
    /// 子樹的筆數——含巢狀的 `["color","graph"]`。用 `starts_with` 而不是
    /// 相等比較：使用者只改 `[color.graph].branches` 時，這一列一樣要亮。
    #[test]
    fn color_row_shows_the_touched_count() {
        let mut s = test_state();
        let editor = Editor::Dialog(Dialog::ColorMenu);
        assert_eq!(editor.current_label(&s.draft, &s.defaults), "未修改");

        s.draft.edits.insert(
            ConfigKey {
                table: COLOR,
                key: "fg",
            },
            Some(crate::color::color_to_config_string(RatatuiColor::Red).into()),
        );
        assert_eq!(editor.current_label(&s.draft, &s.defaults), "1 項已改");

        s.draft.edits.insert(
            ConfigKey {
                table: &["color", "graph"],
                key: "branches",
            },
            Some(toml_edit::Value::from("dummy")),
        );
        assert_eq!(editor.current_label(&s.draft, &s.defaults), "2 項已改");
    }
}

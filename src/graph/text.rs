use clap::ValueEnum;
use ratatui::style::Color as RatatuiColor;
use serde::Deserialize;

use crate::{
    git::CommitHash,
    graph::{Edge, EdgeType, Graph},
};

/// 文字圖 glyph 的語義角色，跟 `GlyphSet` 把它解成哪個字元無關。分開這一層，
/// `glyph_priority` 與 `Glyph::extends_downward` 才能依語義比對而不是比字元 ——
/// ascii `GlyphSet` 底下四個轉角全都解成同一個 `+`，靠比字元的邏輯已經分不出
/// 它們了。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Glyph {
    Blank,
    CommitDot,
    /// 今天是死的：`build_text_cells` 從不放這個 —— HEAD 的空心圓是渲染時才
    /// 代換上去的（`put_text_cells` 裡的 `is_head`），並沒有存進 cell。之所以
    /// 還留成獨立 variant，是因為它就是舊的 `TEXT_HEAD_DOT` 常數直接改名，而且
    /// `glyph_priority` / `graph_text_head_col` 在這次重構之前就已經帶著同一條
    /// 死分支了。
    HeadDot,
    Vert,
    Horiz,
    CornerTL,
    CornerTR,
    CornerBL,
    CornerBR,
    /// junction glyph：一格同時帶三或四個方向。凡是欄位方向被取聯集的地方就會
    /// 產生 —— `Single` 永遠如此，`Double` 則只在 `Column::can_merge` 允許的欄位
    /// 這樣做。不允許的地方，相撞由 priority 定勝負，輸家的方向就直接不畫。
    TeeDown,
    TeeUp,
    TeeRight,
    TeeLeft,
    Cross,
}

impl Glyph {
    /// commit 標記，不是線段的一部分。
    pub fn is_dot(self) -> bool {
        matches!(self, Glyph::CommitDot | Glyph::HeadDot)
    }

    /// 這個 glyph 的筆畫有沒有碰到 cell 下緣 —— 也就是底下那列 inline detail 的
    /// spacer row 該不該把線接下去（`put_text_spacer`）。
    ///
    /// 規則跟著**畫出來**的字元走，不看背後的 edge，換來一個保證：spacer row 永遠
    /// 不無中生有畫出一筆，也永遠不漏畫一筆。上游 `merged` 做過什麼讓步（單獨一條
    /// `Up` 因為沒有 `╵` 可用，被撐成完整的 `│`）、`double_cells` 的
    /// winner-takes-all 丟掉了哪條 edge，spacer 一律照單全收，渲染因此在列與列之間
    /// 保持自洽。改讀 edge 自己的方向會同時犯下兩種錯：被丟掉那條 edge 的
    /// `DIR_DOWN` 會讓線從沒有向下筆畫的 `╯` 底下冒出來，而被撐開的 `Up` 又會讓
    /// spacer 在一條明明碰到下緣的 `│` 底下留白。
    ///
    /// 寫成 exhaustive `match` 而不是 `matches!` 清單：新增 `Glyph` variant 時一定
    /// 要有人回答「它碰不碰得到下緣」，否則編譯不過。
    pub fn extends_downward(self) -> bool {
        match self {
            // 不是筆畫，是坐在線上的節點：線繼續往 commit 的 parent 走。
            //（root commit 底下其實沒東西，但 glyph 層看不出來。）
            Glyph::CommitDot | Glyph::HeadDot => true,
            Glyph::Vert
            | Glyph::CornerTL
            | Glyph::CornerTR
            | Glyph::TeeDown
            | Glyph::TeeRight
            | Glyph::TeeLeft
            | Glyph::Cross => true,
            Glyph::Blank | Glyph::Horiz | Glyph::CornerBL | Glyph::CornerBR | Glyph::TeeUp => false,
        }
    }
}

/// 一段線碰到四個方位中的哪幾個。
///
/// 這是 edge 幾何的唯一真相：兩種 cell 寬度都從它推出自己的 glyph（`Double` 走
/// `halves`，`Single` 走 `merged`），所以新增一個 `EdgeType` 永遠只需要加一筆。
const DIR_UP: u8 = 1;
const DIR_DOWN: u8 = 2;
const DIR_LEFT: u8 = 4;
const DIR_RIGHT: u8 = 8;

/// 一條 edge 實際碰到的方向。
///
/// 刻意**不**把 `Up`/`Down`/`Left`/`Right` 折成完整的線。把半截線畫成整條線是
/// *渲染*層的讓步（glyph set 裡沒有 `╵╷╴╶`），該住在 `merged` 而不是這裡：在這
/// 一層就折，會讓聯集憑空生出不存在的線。一條 `Down` 跟一條 `Horizontal` 共用
/// 同一欄會變成 `┼`，聲稱有線繼續往上 —— 那比原本要修的 bug 更糟，因為少一條線
/// 只是資訊缺席，畫錯的 junction 卻是假資訊，而讀者會照著它走。
fn edge_dirs(edge_type: EdgeType) -> u8 {
    match edge_type {
        EdgeType::Vertical => DIR_UP | DIR_DOWN,
        EdgeType::Up => DIR_UP,
        EdgeType::Down => DIR_DOWN,
        EdgeType::Horizontal => DIR_LEFT | DIR_RIGHT,
        EdgeType::Left => DIR_LEFT,
        EdgeType::Right => DIR_RIGHT,
        EdgeType::RightTop => DIR_DOWN | DIR_LEFT,
        EdgeType::RightBottom => DIR_UP | DIR_LEFT,
        EdgeType::LeftTop => DIR_DOWN | DIR_RIGHT,
        EdgeType::LeftBottom => DIR_UP | DIR_RIGHT,
    }
}

/// `Double` 的排法：一欄橫跨 [symbol, connector] 兩格。connector 只負責帶「往右
/// 延伸」，其餘全都住在 symbol 裡，而 symbol 畫的形狀跟 `Single` 會畫的一樣。
///
/// 一個只有向右這個方向的欄位，根本沒有線抵達它的中心，所以 symbol 那半格保持
/// 空白 —— 這是 symbol 不等於 `merged(dirs)` 的唯一情況。
///
/// 收的可以是單一 edge 的方向（相撞已由 winner-takes-all 在進來之前解決），也
/// 可以是整欄的聯集。
fn halves(dirs: u8) -> (Glyph, Glyph) {
    let symbol = if dirs == DIR_RIGHT {
        Glyph::Blank
    } else {
        merged(dirs)
    };
    let connector = if dirs & DIR_RIGHT != 0 {
        Glyph::Horiz
    } else {
        Glyph::Blank
    };
    (symbol, connector)
}

/// 把每一種方向組合收斂成一個字元。`Single` 直接用它（一欄一格，所以抵達那一欄
/// 的東西全都得擠進一個字元裡）；`Double` 則透過 `halves` 間接用到。
///
/// 對四個布林值是 exhaustive 的，所以每種方向組合都有答案。「只有垂直位元」／
/// 「只有水平位元」那兩條 arm 就是 #19 那個半截線讓步的所在（單獨一條 `Up` 仍然
/// 畫成完整的 `│`）—— 為什麼該住這裡而不是 `edge_dirs`，見那邊的說明。
fn merged(dirs: u8) -> Glyph {
    let up = dirs & DIR_UP != 0;
    let down = dirs & DIR_DOWN != 0;
    let left = dirs & DIR_LEFT != 0;
    let right = dirs & DIR_RIGHT != 0;

    match (up, down, left, right) {
        (false, false, false, false) => Glyph::Blank,
        (_, _, false, false) => Glyph::Vert,
        (false, false, _, _) => Glyph::Horiz,
        (false, true, false, true) => Glyph::CornerTL,
        (false, true, true, false) => Glyph::CornerTR,
        (true, false, false, true) => Glyph::CornerBL,
        (true, false, true, false) => Glyph::CornerBR,
        (true, true, false, true) => Glyph::TeeRight,
        (true, true, true, false) => Glyph::TeeLeft,
        (false, true, true, true) => Glyph::TeeDown,
        (true, false, true, true) => Glyph::TeeUp,
        (true, true, true, true) => Glyph::Cross,
    }
}

/// 把每個 `Glyph` 對應到某種圖形風格底下實際畫出來的字元。
///
/// 欄位型別是 `&'static str` 不是 `char`：所有消費端（`Cell::set_symbol`、
/// ratatui `Span`、`border::Set`）要的都是 `&str`，存成 `char` 只會在每個呼叫點
/// 多推一次 `encode_utf8`／`to_string()`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlyphSet {
    pub commit_dot: &'static str,
    pub head_dot: &'static str,
    pub vert: &'static str,
    pub horiz: &'static str,
    pub corner_tl: &'static str,
    pub corner_tr: &'static str,
    pub corner_bl: &'static str,
    pub corner_br: &'static str,
    pub tee_down: &'static str,
    pub tee_up: &'static str,
    pub tee_right: &'static str,
    pub tee_left: &'static str,
    pub cross: &'static str,
}

impl GlyphSet {
    pub const ROUNDED: GlyphSet = GlyphSet {
        commit_dot: "●",
        head_dot: "◯",
        vert: "│",
        horiz: "─",
        corner_tl: "╭",
        corner_tr: "╮",
        corner_bl: "╰",
        corner_br: "╯",
        // Unicode 沒有圓角的 tee／cross，所以這幾個跟 ANGULAR 一樣 ——
        // 就像 `vert`／`horiz` 本來就是各風格共用的。
        tee_down: "┬",
        tee_up: "┴",
        tee_right: "├",
        tee_left: "┤",
        cross: "┼",
    };

    pub const ANGULAR: GlyphSet = GlyphSet {
        commit_dot: "●",
        head_dot: "◯",
        vert: "│",
        horiz: "─",
        corner_tl: "┌",
        corner_tr: "┐",
        corner_bl: "└",
        corner_br: "┘",
        tee_down: "┬",
        tee_up: "┴",
        tee_right: "├",
        tee_left: "┤",
        cross: "┼",
    };

    pub const ASCII: GlyphSet = GlyphSet {
        commit_dot: "*",
        head_dot: "o",
        vert: "|",
        horiz: "-",
        corner_tl: "+",
        corner_tr: "+",
        corner_bl: "+",
        corner_br: "+",
        tee_down: "+",
        tee_up: "+",
        tee_right: "+",
        tee_left: "+",
        cross: "+",
    };

    pub fn from_style(style: GraphStyle) -> GlyphSet {
        match style {
            GraphStyle::Rounded => GlyphSet::ROUNDED,
            GraphStyle::Angular => GlyphSet::ANGULAR,
            GraphStyle::Ascii => GlyphSet::ASCII,
        }
    }

    pub fn resolve(&self, glyph: Glyph) -> &'static str {
        match glyph {
            Glyph::Blank => " ",
            Glyph::CommitDot => self.commit_dot,
            Glyph::HeadDot => self.head_dot,
            Glyph::Vert => self.vert,
            Glyph::Horiz => self.horiz,
            Glyph::CornerTL => self.corner_tl,
            Glyph::CornerTR => self.corner_tr,
            Glyph::CornerBL => self.corner_bl,
            Glyph::CornerBR => self.corner_br,
            Glyph::TeeDown => self.tee_down,
            Glyph::TeeUp => self.tee_up,
            Glyph::TeeRight => self.tee_right,
            Glyph::TeeLeft => self.tee_left,
            Glyph::Cross => self.cross,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextCell {
    pub glyph: Glyph,
    pub color: RatatuiColor,
}

impl TextCell {
    const BLANK: TextCell = TextCell {
        glyph: Glyph::Blank,
        color: RatatuiColor::Reset,
    };
}

/// 這個 CLI／設定用的 enum 放在這裡（而不是 `lib.rs`），因為 `GlyphSet::from_style`
/// 是它唯一真正的消費者。這是本 crate 慣例拆法的唯一例外（CLI enum 放 `lib.rs`、
/// 領域型別放 `graph`，例如 `GraphWidthType` -> `CellWidthType`）—— 那個拆法是為
/// 了「CLI 值需要在執行期解析」的型別而存在（`Auto` 取決於終端機寬度，在 `check.rs`
/// 解出來）。`GraphStyle` 沒有這道解析步驟，硬留兩個 enum 外加一個做翻譯的 `From`
/// impl 純粹是重複。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GraphStyle {
    #[default]
    Rounded,
    Angular,
    Ascii,
}

/// 共用的「一個 commit → 它的 text cells」查詢，逐幀渲染路徑
///（`CommitListState::text_cells_for_hash`）與批次快照產生器（`build_text_graph`）
/// 都走這裡。只留一份定義，兩個呼叫端就不可能在 `pos_x`／`pos_y`／`cell_count`
/// 怎麼推出來這件事上悄悄分岔。
pub(crate) fn text_cells(
    graph: &Graph,
    commit_hash: &CommitHash,
    colors: &[RatatuiColor],
    width: CellWidthType,
) -> Option<Vec<TextCell>> {
    let &(pos_x, pos_y) = graph.commit_pos_map.get(commit_hash)?;
    let edges = &graph.edges[pos_y];
    let cell_count = graph.cell_count();
    Some(build_text_cells(pos_x, cell_count, edges, colors, width))
}

/// 依 `commit_hashes` 的順序，把 `graph` 裡每個 commit 都批次渲染成文字。
///
/// 給 `tests/graph.rs` 的快照測試用 —— 它要的是一次拿到整張圖，而不是 UI 那種
/// 逐 commit 查詢。若 `graph.commit_hashes` 與 `graph.commit_pos_map` 不同步就
/// panic（兩者在 `calc.rs` 是一起建的，所以實務上不該觸發）。
pub fn build_text_graph(
    graph: &Graph,
    colors: &[RatatuiColor],
    width: CellWidthType,
) -> Vec<Vec<TextCell>> {
    graph
        .commit_hashes
        .iter()
        .map(|hash| {
            text_cells(graph, hash, colors, width)
                .expect("commit_hashes / commit_pos_map out of sync")
        })
        .collect()
}

/// 把 edges 轉成 lazygit 風格的 text cells（每欄一個 symbol 加一個 connector）。
///
/// 回傳 `cell_count * width.cells_per_column()` 格。多條 edge 共用同一欄時，兩種
/// 寬度的做法不同：
///
/// - `Single` 一欄只有一格，所以永遠把每條 edge 的方向取聯集，再用 `merged`
///   收斂成一個字元
/// - `Double` 只在不損失顏色資訊時才取聯集 —— 見 `Column::can_merge` —— 其餘
///   情況把每個半格判給 priority 最高的 edge
///
/// 兩者都還留著一個已知的缺口：落在 commit 自己那一欄的 edge 會被丟掉，因為那格
/// 屬於 dot。實務上無害（線會在隔壁欄繼續），但確實是真的丟了資訊。
pub(crate) fn build_text_cells(
    commit_pos_x: usize,
    cell_count: usize,
    edges: &[Edge],
    colors: &[RatatuiColor],
    width: CellWidthType,
) -> Vec<TextCell> {
    // 空調色盤會讓每條 edge 都是 `Reset`，於是每一欄都被判為同色，`Double` 會
    // 到處取聯集。只有測試走得到。
    let color_of = |idx: usize| -> RatatuiColor {
        if colors.is_empty() {
            RatatuiColor::Reset
        } else {
            colors[idx % colors.len()]
        }
    };

    let columns = accumulate_columns(cell_count, edges, color_of);
    match width {
        CellWidthType::Double => double_cells(&columns, commit_pos_x, edges, color_of),
        CellWidthType::Single => single_cells(&columns, commit_pos_x, color_of(commit_pos_x)),
    }
}

/// 抵達某一欄的所有 edge 顏色，收斂後的結果。
///
/// `Empty` 跟 `Uniform(Reset)` 不是同一回事：空調色盤底下每條 edge 真的都是
/// `Reset`，所以若一開始就是 `Uniform(Reset)`，第一條真正有顏色的 edge 會被
/// 誤判成衝突。
#[derive(Debug, Clone, Copy, Default)]
enum ColumnColors {
    #[default]
    Empty,
    /// 這個 payload 只在累積過程中被讀出來，跟後面進來的 edge 比對。下游沒有
    /// 任何地方會從這裡取顏色 —— 那是 `Column::symbol_color` 的工作。
    Uniform(RatatuiColor),
    Mixed,
}

/// 一欄圖形的累積狀態。
///
/// `symbol_rank` / `symbol_color` 服務 `Single`，`colors` / `traceless` 服務
/// `Double`；兩邊共用 `dirs`。
///
/// 顏色歸先寫入的那個，跟 `place` 的 `>=`（後寫的贏）恰好相反，而且是刻意的：
/// 相撞若畫成看得見的 junction 而不是悄悄丟掉輸家，顏色就不該取決於 `calc.rs`
/// push edge 的順序。`calc.rs` 真的會在同一個 `pos_x` push 好幾條 `Right`、各帶
/// 不同的 `associated_line_pos_x`（多 parent 的 commit 每個 parent 一條），所以
/// 這是實際會發生的差異，不是理論上的。golden 比對的是字元，跨寬度不變式比對
/// 的是 glyph，所以只有 `text.rs` 自己的單元測試釘得住這件事。
#[derive(Debug, Clone, Copy, Default)]
struct Column {
    dirs: u8,
    /// 抵達這一欄的 edge 裡，排名最高的那個的 rank。0 代表「還沒有 edge」——
    /// 真正的 edge 沒有這麼低的排名，而且沒有 edge 的欄位畫的是 `Blank`，它的
    /// 顏色從來不會被讀。
    symbol_rank: u8,
    symbol_color: RatatuiColor,
    colors: ColumnColors,
    /// 這裡有幾條 edge 若輸了會消失得毫無痕跡。只在乎數到 2 為止。
    traceless: u8,
}

impl Column {
    /// `dirs` 是這一條 edge 自己的方向，永遠不是累積後的聯集：單一 edge 的
    /// `merged` 一定是線、轉角或短劃，正是 `glyph_priority` 在排的東西。聯集
    /// 則可能是 junction，`glyph_priority` 排不了。
    fn absorb(&mut self, dirs: u8, color: RatatuiColor) {
        let rank = glyph_priority(merged(dirs));
        if rank > self.symbol_rank {
            self.symbol_rank = rank;
            self.symbol_color = color;
        }
        self.colors = match self.colors {
            ColumnColors::Empty => ColumnColors::Uniform(color),
            ColumnColors::Uniform(seen) if seen == color => ColumnColors::Uniform(seen),
            _ => ColumnColors::Mixed,
        };
        if leaves_no_trace(dirs) {
            self.traceless = self.traceless.saturating_add(1);
        }
        self.dirs |= dirs;
    }

    /// `Double` 能不能把這一欄贏出來的 symbol 換成抵達這欄的所有 edge 的聯集。
    /// 兩個各自獨立、都成立就允許的理由：
    ///
    /// 1. **這裡每條 edge 都同色。** 一個 junction cell 只有一種前景色，把兩條
    ///    不同顏色的線合成一個 `┼` 會抹掉其中一條線的身分 —— 讀者看到的是一條
    ///    線穿過去，實際上是兩條。同色就沒有東西可抹。
    /// 2. **兩條以上的 edge 會消失得毫無痕跡。** 不合併的代價是丟掉一整條線，
    ///    而不只是丟顏色，更糟。41 個 case 的快照測試剛好沒有這種異色相撞，但
    ///    `calc.rs` 的 detour 邏輯會產生：它的重疊掃描只涵蓋
    ///    `(child_pos_y + 1)..pos_y`，漏掉了已經坐在 `pos_y` 這一列本身的
    ///    edge —— 而 `RightBottom` 正好就落在那裡。
    fn can_merge(&self) -> bool {
        matches!(self.colors, ColumnColors::Uniform(_)) || self.traceless >= 2
    }
}

/// 一條 edge 若輸掉它的半格，是不是就消失得沒有任何線索：它的 symbol 半格
/// 非空白（表示它本來想要這一格），而且它不往右延伸（所以 connector 半格也
/// 是空的）。`Vertical` / `Up` / `Down` / `Left` / `RightTop` / `RightBottom`
/// 都是這樣。
fn leaves_no_trace(dirs: u8) -> bool {
    halves(dirs).0 != Glyph::Blank && dirs & DIR_RIGHT == 0
}

fn accumulate_columns(
    cell_count: usize,
    edges: &[Edge],
    color_of: impl Fn(usize) -> RatatuiColor,
) -> Vec<Column> {
    let mut columns = vec![Column::default(); cell_count];
    for edge in edges {
        // 超出範圍的欄位選擇忽略而不是 panic，跟 `place` 的容忍度一致 ——
        // `build_text_cells` 在測試裡會被手寫 edge 呼叫到。（這裡是欄位索引，
        // 那裡是攤平後的 cell 索引；兩者一致是因為
        // `pos_x * per_col >= cell_count * per_col` 恰好等價於
        // `pos_x >= cell_count`。）
        let Some(column) = columns.get_mut(edge.pos_x) else {
            continue;
        };
        column.absorb(
            edge_dirs(edge.edge_type),
            color_of(edge.associated_line_pos_x),
        );
    }
    columns
}

/// `Single`：一欄一格，整個聯集都擠進這一格裡。
fn single_cells(columns: &[Column], commit_pos_x: usize, dot_color: RatatuiColor) -> Vec<TextCell> {
    columns
        .iter()
        .enumerate()
        .map(|(col, column)| {
            if col == commit_pos_x {
                TextCell {
                    glyph: Glyph::CommitDot,
                    color: dot_color,
                }
            } else {
                TextCell {
                    glyph: merged(column.dirs),
                    color: column.symbol_color,
                }
            }
        })
        .collect()
}

/// `Double`：先 winner-takes-all，再對不損失任何資訊的欄位取聯集
///（`Column::can_merge`）。
///
/// 分兩趟而不是一趟，是因為兩個半格是**各自獨立**地在競爭：畫成 `│─` 的一欄，
/// `│` 來自一條線、`─` 來自另一條，所以「選出贏的 edge 再對它 `halves()`」這種
/// 形狀根本表達不出來。只有 symbol 半格的方向來源會因欄位而異；connector 半格
/// 兩種做法結果都一樣（只要有任何一條 edge 往右延伸，winner-takes-all 就會在
/// 那裡畫 `─`，正好等於聯集的 `DIR_RIGHT` 位元）。
///
/// 這裡的覆寫只換掉 glyph、保留顏色，這需要兩件事同時成立：
///
/// 1. 一個非空白的贏家 symbol，它的顏色是取自這一欄某條 edge 的。在同色規則下
///    那就是唯一的顏色；在 traceless 規則下有可能是輸家的顏色，但 junction 的
///    顏色本來就在它連接的幾條線之間任選一個，無所謂。
/// 2. **空白**的贏家 symbol 會讓那格維持 `TextCell::BLANK`，顏色是 `Reset`。
///    `halves(dirs).0` 只有單獨 `DIR_RIGHT` 才會是空白，而一欄若每條 edge 都是
///    單獨的 `Right`，聯集出來也恰好是 `DIR_RIGHT`，所以那裡的覆寫是 no-op。
///    要是未來哪個 `EdgeType` 也弄出空白的 symbol 半格，這裡會悄悄壞成一個
///    用 `Reset` 畫出來的有色 junction —— 所以才需要下面那個 assertion。
fn double_cells(
    columns: &[Column],
    commit_pos_x: usize,
    edges: &[Edge],
    color_of: impl Fn(usize) -> RatatuiColor,
) -> Vec<TextCell> {
    let mut cells = winner_takes_all_cells(commit_pos_x, columns.len(), edges, color_of);
    let per_col = CellWidthType::Double.cells_per_column();

    for (col, column) in columns.iter().enumerate() {
        // 這一欄屬於 dot：`place` 給了它 priority 10，沒有東西的排名比它高。
        if col == commit_pos_x || !column.can_merge() {
            continue;
        }
        let symbol = halves(column.dirs).0;
        let cell = &mut cells[col * per_col];
        debug_assert!(
            cell.glyph != Glyph::Blank || symbol == Glyph::Blank,
            "a blank winner would keep `Reset` under a non-blank union"
        );
        cell.glyph = symbol;
    }
    cells
}

/// 每個半格都判給 priority 最高的 edge，平手就看誰後 push。這就是以前整個
/// `Double` 的做法；現在則是 `Double` 對無法合併的欄位的退路。
///
/// 當兩條 edge 都想要非空白的 symbol、又都不往右延伸時，輸家消失後右半格
/// 完全沒留下任何線索 —— 見 `leaves_no_trace`，這正是 `can_merge` 唯獨在這種
/// 情況要覆寫這裡的原因。
fn winner_takes_all_cells(
    commit_pos_x: usize,
    cell_count: usize,
    edges: &[Edge],
    color_of: impl Fn(usize) -> RatatuiColor,
) -> Vec<TextCell> {
    let per_col = CellWidthType::Double.cells_per_column();
    let mut cells: Vec<TextCell> = vec![TextCell::BLANK; cell_count * per_col];

    let place = |cells: &mut Vec<TextCell>, idx: usize, glyph: Glyph, color: RatatuiColor| {
        if glyph == Glyph::Blank || idx >= cells.len() {
            return;
        }
        if glyph_priority(glyph) >= glyph_priority(cells[idx].glyph) {
            cells[idx] = TextCell { glyph, color };
        }
    };

    place(
        &mut cells,
        commit_pos_x * per_col,
        Glyph::CommitDot,
        color_of(commit_pos_x),
    );

    for edge in edges {
        let (symbol, connector) = halves(edge_dirs(edge.edge_type));
        let color = color_of(edge.associated_line_pos_x);
        let idx = edge.pos_x * per_col;

        place(&mut cells, idx, symbol, color);
        place(&mut cells, idx + per_col - 1, connector, color);
    }

    cells
}

/// 同一格文字圖裡多個 glyph 重疊時的優先序。數字大的贏；水平 `─` 輸給垂直
/// `│`，這樣一條水平線經過時，貫穿的分支才不會斷掉。
///
/// 服務兩個不同的角色。`winner_takes_all_cells` 用它來決定共用的半格該直接
/// 給哪個 glyph。`Column::absorb` 用它的場合是方向在合併而不是競爭，這時它
/// 只用來挑合出來的 junction 該繼承誰的顏色。
fn glyph_priority(glyph: Glyph) -> u8 {
    match glyph {
        Glyph::CommitDot | Glyph::HeadDot => 10,
        // 兩個呼叫端都到不了這裡：`place` 看到的是 `halves` 的輸出加上明確的
        // `CommitDot`，`Column::absorb` 傳進來的是單一 edge 的 `merged`，永遠
        // 不會是 junction。`HeadDot` 是渲染時才代換上去的（`put_text_cells`
        // 裡的 `is_head`），並沒有存進 cell。列在這裡是為了 exhaustive，排名
        // 也選得讓萬一以後真的用得到時順序依然合理。
        Glyph::TeeDown | Glyph::TeeUp | Glyph::TeeRight | Glyph::TeeLeft | Glyph::Cross => 7,
        Glyph::Vert => 5,
        Glyph::CornerTL | Glyph::CornerTR | Glyph::CornerBL | Glyph::CornerBR => 3,
        Glyph::Horiz => 1,
        Glyph::Blank => 0,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellWidthType {
    Double,
    Single,
}

impl CellWidthType {
    /// 一個 graph 欄佔幾格終端機欄位。`Double` 畫 [symbol, connector]；
    /// `Single` 只畫 symbol。
    pub fn cells_per_column(self) -> usize {
        match self {
            CellWidthType::Double => 2,
            CellWidthType::Single => 1,
        }
    }
}

/// graph 欄的寬度，以格數計 —— 就結構而言，一定跟 `build_text_cells` 的輸出
/// 長度一致。這個「一致」正是 issue #21 修的東西：bug 是這個寬度跟
/// `build_text_cells` 的格數在不同地方各算各的，結果可能（而且真的）對不上。
pub fn graph_cell_width(graph: &Graph, width: CellWidthType) -> u16 {
    (graph.cell_count() * width.cells_per_column()) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_cells_simple_vertical() {
        // 單一 commit dot，同一欄有一條 vertical edge。
        let edges = vec![Edge::new(EdgeType::Vertical, 0, 0)];
        let colors = vec![RatatuiColor::Red, RatatuiColor::Green];
        let cells = build_text_cells(0, 1, &edges, &colors, CellWidthType::Double);
        assert_eq!(cells.len(), 2);
        // commit dot 在 pos_x=0 贏（edge 蓋不掉它）。
        assert_eq!(cells[0].glyph, Glyph::CommitDot);
        assert_eq!(cells[0].color, RatatuiColor::Red);
        // vertical 的 connector 是空白。
        assert_eq!(cells[1].glyph, Glyph::Blank);
    }

    #[test]
    fn text_cells_merge_branch() {
        // commit 在 col 0，col 1 有一條分支併進來（在 LeftTop 轉彎）。
        let edges = vec![
            Edge::new(EdgeType::Vertical, 0, 0),
            Edge::new(EdgeType::LeftTop, 1, 1),
        ];
        let colors = vec![RatatuiColor::Red, RatatuiColor::Green];
        let cells = build_text_cells(0, 2, &edges, &colors, CellWidthType::Double);
        assert_eq!(cells.len(), 4);
        assert_eq!(cells[0].glyph, Glyph::CommitDot);
        // col 1：╭ 配 ─ connector。
        assert_eq!(cells[2].glyph, Glyph::CornerTL);
        assert_eq!(cells[2].color, RatatuiColor::Green);
        assert_eq!(cells[3].glyph, Glyph::Horiz);
    }

    #[test]
    fn text_cells_horizontal_run() {
        // commit 在 col 2，水平的 edge 從 col 0 一路穿過來。
        let edges = vec![
            Edge::new(EdgeType::Horizontal, 0, 0),
            Edge::new(EdgeType::Horizontal, 1, 0),
        ];
        let colors = vec![RatatuiColor::Red];
        let cells = build_text_cells(2, 3, &edges, &colors, CellWidthType::Double);
        assert_eq!(cells.len(), 6);
        assert_eq!(cells[0].glyph, Glyph::Horiz);
        assert_eq!(cells[1].glyph, Glyph::Horiz);
        assert_eq!(cells[2].glyph, Glyph::Horiz);
        assert_eq!(cells[3].glyph, Glyph::Horiz);
        // commit dot 在 col 2 贏。
        assert_eq!(cells[4].glyph, Glyph::CommitDot);
    }

    #[test]
    fn text_cells_left_right_stubs_stay_on_own_half() {
        // col 1 的 left stub：左半格 `─`，右半格空白
        let edges = vec![Edge::new(EdgeType::Left, 1, 0)];
        let cells = build_text_cells(0, 2, &edges, &[RatatuiColor::Red], CellWidthType::Double);
        assert_eq!(cells[2].glyph, Glyph::Horiz);
        assert_eq!(cells[3].glyph, Glyph::Blank);

        // col 0 的 right stub：左半格空白，右半格 `─`
        let edges = vec![Edge::new(EdgeType::Right, 0, 0)];
        let cells = build_text_cells(1, 2, &edges, &[RatatuiColor::Red], CellWidthType::Double);
        // commit 在 col 1，所以 cells[2] 是 dot；col 0 的左半格保持空白
        assert_eq!(cells[0].glyph, Glyph::Blank);
        assert_eq!(cells[1].glyph, Glyph::Horiz);
    }

    /// 兩條不同顏色的線交叉：合成一個 `┼` 會抹掉其中一條的顏色，所以這一欄維持
    /// winner-takes-all。這裡的水平線之所以能保留下來，只因為它同時擁有
    /// connector 半格 —— 換成 `RightTop` 就會消失得毫無痕跡，這正是
    /// `leaves_no_trace` 要覆寫顏色規則的原因。
    #[test]
    fn text_cells_double_keeps_winner_when_a_column_is_multi_coloured() {
        let colors = vec![RatatuiColor::Red, RatatuiColor::Green, RatatuiColor::Blue];
        for edges in colliding_edge_orders(EdgeType::Horizontal, 0, EdgeType::Vertical, 2) {
            let cells = build_text_cells(0, 3, &edges, &colors, CellWidthType::Double);
            assert_eq!(cells[2].glyph, Glyph::Vert);
            assert_eq!(cells[2].color, RatatuiColor::Blue);
            assert_eq!(cells[3].glyph, Glyph::Horiz);
        }
    }

    /// 同一條線上的相同兩種 edge type：沒有東西可抹，所以 symbol 半格同時帶著
    /// 兩個方向。
    #[test]
    fn text_cells_double_unions_a_single_coloured_column() {
        let colors = vec![RatatuiColor::Red, RatatuiColor::Green, RatatuiColor::Blue];
        for edges in colliding_edge_orders(EdgeType::Horizontal, 2, EdgeType::Vertical, 2) {
            let cells = build_text_cells(0, 3, &edges, &colors, CellWidthType::Double);
            assert_eq!(cells[2].glyph, Glyph::Cross);
            assert_eq!(cells[2].color, RatatuiColor::Blue);
            assert_eq!(cells[3].glyph, Glyph::Horiz);
        }
    }

    #[test]
    fn text_cells_empty_colors_fallback() {
        let edges = vec![Edge::new(EdgeType::Vertical, 0, 0)];
        let cells = build_text_cells(0, 1, &edges, &[], CellWidthType::Double);
        assert_eq!(cells[0].glyph, Glyph::CommitDot);
        assert_eq!(cells[0].color, RatatuiColor::Reset);
    }

    /// 兩條 edge 在 column 1 相撞，兩種 push 順序都測。每個相撞的斷言都對兩種
    /// 順序都跑一次，所以這裡沒有任何東西會依賴 `calc.rs` 剛好用哪種順序發出
    /// edge。
    fn colliding_edge_orders(
        a: EdgeType,
        a_line: usize,
        b: EdgeType,
        b_line: usize,
    ) -> [Vec<Edge>; 2] {
        [
            vec![Edge::new(a, 1, a_line), Edge::new(b, 1, b_line)],
            vec![Edge::new(b, 1, b_line), Edge::new(a, 1, a_line)],
        ]
    }

    /// 這四種相撞輸家會消失得一乾二淨：symbol 都非空白，connector 都空白，
    /// 右半格完全沒有東西能透露輸掉的那條 edge。（#30 那張表只列了三種 ——
    /// 漏了 `Left`，它的 symbol 是 `─`、connector 是空白，跟轉角一樣。）
    ///
    /// 這四種聯集出來都是 `U|D|L`，所以可合併的欄位不管撞的是哪一種都畫 `┤`。
    /// 這裡它們共用同一條線，所以光靠顏色規則就允許合併；`leaves_no_trace`
    /// 也會允許。
    #[test]
    fn text_cells_double_keeps_both_edges_where_the_loser_left_no_trace() {
        let colors = vec![RatatuiColor::Red];
        let cases: &[(EdgeType, EdgeType)] = &[
            (EdgeType::Vertical, EdgeType::RightTop),
            (EdgeType::Vertical, EdgeType::RightBottom),
            (EdgeType::Vertical, EdgeType::Left),
            (EdgeType::RightTop, EdgeType::RightBottom),
        ];
        for (a, b) in cases {
            // 兩條 edge 都落在 column 1，它的兩個半格是 cells 2 和 3。
            for edges in colliding_edge_orders(*a, 0, *b, 0) {
                let cells = build_text_cells(0, 2, &edges, &colors, CellWidthType::Double);
                assert_eq!(cells[2].glyph, Glyph::TeeLeft, "{a:?} + {b:?}");
                assert_eq!(cells[3].glyph, Glyph::Blank, "{a:?} + {b:?} connector");

                // 沒有合併的話會畫成什麼：某一條 edge 自己的 symbol，絕不會是
                // 兩條都有，而 connector 完全沒有留下另一條的痕跡。這裡直接
                // 呼叫底層函式，所以即使現在沒有任何寬度會用這種輸入走到這裡，
                // 這個斷言依然釘得住。
                let winner = winner_takes_all_cells(0, 2, &edges, |i| colors[i % colors.len()]);
                let alone = [*a, *b].map(|edge| halves(edge_dirs(edge)).0);
                assert!(
                    alone.contains(&winner[2].glyph),
                    "{a:?} + {b:?}: expected one edge's own symbol, got {:?}",
                    winner[2].glyph
                );
                assert_eq!(winner[3].glyph, Glyph::Blank, "{a:?} + {b:?} connector");
            }
        }
    }

    /// `can_merge` 的第二半：兩條若沒合併就會消失得毫無痕跡的 edge，就算顏色
    /// 不同也會被聯集，因為丟掉一整條線比丟掉一個顏色更糟。
    ///
    /// 41 個 case 的快照測試沒有這種形狀的相撞，但 `calc.rs` 的 detour 邏輯
    /// 會產生 —— 它的重疊掃描只涵蓋 `(child_pos_y + 1)..pos_y`，漏掉了已經
    /// 坐在 `pos_y` 這一列的 edge，而 `RightBottom` 正好就落在那裡。
    #[test]
    fn text_cells_double_unions_multi_coloured_columns_that_would_lose_a_line() {
        let colors = vec![RatatuiColor::Red, RatatuiColor::Green, RatatuiColor::Blue];
        for edges in colliding_edge_orders(EdgeType::RightBottom, 1, EdgeType::Vertical, 2) {
            let cells = build_text_cells(0, 2, &edges, &colors, CellWidthType::Double);
            assert_eq!(cells[2].glyph, Glyph::TeeLeft);
            assert_eq!(cells[3].glyph, Glyph::Blank);
        }
    }

    /// `Column` 的顏色平手歸先寫入者，跟 `place` 的 `>=` 恰好相反。只有
    /// `Single` 會讀這個 —— `Double` 的每個顏色都是從 winner-takes-all 那一趟
    /// 拿的。
    ///
    /// `calc.rs` 真的會走到第一種情況：多 parent 的 commit 在同一個 `pos_x`
    /// 為每個 parent push 一條 `Right`，各自帶著自己 parent 的顏色。
    #[test]
    fn single_width_colour_ties_go_to_the_first_edge() {
        let colors = vec![RatatuiColor::Red, RatatuiColor::Green, RatatuiColor::Blue];

        let edges = vec![
            Edge::new(EdgeType::Right, 1, 1),
            Edge::new(EdgeType::Right, 1, 2),
        ];
        let cells = build_text_cells(0, 2, &edges, &colors, CellWidthType::Single);
        assert_eq!(cells[1].glyph, Glyph::Horiz);
        assert_eq!(cells[1].color, RatatuiColor::Green);

        // 兩個等 rank 的轉角，兩種 push 順序都測：看誰先來。
        for (edges, expected) in [
            (
                vec![
                    Edge::new(EdgeType::RightTop, 1, 1),
                    Edge::new(EdgeType::RightBottom, 1, 2),
                ],
                RatatuiColor::Green,
            ),
            (
                vec![
                    Edge::new(EdgeType::RightBottom, 1, 2),
                    Edge::new(EdgeType::RightTop, 1, 1),
                ],
                RatatuiColor::Blue,
            ),
        ] {
            let cells = build_text_cells(0, 2, &edges, &colors, CellWidthType::Single);
            assert_eq!(cells[1].glyph, Glyph::TeeLeft);
            assert_eq!(cells[1].color, expected);
        }
    }

    /// `halves` 取代了一張手寫的 `EdgeType -> (left, right)` 表。那張表原封
    /// 不動搬到這裡當 anchor：靠推導出來的東西需要有個字面值可以核對，否則
    /// 一個寫錯的方向條目跟一個寫錯的 `halves` arm 可能剛好互相抵消，測試照樣
    /// 過。跟 `glyph_set_tables_match_style_charts` 的道理一樣。
    #[rustfmt::skip]
    #[test]
    fn halves_match_the_original_edge_type_table() {
        let table: &[(EdgeType, (Glyph, Glyph))] = &[
            (EdgeType::Vertical,    (Glyph::Vert,     Glyph::Blank)),
            (EdgeType::Up,          (Glyph::Vert,     Glyph::Blank)),
            (EdgeType::Down,        (Glyph::Vert,     Glyph::Blank)),
            (EdgeType::Horizontal,  (Glyph::Horiz,    Glyph::Horiz)),
            (EdgeType::Left,        (Glyph::Horiz,    Glyph::Blank)),
            (EdgeType::Right,       (Glyph::Blank,    Glyph::Horiz)),
            (EdgeType::RightTop,    (Glyph::CornerTR, Glyph::Blank)),
            (EdgeType::RightBottom, (Glyph::CornerBR, Glyph::Blank)),
            (EdgeType::LeftTop,     (Glyph::CornerTL, Glyph::Horiz)),
            (EdgeType::LeftBottom,  (Glyph::CornerBL, Glyph::Horiz)),
        ];
        for (edge_type, expected) in table {
            assert_eq!(halves(edge_dirs(*edge_type)), *expected, "{edge_type:?}");
        }
    }

    /// 每一種方向組合，逐一列出。下面的查找是走 `0..16`，不是走這張表，所以
    /// 漏列或重複列一列都會直接失敗，而不是悄悄縮小了測試的涵蓋範圍 —— 光靠
    /// 長度斷言的話，「漏一列、多一列」這種情況會蒙混過去。
    #[rustfmt::skip]
    #[test]
    fn merged_covers_every_direction_combination() {
        let u = DIR_UP;
        let d = DIR_DOWN;
        let l = DIR_LEFT;
        let r = DIR_RIGHT;
        let cases: &[(u8, Glyph)] = &[
            (0,             Glyph::Blank),
            // 單獨一條半截線仍然畫成整條線：glyph set 裡沒有 `╵╷╴╶`，
            // 這裡就是 #19 讓步的所在。
            (u,             Glyph::Vert),
            (d,             Glyph::Vert),
            (u | d,         Glyph::Vert),
            (l,             Glyph::Horiz),
            (r,             Glyph::Horiz),
            (l | r,         Glyph::Horiz),
            (d | r,         Glyph::CornerTL),
            (d | l,         Glyph::CornerTR),
            (u | r,         Glyph::CornerBL),
            (u | l,         Glyph::CornerBR),
            (u | d | r,     Glyph::TeeRight),
            (u | d | l,     Glyph::TeeLeft),
            (d | l | r,     Glyph::TeeDown),
            (u | l | r,     Glyph::TeeUp),
            (u | d | l | r, Glyph::Cross),
        ];
        for dirs in 0..16u8 {
            let matches: Vec<Glyph> =
                cases.iter().filter(|(d, _)| *d == dirs).map(|(_, g)| *g).collect();
            assert_eq!(matches.len(), 1, "表格對 dirs={dirs:#06b} 少列或重複列了");
            assert_eq!(merged(dirs), matches[0], "dirs={dirs:#06b}");
        }
    }

    /// `extends_downward` 自成一張表，而這是唯一把它綁在 `merged` 幾何上的東西。
    ///
    /// 它靠的等價關係是：一個已經往下延伸的 glyph，再多給它一個 `DIR_DOWN` 也
    /// 不會變成別的 glyph。這樣就涵蓋全部 13 個線段 glyph（`merged` 對它們是滿射），
    /// 而 `extends_downward` 不必自己再把方向位元重講一遍；`merged` 本身又被
    /// `merged_covers_every_direction_combination` 的字面表釘住，所以這條鏈的
    /// 另一端有錨。
    #[test]
    fn extends_downward_agrees_with_the_direction_bits() {
        for dirs in 0..16u8 {
            let glyph = merged(dirs);
            assert_eq!(
                glyph.extends_downward(),
                glyph == merged(dirs | DIR_DOWN),
                "dirs={dirs:#06b} -> {glyph:?}"
            );
        }
    }

    /// dot 不會從 `merged` 出來，上面那圈迴圈碰不到它 —— 而且它的答案本來就不是
    /// 幾何的：commit 是坐在線上的節點，線繼續往它的 parent 走。
    #[test]
    fn dots_extend_downward_for_a_non_geometric_reason() {
        assert!(Glyph::CommitDot.extends_downward());
        assert!(Glyph::HeadDot.extends_downward());
    }

    /// issue #29 要修的 bug 就是這個：`Single` 底下兩條 edge 共用一格，以前
    /// 輸家會直接消失。現在它們的方向會合併。
    ///
    /// 顏色是贏家的（垂直的排名壓過水平的），而且不論 `calc.rs` push edge 的
    /// 順序是什麼，都維持是贏家的 —— 為什麼這裡的平手判斷是嚴格大於、不是
    /// `place` 的 `>=`，見 `Column` 上的說明。
    #[test]
    fn single_width_unions_colliding_edges_into_a_junction() {
        let colors = vec![RatatuiColor::Red, RatatuiColor::Green, RatatuiColor::Blue];
        for edges in colliding_edge_orders(EdgeType::Horizontal, 0, EdgeType::Vertical, 2) {
            let cells = build_text_cells(0, 3, &edges, &colors, CellWidthType::Single);
            assert_eq!(cells[1].glyph, Glyph::Cross);
            assert_eq!(cells[1].color, RatatuiColor::Blue);
        }
    }

    /// 半截線在取聯集之前絕不能先被撐成整條線，否則 junction 會聲稱有 edge
    /// 沒真的抵達的方向。`Down` 被 `Horizontal` 穿過是 `┬`；若把 `Down` 當成
    /// 完整的 `│` 處理，會產生 `┼`，讓讀者以為有一條不存在的線往上延伸而去
    /// 追它。
    #[rustfmt::skip]
    #[test]
    fn single_width_junctions_never_invent_directions() {
        let colors = vec![RatatuiColor::Red];
        let cases: &[(EdgeType, EdgeType, Glyph)] = &[
            (EdgeType::Down, EdgeType::Horizontal, Glyph::TeeDown),
            (EdgeType::Up,   EdgeType::Horizontal, Glyph::TeeUp),
            (EdgeType::Down, EdgeType::Right,      Glyph::CornerTL),
            (EdgeType::Up,   EdgeType::Left,       Glyph::CornerBR),
        ];
        for (a, b, expected) in cases {
            let edges = vec![Edge::new(*a, 1, 0), Edge::new(*b, 1, 0)];
            let cells = build_text_cells(0, 2, &edges, &colors, CellWidthType::Single);
            assert_eq!(cells[1].glyph, *expected, "{a:?} + {b:?}");
        }
    }

    /// 手寫的 edge 可能指向超出欄位數量的位置；`place` 容忍這種情況，
    /// single-width 的累加器也得容忍。
    #[test]
    fn single_width_ignores_out_of_range_edges() {
        let edges = vec![Edge::new(EdgeType::Vertical, 9, 0)];
        let cells = build_text_cells(0, 2, &edges, &[RatatuiColor::Red], CellWidthType::Single);
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[1].glyph, Glyph::Blank);
    }

    /// `Single` 底下單獨一條 edge 長什麼樣子。沒有東西可以相撞就不會有
    /// junction，所以每個 `EdgeType` 都保持它一直以來的 glyph —— 包括半截線，
    /// 因為沒有 `╵╷╴╶` 可用，還是撐成整條線。issue #29 改的是**相撞**時怎麼
    /// 收斂；這張表釘住的是它沒動到沒相撞的情況。
    #[test]
    fn text_cells_single_width_folds_per_edge_type() {
        let colors = vec![RatatuiColor::Red];
        let cases: &[(EdgeType, Glyph)] = &[
            (EdgeType::Vertical, Glyph::Vert),
            (EdgeType::Up, Glyph::Vert),
            (EdgeType::Down, Glyph::Vert),
            (EdgeType::Horizontal, Glyph::Horiz),
            (EdgeType::Left, Glyph::Horiz),
            (EdgeType::Right, Glyph::Horiz),
            (EdgeType::RightTop, Glyph::CornerTR),
            (EdgeType::RightBottom, Glyph::CornerBR),
            (EdgeType::LeftTop, Glyph::CornerTL),
            (EdgeType::LeftBottom, Glyph::CornerBL),
        ];
        for (edge_type, expected) in cases {
            let edges = vec![Edge::new(*edge_type, 1, 0)];
            let cells = build_text_cells(0, 2, &edges, &colors, CellWidthType::Single);
            assert_eq!(cells.len(), 2, "{edge_type:?}");
            assert_eq!(cells[1].glyph, *expected, "{edge_type:?}");
        }
    }

    #[test]
    fn text_cells_single_width_commit_dot_uses_one_cell_per_column() {
        let cells = build_text_cells(1, 3, &[], &[RatatuiColor::Red], CellWidthType::Single);
        assert_eq!(cells.len(), 3);
        assert_eq!(cells[1].glyph, Glyph::CommitDot);
    }

    /// 把每個 `GlyphSet` 的對應關係釘成一張字面表 —— 不是從 `resolve()` 或
    /// `from_style()` 推出來的，否則一個寫錯的表格條目跟一個寫錯的 dispatch
    /// 可能剛好互相抵消，測試照樣過。這裡也是唯一涵蓋到 `corner_tl` /
    /// `corner_bl` / `head_dot` 的地方：這三個 glyph 在 rounded 風格底下沒有
    /// 出現在任何 golden snapshot 裡（見 tests/graph.rs），沒有這張表它們就
    /// 完全沒有測試涵蓋。
    #[test]
    fn glyph_set_tables_match_style_charts() {
        let cases: &[(GlyphSet, [(Glyph, &str); 14])] = &[
            (
                GlyphSet::ROUNDED,
                [
                    (Glyph::Blank, " "),
                    (Glyph::CommitDot, "●"),
                    (Glyph::HeadDot, "◯"),
                    (Glyph::Vert, "│"),
                    (Glyph::Horiz, "─"),
                    (Glyph::CornerTL, "╭"),
                    (Glyph::CornerTR, "╮"),
                    (Glyph::CornerBL, "╰"),
                    (Glyph::CornerBR, "╯"),
                    // junction 沒有圓角的形狀，所以 rounded 跟 angular
                    // 在這裡是一致的。
                    (Glyph::TeeDown, "┬"),
                    (Glyph::TeeUp, "┴"),
                    (Glyph::TeeRight, "├"),
                    (Glyph::TeeLeft, "┤"),
                    (Glyph::Cross, "┼"),
                ],
            ),
            (
                GlyphSet::ANGULAR,
                [
                    (Glyph::Blank, " "),
                    (Glyph::CommitDot, "●"),
                    (Glyph::HeadDot, "◯"),
                    (Glyph::Vert, "│"),
                    (Glyph::Horiz, "─"),
                    (Glyph::CornerTL, "┌"),
                    (Glyph::CornerTR, "┐"),
                    (Glyph::CornerBL, "└"),
                    (Glyph::CornerBR, "┘"),
                    (Glyph::TeeDown, "┬"),
                    (Glyph::TeeUp, "┴"),
                    (Glyph::TeeRight, "├"),
                    (Glyph::TeeLeft, "┤"),
                    (Glyph::Cross, "┼"),
                ],
            ),
            (
                GlyphSet::ASCII,
                [
                    (Glyph::Blank, " "),
                    (Glyph::CommitDot, "*"),
                    (Glyph::HeadDot, "o"),
                    (Glyph::Vert, "|"),
                    (Glyph::Horiz, "-"),
                    (Glyph::CornerTL, "+"),
                    (Glyph::CornerTR, "+"),
                    (Glyph::CornerBL, "+"),
                    (Glyph::CornerBR, "+"),
                    (Glyph::TeeDown, "+"),
                    (Glyph::TeeUp, "+"),
                    (Glyph::TeeRight, "+"),
                    (Glyph::TeeLeft, "+"),
                    (Glyph::Cross, "+"),
                ],
            ),
        ];
        for (set, mappings) in cases {
            for (glyph, expected) in mappings {
                assert_eq!(set.resolve(*glyph), *expected, "{set:?} {glyph:?}");
            }
        }
    }

    #[test]
    fn from_style_selects_matching_glyph_set() {
        assert_eq!(GlyphSet::from_style(GraphStyle::Rounded), GlyphSet::ROUNDED);
        assert_eq!(GlyphSet::from_style(GraphStyle::Angular), GlyphSet::ANGULAR);
        assert_eq!(GlyphSet::from_style(GraphStyle::Ascii), GlyphSet::ASCII);
    }
}

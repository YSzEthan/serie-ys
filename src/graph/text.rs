use clap::ValueEnum;
use ratatui::style::Color as RatatuiColor;
use serde::Deserialize;

use crate::{
    git::CommitHash,
    graph::{Edge, EdgeType, Graph},
};

/// Semantic role of a text-graph glyph, independent of which characters a
/// `GlyphSet` resolves it to. Distinguishing these lets `glyph_priority` and
/// `put_text_spacer`'s whitelist match on meaning instead of comparing
/// characters -- under an ascii `GlyphSet` all four corners resolve to the
/// same `+`, so char-comparison-based logic can no longer tell them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Glyph {
    Blank,
    CommitDot,
    /// Dead today: `build_text_cells` never places this -- the HEAD hollow
    /// dot is substituted at render time (`is_head` in `put_text_cells`),
    /// not stored in the cell. Kept as its own variant because it's a
    /// direct rename of the old `TEXT_HEAD_DOT` const, and `glyph_priority`
    /// / `graph_text_head_col` already carried this same dead branch before
    /// this refactor.
    HeadDot,
    Vert,
    Horiz,
    CornerTL,
    CornerTR,
    CornerBL,
    CornerBR,
    /// Junction glyphs: a cell carrying three or four directions at once.
    /// Produced by the two widths that union a column's directions
    /// (`Single`, `DoubleF`). `DoubleL` settles collisions by priority
    /// instead, so no single character ever has to carry three directions
    /// there -- which is exactly the information loss #30 is about.
    TeeDown,
    TeeUp,
    TeeRight,
    TeeLeft,
    Cross,
}

/// Which of the four compass directions a piece of line touches.
///
/// This is the single source of truth for edge geometry: every cell width
/// derives its glyphs from it (`halves` for the two doubles, `merged` for
/// `Single`), so a new `EdgeType` only ever needs one entry.
const DIR_UP: u8 = 1;
const DIR_DOWN: u8 = 2;
const DIR_LEFT: u8 = 4;
const DIR_RIGHT: u8 = 8;

/// The directions an edge actually touches.
///
/// Deliberately does NOT fold `Up`/`Down`/`Left`/`Right` into full lines.
/// Drawing a half-stub as a full line is a *rendering* concession (there are
/// no `╵╷╴╶` glyphs in the sets), and it belongs in `merged`, not here:
/// folding at this layer makes unions invent lines that don't exist. A
/// `Down` edge sharing a column with a `Horizontal` one would come out as
/// `┼`, claiming a line continues upward -- worse than the bug being fixed,
/// because a missing line is absent information while a wrong junction is
/// false information the reader will follow.
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

/// Double layout: a column spans [symbol, connector]. The connector only
/// ever carries "extends rightward"; everything else lives in the symbol,
/// which draws the same shape `Single` would.
///
/// A column whose only direction is rightward never has a line reaching its
/// centre, so its symbol half stays empty -- the one case where the symbol
/// isn't just `merged(dirs)`.
///
/// Takes either a single edge's directions (`DoubleL`, which resolves
/// collisions before it gets here) or a whole column's union (`DoubleF`).
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

/// Every direction combination resolved to one character. `Single` uses it
/// directly (one cell per column, so everything reaching that column has to
/// fit in one character); both double widths reach it through `halves`.
///
/// Exhaustive over the four booleans, so every direction combination has an
/// answer. The "only vertical bits" / "only horizontal bits" arms are where
/// the half-stub concession from #19 lives (`Up` alone still renders as a
/// full `│`) -- see `edge_dirs` for why it belongs here and not there.
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

/// Maps each `Glyph` to the character a given graph style draws it as.
///
/// Fields are `&'static str`, not `char`: every consumer (`Cell::set_symbol`,
/// ratatui `Span`, `border::Set`) wants `&str`, so storing `char` would just
/// push an `encode_utf8`/`to_string()` onto each call site.
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
        // Unicode has no rounded tee/cross, so these match ANGULAR's --
        // the same way `vert`/`horiz` are already shared across styles.
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

/// CLI/config enum lives here (not in `lib.rs`) because `GlyphSet::from_style`
/// is its only real consumer. This is the one exception to the crate's usual
/// split (CLI enum in `lib.rs`, domain type in `graph`, e.g.
/// `GraphWidthType` -> `CellWidthType`) -- that split exists for types where
/// the CLI value needs runtime resolution (`Auto` depends on terminal
/// width, resolved in `check.rs`). `GraphStyle` has no such resolution step,
/// so keeping two enums plus a translating `From` impl was pure duplication.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GraphStyle {
    #[default]
    Rounded,
    Angular,
    Ascii,
}

/// Shared "one commit → its text cells" lookup used by both the per-frame
/// render path (`CommitListState::text_cells_for_hash`) and the batch
/// snapshot builder (`build_text_graph`). Keeping a single definition means
/// the two callers can't silently drift apart on how
/// `pos_x`/`pos_y`/`cell_count` are derived.
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

/// Batch text-mode render of every commit in `graph`, in `commit_hashes` order.
///
/// Used by the `tests/graph.rs` snapshot suite, which needs the whole graph
/// at once rather than the per-commit lookups the UI does. Panics if
/// `graph.commit_hashes` and `graph.commit_pos_map` are out of sync (they're
/// built together in `calc.rs`, so this should never trigger in practice).
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

/// Convert edges to lazygit-style text cells (symbol + connector per column).
///
/// Returns `cell_count * width.cells_per_column()` cells. There is really
/// only one fork here -- whether a column's edges are unioned or fight over
/// the cell:
///
/// - `DoubleL` gives each half-cell to the highest-priority edge and drops
///   the rest (the pre-#30 behaviour, kept verbatim)
/// - `DoubleF` and `Single` union every edge's directions per column, then
///   resolve the total: `halves` splits it back into [symbol, connector],
///   `merged` squeezes it into one cell. Either way several lines meeting in
///   one column come out as a box-drawing junction (`┼┬┴├┤`) instead of the
///   loser vanishing.
///
/// One acknowledged gap remains in all three: edges on the commit's own
/// column are still dropped, since the dot owns that cell. Harmless in
/// practice (the line continues in the neighbouring column) but it is real
/// lost information.
pub(crate) fn build_text_cells(
    commit_pos_x: usize,
    cell_count: usize,
    edges: &[Edge],
    colors: &[RatatuiColor],
    width: CellWidthType,
) -> Vec<TextCell> {
    let color_of = |idx: usize| -> RatatuiColor {
        if colors.is_empty() {
            RatatuiColor::Reset
        } else {
            colors[idx % colors.len()]
        }
    };

    match width {
        CellWidthType::DoubleL => legacy_double_cells(commit_pos_x, cell_count, edges, color_of),
        CellWidthType::DoubleF => {
            let columns = accumulate_columns(cell_count, edges, color_of);
            fused_double_cells(&columns, commit_pos_x, color_of(commit_pos_x))
        }
        CellWidthType::Single => {
            let columns = accumulate_columns(cell_count, edges, color_of);
            single_cells(&columns, commit_pos_x, color_of(commit_pos_x))
        }
    }
}

/// One graph column's accumulated state, shared by `DoubleF` and `Single`.
///
/// Both colours go to whoever writes first, which is the opposite of
/// `place`'s `>=` (last writer wins) and deliberately so: now that a
/// collision renders as a visible junction rather than silently dropping the
/// loser, the colour shouldn't depend on the order `calc.rs` happens to push
/// edges. `calc.rs` really does push several `Right` edges at one `pos_x`
/// with different `associated_line_pos_x` (one per parent of a multi-parent
/// commit), so this is a reachable difference, not a theoretical one.
/// Goldens compare characters and the cross-width invariants compare glyphs,
/// so only `text.rs`'s own unit tests pin it.
#[derive(Debug, Clone, Copy, Default)]
struct Column {
    dirs: u8,
    /// Rank of the highest-ranked edge to reach this column. 0 means "no
    /// edge yet" -- no real edge ranks that low, and a column with none
    /// draws `Blank`, whose colour is never looked at.
    symbol_rank: u8,
    symbol_color: RatatuiColor,
    /// First edge that extends rightward. Unlike the rank above there's no
    /// value to compare, so the `Option` is what distinguishes "not written"
    /// from "written as `Reset`". Only `DoubleF` draws it.
    connector: Option<RatatuiColor>,
}

impl Column {
    /// `dirs` is this one edge's directions, never the accumulated union:
    /// `merged` of a single edge is always a line, corner or dash, which is
    /// what `glyph_priority` ranks. A union can be a junction, which it
    /// doesn't.
    fn absorb(&mut self, dirs: u8, color: RatatuiColor) {
        let rank = glyph_priority(merged(dirs));
        if rank > self.symbol_rank {
            self.symbol_rank = rank;
            self.symbol_color = color;
        }
        if dirs & DIR_RIGHT != 0 {
            self.connector.get_or_insert(color);
        }
        self.dirs |= dirs;
    }

    /// A column's symbol half. The commit's own column is owned by the dot;
    /// everywhere else draws `glyph` in the winning edge's colour.
    fn symbol_cell(&self, glyph: Glyph, dot: Option<RatatuiColor>) -> TextCell {
        match dot {
            Some(color) => TextCell {
                glyph: Glyph::CommitDot,
                color,
            },
            None => TextCell {
                glyph,
                color: self.symbol_color,
            },
        }
    }
}

fn accumulate_columns(
    cell_count: usize,
    edges: &[Edge],
    color_of: impl Fn(usize) -> RatatuiColor,
) -> Vec<Column> {
    let mut columns = vec![Column::default(); cell_count];
    for edge in edges {
        // Out-of-range columns are ignored rather than panicking, matching
        // `place`'s tolerance -- `build_text_cells` is reachable from tests
        // with hand-built edges. (Column index here, flat cell index there;
        // the two agree because `pos_x * per_col >= cell_count * per_col`
        // iff `pos_x >= cell_count`.)
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

/// `Single`: one cell per column, the whole union squeezed into it.
fn single_cells(columns: &[Column], commit_pos_x: usize, dot_color: RatatuiColor) -> Vec<TextCell> {
    columns
        .iter()
        .enumerate()
        .map(|(col, column)| {
            let dot = (col == commit_pos_x).then_some(dot_color);
            column.symbol_cell(merged(column.dirs), dot)
        })
        .collect()
}

/// `DoubleF`: the same union, split back across [symbol, connector].
///
/// The symbol half is exactly what `Single` draws, except for a column whose
/// only direction is rightward -- there the line never reaches the centre,
/// so the symbol stays blank and the connector carries the `─` alone.
fn fused_double_cells(
    columns: &[Column],
    commit_pos_x: usize,
    dot_color: RatatuiColor,
) -> Vec<TextCell> {
    columns
        .iter()
        .enumerate()
        .flat_map(|(col, column)| {
            let (symbol, connector) = halves(column.dirs);
            let dot = (col == commit_pos_x).then_some(dot_color);
            [
                column.symbol_cell(symbol, dot),
                TextCell {
                    glyph: connector,
                    color: column.connector.unwrap_or(RatatuiColor::Reset),
                },
            ]
        })
        .collect()
}

/// `DoubleL`: the pre-#30 double, frozen.
///
/// Each half-cell goes to the highest-priority edge, ties broken by whoever
/// pushed last. When two edges both want a non-blank symbol and neither
/// extends rightward (`Vertical` / `Left` / `RightTop` / `RightBottom`
/// against each other), the loser vanishes with nothing left in the right
/// half to hint at it. That is the defect #30 is about, and `DoubleF` is
/// where it's fixed -- this function exists because all three widths were
/// wanted side by side, not because it's right. Don't "improve" it: its
/// whole definition is "identical to what double used to draw".
fn legacy_double_cells(
    commit_pos_x: usize,
    cell_count: usize,
    edges: &[Edge],
    color_of: impl Fn(usize) -> RatatuiColor,
) -> Vec<TextCell> {
    let per_col = CellWidthType::DoubleL.cells_per_column();
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

/// Precedence for overlapping glyphs on the same text-graph cell.
/// Higher wins; horizontal `─` loses to vertical `│` so through-branches
/// remain continuous when a horizontal run passes by.
///
/// Serves both paths, in two different roles. `DoubleL` uses it to decide
/// which glyph takes a shared half-cell outright. `DoubleF` and `Single`
/// don't let glyphs compete at all -- their directions combine -- so there
/// it only picks whose colour the resulting junction inherits.
fn glyph_priority(glyph: Glyph) -> u8 {
    match glyph {
        Glyph::CommitDot | Glyph::HeadDot => 10,
        // Unreachable from either caller: `place` sees `halves` output plus
        // an explicit `CommitDot`, and `Column::absorb` passes `merged` of a
        // single edge, which is never a junction. `HeadDot` is substituted
        // at render time (`is_head` in `put_text_cells`) rather than stored.
        // Listed for exhaustiveness, and ranked so the ordering still reads
        // correctly if that ever changes.
        Glyph::TeeDown | Glyph::TeeUp | Glyph::TeeRight | Glyph::TeeLeft | Glyph::Cross => 7,
        Glyph::Vert => 5,
        Glyph::CornerTL | Glyph::CornerTR | Glyph::CornerBL | Glyph::CornerBR => 3,
        Glyph::Horiz => 1,
        Glyph::Blank => 0,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellWidthType {
    /// `double-l`: each half-cell goes to the highest-priority edge, the
    /// rest are dropped. The only double there was before #30.
    DoubleL, // 2 cells
    /// `double-f`: a column's directions are unioned first, then split back
    /// into [symbol, connector]. What `auto` picks when the width allows.
    DoubleF, // 2 cells
    Single,
}

impl CellWidthType {
    /// Terminal columns one graph column occupies. Both doubles draw
    /// [symbol, connector]; `Single` draws the symbol only.
    pub fn cells_per_column(self) -> usize {
        match self {
            CellWidthType::DoubleL | CellWidthType::DoubleF => 2,
            CellWidthType::Single => 1,
        }
    }
}

/// The graph column's width in cells -- identical by construction to
/// `build_text_cells`'s output length. That equality IS issue #21's fix:
/// the bug was this width and `build_text_cells`'s cell count being computed
/// in separate places that could (and did) disagree.
pub fn graph_cell_width(graph: &Graph, width: CellWidthType) -> u16 {
    (graph.cell_count() * width.cells_per_column()) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tests below that use this fixture put at most one edge in any
    /// column, so the two doubles are provably identical there: with a
    /// single edge the union *is* that edge's directions, and `place`'s
    /// priority never gets anyone to compete with. Running them against
    /// both variants pins that equality, and stops `DoubleF` from resting
    /// on the collision tests alone for basic-shape coverage.
    const DOUBLES: [CellWidthType; 2] = [CellWidthType::DoubleL, CellWidthType::DoubleF];

    #[test]
    fn text_cells_simple_vertical() {
        // Single commit dot with a vertical edge on the same column.
        let edges = vec![Edge::new(EdgeType::Vertical, 0, 0)];
        let colors = vec![RatatuiColor::Red, RatatuiColor::Green];
        for width in DOUBLES {
            let cells = build_text_cells(0, 1, &edges, &colors, width);
            assert_eq!(cells.len(), 2, "{width:?}");
            // Commit dot wins at pos_x=0 (edge doesn't clobber).
            assert_eq!(cells[0].glyph, Glyph::CommitDot, "{width:?}");
            assert_eq!(cells[0].color, RatatuiColor::Red, "{width:?}");
            // Connector is blank for vertical.
            assert_eq!(cells[1].glyph, Glyph::Blank, "{width:?}");
        }
    }

    #[test]
    fn text_cells_merge_branch() {
        // Commit at col 0, with a branch coming in from col 1 (curve at LeftTop).
        let edges = vec![
            Edge::new(EdgeType::Vertical, 0, 0),
            Edge::new(EdgeType::LeftTop, 1, 1),
        ];
        let colors = vec![RatatuiColor::Red, RatatuiColor::Green];
        for width in DOUBLES {
            let cells = build_text_cells(0, 2, &edges, &colors, width);
            assert_eq!(cells.len(), 4, "{width:?}");
            assert_eq!(cells[0].glyph, Glyph::CommitDot, "{width:?}");
            // Col 1: ╭ with ─ connector.
            assert_eq!(cells[2].glyph, Glyph::CornerTL, "{width:?}");
            assert_eq!(cells[2].color, RatatuiColor::Green, "{width:?}");
            assert_eq!(cells[3].glyph, Glyph::Horiz, "{width:?}");
        }
    }

    #[test]
    fn text_cells_horizontal_run() {
        // Commit at col 2 with a horizontal edge running across from col 0.
        let edges = vec![
            Edge::new(EdgeType::Horizontal, 0, 0),
            Edge::new(EdgeType::Horizontal, 1, 0),
        ];
        let colors = vec![RatatuiColor::Red];
        for width in DOUBLES {
            let cells = build_text_cells(2, 3, &edges, &colors, width);
            assert_eq!(cells.len(), 6, "{width:?}");
            assert_eq!(cells[0].glyph, Glyph::Horiz, "{width:?}");
            assert_eq!(cells[1].glyph, Glyph::Horiz, "{width:?}");
            assert_eq!(cells[2].glyph, Glyph::Horiz, "{width:?}");
            assert_eq!(cells[3].glyph, Glyph::Horiz, "{width:?}");
            // Commit dot wins at col 2.
            assert_eq!(cells[4].glyph, Glyph::CommitDot, "{width:?}");
        }
    }

    #[test]
    fn text_cells_left_right_stubs_stay_on_own_half() {
        for width in DOUBLES {
            // Left stub at col 1: left half `─`, right half blank
            let edges = vec![Edge::new(EdgeType::Left, 1, 0)];
            let cells = build_text_cells(0, 2, &edges, &[RatatuiColor::Red], width);
            assert_eq!(cells[2].glyph, Glyph::Horiz, "{width:?}");
            assert_eq!(cells[3].glyph, Glyph::Blank, "{width:?}");

            // Right stub at col 0: left half blank, right half `─`
            let edges = vec![Edge::new(EdgeType::Right, 0, 0)];
            let cells = build_text_cells(1, 2, &edges, &[RatatuiColor::Red], width);
            // commit is at col 1 so cells[2] is the dot; col 0 left half stays blank
            assert_eq!(cells[0].glyph, Glyph::Blank, "{width:?}");
            assert_eq!(cells[1].glyph, Glyph::Horiz, "{width:?}");
        }
    }

    /// `DoubleL`'s defining behaviour: the higher-priority glyph takes the
    /// symbol half outright. The horizontal survives only because it also
    /// owns the connector half -- swap it for a `RightTop` and it would
    /// vanish without trace, which is what #30 is about.
    #[test]
    fn text_cells_double_l_lets_vertical_win_over_horizontal() {
        let colors = vec![RatatuiColor::Red, RatatuiColor::Green, RatatuiColor::Blue];
        for edges in colliding_edge_orders(EdgeType::Horizontal, 0, EdgeType::Vertical, 2) {
            let cells = build_text_cells(0, 3, &edges, &colors, CellWidthType::DoubleL);
            assert_eq!(cells[2].glyph, Glyph::Vert);
            assert_eq!(cells[2].color, RatatuiColor::Blue);
            assert_eq!(cells[3].glyph, Glyph::Horiz);
        }
    }

    /// The same collision under `DoubleF`: the symbol half now carries both
    /// edges. Colour still follows the vertical (rank 3 beats rank 1), and
    /// the connector half is unchanged.
    #[test]
    fn text_cells_double_f_unions_vertical_and_horizontal() {
        let colors = vec![RatatuiColor::Red, RatatuiColor::Green, RatatuiColor::Blue];
        for edges in colliding_edge_orders(EdgeType::Horizontal, 0, EdgeType::Vertical, 2) {
            let cells = build_text_cells(0, 3, &edges, &colors, CellWidthType::DoubleF);
            assert_eq!(cells[2].glyph, Glyph::Cross);
            assert_eq!(cells[2].color, RatatuiColor::Blue);
            assert_eq!(cells[3].glyph, Glyph::Horiz);
        }
    }

    #[test]
    fn text_cells_empty_colors_fallback() {
        let edges = vec![Edge::new(EdgeType::Vertical, 0, 0)];
        for width in DOUBLES {
            let cells = build_text_cells(0, 1, &edges, &[], width);
            assert_eq!(cells[0].glyph, Glyph::CommitDot, "{width:?}");
            assert_eq!(cells[0].color, RatatuiColor::Reset, "{width:?}");
        }
    }

    /// Two edges colliding on column 1, in both push orders. Every
    /// collision assertion runs against both, so nothing here can depend on
    /// the order `calc.rs` happens to emit edges in.
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

    /// The four collisions where `DoubleL` loses an edge with zero trace:
    /// both symbols non-blank, both connectors blank, so the loser leaves
    /// nothing behind in the right half either. (#30's table lists three --
    /// it missed `Left`, whose symbol is `─` and whose connector is blank
    /// just like the corners'.)
    ///
    /// All four union to `U|D|L`, so `DoubleF` draws the same `┤` for every
    /// one of them.
    #[test]
    fn text_cells_double_f_keeps_both_edges_where_double_l_dropped_one() {
        let colors = vec![RatatuiColor::Red];
        let cases: &[(EdgeType, EdgeType)] = &[
            (EdgeType::Vertical, EdgeType::RightTop),
            (EdgeType::Vertical, EdgeType::RightBottom),
            (EdgeType::Vertical, EdgeType::Left),
            (EdgeType::RightTop, EdgeType::RightBottom),
        ];
        for (a, b) in cases {
            // Both edges land on column 1, whose halves are cells 2 and 3.
            for edges in colliding_edge_orders(*a, 0, *b, 0) {
                let fused = build_text_cells(0, 2, &edges, &colors, CellWidthType::DoubleF);
                assert_eq!(fused[2].glyph, Glyph::TeeLeft, "{a:?} + {b:?}");
                assert_eq!(fused[3].glyph, Glyph::Blank, "{a:?} + {b:?} connector");

                // The `DoubleL` half of the story: whatever is on screen is
                // one edge's own symbol, never both -- and the connector
                // holds no trace of the other.
                let legacy = build_text_cells(0, 2, &edges, &colors, CellWidthType::DoubleL);
                let alone = [*a, *b].map(|edge| halves(edge_dirs(edge)).0);
                assert!(
                    alone.contains(&legacy[2].glyph),
                    "{a:?} + {b:?}: expected one edge's own symbol, got {:?}",
                    legacy[2].glyph
                );
                assert_eq!(legacy[3].glyph, Glyph::Blank, "{a:?} + {b:?} connector");
            }
        }
    }

    /// Colour ties go to the first writer, so nothing here depends on
    /// `calc.rs`'s push order -- the opposite of `DoubleL`'s `>=`.
    ///
    /// `calc.rs` reaches the connector case for real: a multi-parent commit
    /// pushes one `Right` per parent at the same `pos_x`, each carrying its
    /// own parent's colour.
    #[test]
    fn text_cells_double_f_colour_ties_go_to_the_first_edge() {
        let colors = vec![RatatuiColor::Red, RatatuiColor::Green, RatatuiColor::Blue];

        // Column 1's halves are cells 2 and 3.
        //
        // Two `Right` edges on one column, different lines: the symbol half
        // stays blank (nothing reaches the centre), the connector takes the
        // first edge's colour.
        let edges = vec![
            Edge::new(EdgeType::Right, 1, 1),
            Edge::new(EdgeType::Right, 1, 2),
        ];
        let cells = build_text_cells(0, 2, &edges, &colors, CellWidthType::DoubleF);
        assert_eq!(cells[2].glyph, Glyph::Blank);
        assert_eq!(cells[3].glyph, Glyph::Horiz);
        assert_eq!(cells[3].color, RatatuiColor::Green);

        // Two corners, equal rank: the symbol half keeps the first one's
        // colour even though the second one contributes directions.
        let edges = vec![
            Edge::new(EdgeType::RightTop, 1, 1),
            Edge::new(EdgeType::RightBottom, 1, 2),
        ];
        let cells = build_text_cells(0, 2, &edges, &colors, CellWidthType::DoubleF);
        assert_eq!(cells[2].glyph, Glyph::TeeLeft);
        assert_eq!(cells[2].color, RatatuiColor::Green);

        // Same two edges, opposite order: now the other one is first.
        let edges = vec![
            Edge::new(EdgeType::RightBottom, 1, 2),
            Edge::new(EdgeType::RightTop, 1, 1),
        ];
        let cells = build_text_cells(0, 2, &edges, &colors, CellWidthType::DoubleF);
        assert_eq!(cells[2].glyph, Glyph::TeeLeft);
        assert_eq!(cells[2].color, RatatuiColor::Blue);
    }

    /// `halves` replaced a hand-written `EdgeType -> (left, right)` table.
    /// That table is reproduced here verbatim as the anchor: derivation
    /// needs something literal to be checked against, or a wrong direction
    /// entry and a wrong `halves` arm could cancel out and still pass. Same
    /// reasoning as `glyph_set_tables_match_style_charts`.
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

    /// Every direction combination, spelled out. The lookup below walks
    /// `0..16` rather than the table, so an omitted or duplicated row fails
    /// instead of quietly shrinking what gets checked -- a length assertion
    /// alone would let "one missing, one duplicated" through.
    #[rustfmt::skip]
    #[test]
    fn merged_covers_every_direction_combination() {
        let u = DIR_UP;
        let d = DIR_DOWN;
        let l = DIR_LEFT;
        let r = DIR_RIGHT;
        let cases: &[(u8, Glyph)] = &[
            (0,             Glyph::Blank),
            // A lone half-stub still renders as a full line: there are no
            // `╵╷╴╶` glyphs, so this is where #19's concession lives.
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

    /// The bug issue #29 is about: under `Single` two edges share one cell,
    /// and the loser used to vanish outright. Now their directions combine.
    ///
    /// The colour is the winner's (vertical outranks horizontal), and it
    /// stays the winner's regardless of the order `calc.rs` pushed the
    /// edges -- see `Column` on why the tie-break is strict rather than
    /// `place`'s `>=`.
    #[test]
    fn single_width_unions_colliding_edges_into_a_junction() {
        let colors = vec![RatatuiColor::Red, RatatuiColor::Green, RatatuiColor::Blue];
        for edges in colliding_edge_orders(EdgeType::Horizontal, 0, EdgeType::Vertical, 2) {
            let cells = build_text_cells(0, 3, &edges, &colors, CellWidthType::Single);
            assert_eq!(cells[1].glyph, Glyph::Cross);
            assert_eq!(cells[1].color, RatatuiColor::Blue);
        }
    }

    /// Half-stubs must not be widened into full lines before the union, or
    /// a junction would claim directions no edge actually reaches. `Down`
    /// crossed by a `Horizontal` is `┬`; treating `Down` as a full `│`
    /// would produce `┼` and send the reader chasing a line upward that
    /// isn't there.
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

    /// Hand-built edges may point past the column count; `place` tolerates
    /// that, and the single-width accumulator has to as well.
    #[test]
    fn single_width_ignores_out_of_range_edges() {
        let edges = vec![Edge::new(EdgeType::Vertical, 9, 0)];
        let cells = build_text_cells(0, 2, &edges, &[RatatuiColor::Red], CellWidthType::Single);
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[1].glyph, Glyph::Blank);
    }

    /// What a lone edge looks like under `Single`. With nothing to collide
    /// with there is no junction, so each `EdgeType` keeps the glyph it has
    /// always had -- including the half-stubs, which still widen into full
    /// lines for want of `╵╷╴╶`. Issue #29 changed how *collisions* resolve;
    /// this table pins that it left the uncollided cases alone.
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

    /// Pins every `GlyphSet` mapping as a literal table -- not derived from
    /// `resolve()` or `from_style()`, otherwise a wrong table entry and a
    /// wrong dispatch could cancel out and still pass. This is also the only
    /// place `corner_tl` / `corner_bl` / `head_dot` get covered: none of
    /// those three glyphs appear in any golden snapshot under the rounded
    /// style (see tests/graph.rs), so without this table they'd have zero
    /// test coverage.
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
                    // Junctions have no rounded forms, so rounded and
                    // angular agree here.
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

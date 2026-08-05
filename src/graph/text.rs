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
    /// Produced wherever a column's directions get unioned -- always under
    /// `Single`, and under `Double` for the columns `Column::can_merge`
    /// allows. Where it doesn't, collisions are settled by priority and the
    /// loser's directions are simply not drawn.
    TeeDown,
    TeeUp,
    TeeRight,
    TeeLeft,
    Cross,
}

/// Which of the four compass directions a piece of line touches.
///
/// This is the single source of truth for edge geometry: both cell widths
/// derive their glyphs from it (`halves` for `Double`, `merged` for
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
/// Takes either a single edge's directions (winner-takes-all resolves the
/// collision before it gets here) or a whole column's union.
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
/// fit in one character); `Double` reaches it through `halves`.
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
/// Returns `cell_count * width.cells_per_column()` cells. Where several edges
/// share a column the two widths differ:
///
/// - `Single` has one cell per column, so it always unions every edge's
///   directions and resolves the total through `merged`
/// - `Double` unions only where that doesn't cost colour information --
///   see `Column::can_merge` -- and otherwise gives each half-cell to the
///   highest-priority edge
///
/// One acknowledged gap remains in both: edges on the commit's own column are
/// dropped, since the dot owns that cell. Harmless in practice (the line
/// continues in the neighbouring column) but it is real lost information.
pub(crate) fn build_text_cells(
    commit_pos_x: usize,
    cell_count: usize,
    edges: &[Edge],
    colors: &[RatatuiColor],
    width: CellWidthType,
) -> Vec<TextCell> {
    // An empty palette makes every edge `Reset`, so every column reads as
    // uniform and `Double` unions everywhere. Only reachable from tests.
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

/// The colours of every edge reaching one column, collapsed.
///
/// `Empty` is not the same as `Uniform(Reset)`: with an empty palette every
/// edge really is `Reset`, so starting out as `Uniform(Reset)` would call the
/// first genuinely-coloured edge a conflict.
#[derive(Debug, Clone, Copy, Default)]
enum ColumnColors {
    #[default]
    Empty,
    /// The payload is only read while accumulating, to compare against later
    /// edges. Nothing downstream takes its colour from here -- that is what
    /// `Column::symbol_color` is for.
    Uniform(RatatuiColor),
    Mixed,
}

/// One graph column's accumulated state.
///
/// `symbol_rank` / `symbol_color` serve `Single`, `colors` / `traceless`
/// serve `Double`; both share `dirs`.
///
/// The colour goes to whoever writes first, the opposite of `place`'s `>=`
/// (last writer wins) and deliberately so: where a collision renders as a
/// visible junction rather than silently dropping the loser, the colour
/// shouldn't depend on the order `calc.rs` happens to push edges. `calc.rs`
/// really does push several `Right` edges at one `pos_x` with different
/// `associated_line_pos_x` (one per parent of a multi-parent commit), so this
/// is a reachable difference, not a theoretical one. Goldens compare
/// characters and the cross-width invariants compare glyphs, so only
/// `text.rs`'s own unit tests pin it.
#[derive(Debug, Clone, Copy, Default)]
struct Column {
    dirs: u8,
    /// Rank of the highest-ranked edge to reach this column. 0 means "no
    /// edge yet" -- no real edge ranks that low, and a column with none
    /// draws `Blank`, whose colour is never looked at.
    symbol_rank: u8,
    symbol_color: RatatuiColor,
    colors: ColumnColors,
    /// How many edges here would vanish without a trace if they lost. Only
    /// the count up to 2 matters.
    traceless: u8,
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

    /// Whether `Double` may replace this column's winning symbol with the
    /// union of every edge reaching it. Two independent reasons to allow it:
    ///
    /// 1. **Every edge here is the same colour.** A junction cell has one
    ///    foreground colour, so merging two differently-coloured lines into
    ///    one `┼` erases one line's identity -- the reader sees a single
    ///    line crossing where there are two. Same colour, nothing to erase.
    /// 2. **Two or more edges would vanish without a trace.** Not merging
    ///    costs an entire line rather than a colour, which is worse. The
    ///    41-case snapshot suite happens to have no mixed-colour collision
    ///    of this kind, but `calc.rs`'s detour can produce one: its overlap
    ///    scan covers `(child_pos_y + 1)..pos_y` and so misses edges already
    ///    sitting on row `pos_y` itself, where the `RightBottom` lands.
    fn can_merge(&self) -> bool {
        matches!(self.colors, ColumnColors::Uniform(_)) || self.traceless >= 2
    }
}

/// Whether an edge that loses its half-cell disappears with nothing left to
/// hint at it: its symbol half is non-blank (so it wanted the cell) and it
/// doesn't extend rightward (so the connector half stays empty too). True for
/// `Vertical` / `Up` / `Down` / `Left` / `RightTop` / `RightBottom`.
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

/// `Double`: winner-takes-all, then union the columns where that costs
/// nothing (`Column::can_merge`).
///
/// Two passes rather than one because the two half-cells compete
/// *independently*: a column drawn `│─` has the `│` from one line and the
/// `─` from another, so no "pick a winning edge, then `halves()` it" shape
/// can express it. Only the symbol half's direction source varies by column;
/// the connector half is identical either way (winner-takes-all draws `─`
/// there iff some edge extends rightward, which is exactly the union's
/// `DIR_RIGHT` bit).
///
/// The overwrite replaces the glyph but keeps the colour, which needs two
/// things to hold:
///
/// 1. A non-blank winning symbol took its colour from some edge of this
///    column. Under the uniform-colour rule that is *the* colour; under the
///    traceless rule it may be the loser's, but a junction's colour is
///    arbitrary between the lines it joins anyway.
/// 2. A *blank* winning symbol leaves the cell at `TextCell::BLANK`, whose
///    colour is `Reset`. `halves(dirs).0` is blank only for `DIR_RIGHT`
///    alone, and a column whose every edge is a lone `Right` unions to
///    exactly `DIR_RIGHT`, so the overwrite is a no-op there. If some future
///    `EdgeType` ever gets a blank symbol half, this breaks silently into a
///    coloured junction rendered in `Reset` -- hence the assertion.
fn double_cells(
    columns: &[Column],
    commit_pos_x: usize,
    edges: &[Edge],
    color_of: impl Fn(usize) -> RatatuiColor,
) -> Vec<TextCell> {
    let mut cells = winner_takes_all_cells(commit_pos_x, columns.len(), edges, color_of);
    let per_col = CellWidthType::Double.cells_per_column();

    for (col, column) in columns.iter().enumerate() {
        // The dot owns its own column: `place` gave it priority 10, nothing
        // can outrank it.
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

/// Each half-cell to the highest-priority edge, ties broken by whoever pushed
/// last. This is what all of `Double` used to be; it is now what `Double`
/// falls back to for columns that can't merge.
///
/// When two edges both want a non-blank symbol and neither extends rightward,
/// the loser vanishes with nothing left in the right half to hint at it --
/// see `leaves_no_trace`, which is why `can_merge` overrides this in exactly
/// that case.
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

/// Precedence for overlapping glyphs on the same text-graph cell.
/// Higher wins; horizontal `─` loses to vertical `│` so through-branches
/// remain continuous when a horizontal run passes by.
///
/// Serves two different roles. `winner_takes_all_cells` uses it to decide
/// which glyph takes a shared half-cell outright. `Column::absorb` uses it
/// where directions combine instead of competing, and there it only picks
/// whose colour the resulting junction inherits.
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
    Double,
    Single,
}

impl CellWidthType {
    /// Terminal columns one graph column occupies. `Double` draws
    /// [symbol, connector]; `Single` draws the symbol only.
    pub fn cells_per_column(self) -> usize {
        match self {
            CellWidthType::Double => 2,
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

    #[test]
    fn text_cells_simple_vertical() {
        // Single commit dot with a vertical edge on the same column.
        let edges = vec![Edge::new(EdgeType::Vertical, 0, 0)];
        let colors = vec![RatatuiColor::Red, RatatuiColor::Green];
        let cells = build_text_cells(0, 1, &edges, &colors, CellWidthType::Double);
        assert_eq!(cells.len(), 2);
        // Commit dot wins at pos_x=0 (edge doesn't clobber).
        assert_eq!(cells[0].glyph, Glyph::CommitDot);
        assert_eq!(cells[0].color, RatatuiColor::Red);
        // Connector is blank for vertical.
        assert_eq!(cells[1].glyph, Glyph::Blank);
    }

    #[test]
    fn text_cells_merge_branch() {
        // Commit at col 0, with a branch coming in from col 1 (curve at LeftTop).
        let edges = vec![
            Edge::new(EdgeType::Vertical, 0, 0),
            Edge::new(EdgeType::LeftTop, 1, 1),
        ];
        let colors = vec![RatatuiColor::Red, RatatuiColor::Green];
        let cells = build_text_cells(0, 2, &edges, &colors, CellWidthType::Double);
        assert_eq!(cells.len(), 4);
        assert_eq!(cells[0].glyph, Glyph::CommitDot);
        // Col 1: ╭ with ─ connector.
        assert_eq!(cells[2].glyph, Glyph::CornerTL);
        assert_eq!(cells[2].color, RatatuiColor::Green);
        assert_eq!(cells[3].glyph, Glyph::Horiz);
    }

    #[test]
    fn text_cells_horizontal_run() {
        // Commit at col 2 with a horizontal edge running across from col 0.
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
        // Commit dot wins at col 2.
        assert_eq!(cells[4].glyph, Glyph::CommitDot);
    }

    #[test]
    fn text_cells_left_right_stubs_stay_on_own_half() {
        // Left stub at col 1: left half `─`, right half blank
        let edges = vec![Edge::new(EdgeType::Left, 1, 0)];
        let cells = build_text_cells(0, 2, &edges, &[RatatuiColor::Red], CellWidthType::Double);
        assert_eq!(cells[2].glyph, Glyph::Horiz);
        assert_eq!(cells[3].glyph, Glyph::Blank);

        // Right stub at col 0: left half blank, right half `─`
        let edges = vec![Edge::new(EdgeType::Right, 0, 0)];
        let cells = build_text_cells(1, 2, &edges, &[RatatuiColor::Red], CellWidthType::Double);
        // commit is at col 1 so cells[2] is the dot; col 0 left half stays blank
        assert_eq!(cells[0].glyph, Glyph::Blank);
        assert_eq!(cells[1].glyph, Glyph::Horiz);
    }

    /// Two differently-coloured lines crossing: merging them into one `┼`
    /// would erase one line's colour, so the column keeps winner-takes-all.
    /// The horizontal survives here only because it also owns the connector
    /// half -- swap it for a `RightTop` and it would vanish without trace,
    /// which is why `leaves_no_trace` overrides the colour rule.
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

    /// The same two edge types on one line: nothing to erase, so the symbol
    /// half carries both directions.
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

    /// The four collisions where the loser leaves zero trace: both symbols
    /// non-blank, both connectors blank, so there is nothing in the right
    /// half to hint at the edge that lost. (#30's table lists three -- it
    /// missed `Left`, whose symbol is `─` and whose connector is blank just
    /// like the corners'.)
    ///
    /// All four union to `U|D|L`, so a mergeable column draws `┤` for every
    /// one of them. Here they share a line, so the colour rule alone allows
    /// it; `leaves_no_trace` would too.
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
            // Both edges land on column 1, whose halves are cells 2 and 3.
            for edges in colliding_edge_orders(*a, 0, *b, 0) {
                let cells = build_text_cells(0, 2, &edges, &colors, CellWidthType::Double);
                assert_eq!(cells[2].glyph, Glyph::TeeLeft, "{a:?} + {b:?}");
                assert_eq!(cells[3].glyph, Glyph::Blank, "{a:?} + {b:?} connector");

                // What the fallback would have drawn: one edge's own symbol,
                // never both, and a connector holding no trace of the other.
                // Called directly so this stays pinned even though no width
                // reaches it for these inputs any more.
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

    /// The second half of `can_merge`: two edges that would both vanish
    /// without a trace get unioned even when their colours differ, because
    /// losing a whole line beats losing a colour.
    ///
    /// The 41-case snapshot suite has no collision of this shape, but
    /// `calc.rs`'s detour can produce one -- its overlap scan covers
    /// `(child_pos_y + 1)..pos_y` and so misses edges already sitting on row
    /// `pos_y`, which is exactly where the `RightBottom` lands.
    #[test]
    fn text_cells_double_unions_multi_coloured_columns_that_would_lose_a_line() {
        let colors = vec![RatatuiColor::Red, RatatuiColor::Green, RatatuiColor::Blue];
        for edges in colliding_edge_orders(EdgeType::RightBottom, 1, EdgeType::Vertical, 2) {
            let cells = build_text_cells(0, 2, &edges, &colors, CellWidthType::Double);
            assert_eq!(cells[2].glyph, Glyph::TeeLeft);
            assert_eq!(cells[3].glyph, Glyph::Blank);
        }
    }

    /// `Column`'s colour ties go to the first writer, the opposite of
    /// `place`'s `>=`. Only `Single` reads them -- `Double` takes every
    /// colour from the winner-takes-all pass.
    ///
    /// `calc.rs` reaches the first case for real: a multi-parent commit
    /// pushes one `Right` per parent at the same `pos_x`, each carrying its
    /// own parent's colour.
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

        // Two corners of equal rank, both push orders: whoever came first.
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

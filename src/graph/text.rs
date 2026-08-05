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
    /// Only `Single` produces these -- `Double` splits a column across two
    /// cells, so its two edges never need to share one character.
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

/// `Double` layout: a column spans [symbol, connector]. The connector only
/// ever carries "extends rightward"; everything else lives in the symbol,
/// which draws the same shape `Single` would.
///
/// A lone rightward stub is the one edge whose line never reaches the
/// column's centre, so its symbol half stays empty. Only ever called with a
/// single edge's directions -- unions belong to `Single`, which has one cell
/// to fit them into.
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

/// `Single` layout: one cell per column, so every direction reaching that
/// column has to fit in one character.
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
/// Returns `cell_count * width.cells_per_column()` cells. The two widths
/// resolve `edge_dirs` differently: `Double` splits each column into
/// [symbol, connector] via `halves`, so two edges sharing a column land in
/// different cells and never compete. `Single` has one cell per column, so
/// it unions every edge's directions and resolves the total through
/// `merged`, yielding a box-drawing junction (`┼┬┴├┤`) where several lines
/// meet. Before that union existed, the higher-priority glyph took the cell
/// outright and the loser vanished -- a 5-way merge's connecting lines
/// disappeared entirely, leaving dots and corners with nothing joining them.
///
/// One acknowledged gap remains: edges on the commit's own column are still
/// dropped, since the dot owns that cell. Harmless in practice (the line
/// continues in the neighbouring column) but it is real lost information.
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

    if width == CellWidthType::Single {
        return build_single_width_cells(commit_pos_x, cell_count, edges, color_of);
    }

    let per_col = width.cells_per_column();
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

/// `Single`'s one-cell-per-column path: accumulate directions per column,
/// then resolve each column once.
///
/// Colour uses strict `>` (first writer of the winning priority keeps it),
/// unlike `place`'s `>=`. Two corners carry equal priority, and now that a
/// collision renders as a visible junction rather than silently dropping the
/// loser, `>=` would make the colour depend on the order `calc.rs` happens
/// to push edges. Nothing in the snapshot suite would catch that -- goldens
/// compare characters, not colours -- so the tie is settled here instead.
fn build_single_width_cells(
    commit_pos_x: usize,
    cell_count: usize,
    edges: &[Edge],
    color_of: impl Fn(usize) -> RatatuiColor,
) -> Vec<TextCell> {
    #[derive(Clone, Copy, Default)]
    struct Acc {
        dirs: u8,
        color: RatatuiColor,
        priority: u8,
    }

    let mut acc = vec![Acc::default(); cell_count];

    for edge in edges {
        // Out-of-range columns are ignored rather than panicking, matching
        // `place`'s tolerance -- `build_text_cells` is reachable from tests
        // with hand-built edges.
        let Some(slot) = acc.get_mut(edge.pos_x) else {
            continue;
        };
        let dirs = edge_dirs(edge.edge_type);
        slot.dirs |= dirs;

        let priority = glyph_priority(merged(dirs));
        if priority > slot.priority {
            slot.priority = priority;
            slot.color = color_of(edge.associated_line_pos_x);
        }
    }

    acc.iter()
        .enumerate()
        .map(|(col, slot)| {
            if col == commit_pos_x {
                TextCell {
                    glyph: Glyph::CommitDot,
                    color: color_of(commit_pos_x),
                }
            } else {
                TextCell {
                    glyph: merged(slot.dirs),
                    color: slot.color,
                }
            }
        })
        .collect()
}

/// Precedence for overlapping glyphs on the same text-graph cell.
/// Higher wins; horizontal `─` loses to vertical `│` so through-branches
/// remain continuous when a horizontal run passes by.
///
/// Under `Double` this decides which glyph a shared cell shows. Under
/// `Single` glyphs no longer compete -- their directions combine -- so this
/// only picks whose colour the resulting junction inherits.
fn glyph_priority(glyph: Glyph) -> u8 {
    match glyph {
        Glyph::CommitDot | Glyph::HeadDot => 10,
        // Unreachable today: `place` only ever sees `halves` output, and the
        // other caller passes a single edge's directions (two bits at most),
        // which can't form a junction. Listed for exhaustiveness; ranked
        // above `Vert` so the ordering still reads correctly if that changes.
        Glyph::TeeDown | Glyph::TeeUp | Glyph::TeeRight | Glyph::TeeLeft | Glyph::Cross => 7,
        Glyph::Vert => 5,
        Glyph::CornerTL | Glyph::CornerTR | Glyph::CornerBL | Glyph::CornerBR => 3,
        Glyph::Horiz => 1,
        Glyph::Blank => 0,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellWidthType {
    Double, // 2 cells
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

    #[test]
    fn text_cells_vertical_wins_over_horizontal() {
        let edges_h_first = vec![
            Edge::new(EdgeType::Horizontal, 1, 0),
            Edge::new(EdgeType::Vertical, 1, 2),
        ];
        let edges_v_first = vec![
            Edge::new(EdgeType::Vertical, 1, 2),
            Edge::new(EdgeType::Horizontal, 1, 0),
        ];
        let colors = vec![RatatuiColor::Red, RatatuiColor::Green, RatatuiColor::Blue];
        for edges in [edges_h_first, edges_v_first] {
            let cells = build_text_cells(0, 3, &edges, &colors, CellWidthType::Double);
            assert_eq!(cells[2].glyph, Glyph::Vert);
            assert_eq!(cells[2].color, RatatuiColor::Blue);
        }
    }

    #[test]
    fn text_cells_empty_colors_fallback() {
        let edges = vec![Edge::new(EdgeType::Vertical, 0, 0)];
        let cells = build_text_cells(0, 1, &edges, &[], CellWidthType::Double);
        assert_eq!(cells[0].glyph, Glyph::CommitDot);
        assert_eq!(cells[0].color, RatatuiColor::Reset);
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
    /// edges -- see `build_single_width_cells` on why the tie-break is
    /// strict rather than `place`'s `>=`.
    #[test]
    fn single_width_unions_colliding_edges_into_a_junction() {
        let colors = vec![RatatuiColor::Red, RatatuiColor::Green, RatatuiColor::Blue];
        let h_first = vec![
            Edge::new(EdgeType::Horizontal, 1, 0),
            Edge::new(EdgeType::Vertical, 1, 2),
        ];
        let v_first = vec![
            Edge::new(EdgeType::Vertical, 1, 2),
            Edge::new(EdgeType::Horizontal, 1, 0),
        ];
        for edges in [h_first, v_first] {
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

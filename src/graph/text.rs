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
/// Returns `cell_count * width.cells_per_column()` cells. `Double` draws each
/// column as [symbol, connector]; `Single` draws the symbol only, keeping
/// whichever of the two has higher `glyph_priority` (the same rule that
/// already resolves overlapping glyphs within a column). This means a
/// horizontal run gets swallowed wherever it crosses an already-occupied
/// column -- e.g. a 5-way merge's connecting lines vanish entirely under
/// `Single`, leaving just the dots and corners. That's an accepted trade-off
/// (a compact, occasionally illegible graph beats a graph that doesn't fit
/// at all), not a bug -- see issue #21's follow-up for the proper fix
/// (box-drawing junction glyphs).
pub(crate) fn build_text_cells(
    commit_pos_x: usize,
    cell_count: usize,
    edges: &[Edge],
    colors: &[RatatuiColor],
    width: CellWidthType,
) -> Vec<TextCell> {
    let per_col = width.cells_per_column();
    let mut cells: Vec<TextCell> = vec![TextCell::BLANK; cell_count * per_col];

    let color_of = |idx: usize| -> RatatuiColor {
        if colors.is_empty() {
            RatatuiColor::Reset
        } else {
            colors[idx % colors.len()]
        }
    };

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
        // `left` fills the left half of the column, `right` the right half.
        // Under `Double` they're two distinct cells; under `Single` they
        // collapse onto the same cell and `place`'s priority rule keeps
        // whichever one wins (see the fn doc comment).
        // Half-stubs (Left/Right/Up/Down) only touch their own side so they don't
        // poke into the neighbouring column.
        let (left, right) = match edge.edge_type {
            EdgeType::Vertical | EdgeType::Up | EdgeType::Down => (Glyph::Vert, Glyph::Blank),
            EdgeType::Horizontal => (Glyph::Horiz, Glyph::Horiz),
            EdgeType::Left => (Glyph::Horiz, Glyph::Blank),
            EdgeType::Right => (Glyph::Blank, Glyph::Horiz),
            EdgeType::RightTop => (Glyph::CornerTR, Glyph::Blank),
            EdgeType::RightBottom => (Glyph::CornerBR, Glyph::Blank),
            EdgeType::LeftTop => (Glyph::CornerTL, Glyph::Horiz),
            EdgeType::LeftBottom => (Glyph::CornerBL, Glyph::Horiz),
        };

        let color = color_of(edge.associated_line_pos_x);
        let idx = edge.pos_x * per_col;

        place(&mut cells, idx, left, color);
        place(&mut cells, idx + per_col - 1, right, color);
    }

    cells
}

/// Precedence for overlapping glyphs on the same text-graph cell.
/// Higher wins; horizontal `─` loses to vertical `│` so through-branches
/// remain continuous when a horizontal run passes by.
fn glyph_priority(glyph: Glyph) -> u8 {
    match glyph {
        Glyph::CommitDot | Glyph::HeadDot => 10,
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

    /// Single-mode folding: for every `EdgeType`, `single[c]` must equal
    /// whichever of `double[2c]`/`double[2c+1]` has the higher
    /// `glyph_priority` -- issue #21's table, derived from first
    /// principles rather than hardcoded per-variant.
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
        let cases: &[(GlyphSet, [(Glyph, &str); 9])] = &[
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

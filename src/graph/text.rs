use ratatui::style::Color as RatatuiColor;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphStyle {
    Rounded,
    Angular,
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
) -> Option<Vec<TextCell>> {
    let &(pos_x, pos_y) = graph.commit_pos_map.get(commit_hash)?;
    let edges = &graph.edges[pos_y];
    let cell_count = graph.cell_count();
    Some(build_text_cells(pos_x, cell_count, edges, colors))
}

/// Batch text-mode render of every commit in `graph`, in `commit_hashes` order.
///
/// Used by the `tests/graph.rs` snapshot suite, which needs the whole graph
/// at once rather than the per-commit lookups the UI does. Panics if
/// `graph.commit_hashes` and `graph.commit_pos_map` are out of sync (they're
/// built together in `calc.rs`, so this should never trigger in practice).
pub fn build_text_graph(graph: &Graph, colors: &[RatatuiColor]) -> Vec<Vec<TextCell>> {
    graph
        .commit_hashes
        .iter()
        .map(|hash| {
            text_cells(graph, hash, colors).expect("commit_hashes / commit_pos_map out of sync")
        })
        .collect()
}

/// Convert edges to lazygit-style text cells (symbol + connector per column).
///
/// Returns `cell_count * 2` cells. Each graph column takes 2 chars:
/// [symbol, connector]. The connector is `─` when a horizontal continuation
/// exists to the right, otherwise space.
pub(crate) fn build_text_cells(
    commit_pos_x: usize,
    cell_count: usize,
    edges: &[Edge],
    colors: &[RatatuiColor],
) -> Vec<TextCell> {
    let mut cells: Vec<TextCell> = vec![TextCell::BLANK; cell_count * 2];

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
        commit_pos_x * 2,
        Glyph::CommitDot,
        color_of(commit_pos_x),
    );

    for edge in edges {
        // `left` fills the left half of the 2-char column, `right` the right half.
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
        let idx = edge.pos_x * 2;

        place(&mut cells, idx, left, color);
        place(&mut cells, idx + 1, right, color);
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
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_cells_simple_vertical() {
        // Single commit dot with a vertical edge on the same column.
        let edges = vec![Edge::new(EdgeType::Vertical, 0, 0)];
        let colors = vec![RatatuiColor::Red, RatatuiColor::Green];
        let cells = build_text_cells(0, 1, &edges, &colors);
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
        let cells = build_text_cells(0, 2, &edges, &colors);
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
        let cells = build_text_cells(2, 3, &edges, &colors);
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
        let cells = build_text_cells(0, 2, &edges, &[RatatuiColor::Red]);
        assert_eq!(cells[2].glyph, Glyph::Horiz);
        assert_eq!(cells[3].glyph, Glyph::Blank);

        // Right stub at col 0: left half blank, right half `─`
        let edges = vec![Edge::new(EdgeType::Right, 0, 0)];
        let cells = build_text_cells(1, 2, &edges, &[RatatuiColor::Red]);
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
            let cells = build_text_cells(0, 3, &edges, &colors);
            assert_eq!(cells[2].glyph, Glyph::Vert);
            assert_eq!(cells[2].color, RatatuiColor::Blue);
        }
    }

    #[test]
    fn text_cells_empty_colors_fallback() {
        let edges = vec![Edge::new(EdgeType::Vertical, 0, 0)];
        let cells = build_text_cells(0, 1, &edges, &[]);
        assert_eq!(cells[0].glyph, Glyph::CommitDot);
        assert_eq!(cells[0].color, RatatuiColor::Reset);
    }
}

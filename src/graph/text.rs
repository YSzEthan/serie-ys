use ratatui::style::Color as RatatuiColor;

use crate::{
    git::CommitHash,
    graph::{Edge, EdgeType, Graph},
};

/// Lazygit-style commit dot for a non-HEAD commit.
pub const TEXT_COMMIT_DOT: char = '●';
/// Lazygit-style commit dot for HEAD.
pub const TEXT_HEAD_DOT: char = '◯';
pub const TEXT_VERT: char = '│';
pub const TEXT_HORIZ: char = '─';
pub const TEXT_CORNER_TL: char = '╭';
pub const TEXT_CORNER_TR: char = '╮';
pub const TEXT_CORNER_BL: char = '╰';
pub const TEXT_CORNER_BR: char = '╯';

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextCell {
    pub ch: char,
    pub color: RatatuiColor,
}

impl TextCell {
    const BLANK: TextCell = TextCell {
        ch: ' ',
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

    let place = |cells: &mut Vec<TextCell>, idx: usize, ch: char, color: RatatuiColor| {
        if ch == ' ' || idx >= cells.len() {
            return;
        }
        if glyph_priority(ch) >= glyph_priority(cells[idx].ch) {
            cells[idx] = TextCell { ch, color };
        }
    };

    place(
        &mut cells,
        commit_pos_x * 2,
        TEXT_COMMIT_DOT,
        color_of(commit_pos_x),
    );

    for edge in edges {
        // `left` fills the left half of the 2-char column, `right` the right half.
        // Half-stubs (Left/Right/Up/Down) only touch their own side so they don't
        // poke into the neighbouring column.
        let (left, right) = match edge.edge_type {
            EdgeType::Vertical | EdgeType::Up | EdgeType::Down => (TEXT_VERT, ' '),
            EdgeType::Horizontal => (TEXT_HORIZ, TEXT_HORIZ),
            EdgeType::Left => (TEXT_HORIZ, ' '),
            EdgeType::Right => (' ', TEXT_HORIZ),
            EdgeType::RightTop => (TEXT_CORNER_TR, ' '),
            EdgeType::RightBottom => (TEXT_CORNER_BR, ' '),
            EdgeType::LeftTop => (TEXT_CORNER_TL, TEXT_HORIZ),
            EdgeType::LeftBottom => (TEXT_CORNER_BL, TEXT_HORIZ),
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
fn glyph_priority(ch: char) -> u8 {
    match ch {
        TEXT_COMMIT_DOT | TEXT_HEAD_DOT => 10,
        TEXT_VERT => 5,
        TEXT_CORNER_TL | TEXT_CORNER_TR | TEXT_CORNER_BL | TEXT_CORNER_BR => 3,
        TEXT_HORIZ => 1,
        _ => 0,
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
        assert_eq!(cells[0].ch, TEXT_COMMIT_DOT);
        assert_eq!(cells[0].color, RatatuiColor::Red);
        // Connector is blank for vertical.
        assert_eq!(cells[1].ch, ' ');
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
        assert_eq!(cells[0].ch, TEXT_COMMIT_DOT);
        // Col 1: ╭ with ─ connector.
        assert_eq!(cells[2].ch, '╭');
        assert_eq!(cells[2].color, RatatuiColor::Green);
        assert_eq!(cells[3].ch, '─');
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
        assert_eq!(cells[0].ch, '─');
        assert_eq!(cells[1].ch, '─');
        assert_eq!(cells[2].ch, '─');
        assert_eq!(cells[3].ch, '─');
        // Commit dot wins at col 2.
        assert_eq!(cells[4].ch, TEXT_COMMIT_DOT);
    }

    #[test]
    fn text_cells_left_right_stubs_stay_on_own_half() {
        // Left stub at col 1: left half `─`, right half blank
        let edges = vec![Edge::new(EdgeType::Left, 1, 0)];
        let cells = build_text_cells(0, 2, &edges, &[RatatuiColor::Red]);
        assert_eq!(cells[2].ch, '─');
        assert_eq!(cells[3].ch, ' ');

        // Right stub at col 0: left half blank, right half `─`
        let edges = vec![Edge::new(EdgeType::Right, 0, 0)];
        let cells = build_text_cells(1, 2, &edges, &[RatatuiColor::Red]);
        // commit is at col 1 so cells[2] is the dot; col 0 left half stays blank
        assert_eq!(cells[0].ch, ' ');
        assert_eq!(cells[1].ch, '─');
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
            assert_eq!(cells[2].ch, '│');
            assert_eq!(cells[2].color, RatatuiColor::Blue);
        }
    }

    #[test]
    fn text_cells_empty_colors_fallback() {
        let edges = vec![Edge::new(EdgeType::Vertical, 0, 0)];
        let cells = build_text_cells(0, 1, &edges, &[]);
        assert_eq!(cells[0].ch, TEXT_COMMIT_DOT);
        assert_eq!(cells[0].color, RatatuiColor::Reset);
    }
}

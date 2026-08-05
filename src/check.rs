use ratatui::crossterm::terminal;

use crate::{
    graph::{CellWidthType, Graph},
    GraphWidthType, Result,
};

/// Never refuses to start (#21): an explicit width is honored as-is, and
/// `auto` degrades to `Single` when a double width doesn't fit. If even
/// `Single` doesn't fit, rendering truncates rather than erroring out --
/// `put_text_cells` already clips at the area's right edge, so a graph too
/// wide for the terminal just loses its rightmost columns instead of
/// refusing to open.
pub fn decide_cell_width_type(
    graph: &Graph,
    cell_width_type: Option<GraphWidthType>,
) -> Result<CellWidthType> {
    match cell_width_type {
        Some(GraphWidthType::DoubleL) => Ok(CellWidthType::DoubleL),
        Some(GraphWidthType::DoubleF) => Ok(CellWidthType::DoubleF),
        Some(GraphWidthType::Single) => Ok(CellWidthType::Single),
        Some(GraphWidthType::Auto) | None => {
            // Only `auto` needs to know the terminal size, so only `auto`
            // pays for the I/O (and can fail because of it) -- an explicit
            // width no longer depends on `terminal::size()` succeeding at
            // all.
            let (term_width, _) = terminal::size()?;
            Ok(auto_cell_width_type(
                graph.cell_count(),
                term_width as usize,
            ))
        }
    }
}

/// The graph column's right pad (see `graph_area_cell_width`) plus the
/// marker column right next to it (`calc_cell_widths`'s `marker_cell_width`,
/// always 1) -- the two non-graph columns a graph of this width also needs
/// room for.
const NON_GRAPH_COLUMNS: usize = 2;

/// `DoubleF` rather than `DoubleL` is what `auto` upgrades to: #30 is a bug
/// fix, so the default should be the width that doesn't drop lines.
fn auto_cell_width_type(cell_count: usize, term_width: usize) -> CellWidthType {
    let double_width = cell_count * CellWidthType::DoubleF.cells_per_column() + NON_GRAPH_COLUMNS;
    if double_width <= term_width {
        CellWidthType::DoubleF
    } else {
        CellWidthType::Single
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_prefers_double_when_it_fits() {
        assert_eq!(auto_cell_width_type(3, 20), CellWidthType::DoubleF);
    }

    #[test]
    fn auto_falls_back_to_single_when_double_does_not_fit() {
        // double_width = 3 * 2 + 2 = 8, doesn't fit in 7.
        assert_eq!(auto_cell_width_type(3, 7), CellWidthType::Single);
    }

    #[test]
    fn auto_still_returns_single_when_nothing_fits() {
        // Never errors, never panics -- rendering truncates instead (#21).
        assert_eq!(auto_cell_width_type(100, 1), CellWidthType::Single);
    }
}

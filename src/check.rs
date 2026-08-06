use ratatui::crossterm::terminal;

use crate::{
    graph::{CellWidthType, Graph},
    GraphWidthType, Result,
};

/// 永遠不會拒絕啟動（#21）：明確指定的寬度照原樣採用，`auto` 在 `Double`
/// 放不下時退回 `Single`。就算連 `Single` 都放不下，渲染也只會截斷而不會
/// 回傳錯誤 —— `put_text_cells` 本來就會在區域右邊界裁切，所以放不下終端機
/// 寬度的圖只會失去最右邊幾欄，而不會拒絕開啟。
pub fn decide_cell_width_type(
    graph: &Graph,
    cell_width_type: Option<GraphWidthType>,
) -> Result<CellWidthType> {
    match cell_width_type {
        Some(GraphWidthType::Double) => Ok(CellWidthType::Double),
        Some(GraphWidthType::Single) => Ok(CellWidthType::Single),
        Some(GraphWidthType::Auto) | None => {
            // 只有 `auto` 需要知道終端機大小，所以只有 `auto` 要付出這筆 I/O
            // 成本（也只有它可能因此失敗）—— 明確指定寬度就完全不依賴
            // `terminal::size()` 能否成功。
            let (term_width, _) = terminal::size()?;
            Ok(auto_cell_width_type(
                graph.cell_count(),
                term_width as usize,
            ))
        }
    }
}

/// graph 欄右側留白（見 `graph_area_cell_width`）加上緊鄰的 marker 欄
/// （`calc_cell_widths` 的 `marker_cell_width`，永遠是 1）—— 這是這個寬度的
/// graph 除了本身之外還需要的兩個非 graph 欄。
const NON_GRAPH_COLUMNS: usize = 2;

fn auto_cell_width_type(cell_count: usize, term_width: usize) -> CellWidthType {
    let double_width = cell_count * CellWidthType::Double.cells_per_column() + NON_GRAPH_COLUMNS;
    if double_width <= term_width {
        CellWidthType::Double
    } else {
        CellWidthType::Single
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_prefers_double_when_it_fits() {
        assert_eq!(auto_cell_width_type(3, 20), CellWidthType::Double);
    }

    #[test]
    fn auto_falls_back_to_single_when_double_does_not_fit() {
        // double_width = 3 * 2 + 2 = 8，放不進 7。
        assert_eq!(auto_cell_width_type(3, 7), CellWidthType::Single);
    }

    #[test]
    fn auto_still_returns_single_when_nothing_fits() {
        // 永遠不會回傳錯誤，也不會 panic —— 渲染只會截斷（#21）。
        assert_eq!(auto_cell_width_type(100, 1), CellWidthType::Single);
    }
}

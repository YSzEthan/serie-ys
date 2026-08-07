use ratatui::layout::Constraint;

use crate::config::UserListColumnType;
use crate::graph::CellWidthType;
use crate::{CompactType, GraphWidthType};

/// Name/Date/Hash 三個定寬欄，內容前後各留一格空白。
pub(crate) const PAD: u16 = 2;

/// Marker 濾掉之後，Subject 必須緊接在 Graph 之後，兩者都要在 `columns`
/// 裡 —— 緊湊模式需要 Graph 跟 Subject 共用同一塊 Rect，兩者中間不能夾著
/// 任何會被實際渲染出東西的欄位。
pub(crate) fn compact_possible(columns: &[UserListColumnType]) -> bool {
    let filtered: Vec<&UserListColumnType> = columns
        .iter()
        .filter(|&c| *c != UserListColumnType::Marker)
        .collect();
    filtered
        .windows(2)
        .any(|w| *w[0] == UserListColumnType::Graph && *w[1] == UserListColumnType::Subject)
}

/// 這個版面（給定的 graph 寬度、緊湊與否）要幾欄才放得下。`calc_cell_widths`
/// 的 `total_width` 直接呼叫這個函式 —— 「同一份帳」靠的是同一個函式，不是
/// 同一份資料結構。只計入實際出現在 `columns` 裡的欄位。
pub(crate) fn required_width(
    columns: &[UserListColumnType],
    graph_cell_width: u16,
    compact: bool,
    subject_min_width: u16,
    name_width: u16,
    date_width: u16,
) -> u16 {
    let graph = if columns.contains(&UserListColumnType::Graph) {
        // 右側留白（1）只要 Graph 有出現就存在，跟 Marker 無關；marker
        // 欄則要 `columns` 裡也有 Marker 才算 —— 緊湊模式兩者都不算。
        let extra = if compact {
            0
        } else {
            1 + columns.contains(&UserListColumnType::Marker) as u16
        };
        graph_cell_width + extra
    } else {
        0
    };
    let name = if columns.contains(&UserListColumnType::Name) {
        name_width + PAD
    } else {
        0
    };
    let date = if columns.contains(&UserListColumnType::Date) {
        date_width + PAD
    } else {
        0
    };
    let hash = if columns.contains(&UserListColumnType::Hash) {
        7 + PAD
    } else {
        0
    };
    subject_min_width + graph + name + date + hash
}

/// 每幀決定 graph 要用 `Double` 還是 `Single`、要不要開緊湊。
///
/// 不是四級階梯的 first-fit，是兩條獨立規則 —— 緊湊與寬度是兩個正交維度，
/// 排序又剛好是字典序，攤平成四級再 first-fit 在數學上等價，但會冒出
/// 「寬版緊湊那一級窗口只有 2 欄」這種需要解釋的東西。這裡直接表達
/// 使用者要的語意：
///
/// 規則一（寬度）：緊湊還有機會套用時，用比較寬鬆的（緊湊）預算判斷
/// `Double` 撐不撐得住；緊湊被明確關掉、或版面排不出「Graph 緊接 Subject」
/// 時，必須用真正非緊湊的預算。
/// 規則二（緊湊）：`auto` 時，選好的寬度在非緊湊預算下放不下就開；
/// `on`／`off` 照使用者指定（`on` 但版面排不出來時，規則一已經把
/// `compact_pref` 降級成 `Off`，所以這裡也不會真的打開）。
pub(crate) fn decide(
    columns: &[UserListColumnType],
    cell_count: usize,
    area_width: u16,
    width_pref: Option<GraphWidthType>,
    compact_pref: Option<CompactType>,
    subject_min_width: u16,
    name_width: u16,
    date_width: u16,
) -> (CellWidthType, bool) {
    let compact_pref = if compact_possible(columns) {
        compact_pref
    } else {
        Some(CompactType::Off)
    };

    let req = |w: CellWidthType, compact: bool| {
        required_width(
            columns,
            (cell_count * w.cells_per_column()) as u16,
            compact,
            subject_min_width,
            name_width,
            date_width,
        )
    };

    let assume_compact = compact_pref != Some(CompactType::Off);
    let width = match width_pref {
        Some(GraphWidthType::Double) => CellWidthType::Double,
        Some(GraphWidthType::Single) => CellWidthType::Single,
        Some(GraphWidthType::Auto) | None => {
            if req(CellWidthType::Double, assume_compact) <= area_width {
                CellWidthType::Double
            } else {
                CellWidthType::Single
            }
        }
    };

    let compact = match compact_pref {
        Some(CompactType::On) => true,
        Some(CompactType::Off) => false,
        Some(CompactType::Auto) | None => req(width, false) > area_width,
    };

    (width, compact)
}

/// Subject 以外每個欄位的實際寬度（依 `compact` 與 `columns` 決定 Graph／
/// Marker 是否保留），組成 `Layout::horizontal` 要的 constraints。空間不足
/// 依序砍 Name -> Date -> Hash（Graph/Marker/Subject 永遠保留）。
pub(crate) fn calc_cell_widths(
    area_width: u16,
    subject_min_width: u16,
    graph_cell_width: u16,
    name_width: u16,
    date_width: u16,
    columns: &[UserListColumnType],
    compact: bool,
) -> Vec<Constraint> {
    let (mut graph_w, mut marker_w, mut name_w, mut hash_w, mut date_w) =
        (0u16, 0u16, 0u16, 0u16, 0u16);

    for col in columns {
        match col {
            UserListColumnType::Graph => {
                graph_w = if compact { 0 } else { graph_cell_width + 1 };
            }
            UserListColumnType::Marker => {
                marker_w = if compact { 0 } else { 1 };
            }
            UserListColumnType::Name => {
                name_w = name_width + PAD;
            }
            UserListColumnType::Hash => {
                hash_w = 7 + PAD;
            }
            UserListColumnType::Date => {
                date_w = date_width + PAD;
            }
            UserListColumnType::Subject => {}
        }
    }

    let mut total_width = required_width(
        columns,
        graph_cell_width,
        compact,
        subject_min_width,
        name_width,
        date_width,
    );

    if total_width > area_width {
        total_width = total_width.saturating_sub(name_w);
        name_w = 0;
    }
    if total_width > area_width {
        total_width = total_width.saturating_sub(date_w);
        date_w = 0;
    }
    if total_width > area_width {
        hash_w = 0;
    }

    columns
        .iter()
        .map(|col| match col {
            UserListColumnType::Graph => Constraint::Length(graph_w),
            UserListColumnType::Marker => Constraint::Length(marker_w),
            UserListColumnType::Subject => Constraint::Min(0),
            UserListColumnType::Name => Constraint::Length(name_w),
            UserListColumnType::Hash => Constraint::Length(hash_w),
            UserListColumnType::Date => Constraint::Length(date_w),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_columns() -> [UserListColumnType; 6] {
        [
            UserListColumnType::Graph,
            UserListColumnType::Marker,
            UserListColumnType::Subject,
            UserListColumnType::Date,
            UserListColumnType::Name,
            UserListColumnType::Hash,
        ]
    }

    // ---- compact_possible -------------------------------------------------

    #[test]
    fn compact_possible_true_when_graph_directly_precedes_subject() {
        assert!(compact_possible(&default_columns()));
    }

    #[test]
    fn compact_possible_true_across_a_filtered_out_marker() {
        assert!(compact_possible(&[
            UserListColumnType::Graph,
            UserListColumnType::Marker,
            UserListColumnType::Subject,
        ]));
    }

    #[test]
    fn compact_possible_false_when_subject_precedes_graph() {
        assert!(!compact_possible(&[
            UserListColumnType::Subject,
            UserListColumnType::Graph,
        ]));
    }

    #[test]
    fn compact_possible_false_when_graph_missing() {
        assert!(!compact_possible(&[UserListColumnType::Subject]));
    }

    #[test]
    fn compact_possible_false_when_subject_missing() {
        assert!(!compact_possible(&[UserListColumnType::Graph]));
    }

    #[test]
    fn compact_possible_false_when_a_real_column_sits_between() {
        assert!(!compact_possible(&[
            UserListColumnType::Graph,
            UserListColumnType::Date,
            UserListColumnType::Subject,
        ]));
    }

    // ---- required_width ----------------------------------------------------

    #[test]
    fn required_width_charges_every_configured_column() {
        // subject_min=20, name=20+2, date=10+2, hash=7+2, graph=cell_count*2+2(留白+marker)
        let w = required_width(&default_columns(), 3 * 2, false, 20, 20, 10);
        assert_eq!(w, 20 + (6 + 2) + (10 + 2) + (20 + 2) + (7 + 2));
    }

    #[test]
    fn required_width_compact_saves_exactly_padding_and_marker() {
        let non_compact = required_width(&default_columns(), 6, false, 20, 20, 10);
        let compact = required_width(&default_columns(), 6, true, 20, 20, 10);
        assert_eq!(non_compact - compact, 2, "留白 1 + marker 1");
    }

    #[test]
    fn required_width_ignores_columns_not_configured() {
        let w = required_width(&[UserListColumnType::Subject], 100, false, 20, 999, 999);
        assert_eq!(w, 20, "沒出現的欄位（含 graph）完全不計入");
    }

    // ---- decide --------------------------------------------------------------
    // F = subject_min(20) + date(10+2) + name(20+2) + hash(7+2) = 63

    #[test]
    fn decide_auto_auto_prefers_double_using_the_compact_budget() {
        // c=8: 雙倍緊湊 = 16+63=79 <= 80 -> Double；79 > ? 非緊湊 2c+2+F=81>80 -> compact
        let (w, c) = decide(
            &default_columns(),
            8,
            80,
            Some(GraphWidthType::Auto),
            Some(CompactType::Auto),
            20,
            20,
            10,
        );
        assert_eq!(w, CellWidthType::Double);
        assert!(c, "非緊湊放不下（81>80），auto 要開緊湊");
    }

    #[test]
    fn decide_auto_off_uses_the_non_compact_budget_and_never_compacts() {
        // c=8 非緊湊 2c+2+F=81>80 -> Single；-c off 全程不開緊湊
        let (w, c) = decide(
            &default_columns(),
            8,
            80,
            Some(GraphWidthType::Auto),
            Some(CompactType::Off),
            20,
            20,
            10,
        );
        assert_eq!(w, CellWidthType::Single);
        assert!(!c);
    }

    #[test]
    fn decide_auto_on_always_compacts_when_possible() {
        let (w, c) = decide(
            &default_columns(),
            3,
            80,
            Some(GraphWidthType::Auto),
            Some(CompactType::On),
            20,
            20,
            10,
        );
        assert_eq!(w, CellWidthType::Double);
        assert!(c);
    }

    #[test]
    fn decide_on_is_downgraded_to_off_when_compact_is_not_possible() {
        let non_adjacent = [UserListColumnType::Subject, UserListColumnType::Graph];
        let (_, c) = decide(
            &non_adjacent,
            20,
            10,
            Some(GraphWidthType::Auto),
            Some(CompactType::On),
            20,
            20,
            10,
        );
        assert!(!c, "columns 排不出緊湊時，On 也不會真的套用");
    }

    #[test]
    fn decide_explicit_width_is_never_overridden() {
        let (w, _) = decide(
            &default_columns(),
            100,
            80,
            Some(GraphWidthType::Double),
            Some(CompactType::Auto),
            20,
            20,
            10,
        );
        assert_eq!(w, CellWidthType::Double, "明確指定的寬度永遠照用");
    }

    #[test]
    fn decide_falls_back_to_the_narrowest_combo_when_nothing_fits() {
        let (w, c) = decide(
            &default_columns(),
            1000,
            1,
            Some(GraphWidthType::Auto),
            Some(CompactType::Auto),
            20,
            20,
            10,
        );
        assert_eq!(w, CellWidthType::Single);
        assert!(c, "永遠不會拒絕啟動（#21），放不下就用最窄的組合截斷");
    }
}

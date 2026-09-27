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
///
/// 欄寬上限（`max_width_percent`，只在長線截斷啟用的圖才給）：graph 最多佔
/// `area_width` 的這個比例，回傳的第三個值是實際畫得下的欄數 `cols`（超出
/// 時最後一欄是溢位欄）。寬度用還沒套上限的 `cell_count` 判斷——用套過上限
/// 的欄數，寬圖會從 `Single` 被誤判成 `Double`，能顯示的 lane 少一半；但
/// `Double` 也必須在上限內畫得完，否則 `Single` 本來畫得完的圖反而溢位。
/// 緊湊則用 `cols` 判斷，那才是實際畫出來的寬度。
pub(crate) fn decide(
    columns: &[UserListColumnType],
    cell_count: usize,
    area_width: u16,
    width_pref: Option<GraphWidthType>,
    compact_pref: Option<CompactType>,
    max_width_percent: Option<u16>,
    subject_min_width: u16,
    name_width: u16,
    date_width: u16,
) -> (CellWidthType, bool, usize) {
    let compact_pref = if compact_possible(columns) {
        compact_pref
    } else {
        Some(CompactType::Off)
    };

    let req = |w: CellWidthType, cols: usize, compact: bool| {
        required_width(
            columns,
            (cols * w.cells_per_column()) as u16,
            compact,
            subject_min_width,
            name_width,
            date_width,
        )
    };
    // graph 最多能佔的格數；沒有上限時等於不限。
    let cap_width = max_width_percent.map_or(usize::MAX, |pct| {
        usize::from(area_width) * usize::from(pct) / 100
    });
    let fits_cap = |w: CellWidthType| cell_count * w.cells_per_column() <= cap_width;

    let assume_compact = compact_pref != Some(CompactType::Off);
    let width = match width_pref {
        Some(GraphWidthType::Double) => CellWidthType::Double,
        Some(GraphWidthType::Single) => CellWidthType::Single,
        Some(GraphWidthType::Auto) | None => {
            if req(CellWidthType::Double, cell_count, assume_compact) <= area_width
                && fits_cap(CellWidthType::Double)
            {
                CellWidthType::Double
            } else {
                CellWidthType::Single
            }
        }
    };

    // 至少留一欄 lane 加一欄溢位欄。
    let cols = cell_count.min((cap_width / width.cells_per_column()).max(2));

    let compact = match compact_pref {
        Some(CompactType::On) => true,
        Some(CompactType::Off) => false,
        Some(CompactType::Auto) | None => req(width, cols, false) > area_width,
    };

    (width, compact, cols)
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
        let (w, c, _) = decide(
            &default_columns(),
            8,
            80,
            Some(GraphWidthType::Auto),
            Some(CompactType::Auto),
            None,
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
        let (w, c, _) = decide(
            &default_columns(),
            8,
            80,
            Some(GraphWidthType::Auto),
            Some(CompactType::Off),
            None,
            20,
            20,
            10,
        );
        assert_eq!(w, CellWidthType::Single);
        assert!(!c);
    }

    #[test]
    fn decide_auto_on_always_compacts_when_possible() {
        let (w, c, _) = decide(
            &default_columns(),
            3,
            80,
            Some(GraphWidthType::Auto),
            Some(CompactType::On),
            None,
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
        let (_, c, _) = decide(
            &non_adjacent,
            20,
            10,
            Some(GraphWidthType::Auto),
            Some(CompactType::On),
            None,
            20,
            20,
            10,
        );
        assert!(!c, "columns 排不出緊湊時，On 也不會真的套用");
    }

    #[test]
    fn decide_explicit_width_is_never_overridden() {
        let (w, _, _) = decide(
            &default_columns(),
            100,
            80,
            Some(GraphWidthType::Double),
            Some(CompactType::Auto),
            None,
            20,
            20,
            10,
        );
        assert_eq!(w, CellWidthType::Double, "明確指定的寬度永遠照用");
    }

    #[test]
    fn decide_falls_back_to_the_narrowest_combo_when_nothing_fits() {
        let (w, c, _) = decide(
            &default_columns(),
            1000,
            1,
            Some(GraphWidthType::Auto),
            Some(CompactType::Auto),
            None,
            20,
            20,
            10,
        );
        assert_eq!(w, CellWidthType::Single);
        assert!(c, "永遠不會拒絕啟動（#21），放不下就用最窄的組合截斷");
    }

    // ---- decide：欄寬上限 ------------------------------------------------------

    #[test]
    fn decide_without_cap_draws_every_column() {
        let (_, _, cols) = decide(
            &default_columns(),
            300,
            80,
            Some(GraphWidthType::Auto),
            Some(CompactType::Auto),
            None,
            20,
            20,
            10,
        );
        assert_eq!(cols, 300, "一般 repo（沒套上限）行為跟以前一樣");
    }

    #[test]
    fn decide_caps_columns_to_the_percentage_of_the_area() {
        // 200 格的 50% = 100 格；Single 一欄一格 -> 100 欄。
        let (w, _, cols) = decide(
            &default_columns(),
            300,
            200,
            Some(GraphWidthType::Auto),
            Some(CompactType::Auto),
            Some(50),
            20,
            20,
            10,
        );
        assert_eq!(w, CellWidthType::Single);
        assert_eq!(cols, 100);
    }

    /// `req(Double)` 放得下，但 Double 在上限內畫不完、Single 畫得完：選
    /// Single，不要為了 Double 讓圖溢位。
    #[test]
    fn decide_prefers_single_when_only_single_fits_the_cap() {
        // 60 欄：Double 要 120 格 > 上限 100；Single 60 格放得下。
        // req(Double, 緊湊) = 120 + 63 = 183 <= 200。
        let (w, _, cols) = decide(
            &default_columns(),
            60,
            200,
            Some(GraphWidthType::Auto),
            Some(CompactType::Auto),
            Some(50),
            20,
            20,
            10,
        );
        assert_eq!(w, CellWidthType::Single);
        assert_eq!(cols, 60, "Single 畫得完，沒有溢位欄");
    }

    /// 寬度用套上限之前的欄數判斷：用套過上限的欄數（100）的話，Double
    /// 會被誤判成放得下，能顯示的 lane 反而少一半。
    #[test]
    fn decide_picks_width_from_the_uncapped_count() {
        let (w, _, cols) = decide(
            &default_columns(),
            300,
            400,
            Some(GraphWidthType::Auto),
            Some(CompactType::Auto),
            Some(50),
            20,
            20,
            10,
        );
        assert_eq!(w, CellWidthType::Single);
        assert_eq!(cols, 200);
    }

    /// 緊湊用套上限之後的欄數判斷：套完上限放得下就不必開緊湊。
    #[test]
    fn decide_compacts_based_on_the_capped_width() {
        // 上限 100 欄，非緊湊 100 + 2 + 63 = 165 <= 200。
        let (_, c, _) = decide(
            &default_columns(),
            300,
            200,
            Some(GraphWidthType::Auto),
            Some(CompactType::Auto),
            Some(50),
            20,
            20,
            10,
        );
        assert!(!c);
    }

    #[test]
    fn decide_cap_keeps_at_least_one_lane_and_the_overflow_column() {
        let (_, _, cols) = decide(
            &default_columns(),
            300,
            1,
            Some(GraphWidthType::Auto),
            Some(CompactType::Auto),
            Some(10),
            20,
            20,
            10,
        );
        assert_eq!(cols, 2);
    }
}

use std::ops::Range;

use crate::{
    git::{CommitHash, Repository},
    RemoteOnly,
};

use super::lanes::{self, BuildOpts, Lanes, NOT_LOADED};

/// 長線截斷的門檻：還沒截斷的圖寬超過這麼多欄才啟用。長線在一般 repo 也
/// 存在，但它們的圖寬本來就放得下，截斷只會把好好的線切碎。
pub const TRUNCATE_MIN_GRAPH_WIDTH: usize = 64;

/// 長線截斷的設定（issue #119）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Truncation {
    /// K：線長超過 K 列就截斷，最小 3。
    pub max_edge_rows: usize,
    /// 還沒截斷的圖寬超過這個值才啟用。正式使用一律
    /// [`TRUNCATE_MIN_GRAPH_WIDTH`]；測試傳 0 強制啟用。
    pub min_graph_width: usize,
}

impl Truncation {
    pub fn new(max_edge_rows: usize) -> Truncation {
        Truncation {
            max_edge_rows,
            min_graph_width: TRUNCATE_MIN_GRAPH_WIDTH,
        }
    }
}

/// 產生 edge 的來源。正式 build 只有 `Lanes`；`Fixed` 只給測試手寫 edge
/// 用——很多 fixture（例如只測寬度、不含任何 edge 的 graph）沒有對應的
/// 真實 commit 拓樸，改用 CSR 表達反而會失真。
#[derive(Debug)]
enum RowEdges {
    Lanes(Lanes),
    #[cfg(test)]
    Fixed {
        cols: Vec<u32>,
        edges: Vec<Vec<Edge>>,
        cell_count: usize,
    },
}

impl RowEdges {
    fn row_count(&self) -> usize {
        match self {
            RowEdges::Lanes(l) => l.row_count(),
            #[cfg(test)]
            RowEdges::Fixed { cols, .. } => cols.len(),
        }
    }

    fn cell_count(&self) -> usize {
        match self {
            RowEdges::Lanes(l) => l.cell_count(),
            #[cfg(test)]
            RowEdges::Fixed { cell_count, .. } => *cell_count,
        }
    }

    fn truncated(&self) -> bool {
        match self {
            RowEdges::Lanes(l) => l.truncated(),
            #[cfg(test)]
            RowEdges::Fixed { .. } => false,
        }
    }

    fn col(&self, row: usize) -> usize {
        match self {
            RowEdges::Lanes(l) => l.col(row),
            #[cfg(test)]
            RowEdges::Fixed { cols, .. } => cols[row] as usize,
        }
    }

    fn row_edges(&self, row: usize) -> Vec<Edge> {
        match self {
            RowEdges::Lanes(l) => l.row_edges(row),
            #[cfg(test)]
            RowEdges::Fixed { edges, .. } => edges[row].clone(),
        }
    }

    fn for_each_row_edges(&self, range: Range<usize>, f: impl FnMut(usize, &[Edge])) {
        match self {
            RowEdges::Lanes(l) => l.row_edges_in(range, f),
            // 只有這個分支需要呼叫 `f`，`mut` 只在這裡要求；簽名維持 `f`
            // 不宣告 `mut`，正式 build（沒有這個分支）才不會多一個 unused_mut。
            #[cfg(test)]
            RowEdges::Fixed { edges, .. } => {
                let mut f = f;
                for row in range {
                    f(row, &edges[row]);
                }
            }
        }
    }
}

/// graph 的對外介面。
///
/// row 是 graph 裡的第幾列，raw 是 commit 在 `Repository::all_commits()` 裡的
/// 位置。主 graph 兩者相同；filtered graph 跳過了隱藏的 commit，row 比 raw 小。
#[derive(Debug)]
pub struct Graph {
    /// row → raw。`None` 代表恆等；`Some` 時嚴格遞增，`row_of` 用 binary search。
    raw_of: Option<Vec<u32>>,
    rows: RowEdges,
}

impl Graph {
    /// 給測試手寫 edge 用的建構子，正式 build 不會呼叫（`calc_graph`／
    /// `calc_graph_filtered` 一律走 lane 引擎，見 [`from_lanes`]）。
    ///
    /// `rows[i]` 是第 i 列的 `(raw, col)`，raw 必須嚴格遞增；`raw_count` 是
    /// `all_commits()` 的長度，`rows` 涵蓋全部時就是恆等對應。
    ///
    /// edge 的排序、去重在這裡做：`text.rs` 同 rank 平手時看 edge 順序，
    /// 所以順序本身是 graph 的一部分（lane 引擎的排序在 `row_edges_in` 裡）。
    #[cfg(test)]
    pub fn from_materialized(
        raw_count: usize,
        rows: Vec<(usize, usize)>,
        mut edges: Vec<Vec<Edge>>,
    ) -> Graph {
        debug_assert_eq!(rows.len(), edges.len());
        debug_assert!(rows.windows(2).all(|w| w[0].0 < w[1].0));
        debug_assert!(rows.last().is_none_or(|&(raw, _)| raw < raw_count));

        for es in &mut edges {
            es.sort_by_key(|e| (e.associated_line_pos_x, e.pos_x, e.edge_type));
            es.dedup();
        }
        // 手寫的 edge 可能落在沒有 commit 的欄，所以也要算進來。
        let cell_count = rows
            .iter()
            .map(|&(_, col)| col)
            .chain(edges.iter().flatten().map(|e| e.pos_x))
            .max()
            .map_or(0, |m| m + 1);
        let raw_of =
            (rows.len() != raw_count).then(|| rows.iter().map(|&(raw, _)| raw as u32).collect());
        let cols = rows.iter().map(|&(_, col)| col as u32).collect();

        Graph {
            raw_of,
            rows: RowEdges::Fixed {
                cols,
                edges,
                cell_count,
            },
        }
    }

    fn from_lanes(raw_of: Option<Vec<u32>>, lanes: Lanes) -> Graph {
        Graph {
            raw_of,
            rows: RowEdges::Lanes(lanes),
        }
    }

    pub fn row_count(&self) -> usize {
        self.rows.row_count()
    }

    pub fn cell_count(&self) -> usize {
        self.rows.cell_count()
    }

    /// 這張圖啟用了長線截斷（圖寬超過門檻）。欄寬上限只在這時候才套用。
    pub fn truncated(&self) -> bool {
        self.rows.truncated()
    }

    /// raw 不在這張 graph 裡（被 filter 掉，或 fixture 的 graph 比 commit 少）時回 `None`。
    pub fn row_of(&self, raw: usize) -> Option<usize> {
        match &self.raw_of {
            None => (raw < self.row_count()).then_some(raw),
            Some(raws) => raws.binary_search(&(raw as u32)).ok(),
        }
    }

    pub fn raw_of(&self, row: usize) -> usize {
        self.raw_of.as_ref().map_or(row, |raws| raws[row] as usize)
    }

    pub fn col(&self, row: usize) -> usize {
        self.rows.col(row)
    }

    pub fn row_edges(&self, row: usize) -> Vec<Edge> {
        self.rows.row_edges(row)
    }

    /// 批次走過 `range` 內每一列排序過的 edge，只從最近的 checkpoint 重播
    /// 一次。給需要走完整張圖的呼叫端用（perf 統計、golden 產生）——逐列
    /// 呼叫 `row_edges` 會在範圍內重複從 checkpoint 重播，範圍越大越浪費。
    pub fn for_each_row_edges(&self, range: Range<usize>, f: impl FnMut(usize, &[Edge])) {
        self.rows.for_each_row_edges(range, f);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Edge {
    pub edge_type: EdgeType,
    pub pos_x: usize,
    pub associated_line_pos_x: usize,
}

impl Edge {
    pub fn new(edge_type: EdgeType, pos_x: usize, line_pos_x: usize) -> Self {
        Self {
            edge_type,
            pos_x,
            associated_line_pos_x: line_pos_x,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialOrd, Ord, PartialEq, Eq, Hash)]
pub enum EdgeType {
    Vertical,    // │
    Horizontal,  // ─
    Up,          // ╵
    Down,        // ╷
    Left,        // ╴
    Right,       // ╶
    RightTop,    // ╮
    RightBottom, // ╯
    LeftTop,     // ╭
    LeftBottom,  // ╰
    // 長線截斷（issue #119）的兩端。刻意排在最後：`row_edges_in` 的排序鍵含
    // `edge_type`，加在中間會讓既有 golden 的 edge 順序跟著變。
    TruncDown, // ↓：child 下方一列，線在這裡斷開
    TruncUp,   // ↑：parent 上方一列，線從這裡接回
}

/// 先不截斷建一次；圖寬超過 `trunc.min_graph_width` 才丟掉重建一次截斷版。
/// linux 規模單次建圖約 0.06 s，建兩次換來「門檻跟終端機寬度無關、每次建
/// 圖只判斷一次」的簡單規則。
fn build_lanes(
    parent_start: &[u32],
    parent_idx: &[u32],
    head: Option<u32>,
    reserve_head: bool,
    trunc: Truncation,
) -> Lanes {
    let mut opts = BuildOpts {
        head,
        reserve_head,
        max_edge_rows: None,
    };
    let lanes = lanes::build(parent_start, parent_idx, &opts);
    if lanes.cell_count() <= trunc.min_graph_width {
        return lanes;
    }
    drop(lanes);
    opts.max_edge_rows = Some(u32::try_from(trunc.max_edge_rows).unwrap_or(u32::MAX));
    lanes::build(parent_start, parent_idx, &opts)
}

pub fn calc_graph(
    repository: &Repository,
    head_hint: Option<&CommitHash>,
    reserve_head_col: bool,
    trunc: Truncation,
) -> Graph {
    let (parent_start, parent_idx) = repository.parent_csr();
    let head = head_hint
        .and_then(|h| repository.index_of(h))
        .map(|raw| raw as u32);

    let lanes = build_lanes(parent_start, parent_idx, head, reserve_head_col, trunc);
    Graph::from_lanes(None, lanes)
}

pub fn calc_graph_filtered(
    repository: &Repository,
    remote_only: &RemoteOnly,
    head_hint: Option<&CommitHash>,
    reserve_head_col: bool,
    trunc: Truncation,
) -> Graph {
    let total = repository.all_commits().len();
    let (parent_start_raw, parent_idx_raw) = repository.parent_csr();

    // 可見集合對祖先封閉（`find_remote_only_commits` 從本地 ref 沿 loaded
    // parent 走訪，走得到的才可見，不可能走到一個「parent 可見、自己不可
    // 見」的矛盾狀態），所以只要把 CSR 重新編號到 row 空間，不必改寫拓樸。
    let mut raw_to_row = vec![NOT_LOADED; total];
    let mut raws: Vec<u32> = Vec::new();
    for (raw, row) in raw_to_row.iter_mut().enumerate() {
        if !remote_only.contains(raw) {
            *row = raws.len() as u32;
            raws.push(raw as u32);
        }
    }

    let mut parent_start = Vec::with_capacity(raws.len() + 1);
    let mut parent_idx = Vec::with_capacity(parent_idx_raw.len());
    parent_start.push(0);
    for &raw in &raws {
        let range =
            parent_start_raw[raw as usize] as usize..parent_start_raw[raw as usize + 1] as usize;
        for &p in &parent_idx_raw[range] {
            let mapped = if p == NOT_LOADED {
                NOT_LOADED
            } else {
                let row = raw_to_row[p as usize];
                debug_assert_ne!(row, NOT_LOADED, "可見 commit 的已載入 parent 一定可見");
                row
            };
            parent_idx.push(mapped);
        }
        parent_start.push(parent_idx.len() as u32);
    }

    let head = head_hint
        .and_then(|h| repository.index_of(h))
        .filter(|&raw| !remote_only.contains(raw))
        .map(|raw| raw_to_row[raw]);

    let lanes = build_lanes(&parent_start, &parent_idx, head, reserve_head_col, trunc);
    Graph::from_lanes(Some(raws), lanes)
}

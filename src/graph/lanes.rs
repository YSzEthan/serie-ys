//! merge 密集大 repo 的欄位分配引擎（issue #118）。取代 `calc_commit_positions`
//! ＋`calc_edges` 兩階段、繞道找路的舊演算法。
//!
//! ## 模型
//!
//! 由上往下（row 0 是最新的 commit）一次算完：每一欄（lane）記住自己正在
//! 等哪一列 commit。一列處理時：
//!
//! 1. commit 優先接「first-parent child」在等它的那條 lane（同一列可能有
//!    多條 first-parent child 在等——分支點——取最左邊）；沒有的話接其他在
//!    等它的 lane 中最左邊那條；都沒有就開新 lane。唯一例外是 HEAD 保留欄：
//!    reserve 開啟時，col 0 在 HEAD 落地之前對任何 lane 都是禁區，HEAD 那
//!    一列固定坐 col 0，其他在等它的 lane（不管幾條）全部收斂進來。
//! 2. 其他在等它、卻沒被接手的 lane 在這一列收斂（畫 `╯`／`╰`）。這一列
//!    收斂的欄，這一列不能重新拿去開新 lane（否則同一格的「線在這裡結束」
//!    跟「線穿過去接到另一個 parent」就分不出來）。
//! 3. first parent 接手這條 lane 往下延伸；沒載入的 first parent（`max_count`
//!    截斷）一路開到底，永遠不會被接手。
//! 4. 第二個以後的 parent（merge）：已經有 lane 在等它就接過去（畫 `╮`／
//!    `╭`）；沒有就在最左邊的空位開新 lane。沒載入的非 first parent
//!    （stash 的 index／untracked commit）略過。
//! 5. 沒有 parent 的 root：這一欄留一列墓碑（下一列不能開新 lane），
//!    再下一列釋放，避免看起來像接到不相干的下一個 commit。
//!
//! lane 產生的每一條 edge，`associated_line_pos_x` 一律是這條 lane 自己的
//! 欄（跟舊引擎的 branch 畫法一致；merge 從「跟著 parent 欄」改成「跟著
//! lane」，是跟舊引擎唯一不同的地方，配色因此看得出差異）。
//!
//! ## 儲存
//!
//! 只存「這一列變了什麼」：commit 自己的欄、要不要往下延伸、收斂／開新／
//! 接手的欄位清單。每 [`CHECKPOINT_INTERVAL`] 列另外存一份「當時開著的
//! lane」bitset。查詢時從最近的 checkpoint 重播，推出跟原本語意相同的
//! [`Edge`]。

use std::ops::Range;

use super::{Edge, EdgeType};

/// CSR 裡「沒載入」的 sentinel，跟 `Repository` 的 `PARENT_NOT_LOADED` 同值
/// （`u32::MAX`）；呼叫端把 raw／row 空間的 parent CSR 轉給這裡時，沒載入
/// 或不可見的 parent 一律填這個值。
pub(super) const NOT_LOADED: u32 = u32::MAX;
/// intrusive linked list 的「鏈結尾」sentinel，跟 `NOT_LOADED` 同值但語意
/// 不同：這裡指「沒有下一個節點」，不是「parent 沒載入」。
const NONE: u32 = u32::MAX;

const CHECKPOINT_INTERVAL: usize = 128;

/// 一列裡「收斂」或「開新／接手」的一個欄位變化。
#[derive(Debug, Clone, Copy)]
struct RowEvent {
    col: u32,
    /// `true`：這條 lane 在這一列之後繼續開著（Open／Join，畫 `╮`／`╭`）。
    /// `false`：這條 lane 在這一列收斂、關閉（Converge，畫 `╯`／`╰`）。
    continues: bool,
}

#[derive(Debug)]
pub(super) struct Lanes {
    /// 每列 commit 所在的欄。
    cols: Vec<u32>,
    /// 每列 commit 是否有（已載入的）parent，lane 因此往下延伸。
    down: Vec<bool>,
    /// CSR：第 y 列的事件在 `events[ev_start[y]..ev_start[y + 1]]`。
    ev_start: Vec<u32>,
    events: Vec<RowEvent>,
    /// 每 `CHECKPOINT_INTERVAL` 列一份「開著的 lane」bitset，第 k 份對應
    /// 第 `k * CHECKPOINT_INTERVAL` 列的起始狀態（處理該列之前）。第 0 份
    /// 永遠是空集合。每份 `words` 個 `u64`。
    checkpoints: Vec<u64>,
    words: usize,
    cell_count: usize,
}

impl Lanes {
    pub(super) fn row_count(&self) -> usize {
        self.cols.len()
    }

    pub(super) fn cell_count(&self) -> usize {
        self.cell_count
    }

    pub(super) fn col(&self, row: usize) -> usize {
        self.cols[row] as usize
    }

    /// `row_edges_in(row..row + 1, ..)` 的單列封裝，給只要一列的呼叫端用。
    pub(super) fn row_edges(&self, row: usize) -> Vec<Edge> {
        let mut out = Vec::new();
        self.row_edges_in(row..row + 1, |_, es| out = es.to_vec());
        out
    }

    /// 從最近的 checkpoint 重播到 `rows.end`，把 `rows` 範圍內每一列排序過
    /// 的 edge 交給 `f`。範圍外（checkpoint 到 `rows.start` 之間）只重播、
    /// 不回呼，這是「查一列也要重播一段」的成本換來的：checkpoint 越密，
    /// 這段越短，記憶體越大，`CHECKPOINT_INTERVAL` 就是這個取捨。
    pub(super) fn row_edges_in(&self, rows: Range<usize>, mut f: impl FnMut(usize, &[Edge])) {
        if rows.start >= rows.end || self.words == 0 {
            return;
        }
        let ck_idx = rows.start / CHECKPOINT_INTERVAL;
        let ck_row = ck_idx * CHECKPOINT_INTERVAL;
        let mut open = self.checkpoints[ck_idx * self.words..(ck_idx + 1) * self.words].to_vec();
        let mut buf: Vec<Edge> = Vec::new();

        for y in ck_row..rows.end {
            let c = self.cols[y] as usize;
            let ev = &self.events[self.ev_start[y] as usize..self.ev_start[y + 1] as usize];

            buf.clear();
            if is_open(&open, c) {
                buf.push(Edge::new(EdgeType::Up, c, c));
            }
            // Vertical：(A ∩ B) \ {c}。A 是目前開著的集合，收斂欄（continues
            // == false）會在下面離開 B，其餘留在 B 裡的就是這裡要畫的。
            for_each_set_bit(&open, |i| {
                debug_assert!(i < self.cell_count, "padding bit 不該被設起來");
                if i == c {
                    return;
                }
                if ev.iter().any(|e| !e.continues && e.col as usize == i) {
                    return;
                }
                buf.push(Edge::new(EdgeType::Vertical, i, i));
            });
            for e in ev {
                push_corner(&mut buf, c, e.col as usize, e.continues);
            }
            if self.down[y] {
                buf.push(Edge::new(EdgeType::Down, c, c));
            }

            buf.sort_by_key(|e| (e.associated_line_pos_x, e.pos_x, e.edge_type));
            debug_assert!(
                buf.windows(2).all(|w| w[0] != w[1]),
                "lane 引擎不該對同一列產生重複 edge"
            );

            if y >= rows.start {
                f(y, &buf);
            }

            // 推進到下一列的狀態：先收斂、再視 commit 自己是否往下延伸、
            // 最後開新／接手（open 的欄本來就不在 A 裡；join 的欄本來就在，
            // 重複 set 沒有影響）。
            for e in ev {
                if !e.continues {
                    clear_bit(&mut open, e.col as usize);
                }
            }
            if self.down[y] {
                set_bit(&mut open, c);
            } else {
                clear_bit(&mut open, c);
            }
            for e in ev {
                if e.continues {
                    set_bit(&mut open, e.col as usize);
                }
            }
        }
    }
}

fn is_open(bits: &[u64], i: usize) -> bool {
    bits[i / 64] & (1u64 << (i % 64)) != 0
}

fn set_bit(bits: &mut [u64], i: usize) {
    bits[i / 64] |= 1u64 << (i % 64);
}

fn clear_bit(bits: &mut [u64], i: usize) {
    bits[i / 64] &= !(1u64 << (i % 64));
}

/// 依序走過 `bits` 裡每個為 1 的位元，只花跟「目前開著幾條 lane」成比例
/// 的時間，不是跟總欄寬成比例。
fn for_each_set_bit(bits: &[u64], mut f: impl FnMut(usize)) {
    for (word_idx, &word) in bits.iter().enumerate() {
        let mut word = word;
        while word != 0 {
            let bit = word.trailing_zeros() as usize;
            f(word_idx * 64 + bit);
            word &= word - 1;
        }
    }
}

/// 從 `c` 到 `l` 的轉角：`continues` 決定終點是 Converge（`Bottom`）還是
/// Open／Join（`Top`）。assoc 永遠是 `l`——這條線屬於 lane `l`，不是 commit
/// 自己那欄。`c == l` 不該發生：`l` 一定是「別的」lane，不會恰好是 commit
/// 自己正在坐的那欄。
fn push_corner(buf: &mut Vec<Edge>, c: usize, l: usize, continues: bool) {
    debug_assert_ne!(c, l, "commit 自己的欄不會同時是收斂或開新的目標");
    let (lo, hi) = if c < l { (c, l) } else { (l, c) };
    for x in (lo + 1)..hi {
        buf.push(Edge::new(EdgeType::Horizontal, x, l));
    }
    let (near, far) = match (c < l, continues) {
        (true, true) => (EdgeType::Right, EdgeType::RightTop),
        (true, false) => (EdgeType::Right, EdgeType::RightBottom),
        (false, true) => (EdgeType::Left, EdgeType::LeftTop),
        (false, false) => (EdgeType::Left, EdgeType::LeftBottom),
    };
    buf.push(Edge::new(near, c, l));
    buf.push(Edge::new(far, l, l));
}

/// 從 `start` 開始找第一個沒開著、也不在 `ban` 裡的欄；找不到就開在最後面。
/// `(start..)` 一定會在 `i == lane_open.len()` 時滿足條件，`find` 保證會停。
fn find_vacant(lane_open: &[bool], start: usize, ban: &[u32]) -> u32 {
    (start..)
        .find(|&i| i >= lane_open.len() || (!lane_open[i] && !ban.contains(&(i as u32))))
        .expect("i == lane_open.len() 時條件恆成立") as u32
}

/// `lane_open`／`is_fp`／`wait_next` 一定一起變長，兩個呼叫點都用這個，
/// 不必各自展開三行。
fn grow_lanes(
    lane_open: &mut Vec<bool>,
    is_fp: &mut Vec<bool>,
    wait_next: &mut Vec<u32>,
    len: usize,
) {
    if lane_open.len() < len {
        lane_open.resize(len, false);
        is_fp.resize(len, false);
        wait_next.resize(len, NONE);
    }
}

/// 建構 lane 引擎的結果。`parent_start`/`parent_idx` 是 row 空間的 parent
/// CSR（跟 `Repository` 的 parent CSR 同格式：`parent_idx[parent_start[y]
/// ..parent_start[y + 1]]` 是第 y 列 commit 的 parent row，順序同
/// `parent_commit_hashes`；沒載入或不可見一律填 [`NOT_LOADED`]）。
/// `reserved_head`：`Some(row)` 時 col 0 在該列落地之前對任何 lane 都是
/// 禁區，`None` 時完全不影響欄位分配。
pub(super) fn build(parent_start: &[u32], parent_idx: &[u32], reserved_head: Option<u32>) -> Lanes {
    let n = parent_start.len().saturating_sub(1);
    if n == 0 {
        return Lanes {
            cols: Vec::new(),
            down: Vec::new(),
            ev_start: vec![0],
            events: Vec::new(),
            checkpoints: Vec::new(),
            words: 0,
            cell_count: 0,
        };
    }

    // lane 狀態：`lane_open[col]` 開著時，`is_fp[col]` 分辨它是不是 commit
    // 自己 first-parent 的延伸（rule 1 的優先權），`wait_next[col]` 是它
    // 掛在 `wait_head[target_row]` 那條鏈結串列裡的下一個節點。
    let mut lane_open: Vec<bool> = Vec::new();
    let mut is_fp: Vec<bool> = Vec::new();
    let mut wait_next: Vec<u32> = Vec::new();
    let mut wait_head: Vec<u32> = vec![NONE; n];
    // root 的墓碑：(欄, 設下的那一列)，滿一列後釋放（rule 5）。
    let mut pending_tombs: Vec<(u32, u32)> = Vec::new();

    let mut cols: Vec<u32> = Vec::with_capacity(n);
    let mut down: Vec<bool> = Vec::with_capacity(n);
    let mut ev_start: Vec<u32> = Vec::with_capacity(n + 1);
    ev_start.push(0);
    let mut events: Vec<RowEvent> = Vec::new();

    // 每 CHECKPOINT_INTERVAL 列存一份快照（此時的寬度可能還沒到最終寬度，
    // build 完後統一 pad 成最終寬度再 pack 成 bitset）。
    let mut raw_checkpoints: Vec<Vec<bool>> = vec![Vec::new()];

    let mut pend = reserved_head.is_some();

    for y in 0..n {
        let yu = y as u32;

        pending_tombs.retain(|&(col, set_at)| {
            if yu >= set_at + 2 {
                lane_open[col as usize] = false;
                false
            } else {
                true
            }
        });

        // 走訪鏈結串列，收集在等我的 lane；排序 key 讓 fp 的整組排最前面
        // （rule 1），組內再照欄位由左到右——跟原本「fp_cols ++ other_cols」
        // 兩個各自排序、串接起來的順序完全一樣。
        let mut waiters: Vec<u32> = Vec::new();
        {
            let mut cur = wait_head[y];
            wait_head[y] = NONE;
            while cur != NONE {
                waiters.push(cur);
                cur = wait_next[cur as usize];
            }
        }
        waiters.sort_unstable_by_key(|&l| (!is_fp[l as usize], l));

        let ish = reserved_head == Some(yu);
        let start_col = usize::from(pend);
        let (x, conv): (u32, Vec<u32>) = if ish {
            (0, waiters)
        } else if let Some((&first, rest)) = waiters.split_first() {
            (first, rest.to_vec())
        } else {
            (find_vacant(&lane_open, start_col, &[]), Vec::new())
        };

        let ev_from = events.len();
        for &l in &conv {
            lane_open[l as usize] = false;
            events.push(RowEvent {
                col: l,
                continues: false,
            });
        }

        let parents = &parent_idx[parent_start[y] as usize..parent_start[y + 1] as usize];
        let xu = x as usize;
        grow_lanes(&mut lane_open, &mut is_fp, &mut wait_next, xu + 1);
        lane_open[xu] = true;

        let mut seen: Vec<u32> = Vec::new();
        let rest = match parents.split_first() {
            None => {
                pending_tombs.push((x, yu)); // rule 5：root 留墓碑。
                &[][..]
            }
            Some((&fp_target, rest)) => {
                is_fp[xu] = true;
                if fp_target == NOT_LOADED {
                    wait_next[xu] = NONE; // 沒載入：lane 一路開到底（rule 3）。
                } else {
                    wait_next[xu] = wait_head[fp_target as usize];
                    wait_head[fp_target as usize] = x;
                }
                seen.push(fp_target);
                rest
            }
        };

        for &p in rest {
            if p == NOT_LOADED || seen.contains(&p) {
                continue; // 沒載入的非 first parent、或跟前面重複：略過。
            }
            seen.push(p);

            let l = if wait_head[p as usize] != NONE {
                wait_head[p as usize]
            } else {
                let new_l = find_vacant(&lane_open, start_col, &conv);
                let nl = new_l as usize;
                grow_lanes(&mut lane_open, &mut is_fp, &mut wait_next, nl + 1);
                lane_open[nl] = true;
                is_fp[nl] = false;
                wait_next[nl] = wait_head[p as usize];
                wait_head[p as usize] = new_l;
                new_l
            };
            events.push(RowEvent {
                col: l,
                continues: true,
            });
        }
        let ev = &events[ev_from..];
        debug_assert!(
            ev.iter()
                .enumerate()
                .all(|(i, a)| ev[i + 1..].iter().all(|b| a.col != b.col)),
            "同一列不該對同一欄產生兩個事件"
        );

        ev_start.push(events.len() as u32);
        cols.push(x);
        down.push(!parents.is_empty());

        if ish {
            pend = false;
        }

        if (y + 1) % CHECKPOINT_INTERVAL == 0 {
            raw_checkpoints.push(lane_open.clone());
        }
    }

    let cell_count = lane_open.len().max(1);
    let words = cell_count.div_ceil(64);
    let mut checkpoints = vec![0u64; raw_checkpoints.len() * words];
    for (chunk, snap) in checkpoints.chunks_mut(words).zip(&raw_checkpoints) {
        for (i, _) in snap.iter().enumerate().filter(|&(_, &open)| open) {
            set_bit(chunk, i);
        }
    }

    Lanes {
        cols,
        down,
        ev_start,
        events,
        checkpoints,
        words,
        cell_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 手寫 row 空間 parent CSR 的小工具：每個 commit 一個 `&[u32]` parent
    /// 列表（`NOT_LOADED` 表示沒載入），照順序建成 CSR。
    fn csr(parents: &[&[u32]]) -> (Vec<u32>, Vec<u32>) {
        let mut start = vec![0u32];
        let mut idx = Vec::new();
        for ps in parents {
            idx.extend_from_slice(ps);
            start.push(idx.len() as u32);
        }
        (start, idx)
    }

    fn edges_for(lanes: &Lanes, row: usize) -> Vec<Edge> {
        lanes.row_edges(row)
    }

    fn has(edges: &[Edge], et: EdgeType, pos_x: usize) -> bool {
        edges.iter().any(|e| e.edge_type == et && e.pos_x == pos_x)
    }

    // --- 規則 1／3：first-parent 直線 ---

    #[test]
    fn straight_chain_draws_vertical() {
        // 0 -> 1 -> 2（0 最新）。
        let (start, idx) = csr(&[&[1], &[2], &[]]);
        let lanes = build(&start, &idx, None);
        assert_eq!(lanes.col(0), 0);
        assert_eq!(lanes.col(1), 0);
        assert_eq!(lanes.col(2), 0);
        assert!(has(&edges_for(&lanes, 0), EdgeType::Down, 0));
        assert!(has(&edges_for(&lanes, 1), EdgeType::Up, 0));
        assert!(has(&edges_for(&lanes, 1), EdgeType::Down, 0));
        assert!(has(&edges_for(&lanes, 2), EdgeType::Up, 0));
    }

    /// 分支點：兩個 commit 都以同一個 commit 為（唯一）parent，都算 fp。
    /// 兩條 lane 在 parent 那一列，leftmost 接手，另一條收斂。
    #[test]
    fn fork_point_leftmost_fp_wins() {
        // 0(parent=2), 1(parent=2), 2(root)。0 先處理開 lane 0，
        // 1 開 lane 1，2 兩條都在等 -> 接 lane 0，lane 1 收斂。
        let (start, idx) = csr(&[&[2], &[2], &[]]);
        let lanes = build(&start, &idx, None);
        assert_eq!(lanes.col(0), 0);
        assert_eq!(lanes.col(1), 1);
        assert_eq!(lanes.col(2), 0, "leftmost fp lane 勝出");
        let e2 = edges_for(&lanes, 2);
        assert!(has(&e2, EdgeType::Up, 0));
        assert!(has(&e2, EdgeType::RightBottom, 1), "lane 1 收斂進來");
    }

    // --- 規則 4：merge 開新／接手 ---

    #[test]
    fn merge_joins_existing_wait_without_opening_new_lane() {
        // 0: merge，fp=1，second=4 → 先幫 4 開一條新 lane（col 1）
        // 1 -> 2 是 0 的 fp 鏈延伸
        // 2: merge，fp=3，second=4 → 4 已經有 lane 在等，接過去（join），
        //    不開新 lane；lane 1 這時仍然開著，所以 join 那一列 col 1
        //    同時有 Vertical（線繼續往下）跟轉角（這一列多接一條線進來）
        // 3 -> 4 是 2 的 fp 鏈延伸
        // 4: root，兩條路線最終在這裡匯合：fp 鏈（col 0）留下，
        //    lane 1（一路沒被觸碰）在這裡收斂
        let (start, idx) = csr(&[&[1, 4], &[2], &[3, 4], &[4], &[]]);
        let lanes = build(&start, &idx, None);

        assert_eq!(lanes.col(0), 0);
        assert_eq!(lanes.col(1), 0);
        assert_eq!(lanes.col(2), 0);
        assert_eq!(lanes.col(3), 0);
        assert_eq!(lanes.col(4), 0);

        let e0 = edges_for(&lanes, 0);
        assert!(has(&e0, EdgeType::RightTop, 1), "merge 開新 lane 1");

        let e2 = edges_for(&lanes, 2);
        assert!(
            has(&e2, EdgeType::RightTop, 1),
            "join：lane 1 已經在等 4，接過去而不是開新的"
        );
        assert!(
            has(&e2, EdgeType::Vertical, 1),
            "join 不收斂 lane 1，這一列它同時繼續往下"
        );

        let e4 = edges_for(&lanes, 4);
        assert!(
            has(&e4, EdgeType::RightBottom, 1),
            "lane 1 真正到達 4 時才收斂"
        );
    }

    // --- 規則 2：同一列剛收斂的欄不能同列重開 ---

    #[test]
    fn converged_col_not_reused_same_row() {
        // A -> H（HEAD，reserve；merge fp=2, second=3）-> {2, 3}（root）
        // A 的 fp 鏈在 pend 期間只能落在 col 1；H 落地時被強制 col 0，
        // A 那條 lane（col 1）這一列收斂；H 同一列的 merge（second parent
        // 3）不能重新用剛收斂的 col 1 開新 lane。
        let (start, idx) = csr(&[&[1], &[2, 3], &[], &[]]);
        let lanes = build(&start, &idx, Some(1));

        assert_eq!(lanes.col(0), 1, "pend 時 col 0 禁區，A 落在 col 1");
        assert_eq!(lanes.col(1), 0, "HEAD 固定 col 0");

        let e1 = edges_for(&lanes, 1);
        assert!(
            has(&e1, EdgeType::RightBottom, 1),
            "col 1 的 lane 這一列收斂"
        );
        assert!(
            !has(&e1, EdgeType::RightTop, 1) && !has(&e1, EdgeType::LeftTop, 1),
            "col 1 剛收斂，這一列不能被 merge 重新拿去開新 lane"
        );
        assert_ne!(lanes.col(3), 1, "merge 的新 lane 不能落在剛收斂的 col 1");
    }

    // --- 規則 5：HEAD 保留欄 ---

    #[test]
    fn reserved_head_takes_col0_others_converge() {
        // 0: parent=[1]（HEAD 的「領先」鏈）
        // 1: parent=[2]，HEAD = row 2
        // 2: root
        let (start, idx) = csr(&[&[1], &[2], &[]]);
        let lanes = build(&start, &idx, Some(2));
        assert_eq!(lanes.col(0), 1, "pend 時 col 0 禁區，leaf 落在 col 1");
        assert_eq!(lanes.col(1), 1);
        assert_eq!(lanes.col(2), 0, "HEAD 固定 col 0");
        let e2 = edges_for(&lanes, 2);
        assert!(
            !e2.iter()
                .any(|e| e.pos_x == 0 && e.edge_type == EdgeType::Up),
            "col 0 在 HEAD 落地前沒有任何 lane，HEAD 沒有 Up"
        );
        assert!(
            has(&e2, EdgeType::RightBottom, 1),
            "領先的 lane 收斂進 col 0"
        );
    }

    #[test]
    fn reserved_head_with_no_waiters_is_trivial() {
        // HEAD 是最新的 commit（row 0），沒有人在等它。
        let (start, idx) = csr(&[&[1], &[]]);
        let lanes = build(&start, &idx, Some(0));
        assert_eq!(lanes.col(0), 0);
        assert_eq!(lanes.col(1), 0);
    }

    // --- 規則 6：沒載入的 parent ---

    #[test]
    fn unloaded_first_parent_keeps_lane_open_forever() {
        let (start, idx) = csr(&[&[NOT_LOADED]]);
        let lanes = build(&start, &idx, None);
        assert_eq!(lanes.col(0), 0);
        assert!(has(&edges_for(&lanes, 0), EdgeType::Down, 0));
    }

    #[test]
    fn unloaded_non_first_parent_is_skipped() {
        // stash：parents=[base(=1), index(未載入), untracked(未載入)]
        let (start, idx) = csr(&[&[1, NOT_LOADED, NOT_LOADED], &[]]);
        let lanes = build(&start, &idx, None);
        let e0 = edges_for(&lanes, 0);
        assert_eq!(e0.len(), 1, "沒載入的非 first parent 不產生任何 edge");
        assert!(has(&e0, EdgeType::Down, 0));
    }

    // --- 規則 7：root 墓碑 ---

    #[test]
    fn root_blocks_next_row_then_releases() {
        // 0: root。1、2 都沒有 parent，若 0 的欄立刻釋放，1 會誤用它。
        let (start, idx) = csr(&[&[], &[], &[]]);
        let lanes = build(&start, &idx, None);
        assert_eq!(lanes.col(0), 0);
        assert_eq!(lanes.col(1), 1, "row1 還在 0 的墓碑期，不能用 col 0");
        assert_eq!(lanes.col(2), 0, "row2 起 col 0 釋放");
    }

    // --- checkpoint 重播：跨界一致性 ---

    #[test]
    fn row_edges_in_matches_across_checkpoint_boundary() {
        // 造一條長度超過兩個 checkpoint 區間的直線。
        let n = CHECKPOINT_INTERVAL * 2 + 5;
        let parents: Vec<Vec<u32>> = (0..n)
            .map(|i| {
                if i + 1 < n {
                    vec![(i + 1) as u32]
                } else {
                    vec![]
                }
            })
            .collect();
        let refs: Vec<&[u32]> = parents.iter().map(|v| v.as_slice()).collect();
        let (start, idx) = csr(&refs);
        let lanes = build(&start, &idx, None);

        // 一次批次重播整段，跨過兩個 checkpoint 邊界；逐列查詢每次都從
        // 各自最近的 checkpoint 重播，兩條路徑要得到一樣的答案。
        let mut batch = vec![Vec::new(); n];
        lanes.row_edges_in(0..n, |y, es| batch[y] = es.to_vec());
        for row in [
            0,
            1,
            CHECKPOINT_INTERVAL - 1,
            CHECKPOINT_INTERVAL,
            CHECKPOINT_INTERVAL + 50,
            n - 1,
        ] {
            assert_eq!(
                edges_for(&lanes, row),
                batch[row],
                "row {row} 逐列與批次查詢應一致"
            );
        }

        // 起點落在第二個 checkpoint 之後（不是 0）的批次查詢，走的是不同
        // 的重播起點，要跟上面「從頭開始」的結果一致。
        let mid = CHECKPOINT_INTERVAL + 50;
        let mut from_mid = Vec::new();
        lanes.row_edges_in(mid..n, |y, es| {
            if y == mid {
                from_mid = es.to_vec();
            }
        });
        assert_eq!(
            from_mid, batch[mid],
            "從第二個 checkpoint 開始查詢應與批次一致"
        );
    }

    /// 釘住：HEAD 被 merge 回去時，col 0 只留給 HEAD 自己，不會被 child
    /// 的欄「補救」。跟舊引擎（col 0 只有 HEAD 是 leaf 時才生效）行為不同，
    /// 是這次 #118 的修正重點——見 `head_behind_00X`／`stash_head_001` 系列
    /// golden。
    #[test]
    fn reserved_head_always_lands_on_col0_even_when_merged_back() {
        // child (row0) parents=[head, other]；head (row1) 是 HEAD，
        // 被 child 的 first parent 指到；other (row2) 是 child 的第二個 parent。
        let (start, idx) = csr(&[&[1, 2], &[], &[]]);
        let lanes = build(&start, &idx, Some(1));
        assert_eq!(lanes.col(0), 1, "child 是 leaf，pend 期間落在 col 1");
        assert_eq!(lanes.col(1), 0, "HEAD 固定 col 0，不再跟著 child 的欄");
    }

    // --- 結構性回歸：merge 密集的合成 DAG 不會重蹈舊引擎的覆轍 ---

    /// 固定 seed 的簡易 xorshift，只給下面的合成 DAG 產生器用，不需要
    /// 密碼學等級的隨機性，不加外部依賴。
    struct Xorshift64(u64);

    impl Xorshift64 {
        fn new(seed: u64) -> Self {
            Self(seed | 1) // 避免卡在 0
        }

        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        fn next_range(&mut self, bound: u32) -> u32 {
            if bound == 0 {
                return 0;
            }
            (self.next_u64() % u64::from(bound)) as u32
        }
    }

    /// 合成一個 merge 密集的 DAG（row 空間 parent CSR）：commit `y` 的
    /// parent 一定是 index 更大（更舊）的 commit，first parent 偏好緊接
    /// 在下面的位置；`merge_prob_pct`（0～100）機率再加 1～2 個更舊的
    /// commit 當 merge parent，模擬真實 repo 分支／merge 密集的程度。
    fn gen_merge_dense(n: usize, seed: u64, merge_prob_pct: u32) -> (Vec<u32>, Vec<u32>) {
        let mut rng = Xorshift64::new(seed);
        let mut parent_start = vec![0u32];
        let mut parent_idx = Vec::new();
        for y in 0..n {
            let mut parents: Vec<u32> = Vec::new();
            if y + 1 < n {
                let span = (n - y - 2).min(3) as u32;
                let fp = (y + 1 + rng.next_range(span + 1) as usize).min(n - 1) as u32;
                parents.push(fp);
                if rng.next_range(100) < merge_prob_pct && (fp as usize) + 1 < n {
                    let extra = 1 + rng.next_range(2);
                    for _ in 0..extra {
                        let range = (n - fp as usize - 1) as u32;
                        let p2 = fp + 1 + rng.next_range(range);
                        if !parents.contains(&p2) {
                            parents.push(p2);
                        }
                    }
                }
            }
            parent_idx.extend_from_slice(&parents);
            parent_start.push(parent_idx.len() as u32);
        }
        (parent_start, parent_idx)
    }

    /// 從 `row_edges_in` 給的 edge 反推「這一列開始前，開著幾條 lane」：
    /// commit 自己那欄若有 `Up` 就算一條，收斂（`RightBottom`／
    /// `LeftBottom`）跟一般 `Vertical` 各算一條——這就是驗收公式要的
    /// 「同時跨越的 edge 數」。
    fn open_before(edges: &[Edge], col: usize) -> usize {
        let has_up = edges
            .iter()
            .any(|e| e.edge_type == EdgeType::Up && e.pos_x == col);
        let converging = edges
            .iter()
            .filter(|e| matches!(e.edge_type, EdgeType::RightBottom | EdgeType::LeftBottom))
            .count();
        let vertical = edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::Vertical)
            .count();
        usize::from(has_up) + converging + vertical
    }

    /// 上界＝任一列同時跨越的 edge 數 + root 數 + 1（這裡沒有 `max_count`
    /// 截斷，截斷數是 0）。舊引擎在同一種產生器上量出 516 對 82（issue
    /// #118 的量測記錄）；這裡驗的是新引擎不會重蹈覆轍。
    #[test]
    fn cell_count_stays_within_structural_bound() {
        for (n, seed) in [(2_000, 1), (2_000, 2), (5_000, 3), (5_000, 4), (10_000, 5)] {
            let (parent_start, parent_idx) = gen_merge_dense(n, seed, 35);
            let lanes = build(&parent_start, &parent_idx, None);

            let roots = (0..n)
                .filter(|&y| parent_start[y] == parent_start[y + 1])
                .count();
            let mut max_open = 0usize;
            lanes.row_edges_in(0..n, |y, es| {
                max_open = max_open.max(open_before(es, lanes.col(y)) + 1);
            });

            let bound = max_open + roots + 1;
            assert!(
                lanes.cell_count() <= bound,
                "n={n} seed={seed}: cell_count={} 超過上界 {bound}",
                lanes.cell_count()
            );
        }
    }

    /// 1M 規模，只驗結構不驗時間：debug build 慢上數十倍，CI 機器也不
    /// 穩定，拿時間做斷言遲早會誤報。時間驗收見 linux 的實測（PR 說明）。
    #[test]
    #[ignore]
    fn cell_count_stays_within_bound_at_1m_scale() {
        let n = 1_000_000;
        let (parent_start, parent_idx) = gen_merge_dense(n, 42, 5);
        let lanes = build(&parent_start, &parent_idx, None);

        let roots = (0..n)
            .filter(|&y| parent_start[y] == parent_start[y + 1])
            .count();
        let mut max_open = 0usize;
        lanes.row_edges_in(0..n, |y, es| {
            max_open = max_open.max(open_before(es, lanes.col(y)) + 1);
        });

        let bound = max_open + roots + 1;
        assert!(
            lanes.cell_count() <= bound,
            "cell_count={} 超過上界 {bound}",
            lanes.cell_count()
        );
    }
}

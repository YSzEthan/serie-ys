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

/// 一列裡某一欄的變化。
#[derive(Debug, Clone, Copy)]
struct RowEvent {
    col: u32,
    kind: EventKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventKind {
    /// 這條 lane 在這一列收斂、關閉（畫 `╯`／`╰`）。
    Converge,
    /// 這條 lane 在這一列之後繼續開著（Open／Join，畫 `╮`／`╭`）。
    Continue,
    /// 長線截斷：從上一列延伸下來的 lane 在這一列畫 `↓` 後關閉。
    ArrowDown,
    /// 長線截斷：這一列開一條短 lane 畫 `↑`，下一列接到 parent。`color`
    /// 是被截斷那條線原本的欄，`↑` 與下一列那一段都用它的顏色，上下兩段
    /// 才對得起來（`↑` 不一定開得回原本那一欄）。
    ArrowUp { color: u32 },
}

impl EventKind {
    /// 這一列之後 lane 是否開著。
    fn opens(self) -> bool {
        matches!(self, EventKind::Continue | EventKind::ArrowUp { .. })
    }
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
    /// 建圖時啟用了長線截斷（不代表真的有線被截）。
    truncated: bool,
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

    pub(super) fn truncated(&self) -> bool {
        self.truncated
    }

    fn events_of(&self, y: usize) -> &[RowEvent] {
        &self.events[self.ev_start[y] as usize..self.ev_start[y + 1] as usize]
    }

    /// `row_edges_in(row..row + 1, ..)` 的單列封裝，給只要一列的呼叫端用。
    pub(super) fn row_edges(&self, row: usize) -> Vec<Edge> {
        let mut out = Vec::new();
        self.row_edges_in(row..row + 1, |_, es| out = es.to_vec());
        out
    }

    /// `open`：第 `y` 列開始前的狀態 → 第 `y` 列處理完之後（也就是第
    /// `y + 1` 列開始前）的狀態。只推進 bitset，不組 edge。
    fn advance(&self, open: &mut [u64], y: usize) {
        let c = self.col(y);
        let ev = self.events_of(y);
        // 先收斂、再視 commit 自己是否往下延伸、最後開新／接手（open 的欄
        // 本來就不在 A 裡；join 的欄本來就在，重複 set 沒有影響）。
        for e in ev {
            if !e.kind.opens() {
                clear_bit(open, e.col as usize);
            }
        }
        if self.down[y] {
            set_bit(open, c);
        } else {
            clear_bit(open, c);
        }
        for e in ev {
            if e.kind.opens() {
                set_bit(open, e.col as usize);
            }
        }
    }

    /// 第 `y` 列排序過的 edge。`open` 是第 `y` 列開始前（呼叫
    /// `advance(open, y)` 之前）的狀態，呼叫端負責推進。
    fn build_row(&self, open: &[u64], y: usize, buf: &mut Vec<Edge>) {
        let c = self.col(y);
        let ev = self.events_of(y);
        // 上一列開的 `↑` 在這一列接到 parent（commit 自己的 Up 或收斂
        // 轉角），顏色沿用 `↑` 的。直接查上一列的事件（CSR 可隨機存取），
        // 不必把這個狀態塞進 checkpoint。
        let prev = if y > 0 { self.events_of(y - 1) } else { &[] };
        let line_of = |col: usize| {
            prev.iter()
                .find_map(|e| match e.kind {
                    EventKind::ArrowUp { color } if e.col as usize == col => Some(color as usize),
                    _ => None,
                })
                .unwrap_or(col)
        };

        buf.clear();
        if is_open(open, c) {
            buf.push(Edge::new(EdgeType::Up, c, line_of(c)));
        }
        // Vertical：(A ∩ B) \ {c}。A 是目前開著的集合，收斂欄會在下面
        // 離開 B，其餘留在 B 裡的就是這裡要畫的；在這一列畫 `↓` 的欄
        // 也開著，只是換成箭頭。
        for_each_set_bit(open, |i| {
            debug_assert!(i < self.cell_count, "padding bit 不該被設起來");
            if i == c {
                return;
            }
            match ev.iter().find(|e| e.col as usize == i).map(|e| e.kind) {
                Some(EventKind::Converge) => {}
                Some(EventKind::ArrowDown) => buf.push(Edge::new(EdgeType::TruncDown, i, i)),
                _ => buf.push(Edge::new(EdgeType::Vertical, i, i)),
            }
        });
        for e in ev {
            let l = e.col as usize;
            match e.kind {
                EventKind::Converge => push_corner(buf, c, l, false, line_of(l)),
                EventKind::Continue => push_corner(buf, c, l, true, l),
                EventKind::ArrowDown => {}
                EventKind::ArrowUp { color } => {
                    buf.push(Edge::new(EdgeType::TruncUp, l, color as usize));
                }
            }
        }
        if self.down[y] {
            buf.push(Edge::new(EdgeType::Down, c, c));
        }

        buf.sort_by_key(|e| (e.associated_line_pos_x, e.pos_x, e.edge_type));
        // 唯一會重複的情況：`↑` 借用了別欄的顏色，而那一欄的 lane 也在
        // 這一列收斂，兩個轉角在共用的那一段產生完全相同的 edge（畫出來
        // 也一樣）。其他重複都是引擎的 bug。
        debug_assert!(
            buf.windows(2).all(|w| w[0] != w[1])
                || prev
                    .iter()
                    .any(|e| matches!(e.kind, EventKind::ArrowUp { .. })),
            "lane 引擎不該對同一列產生重複 edge"
        );
        buf.dedup();
    }

    /// 從最近的 checkpoint 重播到 `rows.start`，把 `rows` 範圍內每一列排序
    /// 過的 edge 交給 `f`。範圍外那段只推進 `open`（`advance`），組 edge
    /// （`build_row`）只在 `rows` 內做。checkpoint 越密，這段推進越短，
    /// 記憶體越大，`CHECKPOINT_INTERVAL` 就是這個取捨。
    pub(super) fn row_edges_in(&self, rows: Range<usize>, mut f: impl FnMut(usize, &[Edge])) {
        if rows.start >= rows.end || self.words == 0 {
            return;
        }
        let ck_idx = rows.start / CHECKPOINT_INTERVAL;
        let ck_row = ck_idx * CHECKPOINT_INTERVAL;
        let mut open = self.checkpoints[ck_idx * self.words..(ck_idx + 1) * self.words].to_vec();

        for y in ck_row..rows.start {
            self.advance(&mut open, y);
        }

        let mut buf = Vec::new();
        for y in rows {
            self.build_row(&open, y, &mut buf);
            f(y, &buf);
            self.advance(&mut open, y);
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
/// Open／Join（`Top`）。assoc 是 `line`——這條線屬於 lane `l`，不是 commit
/// 自己那欄；只有剛從 `↑` 接回來的 lane 會沿用 `↑` 的顏色，此外 `line == l`。
/// `c == l` 不該發生：`l` 一定是「別的」lane，不會恰好是 commit 自己正在坐
/// 的那欄。
fn push_corner(buf: &mut Vec<Edge>, c: usize, l: usize, continues: bool, line: usize) {
    debug_assert_ne!(c, l, "commit 自己的欄不會同時是收斂或開新的目標");
    let (lo, hi) = if c < l { (c, l) } else { (l, c) };
    for x in (lo + 1)..hi {
        buf.push(Edge::new(EdgeType::Horizontal, x, line));
    }
    let (near, far) = match (c < l, continues) {
        (true, true) => (EdgeType::Right, EdgeType::RightTop),
        (true, false) => (EdgeType::Right, EdgeType::RightBottom),
        (false, true) => (EdgeType::Left, EdgeType::LeftTop),
        (false, false) => (EdgeType::Left, EdgeType::LeftBottom),
    };
    buf.push(Edge::new(near, c, line));
    buf.push(Edge::new(far, l, line));
}

/// 墓碑：`col` 這一欄在 `until` 列之前不能開新 lane（rule 5）。跟
/// `lane_open` 分開存：墓碑期間這一欄已經沒有線，checkpoint 快照若把它
/// 當成開著，從快照重播的查詢會在墓碑那一列多畫一條 `Vertical`。
#[derive(Debug, Clone, Copy)]
struct Tomb {
    col: u32,
    until: u32,
}

/// `col` 現在能不能開新 lane：沒開著、不在墓碑期、也沒被 `ban` 擋掉。
fn vacant(lane_open: &[bool], tombs: &[Tomb], col: u32, ban: impl Fn(u32) -> bool) -> bool {
    let i = col as usize;
    (i >= lane_open.len() || !lane_open[i]) && !tombs.iter().any(|t| t.col == col) && !ban(col)
}

/// 從 `start` 開始找第一個 [`vacant`] 的欄；找不到就開在最後面。`ban` 只擋
/// 有限個欄，墓碑數也有限，所以 `find` 保證會停。
fn find_vacant(lane_open: &[bool], tombs: &[Tomb], start: usize, ban: impl Fn(u32) -> bool) -> u32 {
    (start as u32..)
        .find(|&col| vacant(lane_open, tombs, col, &ban))
        .expect("ban 與墓碑都只擋有限個欄，總有一欄滿足條件")
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

/// [`build`] 的選項。
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct BuildOpts {
    /// HEAD 所在的列。長線截斷用它保護 HEAD 的 first-parent 鏈，跟
    /// `reserve_head` 無關。
    pub head: Option<u32>,
    /// 為 HEAD 保留 col 0：HEAD 落地之前 col 0 對任何 lane 都是禁區。
    pub reserve_head: bool,
    /// 長線截斷的 K：線長超過 K 列就截斷。`None` 不截斷。
    pub max_edge_rows: Option<u32>,
}

/// K 的下限。K=1 時 `↓` 跟 `↑` 會落在同一列；K=2 時長度 3 的線，`↑` 剛好
/// 落在 `↓` 留下的墓碑那一列，永遠開不回原本那一欄。
pub(super) const MIN_EDGE_ROWS: u32 = 3;

/// 等著在 parent 上方一列開 `↑` 的截斷線（同一個 parent 共用一筆）。
#[derive(Debug, Clone, Copy)]
struct Stub {
    /// `↓` 那一欄：`↑` 優先開回這裡，顏色也跟著它。有 first-parent 線就用
    /// 它的欄，否則用最左邊那條。
    col: u32,
    /// 其中有沒有 first-parent 線（rule 1 的優先權照樣適用）。
    fp: bool,
}

impl Stub {
    fn absorb(&mut self, col: u32, fp: bool) {
        if (!fp, col) < (!self.fp, self.col) {
            self.col = col;
        }
        self.fp |= fp;
    }
}

/// 登記一條要截斷、往 `parent` 的線（`↓` 在 `col`）；同一個 parent 的線
/// 合併成一筆。
fn add_stub(stubs: &mut rustc_hash::FxHashMap<u32, Stub>, parent: u32, col: u32, fp: bool) {
    stubs
        .entry(parent)
        .and_modify(|s| s.absorb(col, fp))
        .or_insert(Stub { col, fp });
}

/// HEAD 的 first-parent 鏈（bitset，每列一個 bit）。鏈上 commit 往 first
/// parent 的線永遠不截斷，主線不會換欄。
fn main_line(parent_start: &[u32], parent_idx: &[u32], head: u32) -> Vec<u64> {
    let n = parent_start.len() - 1;
    let mut bits = vec![0u64; n.div_ceil(64)];
    let mut cur = head;
    loop {
        set_bit(&mut bits, cur as usize);
        let range = parent_start[cur as usize] as usize..parent_start[cur as usize + 1] as usize;
        match parent_idx[range].first() {
            Some(&fp) if fp != NOT_LOADED => cur = fp,
            _ => break,
        }
    }
    bits
}

/// 建構 lane 引擎的結果。`parent_start`/`parent_idx` 是 row 空間的 parent
/// CSR（跟 `Repository` 的 parent CSR 同格式：`parent_idx[parent_start[y]
/// ..parent_start[y + 1]]` 是第 y 列 commit 的 parent row，順序同
/// `parent_commit_hashes`；沒載入或不可見一律填 [`NOT_LOADED`]）。
///
/// ## 長線截斷（`opts.max_edge_rows`，issue #119）
///
/// 線長（child 到 parent 相隔的列數；parent 沒載入時算到最後一列之後）
/// 超過 K 的線，在開線那一刻就決定截斷：
///
/// - child 下方一列畫 `↓`，lane 結束，再下一列留墓碑（同 rule 5）
/// - parent 上方一列開一條短 lane 畫 `↑`，在 parent 那一列接手或收斂；
///   parent 沒載入就只有 `↓`
/// - HEAD first-parent 鏈上的 first-parent 線、merge join 既有的 lane
///   都不截斷（後者在 lane 開的時候已經判過）
pub(super) fn build(parent_start: &[u32], parent_idx: &[u32], opts: &BuildOpts) -> Lanes {
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
            truncated: opts.max_edge_rows.is_some(),
        };
    }

    let reserved_head = opts.head.filter(|_| opts.reserve_head);
    let k = opts.max_edge_rows.map(|k| k.max(MIN_EDGE_ROWS));
    let main = match (k, opts.head) {
        (Some(_), Some(head)) => main_line(parent_start, parent_idx, head),
        _ => Vec::new(),
    };
    let nu = n as u32;
    // 從第 `c` 列往 `p` 的線要不要截斷。`p`／`nu` 理論上一定 >= `c`（這個
    // 順序不變量在 `git.rs` 載入階段建立，這裡沒有本地檢查）；用飽和減法
    // 擋萬一順序被破壞時的 `u32` 溢位，寧可少截斷一條線，也不要溢位成一
    // 個超大值誤判整條線都要截斷。
    let cut = |c: u32, p: u32| {
        let end = if p == NOT_LOADED { nu } else { p };
        k.is_some_and(|k| end.saturating_sub(c) > k)
    };

    // lane 狀態：`lane_open[col]` 開著時，`is_fp[col]` 分辨它是不是 commit
    // 自己 first-parent 的延伸（rule 1 的優先權），`wait_next[col]` 是它
    // 掛在 `wait_head[target_row]` 那條鏈結串列裡的下一個節點。
    let mut lane_open: Vec<bool> = Vec::new();
    let mut is_fp: Vec<bool> = Vec::new();
    let mut wait_next: Vec<u32> = Vec::new();
    let mut wait_head: Vec<u32> = vec![NONE; n];
    // root 與 `↓` 的墓碑，滿一列後釋放（rule 5）。
    let mut tombs: Vec<Tomb> = Vec::new();
    // 這一列開的線裡被截斷的欄，下一列畫 `↓`。
    let mut cut_cols: Vec<u32> = Vec::new();
    let mut arrow_down: Vec<u32> = Vec::new();
    // parent 的列 → 等著在它上方一列開 `↑` 的 stub。
    let mut stubs: rustc_hash::FxHashMap<u32, Stub> = Default::default();

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

        tombs.retain(|t| yu < t.until);

        let ev_from = events.len();
        // 上一列截斷的線在這一列畫 `↓` 後關閉，再下一列留墓碑。先處理，
        // 這一列之後的選欄就不會碰到它。
        std::mem::swap(&mut arrow_down, &mut cut_cols);
        cut_cols.clear();
        for &l in &arrow_down {
            lane_open[l as usize] = false;
            tombs.push(Tomb {
                col: l,
                until: yu + 2,
            });
            events.push(RowEvent {
                col: l,
                kind: EventKind::ArrowDown,
            });
        }

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
        let x = if ish {
            0
        } else if let Some(&first) = waiters.first() {
            first
        } else {
            find_vacant(&lane_open, &tombs, start_col, |_| false)
        };
        // 在等我、卻沒被接手的 lane 收斂。HEAD 那一列 col 0 上可能有在等它
        // 的 `↑`，那條就是 HEAD 接手的 lane，所以用「扣掉 x」而不是「扣掉
        // 第一個」。
        let conv: Vec<u32> = waiters.iter().copied().filter(|&l| l != x).collect();

        for &l in &conv {
            lane_open[l as usize] = false;
            events.push(RowEvent {
                col: l,
                kind: EventKind::Converge,
            });
        }

        let parents = &parent_idx[parent_start[y] as usize..parent_start[y + 1] as usize];
        let xu = x as usize;
        grow_lanes(&mut lane_open, &mut is_fp, &mut wait_next, xu + 1);
        lane_open[xu] = true;

        let mut seen: Vec<u32> = Vec::new();
        let rest = match parents.split_first() {
            None => {
                // rule 5：root 的線在這一列結束，下一列留墓碑。
                lane_open[xu] = false;
                tombs.push(Tomb {
                    col: x,
                    until: yu + 2,
                });
                &[][..]
            }
            Some((&fp_target, rest)) => {
                is_fp[xu] = true;
                wait_next[xu] = NONE;
                let on_main = !main.is_empty() && is_open(&main, y);
                if !on_main && cut(yu, fp_target) {
                    cut_cols.push(x);
                    if fp_target != NOT_LOADED {
                        add_stub(&mut stubs, fp_target, x, true);
                    }
                } else if fp_target != NOT_LOADED {
                    wait_next[xu] = wait_head[fp_target as usize];
                    wait_head[fp_target as usize] = x;
                }
                // 沒截斷、parent 又沒載入：不掛進任何串列，lane 一路開到底
                // （rule 3）。
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
                let new_l = find_vacant(&lane_open, &tombs, start_col, |c| conv.contains(&c));
                let nl = new_l as usize;
                grow_lanes(&mut lane_open, &mut is_fp, &mut wait_next, nl + 1);
                lane_open[nl] = true;
                is_fp[nl] = false;
                if cut(yu, p) {
                    wait_next[nl] = NONE;
                    cut_cols.push(new_l);
                    add_stub(&mut stubs, p, new_l, false);
                } else {
                    wait_next[nl] = wait_head[p as usize];
                    wait_head[p as usize] = new_l;
                }
                new_l
            };
            events.push(RowEvent {
                col: l,
                kind: EventKind::Continue,
            });
        }

        // 下一列的 parent 有截斷線在等：這一列開 `↑`。放在 merge 之後，
        // merge 就不會 join 到 stub（`↑` 跟 `╮` 疊在同一格）。
        if let Some(stub) = stubs.remove(&(yu + 1)) {
            let s = if reserved_head == Some(yu + 1) {
                // pend 期間 col 0 只有 HEAD 的線能用，一定空著。
                debug_assert!(pend && vacant(&lane_open, &tombs, 0, |_| false));
                0
            } else {
                // 避開本列轉角橫跨的範圍：`↑` 壓在 `─` 上會把橫線截斷。
                let (lo, hi) = events[ev_from..]
                    .iter()
                    .filter(|e| matches!(e.kind, EventKind::Converge | EventKind::Continue))
                    .fold((x, x), |(lo, hi), e| (lo.min(e.col), hi.max(e.col)));
                let spans = lo != hi;
                let ban = |c: u32| conv.contains(&c) || (spans && (lo..=hi).contains(&c));
                if stub.col as usize >= start_col && vacant(&lane_open, &tombs, stub.col, ban) {
                    stub.col
                } else {
                    find_vacant(&lane_open, &tombs, start_col, ban)
                }
            };
            let su = s as usize;
            grow_lanes(&mut lane_open, &mut is_fp, &mut wait_next, su + 1);
            lane_open[su] = true;
            is_fp[su] = stub.fp;
            wait_next[su] = wait_head[y + 1];
            wait_head[y + 1] = s;
            events.push(RowEvent {
                col: s,
                kind: EventKind::ArrowUp { color: stub.col },
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
        truncated: k.is_some(),
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

    impl BuildOpts {
        /// 舊的 `reserved_head: Some(row)`：HEAD 在 `row` 且保留 col 0。
        fn reserved(row: u32) -> BuildOpts {
            BuildOpts {
                head: Some(row),
                reserve_head: true,
                max_edge_rows: None,
            }
        }
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
        let lanes = build(&start, &idx, &BuildOpts::default());
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
        let lanes = build(&start, &idx, &BuildOpts::default());
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
        let lanes = build(&start, &idx, &BuildOpts::default());

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
        let lanes = build(&start, &idx, &BuildOpts::reserved(1));

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
        let lanes = build(&start, &idx, &BuildOpts::reserved(2));
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
        let lanes = build(&start, &idx, &BuildOpts::reserved(0));
        assert_eq!(lanes.col(0), 0);
        assert_eq!(lanes.col(1), 0);
    }

    // --- 規則 6：沒載入的 parent ---

    #[test]
    fn unloaded_first_parent_keeps_lane_open_forever() {
        let (start, idx) = csr(&[&[NOT_LOADED]]);
        let lanes = build(&start, &idx, &BuildOpts::default());
        assert_eq!(lanes.col(0), 0);
        assert!(has(&edges_for(&lanes, 0), EdgeType::Down, 0));
    }

    #[test]
    fn unloaded_non_first_parent_is_skipped() {
        // stash：parents=[base(=1), index(未載入), untracked(未載入)]
        let (start, idx) = csr(&[&[1, NOT_LOADED, NOT_LOADED], &[]]);
        let lanes = build(&start, &idx, &BuildOpts::default());
        let e0 = edges_for(&lanes, 0);
        assert_eq!(e0.len(), 1, "沒載入的非 first parent 不產生任何 edge");
        assert!(has(&e0, EdgeType::Down, 0));
    }

    // --- 規則 7：root 墓碑 ---

    #[test]
    fn root_blocks_next_row_then_releases() {
        // 0: root。1、2 都沒有 parent，若 0 的欄立刻釋放，1 會誤用它。
        let (start, idx) = csr(&[&[], &[], &[]]);
        let lanes = build(&start, &idx, &BuildOpts::default());
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
        let lanes = build(&start, &idx, &BuildOpts::default());

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

    /// root 的墓碑欄在 build 裡還佔著（不能開新 lane），重播時卻已經沒有
    /// 線了。checkpoint 若把墓碑也存成「開著」，從它開始重播的查詢會在
    /// 墓碑那一列多畫一條 Vertical，跟從頭批次重播的結果對不上。
    #[test]
    fn tomb_is_not_captured_as_open_in_checkpoint() {
        // 0..=127 一條直線、127 是 root；128.. 另一條直線。第 1 份
        // checkpoint 是第 128 列開始前的狀態，正好落在 127 的墓碑期。
        let n = CHECKPOINT_INTERVAL * 2;
        let parents: Vec<Vec<u32>> = (0..n)
            .map(|i| {
                if i + 1 == CHECKPOINT_INTERVAL || i + 1 == n {
                    vec![]
                } else {
                    vec![(i + 1) as u32]
                }
            })
            .collect();
        let refs: Vec<&[u32]> = parents.iter().map(|v| v.as_slice()).collect();
        let (start, idx) = csr(&refs);
        let lanes = build(&start, &idx, &BuildOpts::default());

        let mut batch = vec![Vec::new(); n];
        lanes.row_edges_in(0..n, |y, es| batch[y] = es.to_vec());
        for row in [CHECKPOINT_INTERVAL, CHECKPOINT_INTERVAL + 1] {
            assert_eq!(
                edges_for(&lanes, row),
                batch[row],
                "row {row}：從 checkpoint 重播要跟從頭重播一致"
            );
        }
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
        let lanes = build(&start, &idx, &BuildOpts::reserved(1));
        assert_eq!(lanes.col(0), 1, "child 是 leaf，pend 期間落在 col 1");
        assert_eq!(lanes.col(1), 0, "HEAD 固定 col 0，不再跟著 child 的欄");
    }

    // --- 長線截斷（issue #119） ---

    fn cut_at(k: u32) -> BuildOpts {
        BuildOpts {
            max_edge_rows: Some(k),
            ..BuildOpts::default()
        }
    }

    fn find(edges: &[Edge], et: EdgeType) -> Vec<(usize, usize)> {
        edges
            .iter()
            .filter(|e| e.edge_type == et)
            .map(|e| (e.pos_x, e.associated_line_pos_x))
            .collect()
    }

    /// 0 的 first parent 是 5（線長 5），1..=4 是另一條直線接到 5。
    fn long_fp_line() -> (Vec<u32>, Vec<u32>) {
        csr(&[&[5], &[2], &[3], &[4], &[5], &[]])
    }

    #[test]
    fn long_line_is_cut_into_down_and_up_arrows() {
        let (start, idx) = long_fp_line();
        let lanes = build(&start, &idx, &cut_at(3));
        assert!(lanes.truncated());

        assert_eq!(find(&edges_for(&lanes, 1), EdgeType::TruncDown), [(0, 0)]);
        assert_eq!(lanes.col(1), 1, "↓ 那一列 col 0 還佔著，不能開新 lane");
        assert!(
            edges_for(&lanes, 2).iter().all(|e| e.pos_x != 0),
            "↓ 下一列是墓碑，col 0 什麼都不畫"
        );
        assert_eq!(
            find(&edges_for(&lanes, 4), EdgeType::TruncUp),
            [(0, 0)],
            "↑ 開回原本那一欄"
        );
        assert_eq!(lanes.col(5), 0, "↑ 是 first-parent 線，parent 接手它");
        assert!(has(&edges_for(&lanes, 5), EdgeType::Up, 0));
        assert!(has(&edges_for(&lanes, 5), EdgeType::RightBottom, 1));
    }

    #[test]
    fn line_of_exactly_k_rows_is_not_cut() {
        let (start, idx) = long_fp_line();
        let lanes = build(&start, &idx, &cut_at(5));
        assert!(has(&edges_for(&lanes, 1), EdgeType::Vertical, 0));
        let arrows = (0..lanes.row_count())
            .flat_map(|y| edges_for(&lanes, y))
            .filter(|e| matches!(e.edge_type, EdgeType::TruncDown | EdgeType::TruncUp))
            .count();
        assert_eq!(arrows, 0);
    }

    #[test]
    fn head_first_parent_chain_is_never_cut() {
        let (start, idx) = long_fp_line();
        let opts = BuildOpts {
            head: Some(0),
            ..cut_at(3)
        };
        let lanes = build(&start, &idx, &opts);
        assert!(has(&edges_for(&lanes, 1), EdgeType::Vertical, 0));
        assert!(find(&edges_for(&lanes, 1), EdgeType::TruncDown).is_empty());
    }

    #[test]
    fn unloaded_parent_gets_down_arrow_only() {
        let (start, idx) = csr(&[&[NOT_LOADED], &[2], &[3], &[4], &[5], &[]]);
        let lanes = build(&start, &idx, &cut_at(3));
        assert_eq!(find(&edges_for(&lanes, 1), EdgeType::TruncDown), [(0, 0)]);
        for y in 0..lanes.row_count() {
            assert!(find(&edges_for(&lanes, y), EdgeType::TruncUp).is_empty());
        }
    }

    /// 同一個 parent 的兩條截斷線（fp 在左、merge 在右）共用一個 `↑`：
    /// 開在 fp 線那一欄、用它的顏色，而且算 first-parent 線。
    #[test]
    fn cut_lines_to_the_same_parent_share_one_up_arrow() {
        let (start, idx) = csr(&[&[6], &[2, 6], &[3], &[4], &[5], &[6], &[]]);
        let lanes = build(&start, &idx, &cut_at(3));
        assert_eq!(find(&edges_for(&lanes, 1), EdgeType::TruncDown), [(0, 0)]);
        assert_eq!(find(&edges_for(&lanes, 2), EdgeType::TruncDown), [(2, 2)]);
        assert_eq!(find(&edges_for(&lanes, 5), EdgeType::TruncUp), [(0, 0)]);
        assert_eq!(lanes.col(6), 0, "↑ 帶 fp，parent 接手它而不是 lane 1");
    }

    /// `↑` 開不回原本那一欄時，換欄但顏色跟著 `↓`，下一列收斂的轉角也是。
    #[test]
    fn up_arrow_keeps_the_colour_when_its_column_is_taken() {
        let (start, idx) = csr(&[&[5], &[2], &[], &[4], &[5], &[]]);
        let lanes = build(&start, &idx, &cut_at(3));
        assert_eq!(lanes.col(3), 0, "墓碑釋放後 col 0 被別的 commit 拿走");
        assert_eq!(find(&edges_for(&lanes, 4), EdgeType::TruncUp), [(1, 0)]);
        assert_eq!(
            find(&edges_for(&lanes, 5), EdgeType::RightBottom),
            [(1, 0)],
            "接回 parent 的那一段沿用 ↑ 的顏色"
        );
    }

    /// `↑` 不開在本列轉角橫跨的範圍裡，否則會把橫線截斷。
    #[test]
    fn up_arrow_avoids_the_span_of_corners_in_its_row() {
        // B（row 1）→ 7 被截斷，↓ 在 col 1。D（row 3）坐 col 2，在 row 6
        // 收斂進 col 0，轉角橫跨 col 0..=2，col 1 雖然空著也不能用。
        let (start, idx) = csr(&[&[2], &[7], &[4], &[6], &[5], &[6], &[7], &[]]);
        let lanes = build(&start, &idx, &cut_at(3));
        assert_eq!(lanes.col(3), 2);
        let e6 = edges_for(&lanes, 6);
        assert!(has(&e6, EdgeType::RightBottom, 2));
        assert_eq!(find(&e6, EdgeType::TruncUp), [(3, 1)]);
    }

    /// reserve 時，在等 HEAD 的 `↑` 開在 col 0，HEAD 那一列直接接手，不會
    /// 把它當成收斂。
    #[test]
    fn up_arrow_to_reserved_head_opens_in_col0() {
        let (start, idx) = long_fp_line();
        let opts = BuildOpts {
            head: Some(5),
            reserve_head: true,
            max_edge_rows: Some(3),
        };
        let lanes = build(&start, &idx, &opts);
        assert_eq!(lanes.col(0), 1);
        assert_eq!(find(&edges_for(&lanes, 1), EdgeType::TruncDown), [(1, 1)]);
        assert_eq!(find(&edges_for(&lanes, 4), EdgeType::TruncUp), [(0, 1)]);
        let e5 = edges_for(&lanes, 5);
        assert_eq!(lanes.col(5), 0);
        assert_eq!(find(&e5, EdgeType::Up), [(0, 1)]);
        assert!(!has(&e5, EdgeType::LeftBottom, 0) && !has(&e5, EdgeType::RightBottom, 0));
    }

    /// 截斷後逐列查詢要跟批次重播一致：`↓`、墓碑、`↑` 都可能落在 checkpoint
    /// 邊界上，merge 密集的合成 DAG 會把各種位置都踩過一遍。
    #[test]
    fn truncated_rows_match_between_single_and_batch_replay() {
        for seed in [7, 8, 9] {
            let n = CHECKPOINT_INTERVAL * 8;
            let (parent_start, parent_idx) = gen_merge_dense(n, seed, 35);
            let lanes = build(&parent_start, &parent_idx, &cut_at(3));
            let mut batch = vec![Vec::new(); n];
            lanes.row_edges_in(0..n, |y, es| batch[y] = es.to_vec());
            for (y, expected) in batch.iter().enumerate() {
                assert_eq!(&edges_for(&lanes, y), expected, "seed={seed} row={y}");
            }
            let ups = batch
                .iter()
                .flatten()
                .filter(|e| e.edge_type == EdgeType::TruncUp)
                .count();
            assert!(ups > 0, "seed={seed}：這組參數應該真的有線被截斷");
        }
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
            let lanes = build(&parent_start, &parent_idx, &BuildOpts::default());

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
        let lanes = build(&parent_start, &parent_idx, &BuildOpts::default());

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

//! 精簡模式（#215）：窄螢幕只看 branch 的順序、重疊與關聯，所以把單親單子的
//! 中間 commit 藏起來，只留有 ref／HEAD 的 commit 與分岔、匯合處。

use crate::{git::Repository, CommitMask};

use super::lanes::NOT_LOADED;

/// 精簡模式的計算結果，跟 `Repository` 同壽命、與終端寬度無關（寬度只決定
/// 要不要套用），所以算一次就能反覆拿來換 graph。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Condensed {
    /// 要藏的 commit，以 raw index 為索引。
    pub mask: CommitMask,
    /// 每個保留的 commit 底下（沿 first parent 往舊的方向）到下一個保留的
    /// commit 之間，被藏掉幾顆；被藏的 commit 自己是 0。以 raw index 為索引。
    hidden_below: Vec<u32>,
}

impl Condensed {
    pub fn hidden_below(&self, raw: usize) -> u32 {
        self.hidden_below.get(raw).copied().unwrap_or(0)
    }
}

/// 沿著唯一的 parent 走到第一個沒被藏的 commit，回傳 `(那個 commit, 跳過了
/// 幾顆)`。`raw` 本身沒被藏就是 `(raw, 0)`。
///
/// 被藏的 commit 一定恰有一個已載入的 parent（見 `condense_csr` 的規則 3），
/// 而且恰有一個 child，所以每顆被藏的 commit 只會被走過一次，整張圖的總成本
/// 是 O(n)。
pub(crate) fn first_kept(
    parent_start: &[u32],
    parent_idx: &[u32],
    hidden: &CommitMask,
    mut raw: u32,
) -> (u32, u32) {
    let mut skipped = 0;
    while hidden.contains(raw as usize) {
        let start = parent_start[raw as usize] as usize;
        debug_assert_eq!(
            parent_start[raw as usize + 1] as usize - start,
            1,
            "被藏的 commit 一定恰有一個 parent"
        );
        raw = parent_idx[start];
        debug_assert_ne!(raw, NOT_LOADED, "被藏的 commit 的 parent 一定已載入");
        skipped += 1;
    }
    (raw, skipped)
}

/// 從 repository 算出精簡結果。`head_raw` 是 HEAD 指向的 commit（HEAD 一定保留）。
pub fn condense(repository: &Repository, head_raw: Option<usize>) -> Condensed {
    let mut protected = vec![false; repository.all_commits().len()];
    for (hash, refs) in repository.refs_with_commits() {
        if let Some(raw) = (!refs.is_empty())
            .then(|| repository.index_of(hash))
            .flatten()
        {
            protected[raw] = true;
        }
    }
    if let Some(raw) = head_raw {
        protected[raw] = true;
    }
    let (parent_start, parent_idx) = repository.parent_csr();
    condense_csr(parent_start, parent_idx, &protected)
}

/// 純粹吃 parent CSR（raw 空間）的版本。`protected[raw]` 為 true 代表該
/// commit 有 ref 或是 HEAD，一定保留。
///
/// commit 被藏要同時符合：
///
/// 1. 不受保護（沒有任何 ref——remote branch、tag、stash 都算——也不是 HEAD）
/// 2. 恰有 1 個 parent
/// 3. 那個 parent 已載入（`NOT_LOADED` 邊界、root 都保留）
/// 4. 恰有 1 個 child
/// 5. 它是那個 child 的 first parent
///
/// 第 5 條讓被 merge 進來的那一條線保留一顆，merge 不會被畫成直線；也讓被藏
/// 的 commit 全都落在 first-parent 那條線上，每個保留的節點只需要一個數字。
pub(crate) fn condense_csr(
    parent_start: &[u32],
    parent_idx: &[u32],
    protected: &[bool],
) -> Condensed {
    let n = protected.len();
    let parents_of =
        |raw: usize| &parent_idx[parent_start[raw] as usize..parent_start[raw + 1] as usize];

    let mut child_count = vec![0u32; n];
    let mut is_first_parent = vec![false; n];
    for raw in 0..n {
        for (i, &p) in parents_of(raw).iter().enumerate() {
            if p != NOT_LOADED {
                child_count[p as usize] += 1;
                is_first_parent[p as usize] |= i == 0;
            }
        }
    }

    let mask = CommitMask::from_bits(
        (0..n)
            .map(|raw| {
                !protected[raw]
                    && matches!(parents_of(raw), [p] if *p != NOT_LOADED)
                    && child_count[raw] == 1
                    && is_first_parent[raw]
            })
            .collect(),
    );

    let hidden_below = (0..n)
        .map(|raw| match parents_of(raw) {
            [first, ..] if *first != NOT_LOADED && !mask.contains(raw) => {
                first_kept(parent_start, parent_idx, &mask, *first).1
            }
            _ => 0,
        })
        .collect();

    Condensed { mask, hidden_below }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用每個 commit 的 parent 清單建 CSR；`NL` 代表沒載入的 parent。
    const NL: u32 = NOT_LOADED;

    fn csr(parents: &[&[u32]]) -> (Vec<u32>, Vec<u32>) {
        let mut start = vec![0];
        let mut idx = Vec::new();
        for ps in parents {
            idx.extend_from_slice(ps);
            start.push(idx.len() as u32);
        }
        (start, idx)
    }

    fn run(parents: &[&[u32]], protected: &[usize]) -> Condensed {
        let (start, idx) = csr(parents);
        let mut flags = vec![false; parents.len()];
        for &raw in protected {
            flags[raw] = true;
        }
        condense_csr(&start, &idx, &flags)
    }

    fn hidden(c: &Condensed) -> Vec<usize> {
        c.mask.iter().collect()
    }

    fn counts(c: &Condensed, n: usize) -> Vec<u32> {
        (0..n).map(|raw| c.hidden_below(raw)).collect()
    }

    #[test]
    fn linear_chain_keeps_only_protected_ends() {
        // 0(HEAD) ← 1 ← 2 ← 3(tag)
        let c = run(&[&[1], &[2], &[3], &[]], &[0, 3]);
        assert_eq!(hidden(&c), vec![1, 2]);
        assert_eq!(counts(&c, 4), vec![2, 0, 0, 0]);
    }

    #[test]
    fn fork_point_without_ref_is_kept() {
        // 0 a1(main,HEAD) 1 a2 | 2 b1(feature) 3 b2 | 4 c1(fork) 5 c2 6 c3(tag)
        let c = run(&[&[1], &[4], &[3], &[4], &[5], &[6], &[]], &[0, 2, 6]);
        assert_eq!(hidden(&c), vec![1, 3, 5]);
        assert_eq!(counts(&c, 7), vec![1, 0, 1, 0, 1, 0, 0]);
    }

    #[test]
    fn merged_in_line_keeps_its_tip() {
        // 0 M(HEAD)[m1, s2]  1 m1[f0]  2 s2[s1]  3 s1[f0]  4 f0
        let c = run(&[&[1, 2], &[4], &[3], &[4], &[]], &[0]);
        // s2 是 M 的第二個 parent，不是 first parent，所以保留
        assert_eq!(hidden(&c), vec![1, 3]);
        assert_eq!(counts(&c, 5), vec![1, 0, 1, 0, 0]);
    }

    #[test]
    fn not_loaded_parent_and_root_are_kept() {
        // 1 的唯一 parent 沒載入；2 是 root
        let c = run(&[&[1], &[NL], &[]], &[0]);
        assert!(hidden(&c).is_empty());
        assert_eq!(counts(&c, 3), vec![0, 0, 0]);
    }

    #[test]
    fn ref_or_head_on_middle_commit_stops_hiding() {
        let c = run(&[&[1], &[2], &[3], &[]], &[0, 1, 3]);
        assert_eq!(hidden(&c), vec![2]);
        assert_eq!(counts(&c, 4), vec![0, 1, 0, 0]);
    }

    #[test]
    fn commit_with_two_children_is_kept() {
        // 0、1 都以 2 為 first parent
        let c = run(&[&[2], &[2], &[3], &[]], &[0, 1, 3]);
        assert!(hidden(&c).is_empty());
    }

    #[test]
    fn first_kept_walks_to_first_visible_ancestor() {
        let (start, idx) = csr(&[&[1], &[2], &[3], &[]]);
        let mask = CommitMask::from_bits(vec![false, true, true, false]);
        assert_eq!(first_kept(&start, &idx, &mask, 1), (3, 2));
        assert_eq!(first_kept(&start, &idx, &mask, 3), (3, 0));
        assert_eq!(first_kept(&start, &idx, &mask, 0), (0, 0));
    }

    #[test]
    fn degenerate_merge_of_own_ancestor_hides_in_between_commit() {
        // M[P, S]，P[S]：P 被藏之後 M 的兩個 parent 都指向 S
        let c = run(&[&[1, 2], &[2], &[]], &[0]);
        assert_eq!(hidden(&c), vec![1]);
        assert_eq!(counts(&c, 3), vec![1, 0, 0]);
    }
}

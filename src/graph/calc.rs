use rustc_hash::{FxHashMap, FxHashSet};

use crate::git::{Commit, CommitHash, Repository};

type CommitPosMap = FxHashMap<CommitHash, (usize, usize)>;

pub trait GraphDataSource {
    fn children_hash(&self, hash: &CommitHash) -> Vec<&CommitHash>;
    fn parents_hash(&self, hash: &CommitHash) -> Vec<&CommitHash>;
}

impl GraphDataSource for Repository {
    fn children_hash(&self, hash: &CommitHash) -> Vec<&CommitHash> {
        self.children_hash(hash)
    }

    fn parents_hash(&self, hash: &CommitHash) -> Vec<&CommitHash> {
        self.parents_hash(hash)
    }
}

#[derive(Debug)]
pub struct Graph {
    pub commit_hashes: Vec<CommitHash>,
    pub commit_pos_map: CommitPosMap,
    pub edges: Vec<Vec<Edge>>,
    pub max_pos_x: usize,
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
}

impl EdgeType {
    pub fn is_vertically_related(&self) -> bool {
        matches!(self, EdgeType::Vertical | EdgeType::Up | EdgeType::Down)
    }

    pub fn has_downward_continuation(&self) -> bool {
        matches!(
            self,
            EdgeType::Vertical | EdgeType::Down | EdgeType::RightTop | EdgeType::LeftTop
        )
    }
}

pub fn calc_graph(repository: &Repository, head_hint: Option<&CommitHash>) -> Graph {
    let commits: Vec<&Commit> = repository.all_commits().iter().collect();

    let commit_pos_map = calc_commit_positions(&commits, repository, head_hint);
    let (mut graph_edges, max_pos_x) = calc_edges(&commit_pos_map, &commits, repository);

    normalize_head_row_invariant(&mut graph_edges, &commit_pos_map, head_hint);
    if !repository.working_changes().is_empty() {
        anchor_head_to_virtual_row(&mut graph_edges, &commit_pos_map, head_hint);
    }

    let commit_hashes = commits.iter().map(|c| c.commit_hash.clone()).collect();

    Graph {
        commit_hashes,
        commit_pos_map,
        edges: graph_edges,
        max_pos_x,
    }
}

/// HEAD row 在 head_pos_x 不能有 Vertical（會穿透空心圓 interior）。
/// Graph invariant：HEAD 是 commit endpoint，不是 pass-through。永遠呼叫。
fn normalize_head_row_invariant(
    edges: &mut [Vec<Edge>],
    commit_pos_map: &CommitPosMap,
    head_hint: Option<&CommitHash>,
) {
    let Some(head_hash) = head_hint else { return };
    let Some(&(head_pos_x, head_pos_y)) = commit_pos_map.get(head_hash) else {
        return;
    };
    edges[head_pos_y].retain(|e| !(e.pos_x == head_pos_x && e.edge_type == EdgeType::Vertical));

    debug_assert!(
        !edges[head_pos_y].iter().any(|e| e.pos_x == head_pos_x
            && matches!(e.edge_type, EdgeType::Vertical | EdgeType::Horizontal)),
        "HEAD row invariant: pos_x must not contain pass-through edges"
    );
}

/// 延伸 HEAD column 往上，讓 virtual uncommitted row 能視覺連下來。
/// 僅在 working tree 有變動（需要顯示 virtual row）時呼叫。
fn anchor_head_to_virtual_row(
    edges: &mut [Vec<Edge>],
    commit_pos_map: &CommitPosMap,
    head_hint: Option<&CommitHash>,
) {
    let Some(head_hash) = head_hint else { return };
    let Some(&(head_pos_x, head_pos_y)) = commit_pos_map.get(head_hash) else {
        return;
    };
    if head_pos_y == 0 {
        return;
    }

    for row in edges.iter_mut().take(head_pos_y) {
        if !row
            .iter()
            .any(|e| e.pos_x == head_pos_x && e.edge_type == EdgeType::Vertical)
        {
            row.push(Edge::new(EdgeType::Vertical, head_pos_x, head_pos_x));
        }
    }
    let head_row = &mut edges[head_pos_y];
    if !head_row
        .iter()
        .any(|e| e.pos_x == head_pos_x && e.edge_type == EdgeType::Up)
    {
        head_row.push(Edge::new(EdgeType::Up, head_pos_x, head_pos_x));
    }

    debug_assert!(
        head_row
            .iter()
            .any(|e| e.pos_x == head_pos_x && e.edge_type == EdgeType::Up),
        "anchor_head_to_virtual_row must leave Up endpoint on head_row"
    );
}

fn calc_commit_positions(
    commits: &[&Commit],
    source: &impl GraphDataSource,
    head_hint: Option<&CommitHash>,
) -> CommitPosMap {
    // Reserve pos_x = HEAD_RESERVED_COL for HEAD until HEAD is placed (keeps
    // uncommitted row + HEAD circle on the leftmost line).
    const HEAD_RESERVED_COL: usize = 0;

    let mut commit_pos_map: CommitPosMap = FxHashMap::default();
    let mut commit_line_state: Vec<Option<CommitHash>> = Vec::new();
    // Reverse index: hash → pos_x for O(1) lookup instead of linear scan
    let mut hash_to_pos: FxHashMap<CommitHash, usize> = FxHashMap::default();
    let mut head_pending = head_hint.is_some_and(|h| commits.iter().any(|c| c.commit_hash == *h));

    for (pos_y, commit) in commits.iter().enumerate() {
        let is_head = head_hint.is_some_and(|h| *h == commit.commit_hash);
        let filtered_children_hash = filtered_children_hash(commit, source);
        if filtered_children_hash.is_empty() {
            let pos_x = if is_head {
                HEAD_RESERVED_COL
            } else {
                let start = if head_pending {
                    HEAD_RESERVED_COL + 1
                } else {
                    0
                };
                get_first_vacant_line_from(&commit_line_state, start)
            };
            add_commit_line(commit, &mut commit_line_state, &mut hash_to_pos, pos_x);
            commit_pos_map.insert(commit.commit_hash.clone(), (pos_x, pos_y));
        } else {
            let pos_x = update_commit_line(
                commit,
                &mut commit_line_state,
                &mut hash_to_pos,
                &filtered_children_hash,
            );
            commit_pos_map.insert(commit.commit_hash.clone(), (pos_x, pos_y));
        }
        if is_head {
            head_pending = false;
        }
    }

    commit_pos_map
}

fn filtered_children_hash<'a>(
    commit: &Commit,
    source: &'a impl GraphDataSource,
) -> Vec<&'a CommitHash> {
    source
        .children_hash(&commit.commit_hash)
        .into_iter()
        .filter(|child_hash| {
            let child_parents_hash = source.parents_hash(child_hash);
            !child_parents_hash.is_empty() && *child_parents_hash[0] == commit.commit_hash
        })
        .collect()
}

fn get_first_vacant_line_from(commit_line_state: &[Option<CommitHash>], start: usize) -> usize {
    commit_line_state
        .iter()
        .enumerate()
        .skip(start)
        .find_map(|(i, c)| c.is_none().then_some(i))
        .unwrap_or_else(|| commit_line_state.len().max(start))
}

fn add_commit_line(
    commit: &Commit,
    commit_line_state: &mut Vec<Option<CommitHash>>,
    hash_to_pos: &mut FxHashMap<CommitHash, usize>,
    pos_x: usize,
) {
    if commit_line_state.len() < pos_x {
        commit_line_state.resize(pos_x, None);
    }
    if commit_line_state.len() == pos_x {
        commit_line_state.push(Some(commit.commit_hash.clone()));
    } else {
        commit_line_state[pos_x] = Some(commit.commit_hash.clone());
    }
    hash_to_pos.insert(commit.commit_hash.clone(), pos_x);
}

// TODO: column 分配不知 pending merge edge，同 col merge 靠 calc_edges detour 繞道。
// 根本修復：此處加 merge edge reservation 讓 column 分配避開被預訂的 column。
fn update_commit_line(
    commit: &Commit,
    commit_line_state: &mut [Option<CommitHash>],
    hash_to_pos: &mut FxHashMap<CommitHash, usize>,
    target_commit_hashes: &[&CommitHash],
) -> usize {
    if commit_line_state.is_empty() {
        return 0;
    }
    let mut min_pos_x = commit_line_state.len().saturating_sub(1);
    for target_hash in target_commit_hashes {
        if let Some(pos_x) = hash_to_pos.remove(*target_hash) {
            commit_line_state[pos_x] = None;
            if min_pos_x > pos_x {
                min_pos_x = pos_x;
            }
        }
    }
    commit_line_state[min_pos_x] = Some(commit.commit_hash.clone());
    hash_to_pos.insert(commit.commit_hash.clone(), min_pos_x);
    min_pos_x
}

#[derive(Debug, Clone)]
struct WrappedEdge<'a> {
    edge: Edge,
    edge_parent_hash: &'a CommitHash,
}

impl<'a> WrappedEdge<'a> {
    fn new(
        edge_type: EdgeType,
        pos_x: usize,
        line_pos_x: usize,
        edge_parent_hash: &'a CommitHash,
    ) -> Self {
        Self {
            edge: Edge::new(edge_type, pos_x, line_pos_x),
            edge_parent_hash,
        }
    }
}

/// 畫 Up/Vertical/Down 直線（parent 到 child 同 col 或 merge 同 col 無 overlap）。
fn draw_vertical_chain<'a>(
    edges: &mut [Vec<WrappedEdge<'a>>],
    col: usize,
    child_row: usize,
    parent_row: usize,
    hash: &'a CommitHash,
) {
    edges[parent_row].push(WrappedEdge::new(EdgeType::Up, col, col, hash));
    for y in ((child_row + 1)..parent_row).rev() {
        edges[y].push(WrappedEdge::new(EdgeType::Vertical, col, col, hash));
    }
    edges[child_row].push(WrappedEdge::new(EdgeType::Down, col, col, hash));
}

fn calc_edges(
    commit_pos_map: &CommitPosMap,
    commits: &[&Commit],
    source: &impl GraphDataSource,
) -> (Vec<Vec<Edge>>, usize) {
    let mut max_pos_x = 0;
    let mut edges: Vec<Vec<WrappedEdge>> = vec![vec![]; commits.len()];

    for commit in commits {
        let (pos_x, pos_y) = commit_pos_map[&commit.commit_hash];
        let hash = &commit.commit_hash;

        for child_hash in source.children_hash(hash) {
            let (child_pos_x, child_pos_y) = commit_pos_map[child_hash];

            debug_assert!(!commits[child_pos_y].parent_commit_hashes.is_empty());
            let child_first_parent_hash = &commits[child_pos_y].parent_commit_hashes[0];
            let is_first_parent = *child_first_parent_hash == *hash;

            match (pos_x == child_pos_x, is_first_parent) {
                (true, true) => {
                    // commit: first-parent 且同 col → 直線
                    draw_vertical_chain(&mut edges, pos_x, child_pos_y, pos_y, hash);
                }
                (_, false) => {
                    // merge: 交給第二 loop detour（忽略 col 相等與否）
                }
                (false, true) => {
                    // branch: first-parent 不同 col → 斜線
                    if pos_x < child_pos_x {
                        edges[pos_y].push(WrappedEdge::new(
                            EdgeType::Right,
                            pos_x,
                            child_pos_x,
                            hash,
                        ));
                        for x in (pos_x + 1)..child_pos_x {
                            edges[pos_y].push(WrappedEdge::new(
                                EdgeType::Horizontal,
                                x,
                                child_pos_x,
                                hash,
                            ));
                        }
                        edges[pos_y].push(WrappedEdge::new(
                            EdgeType::RightBottom,
                            child_pos_x,
                            child_pos_x,
                            hash,
                        ));
                    } else {
                        edges[pos_y].push(WrappedEdge::new(
                            EdgeType::Left,
                            pos_x,
                            child_pos_x,
                            hash,
                        ));
                        for x in (child_pos_x + 1)..pos_x {
                            edges[pos_y].push(WrappedEdge::new(
                                EdgeType::Horizontal,
                                x,
                                child_pos_x,
                                hash,
                            ));
                        }
                        edges[pos_y].push(WrappedEdge::new(
                            EdgeType::LeftBottom,
                            child_pos_x,
                            child_pos_x,
                            hash,
                        ));
                    }
                    for y in ((child_pos_y + 1)..pos_y).rev() {
                        edges[y].push(WrappedEdge::new(
                            EdgeType::Vertical,
                            child_pos_x,
                            child_pos_x,
                            hash,
                        ));
                    }
                    edges[child_pos_y].push(WrappedEdge::new(
                        EdgeType::Down,
                        child_pos_x,
                        child_pos_x,
                        hash,
                    ));
                }
            }
        }

        if max_pos_x < pos_x {
            max_pos_x = pos_x;
        }

        // draw down edge if has parent but parent not in the graph (when max_count is set)
        if !commit.parent_commit_hashes.is_empty()
            && !commit_pos_map.contains_key(&commit.parent_commit_hashes[0])
        {
            edges[pos_y].push(WrappedEdge::new(EdgeType::Down, pos_x, pos_x, hash));
            ((pos_y + 1)..commits.len()).for_each(|y| {
                edges[y].push(WrappedEdge::new(EdgeType::Vertical, pos_x, pos_x, hash));
            });
        }
    }

    for commit in commits {
        let (pos_x, pos_y) = commit_pos_map[&commit.commit_hash];
        let hash = &commit.commit_hash;

        for child_hash in source.children_hash(hash) {
            let (child_pos_x, child_pos_y) = commit_pos_map[child_hash];

            let child_first_parent_hash = &commits[child_pos_y].parent_commit_hashes[0];
            if *child_first_parent_hash == *hash {
                // commit or branch — 已由第一 loop 處理
            } else {
                // merge（同 col 或不同 col 統一處理）
                let mut overlap = false;
                let mut new_pos_x = pos_x;

                let mut skip_judge_overlap = true;
                for y in (child_pos_y + 1)..pos_y {
                    let processing_commit_pos_x =
                        commit_pos_map.get(&commits[y].commit_hash).unwrap().0;
                    if processing_commit_pos_x == new_pos_x {
                        skip_judge_overlap = false;
                        break;
                    }
                    if edges[y]
                        .iter()
                        .filter(|e| e.edge.pos_x == pos_x)
                        .filter(|e| matches!(e.edge.edge_type, EdgeType::Vertical))
                        .any(|e| e.edge_parent_hash != hash)
                    {
                        skip_judge_overlap = false;
                        break;
                    }
                }

                if !skip_judge_overlap {
                    for y in (child_pos_y + 1)..pos_y {
                        let processing_commit_pos_x =
                            commit_pos_map.get(&commits[y].commit_hash).unwrap().0;
                        if processing_commit_pos_x == new_pos_x {
                            overlap = true;
                            if new_pos_x < processing_commit_pos_x + 1 {
                                new_pos_x = processing_commit_pos_x + 1;
                            }
                        }
                        for edge in &edges[y] {
                            if edge.edge.pos_x >= new_pos_x
                                && edge.edge_parent_hash != hash
                                && matches!(edge.edge.edge_type, EdgeType::Vertical)
                            {
                                overlap = true;
                                if new_pos_x < edge.edge.pos_x + 1 {
                                    new_pos_x = edge.edge.pos_x + 1;
                                }
                            }
                        }
                    }
                }

                if overlap {
                    // detour
                    edges[pos_y].push(WrappedEdge::new(EdgeType::Right, pos_x, pos_x, hash));
                    for x in (pos_x + 1)..new_pos_x {
                        edges[pos_y].push(WrappedEdge::new(EdgeType::Horizontal, x, pos_x, hash));
                    }
                    edges[pos_y].push(WrappedEdge::new(
                        EdgeType::RightBottom,
                        new_pos_x,
                        pos_x,
                        hash,
                    ));
                    for y in ((child_pos_y + 1)..pos_y).rev() {
                        edges[y].push(WrappedEdge::new(EdgeType::Vertical, new_pos_x, pos_x, hash));
                    }
                    edges[child_pos_y].push(WrappedEdge::new(
                        EdgeType::RightTop,
                        new_pos_x,
                        pos_x,
                        hash,
                    ));
                    for x in (child_pos_x + 1)..new_pos_x {
                        edges[child_pos_y].push(WrappedEdge::new(
                            EdgeType::Horizontal,
                            x,
                            pos_x,
                            hash,
                        ));
                    }
                    edges[child_pos_y].push(WrappedEdge::new(
                        EdgeType::Right,
                        child_pos_x,
                        pos_x,
                        hash,
                    ));

                    if max_pos_x < new_pos_x {
                        max_pos_x = new_pos_x;
                    }
                } else if pos_x == child_pos_x {
                    // 同 col merge 且無 overlap → 等同 commit 直線
                    draw_vertical_chain(&mut edges, pos_x, child_pos_y, pos_y, hash);
                } else {
                    edges[pos_y].push(WrappedEdge::new(EdgeType::Up, pos_x, pos_x, hash));
                    for y in ((child_pos_y + 1)..pos_y).rev() {
                        edges[y].push(WrappedEdge::new(EdgeType::Vertical, pos_x, pos_x, hash));
                    }
                    if pos_x < child_pos_x {
                        edges[child_pos_y].push(WrappedEdge::new(
                            EdgeType::LeftTop,
                            pos_x,
                            pos_x,
                            hash,
                        ));
                        for x in (pos_x + 1)..child_pos_x {
                            edges[child_pos_y].push(WrappedEdge::new(
                                EdgeType::Horizontal,
                                x,
                                pos_x,
                                hash,
                            ));
                        }
                        edges[child_pos_y].push(WrappedEdge::new(
                            EdgeType::Left,
                            child_pos_x,
                            pos_x,
                            hash,
                        ));
                    } else {
                        edges[child_pos_y].push(WrappedEdge::new(
                            EdgeType::RightTop,
                            pos_x,
                            pos_x,
                            hash,
                        ));
                        for x in (child_pos_x + 1)..pos_x {
                            edges[child_pos_y].push(WrappedEdge::new(
                                EdgeType::Horizontal,
                                x,
                                pos_x,
                                hash,
                            ));
                        }
                        edges[child_pos_y].push(WrappedEdge::new(
                            EdgeType::Right,
                            child_pos_x,
                            pos_x,
                            hash,
                        ));
                    }
                }
            }
        }

        if max_pos_x < pos_x {
            max_pos_x = pos_x;
        }
    }

    let edges: Vec<Vec<Edge>> = edges
        .into_iter()
        .map(|es| {
            let mut es: Vec<Edge> = es.into_iter().map(|e| e.edge).collect();
            es.sort_by_key(|e| (e.associated_line_pos_x, e.pos_x, e.edge_type));
            es.dedup();
            es
        })
        .collect();

    (edges, max_pos_x)
}

struct FilteredRelations {
    children_map: FxHashMap<CommitHash, Vec<CommitHash>>,
    parents_map: FxHashMap<CommitHash, Vec<CommitHash>>,
}

impl GraphDataSource for FilteredRelations {
    fn children_hash(&self, hash: &CommitHash) -> Vec<&CommitHash> {
        self.children_map
            .get(hash)
            .map(|hs| hs.iter().collect())
            .unwrap_or_default()
    }

    fn parents_hash(&self, hash: &CommitHash) -> Vec<&CommitHash> {
        self.parents_map
            .get(hash)
            .map(|hs| hs.iter().collect())
            .unwrap_or_default()
    }
}

/// Walk up ancestors to find the nearest visible parent.
fn find_nearest_visible_parent(
    start: &CommitHash,
    repository: &Repository,
    visible: &FxHashSet<CommitHash>,
) -> Option<CommitHash> {
    let mut stack = vec![start.clone()];
    let mut visited: FxHashSet<CommitHash> = FxHashSet::default();
    visited.insert(start.clone());
    while let Some(current) = stack.pop() {
        if visible.contains(&current) && current != *start {
            return Some(current);
        }
        for parent in repository.parents_hash(&current) {
            if visited.insert(parent.clone()) {
                stack.push(parent.clone());
            }
        }
    }
    None
}

pub fn calc_graph_filtered(
    repository: &Repository,
    visible_hashes: &FxHashSet<CommitHash>,
    head_hint: Option<&CommitHash>,
) -> Graph {
    let commits: Vec<&Commit> = repository
        .all_commits()
        .iter()
        .filter(|c| visible_hashes.contains(&c.commit_hash))
        .collect();

    // Build rewritten parent/children maps
    let mut parents_map: FxHashMap<CommitHash, Vec<CommitHash>> = FxHashMap::default();
    let mut children_map: FxHashMap<CommitHash, Vec<CommitHash>> = FxHashMap::default();

    for commit in &commits {
        let mut rewritten_parents = Vec::new();
        for orig_parent in &commit.parent_commit_hashes {
            if visible_hashes.contains(orig_parent) {
                rewritten_parents.push(orig_parent.clone());
            } else if let Some(ancestor) =
                find_nearest_visible_parent(orig_parent, repository, visible_hashes)
            {
                if !rewritten_parents.contains(&ancestor) {
                    rewritten_parents.push(ancestor);
                }
            }
        }
        for parent in &rewritten_parents {
            children_map
                .entry(parent.clone())
                .or_default()
                .push(commit.commit_hash.clone());
        }
        parents_map.insert(commit.commit_hash.clone(), rewritten_parents);
    }

    let source = FilteredRelations {
        children_map,
        parents_map,
    };

    let effective_head = head_hint.filter(|h| visible_hashes.contains(*h));
    let commit_pos_map = calc_commit_positions(&commits, &source, effective_head);
    let (mut graph_edges, max_pos_x) = calc_edges(&commit_pos_map, &commits, &source);

    normalize_head_row_invariant(&mut graph_edges, &commit_pos_map, effective_head);
    if !repository.working_changes().is_empty() {
        anchor_head_to_virtual_row(&mut graph_edges, &commit_pos_map, effective_head);
    }

    let commit_hashes = commits.iter().map(|c| c.commit_hash.clone()).collect();

    Graph {
        commit_hashes,
        commit_pos_map,
        edges: graph_edges,
        max_pos_x,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head_hash() -> CommitHash {
        CommitHash::from("headhash")
    }

    fn pos_map_for_head(head_pos_x: usize, head_pos_y: usize) -> CommitPosMap {
        let mut map = CommitPosMap::default();
        map.insert(head_hash(), (head_pos_x, head_pos_y));
        map
    }

    #[test]
    fn invariant_no_head_hint_leaves_edges_untouched() {
        let mut edges: Vec<Vec<Edge>> = vec![
            vec![Edge::new(EdgeType::Vertical, 1, 1)],
            vec![Edge::new(EdgeType::Up, 1, 1)],
        ];
        let before = edges.clone();
        normalize_head_row_invariant(&mut edges, &pos_map_for_head(1, 1), None);
        assert_eq!(edges, before);
    }

    #[test]
    fn invariant_removes_vertical_at_head_pos_x() {
        let head = head_hash();
        let pos = pos_map_for_head(1, 2);
        // Row 2 has a pass-through Vertical at HEAD's column — the pierce source.
        let mut edges: Vec<Vec<Edge>> = vec![
            vec![Edge::new(EdgeType::Vertical, 0, 0)],
            vec![Edge::new(EdgeType::Vertical, 0, 0)],
            vec![
                Edge::new(EdgeType::Vertical, 0, 0),
                Edge::new(EdgeType::Vertical, 1, 1),
                Edge::new(EdgeType::Up, 1, 1),
                Edge::new(EdgeType::Down, 1, 1),
            ],
        ];
        normalize_head_row_invariant(&mut edges, &pos, Some(&head));
        assert!(
            !edges[2]
                .iter()
                .any(|e| e.pos_x == 1 && e.edge_type == EdgeType::Vertical),
            "Vertical at head_pos_x on head row must be removed"
        );
        // Non-HEAD Vertical at col 0 untouched.
        assert!(edges[2]
            .iter()
            .any(|e| e.pos_x == 0 && e.edge_type == EdgeType::Vertical));
        assert_eq!(edges[0].len(), 1);
        assert_eq!(edges[1].len(), 1);
    }

    #[test]
    fn invariant_head_pos_y_zero_still_retains() {
        let head = head_hash();
        let pos = pos_map_for_head(0, 0);
        let mut edges: Vec<Vec<Edge>> = vec![vec![
            Edge::new(EdgeType::Vertical, 0, 0),
            Edge::new(EdgeType::Down, 0, 0),
        ]];
        normalize_head_row_invariant(&mut edges, &pos, Some(&head));
        assert!(
            !edges[0]
                .iter()
                .any(|e| e.pos_x == 0 && e.edge_type == EdgeType::Vertical),
            "pierce fix must run even at head_pos_y=0"
        );
    }

    #[test]
    fn anchor_not_called_leaves_no_stray_vertical_above_head() {
        // Regression test: without anchor (clean working tree), HEAD's column
        // must NOT get extra Verticals on rows above HEAD.
        let head = head_hash();
        let pos = pos_map_for_head(1, 3);
        let mut edges: Vec<Vec<Edge>> = vec![
            vec![Edge::new(EdgeType::Vertical, 0, 0)],
            vec![Edge::new(EdgeType::Vertical, 0, 0)],
            vec![Edge::new(EdgeType::Vertical, 0, 0)],
            vec![
                Edge::new(EdgeType::Up, 1, 1),
                Edge::new(EdgeType::Down, 1, 1),
            ],
        ];
        normalize_head_row_invariant(&mut edges, &pos, Some(&head));
        for (i, row) in edges.iter().enumerate().take(3) {
            assert!(
                !row.iter()
                    .any(|e| e.pos_x == 1 && e.edge_type == EdgeType::Vertical),
                "row {i} must not have Vertical at HEAD's col without anchor"
            );
        }
    }

    #[test]
    fn anchor_adds_vertical_above_and_up_on_head_row() {
        let head = head_hash();
        let pos = pos_map_for_head(1, 3);
        let mut edges: Vec<Vec<Edge>> = vec![
            vec![Edge::new(EdgeType::Vertical, 0, 0)],
            vec![Edge::new(EdgeType::Vertical, 0, 0)],
            vec![Edge::new(EdgeType::Vertical, 0, 0)],
            vec![Edge::new(EdgeType::Down, 1, 1)],
        ];
        anchor_head_to_virtual_row(&mut edges, &pos, Some(&head));
        for (i, row) in edges.iter().enumerate().take(3) {
            assert!(
                row.iter()
                    .any(|e| e.pos_x == 1 && e.edge_type == EdgeType::Vertical),
                "row {i} must have Vertical at HEAD's col after anchor"
            );
        }
        assert!(edges[3]
            .iter()
            .any(|e| e.pos_x == 1 && e.edge_type == EdgeType::Up));
    }

    // --- calc_edges tests ---

    fn make_commit(hash: &str, parents: &[&str]) -> Commit {
        Commit {
            commit_hash: CommitHash::from(hash),
            parent_commit_hashes: parents.iter().map(|h| CommitHash::from(*h)).collect(),
            ..Default::default()
        }
    }

    struct MockSource {
        children: FxHashMap<CommitHash, Vec<CommitHash>>,
    }

    impl MockSource {
        fn from_commits(commits: &[&Commit]) -> Self {
            let mut children: FxHashMap<CommitHash, Vec<CommitHash>> = FxHashMap::default();
            for commit in commits {
                for parent in &commit.parent_commit_hashes {
                    children
                        .entry(parent.clone())
                        .or_default()
                        .push(commit.commit_hash.clone());
                }
            }
            Self { children }
        }
    }

    impl GraphDataSource for MockSource {
        fn children_hash(&self, hash: &CommitHash) -> Vec<&CommitHash> {
            self.children
                .get(hash)
                .map(|hs| hs.iter().collect())
                .unwrap_or_default()
        }
        fn parents_hash(&self, _hash: &CommitHash) -> Vec<&CommitHash> {
            Vec::new()
        }
    }

    fn has_edge(edges: &[Vec<Edge>], row: usize, col: usize, et: EdgeType) -> bool {
        edges[row]
            .iter()
            .any(|e| e.pos_x == col && e.edge_type == et)
    }

    /// 同 col first-parent → 直線 Vertical（原有行為）
    #[test]
    fn edges_same_col_first_parent_draws_vertical() {
        // A (y=0, col=1) ← B (y=1, col=1), first parent
        let a = make_commit("a", &["b"]);
        let b = make_commit("b", &[]);
        let commits = vec![&a, &b];
        let source = MockSource::from_commits(&commits);
        let mut pos_map = CommitPosMap::default();
        pos_map.insert(CommitHash::from("a"), (1, 0));
        pos_map.insert(CommitHash::from("b"), (1, 1));

        let (edges, _) = calc_edges(&pos_map, &commits, &source);

        assert!(
            has_edge(&edges, 0, 1, EdgeType::Down),
            "child row should have Down"
        );
        assert!(
            has_edge(&edges, 1, 1, EdgeType::Up),
            "parent row should have Up"
        );
    }

    /// 同 col merge 且中間有其他 commit → 應 detour 繞道，不穿透
    #[test]
    fn edges_same_col_merge_with_intermediate_detours() {
        // Topology (模擬 scanoo-web 的 bug)：
        //   y=0: M (merge, col=1) parents=[E, P]  first-parent=E
        //   y=1: E (col=0)                        M 的 first-parent（相鄰，branch 不生中間 Vertical）
        //   y=2: X (col=1) parents=[Y]            unrelated commit on same col
        //   y=3: Y (col=1) parents=[]             unrelated commit on same col
        //   y=4: P (col=1) parents=[]             merge second-parent
        //
        // P→M 是 merge（non-first-parent），同 col 1，中間有 X, Y 在 col 1
        // 應該 detour 到 col≥2，不在 col 1 rows 2-3 畫穿透 Vertical
        let m = make_commit("M", &["E", "P"]);
        let e = make_commit("E", &[]);
        let x = make_commit("X", &["Y"]);
        let y_c = make_commit("Y", &[]);
        let p = make_commit("P", &[]);
        let commits = vec![&m, &e, &x, &y_c, &p];
        let source = MockSource::from_commits(&commits);
        let mut pos_map = CommitPosMap::default();
        pos_map.insert(CommitHash::from("M"), (1, 0));
        pos_map.insert(CommitHash::from("E"), (0, 1));
        pos_map.insert(CommitHash::from("X"), (1, 2));
        pos_map.insert(CommitHash::from("Y"), (1, 3));
        pos_map.insert(CommitHash::from("P"), (1, 4));

        let (edges, _) = calc_edges(&pos_map, &commits, &source);

        // 中間 rows (X, Y) 在 col 1 不能有 merge 的 pass-through Vertical
        for row in [2, 3] {
            assert!(
                !edges[row]
                    .iter()
                    .any(|e| e.pos_x == 1 && e.edge_type == EdgeType::Vertical),
                "row {row} col 1 must NOT have pass-through Vertical from merge"
            );
        }
        // detour 應在 col≥2 有 Vertical
        for row in [2, 3] {
            assert!(
                edges[row]
                    .iter()
                    .any(|e| e.pos_x >= 2 && e.edge_type == EdgeType::Vertical),
                "row {row} should have detour Vertical at col≥2"
            );
        }
    }

    /// 同 col merge 且相鄰（無中間 commit）→ 直接 Up/Down
    #[test]
    fn edges_same_col_merge_adjacent_draws_up_down() {
        // y=0: M (merge, col=1) parents=[E, P]  first-parent=E
        // y=1: P (col=1)                       merge second-parent
        // y=2: E (col=0)                       merge first-parent
        let m = make_commit("M", &["E", "P"]);
        let p = make_commit("P", &[]);
        let e = make_commit("E", &[]);
        let commits = vec![&m, &p, &e];
        let source = MockSource::from_commits(&commits);
        let mut pos_map = CommitPosMap::default();
        pos_map.insert(CommitHash::from("M"), (1, 0));
        pos_map.insert(CommitHash::from("P"), (1, 1));
        pos_map.insert(CommitHash::from("E"), (0, 2));

        let (edges, _) = calc_edges(&pos_map, &commits, &source);

        assert!(
            has_edge(&edges, 0, 1, EdgeType::Down),
            "child row should have Down at col 1"
        );
        assert!(
            has_edge(&edges, 1, 1, EdgeType::Up),
            "parent row should have Up at col 1"
        );
    }
}

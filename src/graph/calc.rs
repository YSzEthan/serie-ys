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

pub fn calc_graph(repository: &Repository) -> Graph {
    let commits: Vec<&Commit> = repository.all_commits().iter().collect();

    let commit_pos_map = calc_commit_positions(&commits, repository);
    let (graph_edges, max_pos_x) = calc_edges(&commit_pos_map, &commits, repository);
    let commit_hashes = commits.iter().map(|c| c.commit_hash.clone()).collect();

    Graph {
        commit_hashes,
        commit_pos_map,
        edges: graph_edges,
        max_pos_x,
    }
}

fn calc_commit_positions(commits: &[&Commit], source: &impl GraphDataSource) -> CommitPosMap {
    let mut commit_pos_map: CommitPosMap = FxHashMap::default();
    let mut commit_line_state: Vec<Option<CommitHash>> = Vec::new();
    // Reverse index: hash → pos_x for O(1) lookup instead of linear scan
    let mut hash_to_pos: FxHashMap<CommitHash, usize> = FxHashMap::default();

    for (pos_y, commit) in commits.iter().enumerate() {
        let filtered_children_hash = filtered_children_hash(commit, source);
        if filtered_children_hash.is_empty() {
            let pos_x = get_first_vacant_line(&commit_line_state);
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

fn get_first_vacant_line(commit_line_state: &[Option<CommitHash>]) -> usize {
    commit_line_state
        .iter()
        .position(|c| c.is_none())
        .unwrap_or(commit_line_state.len())
}

fn add_commit_line(
    commit: &Commit,
    commit_line_state: &mut Vec<Option<CommitHash>>,
    hash_to_pos: &mut FxHashMap<CommitHash, usize>,
    pos_x: usize,
) {
    if commit_line_state.len() <= pos_x {
        commit_line_state.push(Some(commit.commit_hash.clone()));
    } else {
        commit_line_state[pos_x] = Some(commit.commit_hash.clone());
    }
    hash_to_pos.insert(commit.commit_hash.clone(), pos_x);
}

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

            if pos_x == child_pos_x {
                // commit
                edges[pos_y].push(WrappedEdge::new(EdgeType::Up, pos_x, pos_x, hash));
                for y in ((child_pos_y + 1)..pos_y).rev() {
                    edges[y].push(WrappedEdge::new(EdgeType::Vertical, pos_x, pos_x, hash));
                }
                edges[child_pos_y].push(WrappedEdge::new(EdgeType::Down, pos_x, pos_x, hash));
            } else {
                let child_first_parent_hash = &commits[child_pos_y].parent_commit_hashes[0];
                if *child_first_parent_hash == *hash {
                    // branch
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
                } else {
                    // merge
                    // skip
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

            if pos_x == child_pos_x {
                // commit
                // skip
            } else {
                let child_first_parent_hash = &commits[child_pos_y].parent_commit_hashes[0];
                if *child_first_parent_hash == *hash {
                    // branch
                    // skip
                } else {
                    // merge
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
                            edges[pos_y].push(WrappedEdge::new(
                                EdgeType::Horizontal,
                                x,
                                pos_x,
                                hash,
                            ));
                        }
                        edges[pos_y].push(WrappedEdge::new(
                            EdgeType::RightBottom,
                            new_pos_x,
                            pos_x,
                            hash,
                        ));
                        for y in ((child_pos_y + 1)..pos_y).rev() {
                            edges[y].push(WrappedEdge::new(
                                EdgeType::Vertical,
                                new_pos_x,
                                pos_x,
                                hash,
                            ));
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

    let commit_pos_map = calc_commit_positions(&commits, &source);
    let (graph_edges, max_pos_x) = calc_edges(&commit_pos_map, &commits, &source);
    let commit_hashes = commits.iter().map(|c| c.commit_hash.clone()).collect();

    Graph {
        commit_hashes,
        commit_pos_map,
        edges: graph_edges,
        max_pos_x,
    }
}

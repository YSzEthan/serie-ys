use std::rc::Rc;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    widgets::{Block, Borders, Padding, StatefulWidget, Widget},
};
use rustc_hash::FxHashSet;
use semver::Version;

use crate::{app::AppContext, git::Ref, widget::scroll};

const TREE_BRANCH_ROOT_IDENT: &str = "__branches__";
const TREE_REMOTE_ROOT_IDENT: &str = "__remotes__";
const TREE_TAG_ROOT_IDENT: &str = "__tags__";
const TREE_STASH_ROOT_IDENT: &str = "__stashes__";

const TREE_BRANCH_ROOT_TEXT: &str = "Branches";
const TREE_REMOTE_ROOT_TEXT: &str = "Remotes";
const TREE_TAG_ROOT_TEXT: &str = "Tags";
const TREE_STASH_ROOT_TEXT: &str = "Stashes";

/// 自己攤平渲染的 refs 樹，取代 `tui-tree-widget`。`roots` 在建構時算好、
/// 之後不變；`visible_rows` 隨 `opened` 展開/收合而重算，是「哪些列目前
/// 看得到」的唯一真相來源。`selected` 用 identifier 路徑（而非索引）記，
/// 這樣 `opened` 變動、`visible_rows` 重建之後，只要那個節點還在，選取
/// 狀態就還在，不需要另外做索引位移。
#[derive(Debug, Default)]
pub struct RefListState {
    roots: Vec<RefTreeNode>,
    visible_rows: Vec<VisibleRefRow>,
    selected: Vec<String>,
    opened: FxHashSet<Vec<String>>,
    offset: usize,
}

impl RefListState {
    pub fn new(refs: &[&Ref]) -> Self {
        let selected = vec![TREE_BRANCH_ROOT_IDENT.into()];
        let opened = FxHashSet::from_iter([selected.clone()]);
        let mut state = Self {
            roots: build_ref_tree_nodes(refs),
            visible_rows: Vec::new(),
            selected,
            opened,
            offset: 0,
        };
        state.rebuild_visible_rows();
        state
    }

    pub fn select_next(&mut self) {
        let next = self
            .selected_index()
            .map_or(0, |index| index.saturating_add(1))
            .min(self.visible_rows.len().saturating_sub(1));
        self.select_visible_index(next);
    }

    pub fn select_prev(&mut self) {
        let prev = self
            .selected_index()
            .map_or(self.visible_rows.len().saturating_sub(1), |index| {
                index.saturating_sub(1)
            });
        self.select_visible_index(prev);
    }

    pub fn open_node(&mut self) {
        if self
            .visible_rows
            .iter()
            .any(|row| row.identifier == self.selected && row.has_children)
            && self.opened.insert(self.selected.clone())
        {
            self.rebuild_visible_rows();
        }
    }

    /// 已展開就收合；沒展開（或本來就是葉節點）就跳到父節點——跟
    /// tui-tree-widget 的 `key_left` 行為一致。根層（`selected.len() <= 1`）
    /// 呼叫端會先用 `is_at_root_level()` 擋下改走「關閉整個 refs 畫面」，
    /// 不會走到這裡，但這裡的 `pop()` 對根層也安全（no-op）。
    pub fn close_node(&mut self) {
        if self.opened.remove(&self.selected) {
            self.rebuild_visible_rows();
        } else if self.selected.len() > 1 {
            self.selected.pop();
        }
    }

    pub fn is_at_root_level(&self) -> bool {
        self.selected.len() <= 1
    }

    pub fn selected_ref_name(&self) -> Option<String> {
        self.selected.last().cloned()
    }

    pub fn selected_tag(&self) -> Option<String> {
        if self.selected.len() > 1 && self.selected[0] == TREE_TAG_ROOT_IDENT {
            self.selected.last().cloned()
        } else {
            None
        }
    }

    pub fn selected_local_branch(&self) -> Option<String> {
        if self.selected.len() > 1 && self.selected[0] == TREE_BRANCH_ROOT_IDENT {
            self.selected.last().cloned()
        } else {
            None
        }
    }

    pub fn selected_remote_branch(&self) -> Option<String> {
        if self.selected.len() > 1 && self.selected[0] == TREE_REMOTE_ROOT_IDENT {
            self.selected.last().cloned()
        } else {
            None
        }
    }

    pub fn current_tree_status(&self) -> (Vec<String>, Vec<Vec<String>>) {
        let selected = self.selected.clone();
        let opened = self.opened.iter().cloned().collect();
        (selected, opened)
    }

    /// refresh 還原：呼叫時 `roots`/`visible_rows` 已經是用新的 refs 重建過
    /// 的（`RefListState::new` 在這之前先跑過），這裡只還原 `selected`／
    /// `opened` 兩個使用者操作過的狀態。還原的路徑若已經不存在（ref 被刪、
    /// 或改名），一路往上找最近還看得到的祖先；四個分類根節點永遠存在，
    /// 全部找不到就退回 `__branches__`，不會出現選取一個不存在節點的情況。
    pub fn reset_tree_status(&mut self, selected: Vec<String>, opened: Vec<Vec<String>>) {
        self.opened = opened.into_iter().collect();
        self.rebuild_visible_rows();
        self.selected = selected;
        while !self.selected.is_empty() && self.selected_index().is_none() {
            self.selected.pop();
        }
        if self.selected.is_empty() {
            self.selected = vec![TREE_BRANCH_ROOT_IDENT.into()];
        }
    }

    fn selected_index(&self) -> Option<usize> {
        self.visible_rows
            .iter()
            .position(|row| row.identifier == self.selected)
    }

    fn select_visible_index(&mut self, index: usize) {
        if let Some(row) = self.visible_rows.get(index) {
            self.selected.clone_from(&row.identifier);
        }
    }

    fn rebuild_visible_rows(&mut self) {
        self.visible_rows.clear();
        collect_visible_rows(&self.roots, &self.opened, &[], &mut self.visible_rows);
    }
}

#[derive(Debug)]
struct VisibleRefRow {
    identifier: Vec<String>,
    name: String,
    depth: usize,
    has_children: bool,
}

fn collect_visible_rows(
    nodes: &[RefTreeNode],
    opened: &FxHashSet<Vec<String>>,
    parent: &[String],
    rows: &mut Vec<VisibleRefRow>,
) {
    for node in nodes {
        let mut identifier = parent.to_vec();
        identifier.push(node.identifier.clone());
        rows.push(VisibleRefRow {
            identifier: identifier.clone(),
            name: node.name.clone(),
            depth: parent.len(),
            has_children: !node.children.is_empty(),
        });
        if opened.contains(&identifier) {
            collect_visible_rows(&node.children, opened, &identifier, rows);
        }
    }
}

pub struct RefList {
    ctx: Rc<AppContext>,
}

impl RefList {
    pub fn new(ctx: Rc<AppContext>) -> RefList {
        RefList { ctx }
    }
}

impl StatefulWidget for RefList {
    type State = RefListState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let block = Block::default()
            .borders(Borders::LEFT)
            .style(Style::default().fg(self.ctx.color_theme.divider_fg))
            .padding(Padding::horizontal(1));
        let inner = block.inner(area);
        Widget::render(block, area, buf);
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        // 跟游標移動共用同一個 `scroll::scrolled_offset`，scrolloff 傳
        // 0——refs 樹沒有可設定的邊距，只需要保證選取列在畫面內，用最小
        // 捲動而非每次都重新置頂。
        let index = state.selected_index().unwrap_or(0);
        state.offset = scroll::scrolled_offset(
            index,
            inner.height as usize,
            state.visible_rows.len(),
            state.offset,
            0,
        );

        let item_style = Style::default().fg(self.ctx.color_theme.fg);
        let highlight_style = Style::default()
            .bg(self.ctx.color_theme.ref_selected_bg)
            .fg(self.ctx.color_theme.ref_selected_fg);

        for (line, row) in state
            .visible_rows
            .iter()
            .skip(state.offset)
            .take(inner.height as usize)
            .enumerate()
        {
            let y = inner.y + line as u16;
            let symbol = if !row.has_children {
                "  "
            } else if state.opened.contains(&row.identifier) {
                "\u{25be} " // ▾
            } else {
                "\u{25b8} " // ▸
            };
            let prefix = format!("{}{symbol}", "  ".repeat(row.depth));
            let (text_x, _) = buf.set_stringn(inner.x, y, prefix, inner.width as usize, item_style);
            let remaining = inner.width.saturating_sub(text_x - inner.x);
            buf.set_stringn(text_x, y, &row.name, remaining as usize, item_style);
            if row.identifier == state.selected {
                buf.set_style(Rect::new(inner.x, y, inner.width, 1), highlight_style);
            }
        }
    }
}

fn build_ref_tree_nodes(refs: &[&Ref]) -> Vec<RefTreeNode> {
    let mut branch_refs = Vec::new();
    let mut remote_refs = Vec::new();
    let mut tag_refs = Vec::new();
    let mut stash_refs = Vec::new();

    for &r in refs {
        match r {
            Ref::Tag { name, .. } => tag_refs.push(name.clone()),
            Ref::Branch { name, .. } => branch_refs.push(name.clone()),
            Ref::RemoteBranch { name, .. } => remote_refs.push(name.clone()),
            Ref::Stash { name, message, .. } => stash_refs.push((name.clone(), message.clone())),
        }
    }

    let mut branch_nodes = refs_to_ref_tree_nodes(branch_refs);
    let mut remote_nodes = refs_to_ref_tree_nodes(remote_refs);
    let mut tag_nodes = refs_to_ref_tree_nodes(tag_refs);
    let mut stash_nodes = refs_to_stash_ref_tree_nodes(stash_refs);

    sort_branch_tree_nodes(&mut branch_nodes);
    sort_branch_tree_nodes(&mut remote_nodes);
    sort_tag_tree_nodes(&mut tag_nodes);
    sort_stash_tree_nodes(&mut stash_nodes);

    vec![
        RefTreeNode {
            identifier: TREE_BRANCH_ROOT_IDENT.into(),
            name: TREE_BRANCH_ROOT_TEXT.into(),
            children: branch_nodes,
        },
        RefTreeNode {
            identifier: TREE_REMOTE_ROOT_IDENT.into(),
            name: TREE_REMOTE_ROOT_TEXT.into(),
            children: remote_nodes,
        },
        RefTreeNode {
            identifier: TREE_TAG_ROOT_IDENT.into(),
            name: TREE_TAG_ROOT_TEXT.into(),
            children: tag_nodes,
        },
        RefTreeNode {
            identifier: TREE_STASH_ROOT_IDENT.into(),
            name: TREE_STASH_ROOT_TEXT.into(),
            children: stash_nodes,
        },
    ]
}

#[derive(Debug)]
struct RefTreeNode {
    identifier: String,
    name: String,
    children: Vec<RefTreeNode>,
}

fn refs_to_stash_ref_tree_nodes(ref_name_messages: Vec<(String, String)>) -> Vec<RefTreeNode> {
    let mut nodes: Vec<RefTreeNode> = Vec::new();
    for (name, message) in ref_name_messages {
        let node = RefTreeNode {
            identifier: name.clone(),
            name: message.to_string(),
            children: Vec::new(),
        };
        nodes.push(node);
    }
    nodes
}

fn refs_to_ref_tree_nodes(ref_names: Vec<String>) -> Vec<RefTreeNode> {
    let mut nodes: Vec<RefTreeNode> = Vec::new();

    for ref_name in ref_names {
        let mut current_nodes = &mut nodes;
        let mut parent_identifier = String::new();

        for part in ref_name.split('/') {
            if let Some(index) = current_nodes.iter().position(|n| n.name == part) {
                let node = &mut current_nodes[index];
                parent_identifier.clone_from(&node.identifier);
                current_nodes = &mut node.children;
            } else {
                let identifier = if parent_identifier.is_empty() {
                    part.to_string()
                } else {
                    format!("{parent_identifier}/{part}")
                };
                current_nodes.push(RefTreeNode {
                    identifier: identifier.clone(),
                    name: part.to_string(),
                    children: Vec::new(),
                });
                let Some(last) = current_nodes.last_mut() else {
                    break;
                };
                current_nodes = &mut last.children;
                parent_identifier = identifier;
            }
        }
    }

    nodes
}

fn sort_branch_tree_nodes(nodes: &mut [RefTreeNode]) {
    nodes.sort_by(|a, b| {
        b.children
            .len()
            .cmp(&a.children.len())
            .then(a.name.cmp(&b.name))
    });
    for node in nodes {
        sort_branch_tree_nodes(&mut node.children);
    }
}

fn sort_tag_tree_nodes(nodes: &mut [RefTreeNode]) {
    nodes.sort_by(|a, b| {
        let a_version = parse_semantic_version_tag(&a.name);
        let b_version = parse_semantic_version_tag(&b.name);
        match (a_version, b_version) {
            // 兩者皆為 semver：依版本號遞減排序（新的在前）
            (Some(av), Some(bv)) => bv.cmp(&av),
            // semver tag 排在非 semver tag 前面
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            // 兩者皆非 semver：依名稱遞減排序（Z-A，與 semver 排序方向一致）
            (None, None) => b.name.cmp(&a.name),
        }
    });
}

fn sort_stash_tree_nodes(nodes: &mut [RefTreeNode]) {
    nodes.sort_by(|a, b| a.identifier.cmp(&b.identifier));
}

fn parse_semantic_version_tag(tag: &str) -> Option<Version> {
    let tag = tag.trim_start_matches('v');
    Version::parse(tag).ok()
}

#[cfg(test)]
mod tests {
    use ratatui::{buffer::Buffer, layout::Rect};

    use super::*;
    use crate::{
        color::ColorTheme, config::CoreConfig, config::UiConfig, git::CommitHash, git::FetchPrune,
        keybind::KeyBind, GraphStyle,
    };

    fn test_ctx() -> Rc<AppContext> {
        Rc::new(AppContext {
            keybind: KeyBind::new(None),
            core_config: CoreConfig::default(),
            ui_config: UiConfig::default(),
            color_theme: ColorTheme::default(),
            graph_style: GraphStyle::default(),
            graph_width: None,
            compact: None,
            update: crate::update::UpdateSettings::default(),
            auto_fetch: crate::auto_fetch::AutoFetchSettings::default(),
            fetch_prune: FetchPrune::default(),
            shell_command: Vec::new(),
        })
    }

    fn branch(name: &str) -> Ref {
        Ref::Branch {
            name: name.into(),
            target: CommitHash::from("deadbeef"),
        }
    }

    fn remote_branch(name: &str) -> Ref {
        Ref::RemoteBranch {
            name: name.into(),
            target: CommitHash::from("deadbeef"),
        }
    }

    fn tag(name: &str) -> Ref {
        Ref::Tag {
            name: name.into(),
            target: CommitHash::from("deadbeef"),
        }
    }

    fn stash(name: &str, message: &str) -> Ref {
        Ref::Stash {
            name: name.into(),
            message: message.into(),
            target: CommitHash::from("deadbeef"),
        }
    }

    /// `main`、`feature/a`、`feature/b`、`origin/main`、四個 tag、一個
    /// stash——涵蓋巢狀 branch、remote、tag 排序、stash 四種節點型別。
    fn fixture_refs() -> Vec<Ref> {
        vec![
            branch("main"),
            branch("feature/a"),
            branch("feature/b"),
            remote_branch("origin/main"),
            tag("v1.2.0"),
            tag("v1.10.0"),
            tag("nightly"),
            tag("alpha"),
            stash("stash@{0}", "WIP"),
        ]
    }

    fn fixture_state() -> RefListState {
        let refs = fixture_refs();
        let refs: Vec<&Ref> = refs.iter().collect();
        RefListState::new(&refs)
    }

    fn row_names(state: &RefListState) -> Vec<&str> {
        state.visible_rows.iter().map(|r| r.name.as_str()).collect()
    }

    #[test]
    fn new_opens_branches_root_only() {
        let state = fixture_state();
        assert_eq!(
            row_names(&state),
            ["Branches", "feature", "main", "Remotes", "Tags", "Stashes"],
            "有子節點的 feature 排在 main 前面（sort_branch_tree_nodes：\
             子節點數多的在前）"
        );
        assert_eq!(state.selected, vec![TREE_BRANCH_ROOT_IDENT.to_string()]);
        assert!(state.is_at_root_level());
    }

    #[test]
    fn open_and_select_leaf_reports_local_branch() {
        let mut state = fixture_state();
        state.select_next(); // Branches -> feature
        state.open_node(); // 展開 feature
        state.select_next(); // feature -> feature/a

        assert_eq!(row_names(&state).len(), 8, "展開後多兩個子節點列");
        assert_eq!(state.selected_local_branch(), Some("feature/a".into()));
        assert_eq!(state.selected_remote_branch(), None);
        assert_eq!(state.selected_ref_name(), Some("feature/a".into()));
    }

    #[test]
    fn close_node_on_leaf_moves_to_parent_then_collapses() {
        let mut state = fixture_state();
        state.select_next();
        state.open_node();
        state.select_next(); // feature/a

        state.close_node(); // 葉節點：先跳回父節點
        assert_eq!(
            state.selected,
            vec![TREE_BRANCH_ROOT_IDENT.to_string(), "feature".to_string()]
        );
        assert_eq!(row_names(&state).len(), 8, "父節點還沒收合，列數不變");

        state.close_node(); // 再按一次：收合 feature
        assert_eq!(row_names(&state).len(), 6);
    }

    #[test]
    fn select_prev_and_next_clamp_at_bounds() {
        let mut state = fixture_state();
        let before = state.selected.clone();
        state.select_prev();
        assert_eq!(state.selected, before, "第 0 列按 prev 不動");

        for _ in 0..20 {
            state.select_next();
        }
        let last = state.selected.clone();
        state.select_next();
        assert_eq!(state.selected, last, "已在最後一列時 next 不動");
    }

    #[test]
    fn remote_leaf_reports_remote_branch() {
        let mut state = fixture_state();
        // "origin/main" 依 `/` 拆成巢狀節點，跟 branch 同一套規則：
        // Branches(0) feature(1) main(2) Remotes(3) -> 展開見 origin(4)
        // -> 展開見 origin/main(5)。
        for _ in 0..3 {
            state.select_next();
        }
        state.open_node();
        state.select_next(); // Remotes -> origin
        state.open_node();
        state.select_next(); // origin -> origin/main

        assert_eq!(state.selected_remote_branch(), Some("origin/main".into()));
        assert_eq!(state.selected_local_branch(), None);
    }

    #[test]
    fn tag_sort_semver_desc_then_non_semver_name_desc() {
        let mut state = fixture_state();
        // Branches(0) feature(1) main(2) Remotes(3) Tags(4)
        for _ in 0..4 {
            state.select_next();
        }
        state.open_node();

        let names: Vec<&str> = state
            .visible_rows
            .iter()
            .skip(5) // 跳過根節點與已展開的 Branches 子樹之後的 Tags 本身
            .take(4)
            .map(|r| r.name.as_str())
            .collect();
        assert_eq!(names, ["v1.10.0", "v1.2.0", "nightly", "alpha"]);

        state.select_next();
        assert_eq!(state.selected_tag(), Some("v1.10.0".into()));
    }

    #[test]
    fn reset_tree_status_falls_back_to_nearest_visible_ancestor() {
        let mut state = fixture_state();

        state.reset_tree_status(
            vec![
                TREE_BRANCH_ROOT_IDENT.into(),
                "gone".into(),
                "gone/x".into(),
            ],
            vec![vec![TREE_BRANCH_ROOT_IDENT.into()]],
        );
        assert_eq!(state.selected, vec![TREE_BRANCH_ROOT_IDENT.to_string()]);

        state.reset_tree_status(
            vec![TREE_TAG_ROOT_IDENT.into(), "v1.2.0".into()],
            Vec::new(),
        );
        assert_eq!(state.selected, vec![TREE_TAG_ROOT_IDENT.to_string()]);

        state.reset_tree_status(Vec::new(), Vec::new());
        assert_eq!(state.selected, vec![TREE_BRANCH_ROOT_IDENT.to_string()]);
    }

    #[test]
    fn reset_tree_status_restores_opened_and_selection() {
        let mut state = fixture_state();
        state.select_next();
        state.open_node();
        state.select_next();

        let (selected, opened) = state.current_tree_status();

        let mut restored = fixture_state();
        restored.reset_tree_status(selected.clone(), opened);
        assert_eq!(restored.selected, selected);
        assert_eq!(row_names(&restored).len(), row_names(&state).len());
    }

    #[test]
    fn default_state_navigation_does_not_panic() {
        let mut state = RefListState::default();
        state.select_next();
        state.select_prev();
        state.open_node();
        state.close_node();
        assert!(state.visible_rows.is_empty());
    }

    #[test]
    fn render_draws_symbols_and_scrolls_to_selection() {
        let refs = fixture_refs();
        let refs: Vec<&Ref> = refs.iter().collect();
        let mut state = RefListState::new(&refs);
        let ctx = test_ctx();

        let area = Rect::new(0, 0, 16, 3);
        let mut buf = Buffer::empty(area);
        RefList::new(ctx.clone()).render(area, &mut buf, &mut state);
        let first_row: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(
            first_row.starts_with("│ \u{25be} Branches"),
            "{first_row:?}"
        );

        for _ in 0..4 {
            state.select_next();
        }
        RefList::new(ctx).render(area, &mut buf, &mut state);
        assert!(
            state.offset > 0,
            "選取列超出三行高的畫面，offset 應該跟著捲動"
        );
        let selected_visible_row = state.selected_index().unwrap() - state.offset;
        assert!(selected_visible_row < area.height as usize);
    }
}

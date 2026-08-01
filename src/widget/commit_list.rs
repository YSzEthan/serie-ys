use std::{cell::Cell, rc::Rc};

use laurier::highlight::highlight_matched_text;
use ratatui::{
    buffer::Buffer,
    crossterm::event::{Event, KeyEvent},
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{List, ListItem, Paragraph, StatefulWidget, Widget},
};
use rustc_hash::{FxHashMap, FxHashSet};
use tui_input::{backend::crossterm::EventHandler, Input};

use crate::{
    app::AppContext,
    color::{ratatui_color_to_rgb, ColorTheme},
    config::UserListColumnType,
    fuzzy::SearchMatcher,
    git::{Commit, CommitHash, Head, Ref, WorkingChanges},
    graph::{Glyph, GlyphSet, Graph, TextCell},
    FilteredGraphData,
};

const ELLIPSIS: &str = "...";
const VIRTUAL_ROW_COLOR: Color = Color::Gray;
/// 游標與 viewport 上下邊緣保持的最小距離；撞進這個 margin 時改由 offset 滾動。
const CURSOR_SCROLL_MARGIN: usize = 15;

/// 索引到 `commits: Vec<CommitInfo>` 與 `search_matches: Vec<SearchMatch>` 的位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct RawCommitIdx(pub(crate) usize);

/// `filtered_indices` 內的位置；filter 空時 alias 到 raw（語意上仍是獨立座標）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FilteredIdx(usize);

/// 可視清單內的位置（= FilteredIdx + virtual_row_offset）。
/// 對應 `self.offset + self.selected` 的空間。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VisibleIdx(usize);

#[derive(Debug, Clone, Copy)]
enum MatchStep {
    Next,
    Prev,
}

#[derive(Debug)]
pub struct CommitInfo<'a> {
    commit: &'a Commit,
    refs: Vec<&'a Ref>,
    graph_color: Color,
}

impl<'a> CommitInfo<'a> {
    pub fn new(commit: &'a Commit, refs: Vec<&'a Ref>, graph_color: Color) -> Self {
        Self {
            commit,
            refs,
            graph_color,
        }
    }

    pub fn commit_hash(&self) -> &CommitHash {
        &self.commit.commit_hash
    }

    pub fn refs(&self) -> &[&'a Ref] {
        &self.refs
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchState {
    Inactive,
    Searching {
        start_index: RawCommitIdx,
        match_index: usize,
        ignore_case: bool,
        fuzzy: bool,
        transient_message: TransientMessage,
    },
    Applied {
        match_index: usize,
        total_match: usize,
    },
}

impl SearchState {
    fn update_match_index(&mut self, index: usize) {
        match self {
            SearchState::Searching { match_index, .. } => *match_index = index,
            SearchState::Applied { match_index, .. } => *match_index = index,
            _ => {}
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransientMessage {
    None,
    IgnoreCaseOff,
    IgnoreCaseOn,
    FuzzyOff,
    FuzzyOn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterState {
    Inactive,
    Filtering {
        ignore_case: bool,
        fuzzy: bool,
        transient_message: TransientMessage,
    },
}

#[derive(Debug, Default, Clone)]
struct SearchMatch {
    refs: FxHashMap<String, SearchMatchPosition>,
    subject: Option<SearchMatchPosition>,
    author_name: Option<SearchMatchPosition>,
    commit_hash: Option<SearchMatchPosition>,
    match_index: usize, // 1-based
}

enum SearchField<'a> {
    Subject(&'a str),
    AuthorName(&'a str),
    CommitHash(&'a str),
    Ref(&'a str),
}

impl SearchField<'_> {
    fn text(&self) -> &str {
        match self {
            Self::Subject(s) | Self::AuthorName(s) | Self::CommitHash(s) | Self::Ref(s) => s,
        }
    }
}

/// 搜尋與過濾看的所有欄位，全專案唯一一份清單 —— 加欄位只改這裡，`SearchMatch::new`
/// 與 `commit_quick_matches` 會自動跟上。兩邊各抄一份的話（包括 Stash 這條排除規則），
/// 分歧會讓某列被算進 `match_index` 卻標不出 highlight。
///
/// subject 排第一：`commit_quick_matches` 的 `any()` 靠這個順序短路。
fn search_fields<'a>(ci: &'a CommitInfo<'_>) -> impl Iterator<Item = SearchField<'a>> {
    [
        SearchField::Subject(&ci.commit.subject),
        SearchField::AuthorName(&ci.commit.author_name),
        SearchField::CommitHash(ci.commit.commit_hash.as_short_hash()),
    ]
    .into_iter()
    .chain(
        ci.refs
            .iter()
            .filter(|r| !matches!(r, Ref::Stash { .. }))
            .map(|r| SearchField::Ref(r.name())),
    )
}

impl SearchMatch {
    /// 收 `&SearchMatcher` 而不是 `(query, ignore_case, fuzzy)`：呼叫端在迴圈外就建好
    /// 一個，自己再建一次等於每個 commit 都重折一次 query、多配置一個 `String`。
    fn new(ci: &CommitInfo<'_>, matcher: &SearchMatcher) -> Self {
        let mut m = Self::default();
        for f in search_fields(ci) {
            let Some(pos) = matcher
                .matched_position(f.text())
                .map(SearchMatchPosition::new)
            else {
                continue;
            };
            match f {
                SearchField::Subject(_) => m.subject = Some(pos),
                SearchField::AuthorName(_) => m.author_name = Some(pos),
                SearchField::CommitHash(_) => m.commit_hash = Some(pos),
                SearchField::Ref(name) => {
                    m.refs.insert(name.into(), pos);
                }
            }
        }
        m
    }

    fn matched(&self) -> bool {
        !self.refs.is_empty()
            || self.subject.is_some()
            || self.author_name.is_some()
            || self.commit_hash.is_some()
    }

    fn clear(&mut self) {
        self.refs.clear();
        self.subject = None;
        self.author_name = None;
        self.commit_hash = None;
    }
}

#[derive(Debug, Default, Clone)]
struct SearchMatchPosition {
    matched_indices: Vec<usize>,
}

impl SearchMatchPosition {
    fn new(matched_indices: Vec<usize>) -> Self {
        Self { matched_indices }
    }
}

#[derive(Debug)]
pub struct CommitListState<'a> {
    commits: Vec<CommitInfo<'a>>,
    commit_hash_to_raw: FxHashMap<CommitHash, RawCommitIdx>,
    graph: Rc<Graph>,
    // Shared by the primary and filtered graph: both are built from the same
    // `GraphColorSet` / `Repository`, so there's exactly one of each, not
    // one pair per graph.
    graph_colors: Vec<Color>,
    head_commit_hash: Option<CommitHash>,
    graph_cell_width: u16,
    head: Head,

    // Filtered graph data (for when remote-only commits are hidden)
    filtered: Option<FilteredGraphData>,
    // Marker-overlay color map (commit_hash -> color), keyed differently
    // from `graph_colors` above (which is a pos_x-indexed palette). Not the
    // same concept despite the similar name -- do not merge.
    filtered_graph_colors: Option<FxHashMap<CommitHash, Color>>,

    ref_name_to_commit_index_map: FxHashMap<String, RawCommitIdx>,

    search_state: SearchState,
    search_input: Input,
    search_matches: Vec<SearchMatch>,

    // Optimization: track previous search for incremental search
    last_search_query: String,
    last_matched_indices: Vec<RawCommitIdx>,
    last_search_ignore_case: bool,
    last_search_fuzzy: bool,

    // Filter mode
    filter_state: FilterState,
    filter_input: Input,
    filtered_indices: Vec<RawCommitIdx>,
    text_filtered_indices: Vec<RawCommitIdx>,

    selected: usize,
    offset: usize,
    total: usize,
    height: usize,

    inline_detail_height: u16,

    show_remote_refs: bool,
    remote_only_commits: FxHashSet<CommitHash>,
    needs_graph_clear: bool,

    name_cell_width: u16,

    default_ignore_case: bool,
    default_fuzzy: bool,

    working_changes: Option<WorkingChanges>,

    pub(crate) selected_row_overflows: Cell<bool>,
}

impl<'a> CommitListState<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        commits: Vec<CommitInfo<'a>>,
        graph: Rc<Graph>,
        graph_colors: Vec<Color>,
        head_commit_hash: Option<CommitHash>,
        graph_cell_width: u16,
        head: Head,
        ref_name_to_commit_index_map: FxHashMap<String, RawCommitIdx>,
        default_ignore_case: bool,
        default_fuzzy: bool,
        filtered: Option<FilteredGraphData>,
        filtered_graph_colors: Option<FxHashMap<CommitHash, Color>>,
        remote_only_commits: FxHashSet<CommitHash>,
        working_changes: Option<WorkingChanges>,
    ) -> CommitListState<'a> {
        let commit_count = commits.len();
        let has_virtual_row = working_changes.as_ref().is_some_and(|wc| !wc.is_empty());
        let vr_offset = if has_virtual_row { 1 } else { 0 };
        let total = commit_count + vr_offset;
        let name_cell_width = commits
            .iter()
            .map(|c| console::measure_text_width(&c.commit.author_name) as u16)
            .max()
            .unwrap_or(0);
        let commit_hash_to_raw = commits
            .iter()
            .enumerate()
            .map(|(i, c)| (c.commit.commit_hash.clone(), RawCommitIdx(i)))
            .collect();
        CommitListState {
            commits,
            commit_hash_to_raw,
            graph,
            graph_colors,
            head_commit_hash,
            graph_cell_width,
            head,
            filtered,
            filtered_graph_colors,
            ref_name_to_commit_index_map,
            search_state: SearchState::Inactive,
            search_input: Input::default(),
            search_matches: vec![SearchMatch::default(); commit_count],
            last_search_query: String::new(),
            last_matched_indices: Vec::new(),
            last_search_ignore_case: false,
            last_search_fuzzy: false,
            filter_state: FilterState::Inactive,
            filter_input: Input::default(),
            filtered_indices: Vec::new(),
            text_filtered_indices: Vec::new(),
            selected: 0,
            offset: 0,
            total,
            height: 0,
            inline_detail_height: 0,
            show_remote_refs: true,
            remote_only_commits,
            needs_graph_clear: false,
            name_cell_width,
            default_ignore_case,
            default_fuzzy,
            working_changes,
            selected_row_overflows: Cell::new(false),
        }
    }

    pub fn into_graph_parts(self) -> (Option<FilteredGraphData>, FxHashSet<CommitHash>) {
        (self.filtered, self.remote_only_commits)
    }

    pub fn graph_area_cell_width(&self) -> u16 {
        let w = if !self.show_remote_refs {
            self.filtered
                .as_ref()
                .map_or(self.graph_cell_width, |f| f.cell_width)
        } else {
            self.graph_cell_width
        };
        w + 1 // right pad
    }

    pub fn name_cell_width(&self) -> u16 {
        self.name_cell_width
    }

    pub fn set_inline_detail_height(&mut self, h: u16) {
        self.inline_detail_height = h;
    }

    /// Calculate the Rect for inline detail content (right of graph+marker columns).
    /// `content_area` is the commit list content area (below header).
    /// `graph_marker_width` is the combined width of graph + marker columns.
    pub fn inline_detail_rect(&self, content_area: Rect, graph_marker_width: u16) -> Option<Rect> {
        if self.inline_detail_height == 0 {
            return None;
        }
        let y = content_area.top() + self.selected as u16 + 1;
        let x = content_area.left() + graph_marker_width;
        let w = content_area.width.saturating_sub(graph_marker_width);
        if w == 0 || y >= content_area.bottom() {
            return None;
        }
        let h = self
            .inline_detail_height
            .min(content_area.bottom().saturating_sub(y));
        Some(Rect::new(x, y, w, h))
    }

    pub fn toggle_remote_refs(&mut self) -> bool {
        self.show_remote_refs = !self.show_remote_refs;
        self.request_graph_clear();
        self.rebuild_filtered_indices();
        self.show_remote_refs
    }

    pub fn show_remote_refs(&self) -> bool {
        self.show_remote_refs
    }

    /// Restore the remote-refs visibility flag after rebuilding a fresh
    /// `CommitListState` (used by the refresh path to carry the user's
    /// toggle across App instances).
    ///
    /// Contract: the caller is responsible for a full terminal redraw.
    /// The refresh path already does this via `terminal.clear()` in
    /// `lib.rs`, so this setter deliberately does **not** set
    /// `needs_graph_clear` — doing so would cause a double clear and an
    /// extra blank frame.
    ///
    /// Do not call from interactive key handlers. Use `toggle_remote_refs`
    /// for those; it owns the full widget-local invalidation contract.
    pub fn set_show_remote_refs(&mut self, show: bool) {
        if self.show_remote_refs == show {
            return;
        }
        self.show_remote_refs = show;
        // rebuild_filtered_indices clamps selected/offset into the new index
        // space, so callers don't have to care about ordering vs reset_height.
        self.rebuild_filtered_indices();
    }

    pub fn take_graph_clear(&mut self) -> bool {
        std::mem::replace(&mut self.needs_graph_clear, false)
    }

    pub fn request_graph_clear(&mut self) {
        self.needs_graph_clear = true;
    }

    pub fn has_virtual_row(&self) -> bool {
        self.working_changes
            .as_ref()
            .is_some_and(|wc| !wc.is_empty())
    }

    fn virtual_row_offset(&self) -> usize {
        if self.has_virtual_row() {
            1
        } else {
            0
        }
    }

    pub fn is_virtual_row_selected(&self) -> bool {
        self.has_virtual_row() && self.offset + self.selected == 0
    }

    fn first_visible_commit_hash(&self) -> Option<&CommitHash> {
        let idx: RawCommitIdx = if self.filtered_indices.is_empty() {
            RawCommitIdx(0)
        } else {
            *self.filtered_indices.first()?
        };
        self.commits.get(idx.0).map(|c| &c.commit.commit_hash)
    }

    // --- 座標系 accessor / 轉換 ---------------------------------------------
    // 不變式：`self.offset` 與 `self.selected` 的任何直接賦值都必須走
    // `set_visible_selection`，避免 offset + selected 越過 `total`。

    fn commit(&self, idx: RawCommitIdx) -> &CommitInfo<'a> {
        &self.commits[idx.0]
    }

    fn search_match(&self, idx: RawCommitIdx) -> &SearchMatch {
        &self.search_matches[idx.0]
    }

    fn search_match_mut(&mut self, idx: RawCommitIdx) -> &mut SearchMatch {
        &mut self.search_matches[idx.0]
    }

    fn raw_to_filtered(&self, raw: RawCommitIdx) -> Option<FilteredIdx> {
        resolve_raw_to_filtered(&self.filtered_indices, self.commits.len(), raw)
    }

    /// `None` 代表輸入的 `FilteredIdx` 越界。caller 不得 fallback 成 `RawCommitIdx(0)`：
    /// 合法對應只有「早退 / 游標不動」或 `debug_assert!`（render path invariant）。
    fn filtered_to_raw(&self, f: FilteredIdx) -> Option<RawCommitIdx> {
        resolve_filtered_to_raw(&self.filtered_indices, self.commits.len(), f)
    }

    fn visible_to_filtered(&self, v: VisibleIdx) -> FilteredIdx {
        FilteredIdx(v.0.saturating_sub(self.virtual_row_offset()))
    }

    fn filtered_to_visible(&self, f: FilteredIdx) -> VisibleIdx {
        VisibleIdx(f.0 + self.virtual_row_offset())
    }

    fn raw_to_visible(&self, raw: RawCommitIdx) -> Option<VisibleIdx> {
        self.raw_to_filtered(raw)
            .map(|f| self.filtered_to_visible(f))
    }

    fn current_visible(&self) -> VisibleIdx {
        VisibleIdx(self.offset + self.selected)
    }

    /// 唯一允許寫 `self.offset` / `self.selected` 的入口（相對位移除外）。
    fn set_visible_selection(&mut self, target: VisibleIdx) {
        if let Some((offset, selected)) = compute_selection(target, self.total, self.height) {
            self.offset = offset;
            self.selected = selected;
        }
    }

    pub fn working_changes(&self) -> Option<&WorkingChanges> {
        self.working_changes.as_ref()
    }

    fn rebuild_filtered_indices(&mut self) {
        let has_text_filter = !self.filter_input.value().is_empty();
        let has_remote_filter = !self.show_remote_refs;
        let vr = self.virtual_row_offset();
        let prev_visible = self.current_visible();

        if !has_text_filter && !has_remote_filter {
            self.filtered_indices.clear();
            self.total = self.commits.len() + vr;
        } else {
            let base: Box<dyn Iterator<Item = RawCommitIdx>> = if has_text_filter {
                Box::new(self.text_filtered_indices.iter().copied())
            } else {
                Box::new((0..self.commits.len()).map(RawCommitIdx))
            };

            if has_remote_filter {
                self.filtered_indices = base
                    .filter(|raw| {
                        !self
                            .remote_only_commits
                            .contains(self.commits[raw.0].commit_hash())
                    })
                    .collect();
            } else {
                self.filtered_indices = base.collect();
            }

            self.total = self.filtered_indices.len() + vr;
        }

        let clamped = prev_visible.0.min(self.total.saturating_sub(1));
        self.offset = 0;
        self.selected = 0;
        self.set_visible_selection(VisibleIdx(clamped));
    }

    fn effective_scroll_margin(&self) -> usize {
        CURSOR_SCROLL_MARGIN.min(self.height / 3)
    }

    pub fn select_next(&mut self) {
        if self.total == 0 || self.height == 0 {
            return;
        }
        if self.offset + self.selected + 1 >= self.total {
            return;
        }
        let margin = self.effective_scroll_margin();
        let can_scroll_more = self.offset + self.height < self.total;
        let at_bottom_margin = self.selected + margin + 1 >= self.height;

        if at_bottom_margin && can_scroll_more {
            self.offset += 1;
        } else {
            self.selected += 1;
        }
    }

    pub fn select_parent(&mut self) {
        if self.total == 0 || self.is_virtual_row_selected() {
            return;
        }
        if let Some(target_commit) = self.selected_commit_parent_hash().cloned() {
            if self.commit_hash_to_raw.contains_key(&target_commit) {
                while target_commit.as_str() != self.selected_commit_hash().as_str() {
                    self.select_next();
                }
            }
        }
    }

    pub fn selected_commit_parent_hash(&self) -> Option<&CommitHash> {
        if self.total == 0 || self.is_virtual_row_selected() {
            return None;
        }
        self.commit(self.current_selected_raw())
            .commit
            .parent_commit_hashes
            .first()
    }

    pub fn select_prev(&mut self) {
        if self.height == 0 {
            return;
        }
        let at_top_margin = self.selected < self.effective_scroll_margin();

        if at_top_margin && self.offset > 0 {
            self.offset -= 1;
        } else if self.selected > 0 {
            self.selected -= 1;
        } else if self.offset > 0 {
            self.offset -= 1;
        }
    }

    pub fn select_first(&mut self) {
        self.set_visible_selection(VisibleIdx(0));
    }

    pub fn select_last(&mut self) {
        if self.total == 0 {
            return;
        }
        self.set_visible_selection(VisibleIdx(self.total - 1));
    }

    pub fn scroll_down(&mut self) {
        if self.offset + self.height < self.total {
            self.offset += 1;
            if self.selected > 0 {
                self.selected -= 1;
            }
        }
    }

    pub fn scroll_up(&mut self) {
        if self.height == 0 {
            return;
        }
        if self.offset > 0 {
            self.offset -= 1;
            if self.selected < self.height - 1 {
                self.selected += 1;
            }
        }
    }

    pub fn select_next_match(&mut self) {
        if self.commits.is_empty() {
            return;
        }
        self.select_next_match_index(self.current_selected_raw());
    }

    pub fn select_prev_match(&mut self) {
        if self.commits.is_empty() {
            return;
        }
        self.select_prev_match_index(self.current_selected_raw());
    }

    pub fn selected_commit_hash(&self) -> &CommitHash {
        // When virtual row is selected, return first commit hash as fallback
        &self.commit(self.current_selected_raw()).commit.commit_hash
    }

    pub fn selected_commit_subject(&self) -> &str {
        // 鏡像 selected_commit_hash：虛擬行時回 fallback，由 caller 以
        // is_virtual_row_selected() 擋掉。
        &self.commit(self.current_selected_raw()).commit.subject
    }

    pub fn selected_commit_refs(&self) -> &[&'a Ref] {
        if self.is_virtual_row_selected() {
            return &[];
        }
        self.commit(self.current_selected_raw()).refs()
    }

    /// 當前選中的 raw commit index。虛擬行選中時退而求其次回 `RawCommitIdx(0)`。
    /// Invariant：`total > 0` 時必回合法 raw；render path 依此不處理 `None`。
    pub fn current_selected_raw(&self) -> RawCommitIdx {
        let filtered = self.visible_to_filtered(self.current_visible());
        match self.filtered_to_raw(filtered) {
            Some(raw) => raw,
            None => {
                debug_assert!(false, "current_selected_raw: filtered idx out of range");
                RawCommitIdx(0)
            }
        }
    }

    pub fn current_list_status(&self) -> (usize, usize, usize) {
        (self.selected, self.offset, self.height)
    }

    pub fn reset_height(&mut self, height: usize) {
        self.height = height;
    }

    pub fn select_ref(&mut self, ref_name: &str) {
        let Some(&raw) = self.ref_name_to_commit_index_map.get(ref_name) else {
            return;
        };
        if let Some(target) = self.raw_to_visible(raw) {
            self.set_visible_selection(target);
        }
    }

    pub fn select_commit_hash(&mut self, commit_hash: &CommitHash) {
        let Some(&raw) = self.commit_hash_to_raw.get(commit_hash) else {
            return;
        };
        if let Some(target) = self.raw_to_visible(raw) {
            self.set_visible_selection(target);
        }
    }

    /// 把游標移到 HEAD 指向的 commit,畫面比照上下移動的 scroll margin 規則捲動
    /// (不把 HEAD 硬拉到最上面)。HEAD 不存在或被 filter 濾掉時靜默不動。
    pub fn select_head(&mut self) {
        let Some(head) = self.head_commit_hash.clone() else {
            return;
        };
        let Some(&raw) = self.commit_hash_to_raw.get(&head) else {
            return;
        };
        let Some(target) = self.raw_to_visible(raw) else {
            return;
        };
        // 逐格移動 → 自動沿用 select_next / select_prev 的 scroll margin 行為。
        while self.current_visible().0 > target.0 {
            self.select_prev();
        }
        while self.current_visible().0 < target.0 {
            self.select_next();
        }
    }

    pub fn search_state(&self) -> SearchState {
        self.search_state
    }

    pub fn start_search(&mut self) {
        if let SearchState::Inactive | SearchState::Applied { .. } = self.search_state {
            self.search_state = SearchState::Searching {
                start_index: self.current_selected_raw(),
                match_index: 0,
                ignore_case: self.default_ignore_case,
                fuzzy: self.default_fuzzy,
                transient_message: TransientMessage::None,
            };
            self.search_input.reset();
            self.clear_search_matches();
        }
    }

    pub fn handle_search_input(&mut self, key: KeyEvent) {
        if let SearchState::Searching {
            transient_message, ..
        } = &mut self.search_state
        {
            *transient_message = TransientMessage::None;
        }

        if let SearchState::Searching {
            start_index,
            ignore_case,
            fuzzy,
            ..
        } = self.search_state
        {
            self.search_input.handle_event(&Event::Key(key));
            self.update_search_matches(ignore_case, fuzzy);
            self.select_current_or_next_match_index(start_index);
        }
    }

    pub fn apply_search(&mut self) {
        if let SearchState::Searching { match_index, .. } = self.search_state {
            if self.search_input.value().is_empty() {
                self.search_state = SearchState::Inactive;
            } else {
                let total_match = self.search_matches.iter().filter(|m| m.matched()).count();
                self.search_state = SearchState::Applied {
                    match_index,
                    total_match,
                };
            }
        }
    }

    pub fn cancel_search(&mut self) {
        if let SearchState::Searching { .. } | SearchState::Applied { .. } = self.search_state {
            self.search_state = SearchState::Inactive;
            self.search_input.reset();
            self.clear_search_matches();
        }
    }

    pub fn toggle_ignore_case(&mut self) {
        if let SearchState::Searching {
            ignore_case,
            transient_message,
            ..
        } = &mut self.search_state
        {
            *ignore_case = !*ignore_case;
            *transient_message = if *ignore_case {
                TransientMessage::IgnoreCaseOn
            } else {
                TransientMessage::IgnoreCaseOff
            };
        }

        if let SearchState::Searching {
            start_index,
            ignore_case,
            fuzzy,
            ..
        } = self.search_state
        {
            self.update_search_matches(ignore_case, fuzzy);
            self.select_current_or_next_match_index(start_index);
        }
    }

    pub fn toggle_fuzzy(&mut self) {
        if let SearchState::Searching {
            fuzzy,
            transient_message,
            ..
        } = &mut self.search_state
        {
            *fuzzy = !*fuzzy;
            *transient_message = if *fuzzy {
                TransientMessage::FuzzyOn
            } else {
                TransientMessage::FuzzyOff
            };
        }

        if let SearchState::Searching {
            start_index,
            ignore_case,
            fuzzy,
            ..
        } = self.search_state
        {
            self.update_search_matches(ignore_case, fuzzy);
            self.select_current_or_next_match_index(start_index);
        }
    }

    pub fn search_query_string(&self) -> Option<String> {
        if let SearchState::Searching { .. } = self.search_state {
            let query = self.search_input.value();
            Some(format!("/{query}"))
        } else {
            None
        }
    }

    pub fn matched_query_string(&self) -> Option<(String, bool)> {
        if let SearchState::Applied {
            match_index,
            total_match,
            ..
        } = self.search_state
        {
            let query = self.search_input.value();
            if total_match == 0 {
                let msg = format!("No matches found (query: \"{query}\")");
                Some((msg, false))
            } else {
                let msg = format!("Match {match_index} of {total_match} (query: \"{query}\")");
                Some((msg, true))
            }
        } else {
            None
        }
    }

    pub fn search_query_cursor_position(&self) -> u16 {
        self.search_input.visual_cursor() as u16 + 1 // add 1 for "/"
    }

    pub fn transient_message_string(&self) -> Option<String> {
        if let SearchState::Searching {
            transient_message, ..
        } = self.search_state
        {
            match transient_message {
                TransientMessage::None => None,
                TransientMessage::IgnoreCaseOn => Some("Ignore case: ON ".to_string()),
                TransientMessage::IgnoreCaseOff => Some("Ignore case: OFF".to_string()),
                TransientMessage::FuzzyOn => Some("Fuzzy match: ON ".to_string()),
                TransientMessage::FuzzyOff => Some("Fuzzy match: OFF".to_string()),
            }
        } else {
            None
        }
    }

    fn update_search_matches(&mut self, ignore_case: bool, fuzzy: bool) {
        let query = self.search_input.value().to_string();

        // Early return for empty query
        if query.is_empty() {
            self.clear_search_matches();
            self.last_search_query.clear();
            self.last_matched_indices.clear();
            return;
        }

        let matcher = SearchMatcher::new(&query, ignore_case, fuzzy);

        // Determine if we can use incremental search:
        // - New query extends the previous query (user typing more chars)
        // - Same search settings (ignore_case, fuzzy)
        let settings_unchanged =
            ignore_case == self.last_search_ignore_case && fuzzy == self.last_search_fuzzy;
        let can_use_incremental = settings_unchanged
            && !self.last_search_query.is_empty()
            && query.starts_with(&self.last_search_query)
            && !self.last_matched_indices.is_empty();

        // 增量搜尋只是換候選來源，比對本身一模一樣 —— 兩條路徑各寫一份迴圈的話，
        // 最不能分歧的 `match_index` 發號就有兩個地方會錯。
        // `mem::take` 避免對 Vec 做額外 clone；函式結尾會覆寫回去。
        let candidates: Vec<RawCommitIdx> = if can_use_incremental {
            std::mem::take(&mut self.last_matched_indices)
        } else {
            (0..self.commits.len()).map(RawCommitIdx).collect()
        };
        self.clear_search_matches();

        let mut new_matched_indices = Vec::new();
        let mut match_index = 1;
        for raw in candidates {
            // 不先用 `commit_quick_matches` 篩：那道閘門在這裡省不到東西。
            // `SearchMatch::new` 不會短路，命中與否都要把每個欄位跑完，所以閘門對
            // 沒命中的 commit 成本相同、對命中的則是純粹多跑一趟。少了它，
            // `matched()` 也就成了「這列算不算命中」的唯一來源。
            let mut m = SearchMatch::new(self.commit(raw), &matcher);
            if m.matched() {
                m.match_index = match_index;
                match_index += 1;
                *self.search_match_mut(raw) = m;
                new_matched_indices.push(raw);
            }
        }

        self.last_search_query = query;
        self.last_matched_indices = new_matched_indices;
        self.last_search_ignore_case = ignore_case;
        self.last_search_fuzzy = fuzzy;
    }

    /// filter 只要 bool，`any()` 命中即停。
    ///
    /// **別把它換成 `SearchMatch::new(..).matched()`** —— 後者不會短路，會對每個欄位
    /// 算完整的 highlight 位置再全部丟掉。查詢 `a` 打在大 repo 上時幾乎每列都命中
    /// subject，那是每次按鍵好幾倍的差距。反過來，搜尋路徑不該用這道閘門：它在那裡
    /// 只是把同一份比對多跑一次（見 `update_search_matches`）。
    fn commit_quick_matches(matcher: &SearchMatcher, commit_info: &CommitInfo<'_>) -> bool {
        search_fields(commit_info).any(|f| matcher.matches(f.text()))
    }

    fn clear_search_matches(&mut self) {
        self.search_matches.iter_mut().for_each(|m| m.clear());
    }

    fn select_current_or_next_match_index(&mut self, current: RawCommitIdx) {
        if self.search_match(current).matched() && self.is_raw_visible(current) {
            self.select_raw(current);
            let mi = self.search_match(current).match_index;
            self.search_state.update_match_index(mi);
        } else {
            self.select_next_match_index(current)
        }
    }

    fn select_next_match_index(&mut self, current: RawCommitIdx) {
        self.select_match_in_direction(current, MatchStep::Next);
    }

    fn select_prev_match_index(&mut self, current: RawCommitIdx) {
        self.select_match_in_direction(current, MatchStep::Prev);
    }

    fn select_match_in_direction(&mut self, current: RawCommitIdx, step: MatchStep) {
        let len = self.commits.len();
        if len == 0 {
            return;
        }
        let advance = |i: usize| match step {
            MatchStep::Next => (i + 1) % len,
            MatchStep::Prev => (i + len - 1) % len,
        };
        let mut i = advance(current.0);
        while i != current.0 {
            let raw = RawCommitIdx(i);
            if self.search_match(raw).matched() && self.is_raw_visible(raw) {
                self.select_raw(raw);
                let mi = self.search_match(raw).match_index;
                self.search_state.update_match_index(mi);
                return;
            }
            i = advance(i);
        }
    }

    fn is_raw_visible(&self, raw: RawCommitIdx) -> bool {
        self.raw_to_filtered(raw).is_some()
    }

    fn select_raw(&mut self, raw: RawCommitIdx) {
        if let Some(target) = self.raw_to_visible(raw) {
            self.set_visible_selection(target);
        }
    }

    fn current_graph(&self) -> &Graph {
        if !self.show_remote_refs {
            if let Some(ref f) = self.filtered {
                return &f.graph;
            }
        }
        &self.graph
    }

    fn text_cells_for_hash(&self, hash: &CommitHash) -> Option<Vec<TextCell>> {
        crate::graph::text_cells(self.current_graph(), hash, &self.graph_colors)
    }

    fn marker_color(&self, commit_info: &CommitInfo<'_>) -> Color {
        if !self.show_remote_refs {
            if let Some(ref colors) = self.filtered_graph_colors {
                if let Some(&color) = colors.get(commit_info.commit_hash()) {
                    return color;
                }
            }
        }
        commit_info.graph_color
    }

    // Filter mode methods

    pub fn filter_state(&self) -> FilterState {
        self.filter_state
    }

    pub fn start_filter(&mut self) {
        if let FilterState::Inactive = self.filter_state {
            // Filter mode uses fuzzy by default for better UX
            self.filter_state = FilterState::Filtering {
                ignore_case: true,
                fuzzy: true,
                transient_message: TransientMessage::None,
            };
            self.filter_input.reset();
            self.filtered_indices.clear();
            self.update_filter();
        }
    }

    pub fn handle_filter_input(&mut self, key: KeyEvent) {
        if let FilterState::Filtering {
            transient_message, ..
        } = &mut self.filter_state
        {
            *transient_message = TransientMessage::None;
        }

        if let FilterState::Filtering {
            ignore_case, fuzzy, ..
        } = self.filter_state
        {
            self.filter_input.handle_event(&Event::Key(key));
            self.update_filter_matches(ignore_case, fuzzy);
        }
    }

    pub fn cancel_filter(&mut self) {
        self.filter_state = FilterState::Inactive;
        self.filter_input.reset();
        self.text_filtered_indices.clear();
        self.rebuild_filtered_indices();
        self.set_visible_selection(VisibleIdx(0));
    }

    pub fn apply_filter(&mut self) {
        if let FilterState::Filtering { .. } = self.filter_state {
            self.filter_state = FilterState::Inactive;
            // Keep filtered_indices active
        }
    }

    pub fn toggle_filter_ignore_case(&mut self) {
        if let FilterState::Filtering {
            ignore_case,
            fuzzy,
            transient_message,
        } = &mut self.filter_state
        {
            *ignore_case = !*ignore_case;
            *transient_message = if *ignore_case {
                TransientMessage::IgnoreCaseOn
            } else {
                TransientMessage::IgnoreCaseOff
            };
            let ic = *ignore_case;
            let fz = *fuzzy;
            self.update_filter_matches(ic, fz);
        }
    }

    pub fn toggle_filter_fuzzy(&mut self) {
        if let FilterState::Filtering {
            ignore_case,
            fuzzy,
            transient_message,
        } = &mut self.filter_state
        {
            *fuzzy = !*fuzzy;
            *transient_message = if *fuzzy {
                TransientMessage::FuzzyOn
            } else {
                TransientMessage::FuzzyOff
            };
            let ic = *ignore_case;
            let fz = *fuzzy;
            self.update_filter_matches(ic, fz);
        }
    }

    pub fn filter_query_string(&self) -> Option<String> {
        if let FilterState::Filtering { .. } = self.filter_state {
            Some(format!("filter: {}", self.filter_input.value()))
        } else {
            None
        }
    }

    pub fn filter_query_cursor_position(&self) -> u16 {
        // "filter: " prefix is 8 chars
        8 + self.filter_input.visual_cursor() as u16
    }

    pub fn filter_transient_message_string(&self) -> Option<String> {
        if let FilterState::Filtering {
            transient_message, ..
        } = self.filter_state
        {
            match transient_message {
                TransientMessage::None => None,
                TransientMessage::IgnoreCaseOn => Some("Ignore case: ON ".to_string()),
                TransientMessage::IgnoreCaseOff => Some("Ignore case: OFF".to_string()),
                TransientMessage::FuzzyOn => Some("Fuzzy match: ON ".to_string()),
                TransientMessage::FuzzyOff => Some("Fuzzy match: OFF".to_string()),
            }
        } else {
            None
        }
    }

    fn update_filter(&mut self) {
        if let FilterState::Filtering {
            ignore_case, fuzzy, ..
        } = self.filter_state
        {
            self.update_filter_matches(ignore_case, fuzzy);
        }
    }

    fn update_filter_matches(&mut self, ignore_case: bool, fuzzy: bool) {
        let query = self.filter_input.value().to_string();

        self.text_filtered_indices.clear();

        if !query.is_empty() {
            let matcher = SearchMatcher::new(&query, ignore_case, fuzzy);
            for (i, commit_info) in self.commits.iter().enumerate() {
                if Self::commit_quick_matches(&matcher, commit_info) {
                    self.text_filtered_indices.push(RawCommitIdx(i));
                }
            }
        }

        self.rebuild_filtered_indices();
        self.set_visible_selection(VisibleIdx(0));
    }
}

pub struct CommitList<'a> {
    ctx: Rc<AppContext>,
    marquee_frame: u64,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a> CommitList<'a> {
    pub fn new(ctx: Rc<AppContext>, marquee_frame: u64) -> Self {
        Self {
            ctx,
            marquee_frame,
            _marker: std::marker::PhantomData,
        }
    }
}

impl<'a> StatefulWidget for CommitList<'a> {
    type State = CommitListState<'a>;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        if area.height < 2 {
            return;
        }

        let [header_area, content_area] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);

        self.update_state(content_area, state);

        let name_width = if state.name_cell_width() > 0 {
            state.name_cell_width()
        } else {
            self.ctx.ui_config.list.name_width
        };
        let constraints = calc_cell_widths(
            area.width,
            self.ctx.ui_config.list.subject_min_width,
            state.graph_area_cell_width(),
            name_width,
            self.ctx.ui_config.list.date_width,
            &self.ctx.ui_config.list.columns,
        );

        let header_chunks = Layout::horizontal(constraints.clone()).split(header_area);
        let header_style = Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD);
        for (i, col) in self.ctx.ui_config.list.columns.iter().enumerate() {
            let title = match col {
                UserListColumnType::Graph => "Graph",
                UserListColumnType::Marker => "",
                UserListColumnType::Subject => "Description",
                UserListColumnType::Name => "Author",
                UserListColumnType::Hash => "Commit",
                UserListColumnType::Date => "Date",
            };
            if !title.is_empty() {
                Paragraph::new(Line::from(vec![
                    Span::raw(" "),
                    Span::styled(title, header_style),
                ]))
                .render(header_chunks[i], buf);
            }
        }

        let content_chunks = Layout::horizontal(constraints).split(content_area);

        for (i, col) in self.ctx.ui_config.list.columns.iter().enumerate() {
            match col {
                UserListColumnType::Graph => {
                    self.render_graph(buf, content_chunks[i], state);
                }
                UserListColumnType::Marker => {
                    self.render_marker(buf, content_chunks[i], state);
                }
                UserListColumnType::Subject => {
                    self.render_subject(buf, content_chunks[i], state);
                }
                UserListColumnType::Name => {
                    self.render_name(buf, content_chunks[i], state);
                }
                UserListColumnType::Hash => {
                    self.render_hash(buf, content_chunks[i], state);
                }
                UserListColumnType::Date => {
                    self.render_date(buf, content_chunks[i], state);
                }
            }
        }
    }
}

impl CommitList<'_> {
    fn update_state(&self, area: Rect, state: &mut CommitListState<'_>) {
        state.height = (area.height as usize).saturating_sub(state.inline_detail_height as usize);

        if state.total > state.height && state.total - state.height < state.offset {
            let diff = state.offset - (state.total - state.height);
            state.selected += diff;
            state.offset -= diff;
        }
        if state.selected >= state.height {
            let diff = state.selected - state.height + 1;
            state.selected -= diff;
            state.offset += diff;
        }
        // Visible-row text cells are computed on demand by render_graph via
        // rendering_commit_info_iter(), which is the single source of truth
        // for "what rows are visible" -- no separate preload pass needed.
    }

    /// Single seam for GraphStyle -> GlyphSet selection.
    fn glyphs(&self) -> GlyphSet {
        GlyphSet::from_style(self.ctx.graph_style)
    }

    fn render_graph(&self, buf: &mut Buffer, area: Rect, state: &CommitListState<'_>) {
        if area.is_empty() {
            return;
        }
        let gap = state.inline_detail_height;
        let head_hash = state.head_commit_hash.as_ref();
        let selected_bg = ratatui_color_to_rgb(self.ctx.color_theme.list_selected_bg);

        let head_col = head_hash.and_then(|h| self.graph_text_head_col(state, h));
        let virtual_row_visible = state.has_virtual_row() && state.offset == 0;

        if virtual_row_visible {
            let y = area.top();
            // ◯ fallback 次序：HEAD column → 第一個可見 commit 的 dot column → 0
            let col = head_col.unwrap_or_else(|| {
                state
                    .first_visible_commit_hash()
                    .and_then(|h| self.graph_text_head_col(state, h))
                    .unwrap_or(0)
            });
            self.put_text_cell(buf, area, y, col, Glyph::HeadDot, VIRTUAL_ROW_COLOR);
            if state.selected == 0 {
                apply_row_bg(buf, area, y, selected_bg);
            }
        }

        let head_line_col = head_col.filter(|_| virtual_row_visible);
        let mut seen_head = false;
        for (display_i, _, commit_info) in self.rendering_commit_info_iter(state) {
            let y_offset = if gap > 0 && display_i > state.selected {
                gap
            } else {
                0
            };
            let y = area.top() + display_i as u16 + y_offset;
            if y >= area.bottom() {
                continue;
            }
            let hash = &commit_info.commit.commit_hash;
            // `None` here now means only one thing: `hash` isn't in
            // `current_graph().commit_pos_map` -- i.e. the graph and the
            // commit list have desynced. There's no "not preloaded yet"
            // case anymore since text cells are computed on demand.
            let Some(cells) = state.text_cells_for_hash(hash) else {
                continue;
            };
            let is_head = head_hash == Some(hash);
            let is_selected = display_i == state.selected;
            self.put_text_cells(buf, area, y, &cells, is_head);

            if !seen_head {
                if is_head {
                    seen_head = true;
                } else if let Some(hc) = head_line_col {
                    if cells.get(hc).is_some_and(|c| c.glyph == Glyph::Blank) {
                        self.put_text_cell(buf, area, y, hc, Glyph::Vert, VIRTUAL_ROW_COLOR);
                    }
                }
            }

            if is_selected {
                apply_row_bg(buf, area, y, selected_bg);
            }
        }

        // Spacer rows (inline detail gap): draw `│` at each active column.
        if gap > 0 {
            let spacer_hash = if state.is_virtual_row_selected() {
                state.first_visible_commit_hash().cloned()
            } else {
                Some(
                    state
                        .commit(state.current_selected_raw())
                        .commit
                        .commit_hash
                        .clone(),
                )
            };
            if let Some(hash) = spacer_hash {
                if let Some(cells) = state.text_cells_for_hash(&hash) {
                    let gray = state.is_virtual_row_selected();
                    for gap_row in 0..gap {
                        let y = area.top() + state.selected as u16 + 1 + gap_row;
                        if y >= area.bottom() {
                            break;
                        }
                        self.put_text_spacer(buf, area, y, &cells, gray);
                    }
                }
            }
        }
    }

    /// Returns the text-graph column (in cells, not chars) of `hash` on the
    /// current graph, or None if missing.
    fn graph_text_head_col(&self, state: &CommitListState<'_>, hash: &CommitHash) -> Option<usize> {
        let cells = state.text_cells_for_hash(hash)?;
        cells
            .iter()
            .position(|c| matches!(c.glyph, Glyph::CommitDot | Glyph::HeadDot))
    }

    fn put_text_cells(
        &self,
        buf: &mut Buffer,
        area: Rect,
        y: u16,
        cells: &[TextCell],
        is_head: bool,
    ) {
        let glyphs = self.glyphs();
        for (i, cell) in cells.iter().enumerate() {
            let x = area.left() + i as u16;
            if x >= area.right() {
                break;
            }
            let (glyph, bold) = if is_head && cell.glyph == Glyph::CommitDot {
                (Glyph::HeadDot, true)
            } else {
                (cell.glyph, false)
            };
            let s = glyphs.resolve(glyph);
            let mut style = Style::default().fg(cell.color);
            if bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            buf[(x, y)].set_symbol(s).set_style(style);
        }
    }

    fn put_text_spacer(
        &self,
        buf: &mut Buffer,
        area: Rect,
        y: u16,
        cells: &[TextCell],
        gray: bool,
    ) {
        let glyphs = self.glyphs();
        for (i, cell) in cells.iter().enumerate() {
            let x = area.left() + i as u16;
            if x >= area.right() {
                break;
            }
            // Horizontal-only edges don't extend into the spacer row, so only
            // redraw `│` at columns that had a dot or vertical-reaching glyph.
            let draw_vertical = matches!(
                cell.glyph,
                Glyph::CommitDot
                    | Glyph::Vert
                    | Glyph::HeadDot
                    | Glyph::CornerTL
                    | Glyph::CornerTR
                    | Glyph::CornerBL
                    | Glyph::CornerBR
            );
            if !draw_vertical {
                continue;
            }
            let color = if gray { VIRTUAL_ROW_COLOR } else { cell.color };
            let s = glyphs.resolve(Glyph::Vert);
            buf[(x, y)]
                .set_symbol(s)
                .set_style(Style::default().fg(color));
        }
    }

    fn put_text_cell(
        &self,
        buf: &mut Buffer,
        area: Rect,
        y: u16,
        col: usize,
        glyph: Glyph,
        color: Color,
    ) {
        let x = area.left() + col as u16;
        if x >= area.right() {
            return;
        }
        let s = self.glyphs().resolve(glyph);
        buf[(x, y)]
            .set_symbol(s)
            .set_style(Style::default().fg(color));
    }

    fn render_marker(&self, buf: &mut Buffer, area: Rect, state: &CommitListState<'_>) {
        if area.is_empty() {
            return;
        }
        let gap = state.inline_detail_height;
        let vert = self.glyphs().vert;
        let mut items: Vec<ListItem> = Vec::new();
        if state.has_virtual_row() && state.offset == 0 {
            let mut line = Line::from(vert.fg(Color::Gray));
            if state.selected == 0 {
                line = line
                    .bg(ratatui_color_to_rgb(self.ctx.color_theme.list_selected_bg))
                    .fg(Color::Gray);
            }
            items.push(ListItem::new(line));
            // Insert marker gap when virtual row is selected
            if gap > 0 && state.selected == 0 {
                for _ in 0..gap {
                    items.push(ListItem::new(vert.fg(Color::Gray)));
                }
            }
        }
        self.rendering_commit_info_iter(state)
            .for_each(|(display_i, _, commit_info)| {
                let color = state.marker_color(commit_info);
                let mut line = Line::from(vert.fg(color));
                if display_i == state.selected {
                    line = line.bg(ratatui_color_to_rgb(self.ctx.color_theme.list_selected_bg));
                }
                items.push(ListItem::new(line));
                if gap > 0 && display_i == state.selected && !state.is_virtual_row_selected() {
                    let sel_color = state.marker_color(state.commit(state.current_selected_raw()));
                    for _ in 0..gap {
                        items.push(ListItem::new(vert.fg(sel_color)));
                    }
                }
            });
        Widget::render(List::new(items), area, buf)
    }

    fn insert_gap<'b>(
        items: &mut Vec<ListItem<'b>>,
        state: &CommitListState<'_>,
        is_virtual: bool,
        display_i: usize,
    ) {
        let gap = state.inline_detail_height;
        if gap == 0 {
            return;
        }
        let should_insert = if is_virtual {
            state.is_virtual_row_selected()
        } else {
            display_i == state.selected && !state.is_virtual_row_selected()
        };
        if should_insert {
            for _ in 0..gap {
                items.push(ListItem::new(Line::raw("")));
            }
        }
    }

    fn render_subject(&self, buf: &mut Buffer, area: Rect, state: &CommitListState<'_>) {
        let max_width = (area.width as usize).saturating_sub(2);
        if area.is_empty() || max_width == 0 {
            state.selected_row_overflows.set(false);
            return;
        }
        let mut items: Vec<ListItem> = Vec::new();
        let mut any_selected_overflow = false;
        let marquee_frame = self.marquee_frame;
        let selected = state.selected;
        // Virtual row
        if state.has_virtual_row() && state.offset == 0 {
            let count = state.working_changes().map_or(0, |wc| wc.file_count());
            let text = format!("Uncommitted Changes ({count})");
            let spans = vec![Span::styled(
                text,
                Style::default()
                    .fg(VIRTUAL_ROW_COLOR)
                    .add_modifier(Modifier::ITALIC),
            )];
            items.push(self.to_commit_list_item(0, spans, state));
            Self::insert_gap(&mut items, state, true, 0);
        }
        self.rendering_commit_info_iter(state)
            .for_each(|(display_i, raw, commit_info)| {
                let mut spans = refs_spans(
                    commit_info,
                    &state.head,
                    &state.search_match(raw).refs,
                    &self.ctx.color_theme,
                    state.show_remote_refs,
                );
                let ref_spans_width: usize = spans.iter().map(|s| s.width()).sum();
                let avail = max_width.saturating_sub(ref_spans_width);
                let commit = &commit_info.commit;
                if avail > ELLIPSIS.len() {
                    // byte-len 是視覺寬度的下界（ASCII 相等、非 ASCII byte 更多），
                    // 用它先短路大多數「明顯放得下」的 row，省一次寬度計算。
                    // 寬度基準必須跟 `scroll_window` 一致，理由見 `marquee::display_width`。
                    let overflow = commit.subject.len() > avail
                        && crate::widget::marquee::display_width(&commit.subject) > avail;
                    let is_selected = display_i == selected;
                    let search_pos = state.search_match(raw).subject.as_ref();
                    let sub_spans = if is_selected && overflow {
                        any_selected_overflow = true;
                        marquee_subject_spans(
                            &commit.subject,
                            avail,
                            marquee_frame,
                            search_pos,
                            &self.ctx.color_theme,
                        )
                    } else {
                        let subject = if overflow {
                            console::truncate_str(&commit.subject, avail, ELLIPSIS).to_string()
                        } else {
                            commit.subject.to_string()
                        };
                        if let Some(pos) = search_pos {
                            highlighted_spans(
                                subject.into(),
                                pos.clone(),
                                self.ctx.color_theme.list_subject_fg,
                                Modifier::empty(),
                                &self.ctx.color_theme,
                                overflow,
                            )
                        } else {
                            vec![subject.fg(self.ctx.color_theme.list_subject_fg)]
                        }
                    };
                    spans.extend(sub_spans);
                }
                items.push(self.to_commit_list_item(display_i, spans, state));
                Self::insert_gap(&mut items, state, false, display_i);
            });
        state.selected_row_overflows.set(any_selected_overflow);
        Widget::render(List::new(items), area, buf);
    }

    fn render_name(&self, buf: &mut Buffer, area: Rect, state: &CommitListState<'_>) {
        let max_width = (area.width as usize).saturating_sub(2);
        if area.is_empty() || max_width == 0 {
            return;
        }
        let mut items: Vec<ListItem> = Vec::new();
        if state.has_virtual_row() && state.offset == 0 {
            items.push(self.to_commit_list_item(0, vec!["-".fg(VIRTUAL_ROW_COLOR)], state));
            Self::insert_gap(&mut items, state, true, 0);
        }
        self.rendering_commit_iter(state)
            .for_each(|(display_i, raw, commit)| {
                let truncate = console::measure_text_width(&commit.author_name) > max_width;
                let name = if truncate {
                    console::truncate_str(&commit.author_name, max_width, ELLIPSIS).to_string()
                } else {
                    commit.author_name.to_string()
                };
                let spans = if let Some(pos) = state.search_match(raw).author_name.clone() {
                    highlighted_spans(
                        name.into(),
                        pos,
                        self.ctx.color_theme.list_name_fg,
                        Modifier::empty(),
                        &self.ctx.color_theme,
                        truncate,
                    )
                } else {
                    vec![name.fg(self.ctx.color_theme.list_name_fg)]
                };
                items.push(self.to_commit_list_item(display_i, spans, state));
                Self::insert_gap(&mut items, state, false, display_i);
            });
        Widget::render(List::new(items), area, buf);
    }

    fn render_hash(&self, buf: &mut Buffer, area: Rect, state: &CommitListState<'_>) {
        if area.is_empty() {
            return;
        }
        let mut items: Vec<ListItem> = Vec::new();
        if state.has_virtual_row() && state.offset == 0 {
            items.push(self.to_commit_list_item(0, vec!["-".fg(VIRTUAL_ROW_COLOR)], state));
            Self::insert_gap(&mut items, state, true, 0);
        }
        self.rendering_commit_iter(state)
            .for_each(|(display_i, raw, commit)| {
                let hash = commit.commit_hash.as_short_hash();
                let spans = if let Some(pos) = state.search_match(raw).commit_hash.clone() {
                    highlighted_spans(
                        hash.into(),
                        pos,
                        self.ctx.color_theme.list_hash_fg,
                        Modifier::empty(),
                        &self.ctx.color_theme,
                        false,
                    )
                } else {
                    vec![hash.fg(self.ctx.color_theme.list_hash_fg)]
                };
                items.push(self.to_commit_list_item(display_i, spans, state));
                Self::insert_gap(&mut items, state, false, display_i);
            });
        Widget::render(List::new(items), area, buf);
    }

    fn render_date(&self, buf: &mut Buffer, area: Rect, state: &CommitListState<'_>) {
        if area.is_empty() {
            return;
        }
        let mut items: Vec<ListItem> = Vec::new();
        if state.has_virtual_row() && state.offset == 0 {
            items.push(self.to_commit_list_item(0, vec!["-".fg(VIRTUAL_ROW_COLOR)], state));
            Self::insert_gap(&mut items, state, true, 0);
        }
        self.rendering_commit_iter(state)
            .for_each(|(display_i, _raw, commit)| {
                let date = &commit.author_date;
                let date_str = if self.ctx.ui_config.list.date_local {
                    let local = date.with_timezone(&chrono::Local);
                    local
                        .format(&self.ctx.ui_config.list.date_format)
                        .to_string()
                } else {
                    date.format(&self.ctx.ui_config.list.date_format)
                        .to_string()
                };
                items.push(self.to_commit_list_item(
                    display_i,
                    vec![date_str.fg(self.ctx.color_theme.list_date_fg)],
                    state,
                ));
                Self::insert_gap(&mut items, state, false, display_i);
            });
        Widget::render(List::new(items), area, buf);
    }

    /// Returns iterator of (display_idx, raw_idx, &CommitInfo)
    /// display_idx: position on screen (0, 1, 2, ...)
    /// raw_idx: actual index in commits Vec (for search_matches access)
    /// Skips the virtual row (if present and visible).
    fn rendering_commit_info_iter<'b>(
        &'b self,
        state: &'b CommitListState<'_>,
    ) -> impl Iterator<Item = (usize, RawCommitIdx, &'b CommitInfo<'b>)> {
        let vr_offset = state.virtual_row_offset();
        let total_visible = state.height.min(state.total.saturating_sub(state.offset));
        let start = if state.offset == 0 { vr_offset } else { 0 };
        (start..total_visible).filter_map(move |display_idx| {
            let visible_idx = state.offset + display_idx;
            let filtered = FilteredIdx(visible_idx - vr_offset);
            let raw = state.filtered_to_raw(filtered)?;
            Some((display_idx, raw, state.commit(raw)))
        })
    }

    fn rendering_commit_iter<'b>(
        &'b self,
        state: &'b CommitListState<'_>,
    ) -> impl Iterator<Item = (usize, RawCommitIdx, &'b Commit)> {
        self.rendering_commit_info_iter(state)
            .map(|(display_i, raw, commit_info)| (display_i, raw, commit_info.commit))
    }

    fn to_commit_list_item<'a, 'b>(
        &'b self,
        i: usize,
        spans: Vec<Span<'a>>,
        state: &'b CommitListState<'_>,
    ) -> ListItem<'a> {
        let mut spans = spans;
        spans.insert(0, Span::raw(" "));
        spans.push(Span::raw(" "));
        let mut line = Line::from(spans);
        if i == state.selected {
            line = line
                .bg(ratatui_color_to_rgb(self.ctx.color_theme.list_selected_bg))
                .fg(self.ctx.color_theme.list_selected_fg);
        }
        ListItem::new(line)
    }
}

fn apply_row_bg(buf: &mut Buffer, area: Rect, y: u16, bg: Color) {
    // Overwrite only the bg channel so previously-written fg/modifier on the
    // graph cells survives.
    for x in area.left()..area.right() {
        buf[(x, y)].set_bg(bg);
    }
}

fn refs_spans<'a>(
    commit_info: &'a CommitInfo<'_>,
    head: &'a Head,
    refs_matches: &'a FxHashMap<String, SearchMatchPosition>,
    color_theme: &'a ColorTheme,
    show_remote_refs: bool,
) -> Vec<Span<'a>> {
    let refs = &commit_info.refs;

    if refs.len() == 1 {
        if let Ref::Stash { name, .. } = refs[0] {
            return vec![
                Span::raw(name.clone())
                    .fg(color_theme.list_ref_stash_fg)
                    .bold(),
                Span::raw(" "),
            ];
        }
    }

    let is_head_branch = |n: &str| matches!(head, Head::Branch { name: hn } if hn == n);
    // tag arm 高亮條件：detached HEAD 指向此 commit。
    let is_head_detached_here = matches!(
        head,
        Head::Detached { target } if commit_info.commit.commit_hash == *target,
    );

    let ref_spans: Vec<(Vec<Span>, &String)> = refs
        .iter()
        .filter_map(|r| match r {
            Ref::Branch { name, .. } => {
                // 如果存在對應的 remote branch，隱藏本地分支（HEAD branch 也適用：
                // 此時 RemoteBranch arm 內會把對應的 dev 部分高亮表達 HEAD）。
                let has_remote = refs.iter().any(|r| {
                    matches!(r, Ref::RemoteBranch { name: rn, .. } if rn.ends_with(&format!("/{name}")))
                });
                if has_remote && show_remote_refs {
                    return None;
                }
                let is_head = is_head_branch(name);
                let fg = color_theme.list_ref_branch_fg;
                let mut spans = refs_matches
                    .get(name)
                    .map(|pos| {
                        highlighted_spans(
                            name.into(),
                            pos.clone(),
                            fg,
                            Modifier::BOLD,
                            color_theme,
                            false,
                        )
                    })
                    .unwrap_or_else(|| vec![Span::raw(name).fg(fg).bold()]);
                if is_head {
                    spans = highlight_as_head(spans, color_theme);
                }
                Some((spans, name))
            }
            Ref::RemoteBranch { name, .. } => {
                if !show_remote_refs {
                    return None;
                }
                // 三段分色：remote(紅) + /(paren色) + branch_name
                // 有對應本地分支 → branch_name 綠色，否則紅色
                let spans = if let Some(slash_pos) = name.find('/') {
                    let remote_part = &name[..slash_pos];
                    let branch_part = &name[slash_pos + 1..];
                    let has_local = refs.iter().any(|r| {
                        matches!(r, Ref::Branch { name: ln, .. } if ln == branch_part)
                    });
                    if has_local {
                        let is_head = is_head_branch(branch_part);
                        let mut branch_span = Span::raw(branch_part.to_string()).bold();
                        branch_span = if is_head {
                            branch_span
                                .fg(Color::Black)
                                .bg(color_theme.list_head_fg)
                        } else {
                            branch_span.fg(color_theme.list_ref_branch_fg)
                        };
                        vec![
                            Span::raw(remote_part.to_string())
                                .fg(color_theme.list_ref_remote_branch_fg)
                                .bold(),
                            Span::raw("/")
                                .fg(color_theme.list_ref_paren_fg)
                                .bold(),
                            branch_span,
                        ]
                    } else {
                        vec![Span::raw(name)
                            .fg(color_theme.list_ref_remote_branch_fg)
                            .bold()]
                    }
                } else {
                    vec![Span::raw(name)
                        .fg(color_theme.list_ref_remote_branch_fg)
                        .bold()]
                };
                Some((spans, name))
            }
            Ref::Tag { name, .. } => {
                let fg = color_theme.list_ref_tag_fg;
                let mut spans = refs_matches
                    .get(name)
                    .map(|pos| {
                        highlighted_spans(
                            name.into(),
                            pos.clone(),
                            fg,
                            Modifier::BOLD,
                            color_theme,
                            false,
                        )
                    })
                    .unwrap_or_else(|| vec![Span::raw(name).fg(fg).bold()]);
                if is_head_detached_here {
                    spans = highlight_as_head(spans, color_theme);
                }
                Some((spans, name))
            }
            Ref::Stash { .. } => None,
        })
        .collect();

    let mut spans = vec![Span::raw("(").fg(color_theme.list_ref_paren_fg).bold()];

    // HEAD（含 detached）由 graph 上的空心圓表達，文字不再顯示。

    let refs_len = ref_spans.len();
    for (i, ss) in ref_spans.into_iter().enumerate() {
        let (ref_spans, _ref_name) = ss;
        spans.extend(ref_spans);
        if i < refs_len - 1 {
            spans.push(Span::raw(", ").fg(color_theme.list_ref_paren_fg).bold());
        }
    }

    spans.push(Span::raw(") ").fg(color_theme.list_ref_paren_fg).bold());

    if spans.len() == 2 {
        spans.clear(); // contains only "(" and ")", so clear it
    }

    spans
}

fn highlight_as_head<'a>(spans: Vec<Span<'a>>, color_theme: &ColorTheme) -> Vec<Span<'a>> {
    spans
        .into_iter()
        .map(|s| s.fg(Color::Black).bg(color_theme.list_head_fg))
        .collect()
}

/// 回傳 marquee 視窗內的 subject spans。Scroll offset 由
/// `crate::widget::marquee::scroll_window` 處理；這邊只負責 search highlight。
fn marquee_subject_spans(
    subject: &str,
    available: usize,
    marquee_frame: u64,
    search_pos: Option<&SearchMatchPosition>,
    color_theme: &ColorTheme,
) -> Vec<Span<'static>> {
    let slice = crate::widget::marquee::scroll_window(subject, available, marquee_frame);

    if let Some(pos) = search_pos {
        let shift = if slice.prepended_space { 1 } else { 0 };
        let translated: Vec<usize> = pos
            .matched_indices
            .iter()
            .copied()
            .filter(|&bi| bi >= slice.start_byte && bi < slice.end_byte)
            .map(|bi| bi - slice.start_byte + shift)
            .collect();
        highlighted_spans(
            slice.text.into(),
            SearchMatchPosition::new(translated),
            color_theme.list_subject_fg,
            Modifier::empty(),
            color_theme,
            false,
        )
    } else {
        vec![slice.text.fg(color_theme.list_subject_fg)]
    }
}

fn highlighted_spans(
    s: Span<'_>,
    pos: SearchMatchPosition,
    base_fg: Color,
    base_modifier: Modifier,
    color_theme: &ColorTheme,
    truncate: bool,
) -> Vec<Span<'static>> {
    let mut hm = highlight_matched_text(vec![s])
        .matched_indices(pos.matched_indices)
        .not_matched_style(Style::default().fg(base_fg).add_modifier(base_modifier))
        .matched_style(
            Style::default()
                .fg(color_theme.list_match_fg)
                .bg(color_theme.list_match_bg)
                .add_modifier(base_modifier),
        );
    if truncate {
        hm = hm.ellipsis(ELLIPSIS);
    }
    hm.into_spans()
}

fn calc_cell_widths(
    area_width: u16,
    subject_min_width: u16,
    graph_width: u16,
    name_width: u16,
    date_width: u16,
    columns: &[UserListColumnType],
) -> Vec<Constraint> {
    let pad = 2;
    let (
        mut graph_cell_width,
        mut marker_cell_width,
        mut name_cell_width,
        mut hash_cell_width,
        mut date_cell_width,
    ) = (0, 0, 0, 0, 0);

    for col in columns {
        match col {
            UserListColumnType::Graph => {
                graph_cell_width = graph_width;
            }
            UserListColumnType::Marker => {
                marker_cell_width = 1;
            }
            UserListColumnType::Name => {
                name_cell_width = name_width + pad;
            }
            UserListColumnType::Hash => {
                hash_cell_width = 7 + pad;
            }
            UserListColumnType::Date => {
                date_cell_width = date_width + pad;
            }
            UserListColumnType::Subject => {}
        }
    }

    let mut total_width = graph_cell_width
        + marker_cell_width
        + hash_cell_width
        + name_cell_width
        + date_cell_width
        + subject_min_width;

    if total_width > area_width {
        total_width = total_width.saturating_sub(name_cell_width);
        name_cell_width = 0;
    }
    if total_width > area_width {
        total_width = total_width.saturating_sub(date_cell_width);
        date_cell_width = 0;
    }
    if total_width > area_width {
        hash_cell_width = 0;
    }

    let mut constraints = Vec::new();
    for col in columns {
        match col {
            UserListColumnType::Graph => {
                constraints.push(Constraint::Length(graph_cell_width));
            }
            UserListColumnType::Marker => {
                constraints.push(Constraint::Length(marker_cell_width));
            }
            UserListColumnType::Subject => {
                constraints.push(Constraint::Min(0));
            }
            UserListColumnType::Name => {
                constraints.push(Constraint::Length(name_cell_width));
            }
            UserListColumnType::Hash => {
                constraints.push(Constraint::Length(hash_cell_width));
            }
            UserListColumnType::Date => {
                constraints.push(Constraint::Length(date_cell_width));
            }
        }
    }
    constraints
}

/// Pure：把 raw commit index 轉成 filtered view 的位置。filter 空時 alias 到 raw。
fn resolve_raw_to_filtered(
    filtered_indices: &[RawCommitIdx],
    commits_len: usize,
    raw: RawCommitIdx,
) -> Option<FilteredIdx> {
    if filtered_indices.is_empty() {
        (raw.0 < commits_len).then_some(FilteredIdx(raw.0))
    } else {
        filtered_indices
            .iter()
            .position(|r| *r == raw)
            .map(FilteredIdx)
    }
}

/// Pure：把 filtered view 的位置轉回 raw commit index。越界回 None。
fn resolve_filtered_to_raw(
    filtered_indices: &[RawCommitIdx],
    commits_len: usize,
    f: FilteredIdx,
) -> Option<RawCommitIdx> {
    if filtered_indices.is_empty() {
        (f.0 < commits_len).then_some(RawCommitIdx(f.0))
    } else {
        filtered_indices.get(f.0).copied()
    }
}

/// Pure：給定 (target, total, height) 算出 (offset, selected)。
/// target 越界或 height=0 回 None（caller 不動游標）。
/// 合併 `total > height` 與 `total <= height` 兩分支成單一公式：
/// `total <= height` 時 `max_offset = 0`，自動退化成 `selected = target.0`。
fn compute_selection(target: VisibleIdx, total: usize, height: usize) -> Option<(usize, usize)> {
    if target.0 >= total || height == 0 {
        return None;
    }
    let max_offset = total.saturating_sub(height);
    let offset = target.0.min(max_offset);
    let selected = target.0 - offset;
    debug_assert!(selected < height);
    Some((offset, selected))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit_fixture() -> Commit {
        Commit {
            subject: "修正顯示問題".into(),
            author_name: "Alice".into(),
            commit_hash: "abc1234def".into(),
            ..Default::default()
        }
    }

    // ---- render_graph_text --------------------------------------------
    //
    // CommitList implements StatefulWidget directly, so no Terminal /
    // TestBackend is needed — Buffer::empty() + render() is enough.
    // Fixtures/constants live in their own submodule so they don't collide
    // with the rest of `mod tests`.
    mod render_graph_tests {
        use super::*;
        use crate::{
            color::GraphColorSet,
            config::{CoreConfig, GraphColorConfig, UiConfig},
            git::FileChange,
            graph::{Edge, EdgeType, GraphStyle},
            keybind::KeyBind,
        };

        const TERM_W: u16 = 80;

        const RED: Color = Color::Rgb(0xFF, 0x00, 0x00);
        const GREEN: Color = Color::Rgb(0x00, 0xFF, 0x00);
        const BLUE: Color = Color::Rgb(0x00, 0x00, 0xFF);
        // ratatui_color_to_rgb(ColorTheme::default().list_selected_bg) == DarkGray
        const SELECTED_BG: Color = Color::Rgb(80, 80, 80);

        fn test_ctx_styled(graph_style: GraphStyle) -> Rc<AppContext> {
            Rc::new(AppContext {
                keybind: KeyBind::new(None),
                core_config: CoreConfig::default(),
                ui_config: UiConfig::default(),
                color_theme: ColorTheme::default(),
                graph_style,
            })
        }

        fn test_graph_color_set() -> GraphColorSet {
            GraphColorSet::new(&GraphColorConfig {
                branches: vec!["#FF0000".into(), "#00FF00".into(), "#0000FF".into()],
            })
        }

        /// Commit hash must be >= 7 chars: `render_hash` uses `as_short_hash()`
        /// ([0..7]); a shorter hash panics.
        fn text_graph_commits() -> Vec<Commit> {
            [
                ("aaaaaaa", "first"),
                ("bbbbbbb", "second"),
                ("ccccccc", "third"),
            ]
            .into_iter()
            .map(|(hash, subject)| Commit {
                commit_hash: hash.into(),
                subject: subject.into(),
                author_name: "alice".into(),
                ..Default::default()
            })
            .collect()
        }

        /// 3-commit graph shared by tests 1-4.
        ///
        /// c0's dot is deliberately at pos_x=1, NOT pos_x=0: `render_graph_text`'s
        /// virtual-row column fallback is `head_col -> first-visible-commit's dot
        /// col -> literal 0`. If HEAD's dot sat at column 0, all three paths would
        /// coincide and a regression that hardcodes column 0 would slip past the
        /// virtual-row test undetected.
        fn text_graph(commits: &[Commit]) -> Graph {
            let h = |i: usize| commits[i].commit_hash.clone();
            Graph {
                commit_hashes: commits.iter().map(|c| c.commit_hash.clone()).collect(),
                commit_pos_map: [(h(0), (1, 0)), (h(1), (0, 1)), (h(2), (2, 2))]
                    .into_iter()
                    .collect(),
                edges: vec![
                    vec![
                        Edge::new(EdgeType::Vertical, 0, 0),
                        Edge::new(EdgeType::Horizontal, 2, 2),
                    ],
                    vec![
                        Edge::new(EdgeType::Horizontal, 1, 2),
                        Edge::new(EdgeType::LeftTop, 2, 2),
                    ],
                    vec![Edge::new(EdgeType::Vertical, 0, 0)],
                ],
                max_pos_x: 2,
            }
        }

        /// Deliberately different shape from `text_graph` (linear, all three
        /// commits at pos_x=0, no merge) so `filtered_graph_manager_fills_text_cells`
        /// can tell whether `current_graph()` actually picked this graph
        /// instead of the primary one -- if both fixtures rendered
        /// identically, a `current_graph()` that always returns the primary
        /// graph would pass the test undetected.
        fn text_graph_filtered(commits: &[Commit]) -> Graph {
            let h = |i: usize| commits[i].commit_hash.clone();
            Graph {
                commit_hashes: commits.iter().map(|c| c.commit_hash.clone()).collect(),
                commit_pos_map: [(h(0), (0, 0)), (h(1), (0, 1)), (h(2), (0, 2))]
                    .into_iter()
                    .collect(),
                edges: vec![
                    vec![Edge::new(EdgeType::Vertical, 0, 0)],
                    vec![Edge::new(EdgeType::Vertical, 0, 0)],
                    vec![],
                ],
                max_pos_x: 2,
            }
        }

        struct Opts {
            /// Index into `commits`, fed to `CommitListState`'s shared
            /// `head_commit_hash` (drives `is_head` in `put_text_cells`).
            /// Distinct from `CommitListState`'s own `head: Head` field, which
            /// only affects ref rendering and doesn't matter for these tests.
            head_hash: Option<usize>,
            working_changes: bool,
            inline_detail_height: u16,
            /// When true, builds a second `Graph` fixture (same shape as the
            /// primary) and routes rendering through it via `filtered`
            /// and `set_show_remote_refs(false)`. This is the only path
            /// `render_graph`'s filtered branch exercises -- without it,
            /// that branch has zero test coverage.
            filtered: bool,
        }

        impl Default for Opts {
            fn default() -> Self {
                Self {
                    head_hash: Some(0),
                    working_changes: false,
                    inline_detail_height: 0,
                    filtered: false,
                }
            }
        }

        fn build_state(commits: &[Commit], graph: Graph, opts: Opts) -> CommitListState<'_> {
            let graph_cell_width = (graph.max_pos_x + 1) as u16 * 2;
            let head_hash = opts.head_hash.map(|i| commits[i].commit_hash.clone());
            let graph_colors: Vec<Color> = test_graph_color_set()
                .colors
                .iter()
                .map(|c| c.to_ratatui_color())
                .collect();
            let infos = commits
                .iter()
                .map(|c| CommitInfo::new(c, Vec::new(), Color::Reset))
                .collect();
            let working = opts.working_changes.then(|| WorkingChanges {
                staged: vec![FileChange::Modify {
                    path: "src/a.rs".into(),
                    stats: None,
                }],
                unstaged: Vec::new(),
            });
            let filtered = opts.filtered.then(|| FilteredGraphData {
                graph: Rc::new(text_graph_filtered(commits)),
                cell_width: graph_cell_width,
            });
            let mut state = CommitListState::new(
                infos,
                Rc::new(graph),
                graph_colors,
                head_hash,
                graph_cell_width,
                Head::None,
                FxHashMap::default(),
                false,
                false,
                filtered,
                None,
                FxHashSet::default(),
                working,
            );
            if opts.filtered {
                state.set_show_remote_refs(false);
            }
            state.set_inline_detail_height(opts.inline_detail_height);
            state
        }

        fn render_commit_list(state: &mut CommitListState<'_>, height: u16) -> Buffer {
            render_commit_list_styled(state, height, GraphStyle::Rounded)
        }

        fn render_commit_list_styled(
            state: &mut CommitListState<'_>,
            height: u16,
            graph_style: GraphStyle,
        ) -> Buffer {
            let ctx = test_ctx_styled(graph_style);
            assert!(
                matches!(
                    ctx.ui_config.list.columns.first(),
                    Some(UserListColumnType::Graph)
                ),
                "fixture assumes Graph is the first column"
            );
            let area = Rect::new(0, 0, TERM_W, height);
            let mut buf = Buffer::empty(area);
            CommitList::new(ctx, 0).render(area, &mut buf, state);
            buf
        }

        /// Graph column only (x in 0..6), one string per row in `rows`.
        fn graph_rows(buf: &Buffer, rows: std::ops::RangeInclusive<u16>) -> Vec<String> {
            rows.map(|y| (0..6).map(|x| buf[(x, y)].symbol()).collect())
                .collect()
        }

        #[test]
        fn render_graph_draws_cells_below_header() {
            let commits = text_graph_commits();
            let mut state = build_state(&commits, text_graph(&commits), Opts::default());
            let buf = render_commit_list(&mut state, 10);

            assert_eq!(
                graph_rows(&buf, 1..=3),
                ["│ ◯ ──", "● ──╭─", "│   ● "],
                "row 1..3 are c0(HEAD)/c1/c2; row 0 is the header"
            );

            // c0 (HEAD, selected): dot col is green (pos_x=1), the Vertical
            // edge at col 0 is red (line 0), the Horizontal edges are blue
            // (line 2) -- colors follow associated_line_pos_x, not pos_x.
            assert_eq!(buf[(0, 1)].fg, RED);
            assert_eq!(buf[(2, 1)].fg, GREEN);
            assert_eq!(buf[(4, 1)].fg, BLUE);
            assert!(buf[(2, 1)].modifier.contains(Modifier::BOLD));
            for x in 0..7 {
                assert_eq!(buf[(x, 1)].bg, SELECTED_BG, "selected row bg at x={x}");
            }

            // c1 (not selected): no bg override.
            assert_ne!(buf[(0, 2)].bg, SELECTED_BG);
        }

        #[test]
        fn render_graph_uses_ascii_style() {
            // Only this test goes through AppContext -> glyphs() -> put_text_cell;
            // tests/graph.rs calls GlyphSet::resolve() directly and never
            // exercises that wiring, so this is the sole end-to-end coverage
            // for "-s ascii actually changes what's on screen".
            let commits = text_graph_commits();
            let mut state = build_state(&commits, text_graph(&commits), Opts::default());
            let buf = render_commit_list_styled(&mut state, 10, GraphStyle::Ascii);

            assert_eq!(
                graph_rows(&buf, 1..=3),
                ["| o --", "* --+-", "|   * "],
                "ascii style substitutes every box-drawing glyph 1:1"
            );

            // Marker column sits right after the graph column (width 7 =
            // graph_area_cell_width's `w + 1`) and shares the same
            // `self.glyphs()` call, so it must switch style too.
            assert_eq!(buf[(7, 1)].symbol(), "|");
        }

        #[test]
        fn head_commit_dot_is_hollow_and_bold() {
            let commits = text_graph_commits();

            let mut with_head = build_state(&commits, text_graph(&commits), Opts::default());
            let buf_a = render_commit_list(&mut with_head, 10);
            assert_eq!(buf_a[(2, 1)].symbol(), "◯");
            assert!(buf_a[(2, 1)].modifier.contains(Modifier::BOLD));

            let mut without_head = build_state(
                &commits,
                text_graph(&commits),
                Opts {
                    head_hash: None,
                    ..Default::default()
                },
            );
            let buf_b = render_commit_list(&mut without_head, 10);
            assert_eq!(buf_b[(2, 1)].symbol(), "●");
            assert!(!buf_b[(2, 1)].modifier.contains(Modifier::BOLD));
            // Color is unaffected by head/non-head.
            assert_eq!(buf_b[(2, 1)].fg, GREEN);

            // c1/c2 are never HEAD in either state: plain dot, never bold.
            for buf in [&buf_a, &buf_b] {
                assert_eq!(buf[(0, 2)].symbol(), "●");
                assert!(!buf[(0, 2)].modifier.contains(Modifier::BOLD));
                assert_eq!(buf[(4, 3)].symbol(), "●");
                assert!(!buf[(4, 3)].modifier.contains(Modifier::BOLD));
            }
        }

        #[test]
        fn spacer_row_extends_only_vertical_columns() {
            let commits = text_graph_commits();
            let mut state = build_state(
                &commits,
                text_graph(&commits),
                Opts {
                    inline_detail_height: 2,
                    ..Default::default()
                },
            );
            let buf = render_commit_list(&mut state, 10);

            // Selected row is c0 (pos_y=0): cells are [Vert, ' ', Dot, ' ', Horiz, Horiz].
            // Only the Vertical (idx0) and the Dot (idx2) are in put_text_spacer's
            // whitelist -- the Horizontal pair (idx4/5) must NOT extend down.
            for y in [2u16, 3u16] {
                assert_eq!(buf[(0, y)].symbol(), "│", "vertical edge extends at y={y}");
                assert_eq!(buf[(0, y)].fg, RED);
                assert_eq!(buf[(2, y)].symbol(), "│", "dot column extends at y={y}");
                assert_eq!(buf[(2, y)].fg, GREEN);
                assert_ne!(
                    buf[(4, y)].symbol(),
                    "─",
                    "TEXT_HORIZ must not extend into the spacer row at y={y}"
                );
                assert_eq!(buf[(4, y)].symbol(), " ");
            }

            // c1/c2 pushed down by the 2-row gap.
            assert_eq!(graph_rows(&buf, 4..=5), ["● ──╭─", "│   ● "]);
        }

        #[test]
        fn virtual_row_draws_gray_head_dot_at_top() {
            let commits = text_graph_commits();
            let mut state = build_state(
                &commits,
                text_graph(&commits),
                Opts {
                    working_changes: true,
                    inline_detail_height: 1,
                    ..Default::default()
                },
            );
            let buf = render_commit_list(&mut state, 10);

            // Virtual row at buffer row 1, dot at HEAD's column (idx2, not 0).
            assert_eq!(buf[(2, 1)].symbol(), "◯");
            assert_eq!(
                buf[(2, 1)].fg,
                Color::Gray,
                "VIRTUAL_ROW_COLOR, not theme-converted"
            );
            assert!(!buf[(2, 1)].modifier.contains(Modifier::BOLD));
            for x in 0..7 {
                assert_eq!(
                    buf[(x, 1)].bg,
                    SELECTED_BG,
                    "virtual row is selected by default"
                );
            }

            // Gray spacer row (virtual row selected + gap=1) comes first, at
            // row 2 -- `y_offset` pushes every commit past `state.selected`
            // (0, the virtual row) down by `gap` before the spacer itself.
            // Cells that redraw at all must use VIRTUAL_ROW_COLOR, not their
            // own color.
            let spacer_y = 2u16;
            assert_eq!(buf[(0, spacer_y)].symbol(), "│");
            assert_eq!(buf[(0, spacer_y)].fg, Color::Gray);
            assert_eq!(buf[(2, spacer_y)].symbol(), "│");
            assert_eq!(buf[(2, spacer_y)].fg, Color::Gray);

            // c0's own (bold, colored) HEAD dot, pushed down past the spacer
            // to row 3.
            assert_eq!(buf[(2, 3)].symbol(), "◯");
            assert!(buf[(2, 3)].modifier.contains(Modifier::BOLD));
            assert_eq!(buf[(2, 3)].fg, GREEN);

            // c1/c2 shift down by the same gap.
            assert_eq!(graph_rows(&buf, 4..=5), ["● ──╭─", "│   ● "]);
        }

        #[test]
        fn head_upward_connector_line_fills_gap() {
            // Dedicated 2-commit graph reusing the first two of
            // `text_graph_commits()`: c0 has NO edges at all (its column-0
            // cell is blank), c1 (HEAD) sits at column 0. The virtual row's
            // dot lands on HEAD's column; render_graph_text must punch a gray
            // connector through c0's blank cell so the line reads continuous.
            let all_commits = text_graph_commits();
            let commits = &all_commits[..2];
            let h = |i: usize| commits[i].commit_hash.clone();
            let graph = Graph {
                commit_hashes: commits.iter().map(|c| c.commit_hash.clone()).collect(),
                commit_pos_map: [(h(0), (1, 0)), (h(1), (0, 1))].into_iter().collect(),
                edges: vec![vec![], vec![]],
                max_pos_x: 1,
            };
            let mut state = build_state(
                commits,
                graph,
                Opts {
                    head_hash: Some(1),
                    working_changes: true,
                    ..Default::default()
                },
            );
            let buf = render_commit_list(&mut state, 10);

            // Virtual row dot at HEAD's column (c1 is at pos_x=0 -> idx 0).
            assert_eq!(buf[(0, 1)].symbol(), "◯");
            // c0's row has no edge at column 0, so without the connector this
            // cell would be blank. It must be filled with a gray `│`.
            assert_eq!(buf[(0, 2)].symbol(), "│");
            assert_eq!(buf[(0, 2)].fg, Color::Gray);
            // c1 (HEAD) itself, unaffected by the connector logic.
            assert_eq!(buf[(0, 3)].symbol(), "◯");
            assert!(buf[(0, 3)].modifier.contains(Modifier::BOLD));
        }

        #[test]
        fn filtered_graph_manager_fills_text_cells() {
            // `render_graph`'s `current_graph()` picks `filtered`
            // instead of `graph` whenever `show_remote_refs` is off and a
            // filtered graph exists (the "hide remote-only commits" path).
            // Nothing else in this suite ever sets `filtered: true`, so
            // without this test the filtered branch could silently stop
            // rendering (e.g. `current_graph()` regressing to always return
            // the primary graph) and no test would notice.
            //
            // `text_graph_filtered` is deliberately a different shape from
            // `text_graph` (the primary fixture) -- if both rendered
            // identically, a `current_graph()` that ignores `filtered`
            // entirely would still pass this assertion undetected.
            let commits = text_graph_commits();
            let mut state = build_state(
                &commits,
                text_graph(&commits),
                Opts {
                    filtered: true,
                    ..Default::default()
                },
            );
            assert!(!state.show_remote_refs());
            let buf = render_commit_list(&mut state, 10);

            assert_eq!(
                graph_rows(&buf, 1..=3),
                ["◯     ", "●     ", "●     "],
                "must render text_graph_filtered's shape, not the primary graph's"
            );
        }
    }

    /// `search_fields` 是搜尋與 filter 的唯一欄位來源，排除 Stash 是它唯一的非平凡
    /// 規則 —— 之前這條規則在 `SearchMatch::new` 與 `commit_quick_matches` 各抄一份。
    #[test]
    fn search_match_covers_every_field_and_skips_stash() {
        let c = commit_fixture();
        let branch = Ref::Branch {
            name: "feature/x".into(),
            target: "abc1234def".into(),
        };
        let stash = Ref::Stash {
            name: "stash@{0}".into(),
            message: "wip".into(),
            target: "abc1234def".into(),
        };
        let info = CommitInfo::new(&c, vec![&branch, &stash], Color::Reset);

        let hit = |q: &str| SearchMatch::new(&info, &SearchMatcher::new(q, false, false));

        assert!(hit("修正").subject.is_some(), "subject");
        assert!(hit("Alice").author_name.is_some(), "author_name");
        assert!(hit("abc1234").commit_hash.is_some(), "commit_hash");
        assert!(hit("feature").refs.contains_key("feature/x"), "branch ref");

        // stash 的名稱與訊息都不該被搜到
        let m = hit("stash@");
        assert!(m.refs.is_empty() && !m.matched(), "stash 不該進搜尋範圍");
    }

    #[test]
    fn test_calc_cell_widths_all_columns() {
        let area_width = 80;
        let subject_min_width = 20;
        let graph_width = 6;
        let name_width = 10;
        let date_width = 15;
        let columns = vec![
            UserListColumnType::Graph,
            UserListColumnType::Marker,
            UserListColumnType::Subject,
            UserListColumnType::Date,
            UserListColumnType::Name,
            UserListColumnType::Hash,
        ];

        let actual = calc_cell_widths(
            area_width,
            subject_min_width,
            graph_width,
            name_width,
            date_width,
            &columns,
        );

        let expected = vec![
            Constraint::Length(6),  // Graph
            Constraint::Length(1),  // Marker
            Constraint::Min(0),     // Subject
            Constraint::Length(17), // Date (15 + 2 pad)
            Constraint::Length(12), // Name (10 + 2 pad)
            Constraint::Length(9),  // Hash (7 + 2 pad)
        ];
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_calc_cell_width_all_columns_small_area_remove_name_date_hash() {
        let area_width = 30;
        let subject_min_width = 20;
        let graph_width = 6;
        let name_width = 10;
        let date_width = 15;
        let columns = vec![
            UserListColumnType::Graph,
            UserListColumnType::Marker,
            UserListColumnType::Subject,
            UserListColumnType::Date,
            UserListColumnType::Name,
            UserListColumnType::Hash,
        ];

        let actual = calc_cell_widths(
            area_width,
            subject_min_width,
            graph_width,
            name_width,
            date_width,
            &columns,
        );

        // Graph + Marker + Subject + Hash = 6 + 1 + 20 + 9 = 36 > 30
        // => Name, Date, and Hash are removed
        let expected = vec![
            Constraint::Length(6), // Graph
            Constraint::Length(1), // Marker
            Constraint::Min(0),    // Subject
            Constraint::Length(0), // Date removed
            Constraint::Length(0), // Name removed
            Constraint::Length(0), // Hash removed
        ];
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_calc_cell_width_all_columns_small_area_remove_name_date() {
        let area_width = 40;
        let subject_min_width = 20;
        let graph_width = 6;
        let name_width = 10;
        let date_width = 15;
        let columns = vec![
            UserListColumnType::Graph,
            UserListColumnType::Marker,
            UserListColumnType::Subject,
            UserListColumnType::Date,
            UserListColumnType::Name,
            UserListColumnType::Hash,
        ];

        let actual = calc_cell_widths(
            area_width,
            subject_min_width,
            graph_width,
            name_width,
            date_width,
            &columns,
        );

        // Graph + Marker + Subject + Hash = 6 + 1 + 20 + 9 = 36
        // Graph + Marker + Subject + Date + Hash = 6 + 1 + 20 + 17 + 9 = 53 > 40
        // => Name and Date are removed
        let expected = vec![
            Constraint::Length(6), // Graph
            Constraint::Length(1), // Marker
            Constraint::Min(0),    // Subject
            Constraint::Length(0), // Date removed
            Constraint::Length(0), // Name removed
            Constraint::Length(9), // Hash (7 + 2 pad)
        ];
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_calc_cell_width_all_columns_small_area_remove_name() {
        let area_width = 60;
        let subject_min_width = 20;
        let graph_width = 6;
        let name_width = 10;
        let date_width = 15;
        let columns = vec![
            UserListColumnType::Graph,
            UserListColumnType::Marker,
            UserListColumnType::Subject,
            UserListColumnType::Date,
            UserListColumnType::Name,
            UserListColumnType::Hash,
        ];

        let actual = calc_cell_widths(
            area_width,
            subject_min_width,
            graph_width,
            name_width,
            date_width,
            &columns,
        );

        // Graph + Marker + Subject + Date + Hash = 6 + 1 + 20 + 17 + 9 = 53 <= 60
        // Graph + Marker + Subject + Name + Date + Hash = 6 + 1 + 20 + 12 + 17 + 9 = 65 > 60
        // => Name is removed
        let expected = vec![
            Constraint::Length(6),  // Graph
            Constraint::Length(1),  // Marker
            Constraint::Min(0),     // Subject
            Constraint::Length(17), // Date (15 + 2 pad)
            Constraint::Length(0),  // Name removed
            Constraint::Length(9),  // Hash (7 + 2 pad)
        ];
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_calc_cell_width_columns_order() {
        let area_width = 80;
        let subject_min_width = 20;
        let graph_width = 6;
        let name_width = 10;
        let date_width = 15;
        let columns = vec![
            UserListColumnType::Date,
            UserListColumnType::Subject,
            UserListColumnType::Hash,
            UserListColumnType::Graph,
        ];

        let actual = calc_cell_widths(
            area_width,
            subject_min_width,
            graph_width,
            name_width,
            date_width,
            &columns,
        );

        let expected = vec![
            Constraint::Length(17), // Date (15 + 2 pad)
            Constraint::Min(0),     // Subject
            Constraint::Length(9),  // Hash (7 + 2 pad)
            Constraint::Length(6),  // Graph
        ];
        assert_eq!(actual, expected);
    }

    // --- 座標系 regression tests ---------------------------------------------
    // 這些 test 聚焦在 pure function（`resolve_*` / `compute_selection`），
    // 避開 CommitListState fixture 的建構成本；涵蓋原始 panic 路徑。

    #[test]
    fn filtered_to_raw_empty_filter_passes_through_when_in_range() {
        assert_eq!(
            resolve_filtered_to_raw(&[], 10, FilteredIdx(5)),
            Some(RawCommitIdx(5))
        );
    }

    #[test]
    fn filtered_to_raw_empty_filter_out_of_range_returns_none() {
        assert_eq!(resolve_filtered_to_raw(&[], 10, FilteredIdx(10)), None);
    }

    #[test]
    fn filtered_to_raw_active_filter_out_of_range_returns_none_no_panic() {
        // 原始 panic 場景：filtered_indices.len() = 234，index 309 越界。
        let filtered: Vec<RawCommitIdx> = (0..234).map(RawCommitIdx).collect();
        assert_eq!(
            resolve_filtered_to_raw(&filtered, 500, FilteredIdx(309)),
            None,
            "越界 FilteredIdx 應返回 None 而非 panic"
        );
    }

    #[test]
    fn filtered_to_raw_active_filter_returns_mapped_raw() {
        let filtered = vec![RawCommitIdx(3), RawCommitIdx(7), RawCommitIdx(12)];
        assert_eq!(
            resolve_filtered_to_raw(&filtered, 20, FilteredIdx(2)),
            Some(RawCommitIdx(12))
        );
    }

    #[test]
    fn raw_to_filtered_finds_position() {
        let filtered = vec![RawCommitIdx(3), RawCommitIdx(7), RawCommitIdx(12)];
        assert_eq!(
            resolve_raw_to_filtered(&filtered, 20, RawCommitIdx(7)),
            Some(FilteredIdx(1))
        );
    }

    #[test]
    fn raw_to_filtered_filtered_out_returns_none() {
        let filtered = vec![RawCommitIdx(3), RawCommitIdx(7), RawCommitIdx(12)];
        // raw=5 不在 filter 內 → None（caller 應「游標不動」而非 fallback 到 0）
        assert_eq!(
            resolve_raw_to_filtered(&filtered, 20, RawCommitIdx(5)),
            None
        );
    }

    #[test]
    fn raw_to_filtered_empty_filter_alias_to_raw() {
        assert_eq!(
            resolve_raw_to_filtered(&[], 10, RawCommitIdx(5)),
            Some(FilteredIdx(5))
        );
    }

    #[test]
    fn compute_selection_within_first_page() {
        // total=10, height=5, target=2：total > height 時，畫面從 target 開始捲，
        // 游標 pin 在畫面頂端（offset=target, selected=0）—— 與原版 select_index 行為一致。
        let (offset, selected) = compute_selection(VisibleIdx(2), 10, 5).unwrap();
        assert_eq!((offset, selected), (2, 0));
    }

    #[test]
    fn compute_selection_beyond_first_page_pins_max_offset() {
        // total=10, height=5, target=8 → offset pin 到 max_offset=5，selected=3
        let (offset, selected) = compute_selection(VisibleIdx(8), 10, 5).unwrap();
        assert_eq!((offset, selected), (5, 3));
        assert!(offset + selected < 10);
        assert!(selected < 5);
    }

    #[test]
    fn compute_selection_total_le_height_uses_selected_only() {
        // total=3, height=10（height 比 total 大）→ 公式退化成 offset=0
        let (offset, selected) = compute_selection(VisibleIdx(2), 3, 10).unwrap();
        assert_eq!((offset, selected), (0, 2));
    }

    #[test]
    fn compute_selection_out_of_range_returns_none() {
        // target >= total：不動游標（防呆原 panic 場景）
        assert!(compute_selection(VisibleIdx(10), 10, 5).is_none());
        assert!(compute_selection(VisibleIdx(309), 234, 50).is_none());
    }

    #[test]
    fn compute_selection_zero_height_returns_none() {
        // height=0（例如畫面尚未配置）→ 不動游標
        assert!(compute_selection(VisibleIdx(0), 10, 0).is_none());
    }

    #[test]
    fn compute_selection_never_puts_cursor_off_screen() {
        // 窮舉：任何合法 target 都應產生 offset + selected < total、selected < height
        for total in 1..30 {
            for height in 1..20 {
                for t in 0..total {
                    let Some((offset, selected)) = compute_selection(VisibleIdx(t), total, height)
                    else {
                        continue;
                    };
                    assert!(
                        offset + selected < total,
                        "invariant: offset+selected < total (t={t}, total={total}, h={height})"
                    );
                    assert!(
                        selected < height,
                        "invariant: selected < height (t={t}, total={total}, h={height})"
                    );
                }
            }
        }
    }

    #[test]
    fn select_commit_hash_panic_scenario_does_not_panic() {
        // 原始 panic 場景模擬：
        // commits.len()=500, filtered_indices.len()=234（隱藏 remote-only 後）。
        // 使用者選一個 raw=309 的 commit → 轉換應該回 None（不在 filter 內），
        // caller 不動游標，整體不 panic。
        let filtered: Vec<RawCommitIdx> = (0..234).map(RawCommitIdx).collect();
        let target_raw = RawCommitIdx(309);
        let visible = resolve_raw_to_filtered(&filtered, 500, target_raw);
        assert_eq!(visible, None, "raw=309 不在 filtered_indices 內 → None");

        // 若 target_raw 恰好在 filter 內（例如 raw=100 → filtered=100），
        // 再走 compute_selection 必得合法 (offset, selected)。
        let in_filter = resolve_raw_to_filtered(&filtered, 500, RawCommitIdx(100)).unwrap();
        // filtered total = 234 + vr(0); height=50；target=100
        let (offset, selected) = compute_selection(VisibleIdx(in_filter.0), 234, 50).unwrap();
        assert!(offset + selected < 234);
        assert!(selected < 50);
    }
}

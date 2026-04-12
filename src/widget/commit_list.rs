use std::rc::Rc;

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
    graph::GraphImageManager,
};

const ELLIPSIS: &str = "...";
const VIRTUAL_ROW_COLOR: Color = Color::Gray;

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
        start_index: usize,
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

impl SearchMatch {
    fn new<'a>(
        c: &Commit,
        refs: impl Iterator<Item = &'a Ref>,
        q: &str,
        ignore_case: bool,
        fuzzy: bool,
    ) -> Self {
        let matcher = SearchMatcher::new(q, ignore_case, fuzzy);
        let refs = refs
            .filter(|r| !matches!(r, Ref::Stash { .. }))
            .filter_map(|r| {
                matcher
                    .matched_position(r.name())
                    .map(SearchMatchPosition::new)
                    .map(|pos| (r.name().into(), pos))
            })
            .collect();
        let subject = matcher
            .matched_position(&c.subject)
            .map(SearchMatchPosition::new);
        let author_name = matcher
            .matched_position(&c.author_name)
            .map(SearchMatchPosition::new);
        let commit_hash = matcher
            .matched_position(c.commit_hash.as_short_hash())
            .map(SearchMatchPosition::new);
        Self {
            refs,
            subject,
            author_name,
            commit_hash,
            match_index: 0,
        }
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
    commit_hash_set: FxHashSet<CommitHash>,
    graph_image_manager: GraphImageManager,
    graph_cell_width: u16,
    head: Head,

    // Filtered graph data (for when remote-only commits are hidden)
    filtered_graph_image_manager: Option<GraphImageManager>,
    filtered_graph_cell_width: u16,
    filtered_graph_colors: Option<FxHashMap<CommitHash, Color>>,

    ref_name_to_commit_index_map: FxHashMap<String, usize>,

    search_state: SearchState,
    search_input: Input,
    search_matches: Vec<SearchMatch>,

    // Optimization: track previous search for incremental search
    last_search_query: String,
    last_matched_indices: Vec<usize>,
    last_search_ignore_case: bool,
    last_search_fuzzy: bool,

    // Filter mode
    filter_state: FilterState,
    filter_input: Input,
    filtered_indices: Vec<usize>,
    text_filtered_indices: Vec<usize>,

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
}

impl<'a> CommitListState<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        commits: Vec<CommitInfo<'a>>,
        graph_image_manager: GraphImageManager,
        graph_cell_width: u16,
        head: Head,
        ref_name_to_commit_index_map: FxHashMap<String, usize>,
        default_ignore_case: bool,
        default_fuzzy: bool,
        filtered_graph_image_manager: Option<GraphImageManager>,
        filtered_graph_cell_width: u16,
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
        let commit_hash_set = commits
            .iter()
            .map(|c| c.commit.commit_hash.clone())
            .collect();
        CommitListState {
            commits,
            commit_hash_set,
            graph_image_manager,
            graph_cell_width,
            head,
            filtered_graph_image_manager,
            filtered_graph_cell_width,
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
        }
    }

    pub fn graph_area_cell_width(&self) -> u16 {
        let w = if !self.show_remote_refs && self.filtered_graph_image_manager.is_some() {
            self.filtered_graph_cell_width
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
        self.needs_graph_clear = true;
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
    /// Contract: the caller is responsible for clearing image overlays.
    /// The refresh path already does this via `clear_image_area` in `lib.rs`,
    /// so this setter deliberately does **not** set `needs_graph_clear` —
    /// doing so would cause a double clear and an extra blank frame.
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
        let idx = if self.filtered_indices.is_empty() {
            0
        } else {
            *self.filtered_indices.first()?
        };
        self.commits.get(idx).map(|c| &c.commit.commit_hash)
    }

    pub fn working_changes(&self) -> Option<&WorkingChanges> {
        self.working_changes.as_ref()
    }

    fn rebuild_filtered_indices(&mut self) {
        let has_text_filter = !self.filter_input.value().is_empty();
        let has_remote_filter = !self.show_remote_refs;
        let vr = self.virtual_row_offset();

        if !has_text_filter && !has_remote_filter {
            self.filtered_indices.clear();
            self.total = self.commits.len() + vr;
        } else {
            let base: Box<dyn Iterator<Item = usize>> = if has_text_filter {
                Box::new(self.text_filtered_indices.iter().copied())
            } else {
                Box::new(0..self.commits.len())
            };

            if has_remote_filter {
                self.filtered_indices = base
                    .filter(|&i| {
                        !self
                            .remote_only_commits
                            .contains(self.commits[i].commit_hash())
                    })
                    .collect();
            } else {
                self.filtered_indices = base.collect();
            }

            self.total = self.filtered_indices.len() + vr;
        }

        self.selected = self.selected.min(self.total.saturating_sub(1));
        self.offset = self.offset.min(self.total.saturating_sub(self.height));
    }

    pub fn select_next(&mut self) {
        if self.total == 0 || self.height == 0 {
            return;
        }
        if self.selected < (self.total - 1).min(self.height - 1) {
            self.selected += 1;
        } else if self.selected + self.offset < self.total - 1 {
            self.offset += 1;
        }
    }

    pub fn select_parent(&mut self) {
        if self.total == 0 || self.is_virtual_row_selected() {
            return;
        }
        if let Some(target_commit) = self.selected_commit_parent_hash().cloned() {
            if self.commit_hash_set.contains(&target_commit) {
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
        self.commits[self.current_selected_index()]
            .commit
            .parent_commit_hashes
            .first()
    }

    pub fn select_prev(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        } else if self.offset > 0 {
            self.offset -= 1;
        }
    }

    pub fn select_first(&mut self) {
        self.selected = 0;
        self.offset = 0;
    }

    pub fn select_last(&mut self) {
        if self.total == 0 || self.height == 0 {
            return;
        }
        let max_selected = self.height.min(self.total) - 1;
        self.selected = max_selected;
        self.offset = self.total.saturating_sub(self.height);
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

    fn select_index(&mut self, index: usize) {
        if index < self.total {
            if self.total > self.height {
                self.selected = 0;
                self.offset = index;
            } else {
                self.selected = index;
            }
        }
    }

    pub fn select_next_match(&mut self) {
        if self.commits.is_empty() {
            return;
        }
        self.select_next_match_index(self.current_selected_index());
    }

    pub fn select_prev_match(&mut self) {
        if self.commits.is_empty() {
            return;
        }
        self.select_prev_match_index(self.current_selected_index());
    }

    pub fn selected_commit_hash(&self) -> &CommitHash {
        // When virtual row is selected, return first commit hash as fallback
        let idx = self.current_selected_index();
        &self.commits[idx].commit.commit_hash
    }

    pub fn selected_commit_refs(&self) -> &[&'a Ref] {
        if self.is_virtual_row_selected() {
            return &[];
        }
        self.commits[self.current_selected_index()].refs()
    }

    /// Returns the real commit index (in commits Vec) for the currently selected item.
    /// When virtual row is selected, returns 0 (first commit) as fallback.
    pub fn current_selected_index(&self) -> usize {
        let visible_idx = self.offset + self.selected;
        let adjusted = visible_idx.saturating_sub(self.virtual_row_offset());
        self.real_commit_index(adjusted)
    }

    pub fn current_list_status(&self) -> (usize, usize, usize) {
        (self.selected, self.offset, self.height)
    }

    pub fn reset_height(&mut self, height: usize) {
        self.height = height;
    }

    pub fn select_ref(&mut self, ref_name: &str) {
        if let Some(&index) = self.ref_name_to_commit_index_map.get(ref_name) {
            let visible_index = index + self.virtual_row_offset();
            if self.total > self.height {
                self.selected = 0;
                self.offset = visible_index;
            } else {
                self.selected = visible_index;
            }
        }
    }

    pub fn select_commit_hash(&mut self, commit_hash: &CommitHash) {
        if !self.commit_hash_set.contains(commit_hash) {
            return;
        }
        let vr = self.virtual_row_offset();
        for (i, commit_info) in self.commits.iter().enumerate() {
            if commit_info.commit.commit_hash == *commit_hash {
                let visible_i = i + vr;
                if self.total > self.height {
                    self.selected = 0;
                    self.offset = visible_i;
                } else {
                    self.selected = visible_i;
                }
                break;
            }
        }
    }

    pub fn search_state(&self) -> SearchState {
        self.search_state
    }

    pub fn start_search(&mut self) {
        if let SearchState::Inactive | SearchState::Applied { .. } = self.search_state {
            self.search_state = SearchState::Searching {
                start_index: self.current_selected_index(),
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

        let mut new_matched_indices = Vec::new();
        let mut match_index = 1;

        if can_use_incremental {
            // Incremental search: only check previously matched commits
            // First, clear all previous matches
            for &i in &self.last_matched_indices {
                self.search_matches[i].clear();
            }

            // Then search only among previously matched
            for &i in &self.last_matched_indices {
                let commit_info = &self.commits[i];
                if Self::commit_quick_matches(&matcher, commit_info) {
                    let mut m = SearchMatch::new(
                        commit_info.commit,
                        commit_info.refs.iter().copied(),
                        &query,
                        ignore_case,
                        fuzzy,
                    );
                    m.match_index = match_index;
                    match_index += 1;
                    self.search_matches[i] = m;
                    new_matched_indices.push(i);
                }
            }
        } else {
            // Full search: check all commits
            self.clear_search_matches();

            for (i, commit_info) in self.commits.iter().enumerate() {
                // Quick check first to avoid creating SearchMatch for non-matching commits
                if Self::commit_quick_matches(&matcher, commit_info) {
                    let mut m = SearchMatch::new(
                        commit_info.commit,
                        commit_info.refs.iter().copied(),
                        &query,
                        ignore_case,
                        fuzzy,
                    );
                    m.match_index = match_index;
                    match_index += 1;
                    self.search_matches[i] = m;
                    new_matched_indices.push(i);
                }
            }
        }

        self.last_search_query = query;
        self.last_matched_indices = new_matched_indices;
        self.last_search_ignore_case = ignore_case;
        self.last_search_fuzzy = fuzzy;
    }

    /// Quick check if commit matches any searchable field
    fn commit_quick_matches(matcher: &SearchMatcher, commit_info: &CommitInfo<'_>) -> bool {
        let commit = &commit_info.commit;

        // Check subject first (most likely match)
        if matcher.matches(&commit.subject) {
            return true;
        }

        // Check author name
        if matcher.matches(&commit.author_name) {
            return true;
        }

        // Check commit hash
        if matcher.matches(commit.commit_hash.as_short_hash()) {
            return true;
        }

        // Check refs
        for r in &commit_info.refs {
            if !matches!(r, Ref::Stash { .. }) && matcher.matches(r.name()) {
                return true;
            }
        }

        false
    }

    fn clear_search_matches(&mut self) {
        self.search_matches.iter_mut().for_each(|m| m.clear());
    }

    fn select_current_or_next_match_index(&mut self, current_index: usize) {
        if self.search_matches[current_index].matched() && self.is_index_visible(current_index) {
            self.select_real_index(current_index);
            self.search_state
                .update_match_index(self.search_matches[current_index].match_index);
        } else {
            self.select_next_match_index(current_index)
        }
    }

    fn select_next_match_index(&mut self, current_index: usize) {
        // Always iterate over full commits list since search_matches uses real indices
        let len = self.commits.len();
        if len == 0 {
            return;
        }
        let mut i = (current_index + 1) % len;
        while i != current_index {
            if self.search_matches[i].matched() && self.is_index_visible(i) {
                self.select_real_index(i);
                self.search_state
                    .update_match_index(self.search_matches[i].match_index);
                return;
            }
            i = (i + 1) % len;
        }
    }

    fn select_prev_match_index(&mut self, current_index: usize) {
        // Always iterate over full commits list since search_matches uses real indices
        let len = self.commits.len();
        if len == 0 {
            return;
        }
        let mut i = (current_index + len - 1) % len;
        while i != current_index {
            if self.search_matches[i].matched() && self.is_index_visible(i) {
                self.select_real_index(i);
                self.search_state
                    .update_match_index(self.search_matches[i].match_index);
                return;
            }
            i = (i + len - 1) % len;
        }
    }

    /// Check if a real commit index is visible (considering filter)
    fn is_index_visible(&self, real_index: usize) -> bool {
        if self.filtered_indices.is_empty() {
            true // No filter, all indices visible
        } else {
            self.filtered_indices.contains(&real_index)
        }
    }

    /// Select a commit by its real index (in commits Vec), converting to visible index
    fn select_real_index(&mut self, real_index: usize) {
        let vr = self.virtual_row_offset();
        if self.filtered_indices.is_empty() {
            self.select_index(real_index + vr);
        } else if let Some(visible_idx) =
            self.filtered_indices.iter().position(|&i| i == real_index)
        {
            self.select_index(visible_idx + vr);
        }
    }

    fn current_image_manager(&self) -> &GraphImageManager {
        if !self.show_remote_refs {
            if let Some(ref mgr) = self.filtered_graph_image_manager {
                return mgr;
            }
        }
        &self.graph_image_manager
    }

    fn encoded_image(&self, commit_info: &CommitInfo<'_>) -> &str {
        self.current_image_manager()
            .encoded_image(&commit_info.commit.commit_hash)
    }

    fn spacer_image(&self, commit_info: &CommitInfo<'_>) -> &str {
        self.current_image_manager()
            .spacer_image(&commit_info.commit.commit_hash)
    }

    fn selected_image(&self) -> Option<&str> {
        self.current_image_manager().selected_image()
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
        self.selected = 0;
        self.offset = 0;
        self.rebuild_filtered_indices();
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
                    self.text_filtered_indices.push(i);
                }
            }
        }

        self.selected = 0;
        self.offset = 0;
        self.rebuild_filtered_indices();
    }

    /// Map visible index to real commit index
    fn real_commit_index(&self, visible_idx: usize) -> usize {
        if self.filtered_indices.is_empty() {
            visible_idx
        } else {
            self.filtered_indices[visible_idx]
        }
    }
}

pub struct CommitList<'a> {
    ctx: Rc<AppContext>,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a> CommitList<'a> {
    pub fn new(ctx: Rc<AppContext>) -> Self {
        Self {
            ctx,
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

        // Load graph images for visible commits
        let has_filter = !state.filtered_indices.is_empty();
        let use_filtered = !state.show_remote_refs && state.filtered_graph_image_manager.is_some();
        let vr_offset = state.virtual_row_offset();
        for display_idx in 0..state.height.min(state.total.saturating_sub(state.offset)) {
            let visible_idx = state.offset + display_idx;
            if visible_idx < vr_offset {
                continue; // skip virtual row
            }
            let commit_visible_idx = visible_idx - vr_offset;
            let real_idx = if has_filter {
                state.filtered_indices[commit_visible_idx]
            } else {
                commit_visible_idx
            };
            let hash = &state.commits[real_idx].commit.commit_hash;
            if use_filtered {
                state
                    .filtered_graph_image_manager
                    .as_mut()
                    .unwrap()
                    .load_encoded_image(hash);
            } else {
                state.graph_image_manager.load_encoded_image(hash);
            }
        }

        // Cache first visible commit hash to avoid repeated Arc refcount bumps
        let first_hash_opt = state.first_visible_commit_hash().cloned();

        // Load virtual row images
        if state.has_virtual_row() {
            if let Some(ref first_hash) = first_hash_opt {
                let mgr = if use_filtered {
                    state.filtered_graph_image_manager.as_mut().unwrap()
                } else {
                    &mut state.graph_image_manager
                };
                mgr.load_virtual_row_image(first_hash);
                mgr.load_selected_virtual_row_image(first_hash);
                mgr.load_first_commit_with_up_image(first_hash);
                mgr.load_selected_first_commit_with_up_image(first_hash);
            }
        }

        // Load spacer image for selected commit when inline detail is active
        // When virtual row is selected, use the first commit's spacer for gap continuation
        if state.inline_detail_height > 0 {
            let is_vr = state.is_virtual_row_selected();
            let hash = if is_vr {
                first_hash_opt
            } else {
                let selected_idx = state.current_selected_index();
                Some(state.commits[selected_idx].commit.commit_hash.clone())
            };
            if let Some(hash) = hash {
                if use_filtered {
                    let mgr = state.filtered_graph_image_manager.as_mut().unwrap();
                    if is_vr {
                        mgr.load_gray_spacer_image(&hash);
                    } else {
                        mgr.load_spacer_image(&hash);
                        mgr.load_selected_image(&hash);
                    }
                } else if is_vr {
                    state.graph_image_manager.load_gray_spacer_image(&hash);
                } else {
                    state.graph_image_manager.load_spacer_image(&hash);
                    state.graph_image_manager.load_selected_image(&hash);
                }
            }
        }
    }

    fn render_graph(&self, buf: &mut Buffer, area: Rect, state: &CommitListState<'_>) {
        if area.is_empty() {
            return;
        }
        let gap = state.inline_detail_height;
        // Virtual row: render PNG image (gray hollow circle)
        if state.has_virtual_row() && state.offset == 0 {
            let y = area.top();
            let image = if state.selected == 0 && gap > 0 {
                state
                    .current_image_manager()
                    .selected_virtual_row_image()
                    .or_else(|| state.current_image_manager().virtual_row_image())
            } else {
                state.current_image_manager().virtual_row_image()
            };
            if let Some(img) = image {
                buf[(area.left(), y)].set_symbol(img);
                for w in 1..area.width - 1 {
                    buf[(area.left() + w, y)].set_skip(true);
                }
                if state.selected == 0 && gap > 0 && area.width >= 2 {
                    buf[(area.left() + area.width - 1, y)].set_style(
                        Style::default()
                            .bg(ratatui_color_to_rgb(self.ctx.color_theme.list_selected_bg)),
                    );
                }
            }
        }
        let use_up_image = state.has_virtual_row() && state.offset == 0;
        self.rendering_commit_info_iter(state)
            .for_each(|(display_i, real_i, commit_info)| {
                let y_offset = if gap > 0 && display_i > state.selected {
                    gap
                } else {
                    0
                };
                let y = area.top() + display_i as u16 + y_offset;
                if y < area.bottom() {
                    let image = if use_up_image && real_i == 0 {
                        // First commit with Up edge for virtual row connection
                        if gap > 0 && display_i == state.selected {
                            state
                                .current_image_manager()
                                .selected_first_commit_with_up_image()
                                .unwrap_or_else(|| state.encoded_image(commit_info))
                        } else {
                            state
                                .current_image_manager()
                                .first_commit_with_up_image()
                                .unwrap_or_else(|| state.encoded_image(commit_info))
                        }
                    } else if gap > 0 && display_i == state.selected {
                        state
                            .selected_image()
                            .unwrap_or_else(|| state.encoded_image(commit_info))
                    } else {
                        state.encoded_image(commit_info)
                    };
                    buf[(area.left(), y)].set_symbol(image);
                    for w in 1..area.width - 1 {
                        buf[(area.left() + w, y)].set_skip(true);
                    }
                    if display_i == state.selected && gap > 0 && area.width >= 2 {
                        buf[(area.left() + area.width - 1, y)].set_style(
                            Style::default()
                                .bg(ratatui_color_to_rgb(self.ctx.color_theme.list_selected_bg)),
                        );
                    }
                }
            });

        // Render spacer images in the gap rows
        if gap > 0 {
            if state.is_virtual_row_selected() {
                // Virtual row: use gray spacer for gap continuation
                if let Some(spacer) = state.current_image_manager().gray_spacer_image() {
                    for gap_row in 0..gap {
                        let y = area.top() + state.selected as u16 + 1 + gap_row;
                        if y < area.bottom() {
                            buf[(area.left(), y)].set_symbol(spacer);
                            for w in 1..area.width - 1 {
                                buf[(area.left() + w, y)].set_skip(true);
                            }
                        }
                    }
                }
            } else {
                // Normal commit: use plain spacer without background
                let selected_idx = state.current_selected_index();
                let spacer_commit = &state.commits[selected_idx];
                let spacer = state.spacer_image(spacer_commit);
                for gap_row in 0..gap {
                    let y = area.top() + state.selected as u16 + 1 + gap_row;
                    if y < area.bottom() {
                        buf[(area.left(), y)].set_symbol(spacer);
                        for w in 1..area.width - 1 {
                            buf[(area.left() + w, y)].set_skip(true);
                        }
                    }
                }
            }
        }
    }

    fn render_marker(&self, buf: &mut Buffer, area: Rect, state: &CommitListState<'_>) {
        if area.is_empty() {
            return;
        }
        let gap = state.inline_detail_height;
        let mut items: Vec<ListItem> = Vec::new();
        if state.has_virtual_row() && state.offset == 0 {
            let mut line = Line::from("│".fg(Color::Gray));
            if state.selected == 0 && gap > 0 {
                line = line
                    .bg(ratatui_color_to_rgb(self.ctx.color_theme.list_selected_bg))
                    .fg(Color::Gray);
            }
            items.push(ListItem::new(line));
            // Insert marker gap when virtual row is selected
            if gap > 0 && state.selected == 0 {
                for _ in 0..gap {
                    items.push(ListItem::new("│".fg(Color::Gray)));
                }
            }
        }
        self.rendering_commit_info_iter(state)
            .for_each(|(display_i, _, commit_info)| {
                let color = state.marker_color(commit_info);
                let mut line = Line::from("│".fg(color));
                if display_i == state.selected && gap > 0 {
                    line = line.bg(ratatui_color_to_rgb(self.ctx.color_theme.list_selected_bg));
                }
                items.push(ListItem::new(line));
                if gap > 0 && display_i == state.selected && !state.is_virtual_row_selected() {
                    let selected_idx = state.current_selected_index();
                    let sel_color = state.marker_color(&state.commits[selected_idx]);
                    for _ in 0..gap {
                        items.push(ListItem::new("│".fg(sel_color)));
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
            return;
        }
        let mut items: Vec<ListItem> = Vec::new();
        // Virtual row
        if state.has_virtual_row() && state.offset == 0 {
            let count = state.working_changes().map_or(0, |wc| wc.file_count());
            let text = format!("Uncommitted Changes ({})", count);
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
            .for_each(|(display_i, real_i, commit_info)| {
                let mut spans = refs_spans(
                    commit_info,
                    &state.head,
                    &state.search_matches[real_i].refs,
                    &self.ctx.color_theme,
                    state.show_remote_refs,
                );
                let ref_spans_width: usize = spans.iter().map(|s| s.width()).sum();
                let max_width = max_width.saturating_sub(ref_spans_width);
                let commit = &commit_info.commit;
                if max_width > ELLIPSIS.len() {
                    let truncate = console::measure_text_width(&commit.subject) > max_width;
                    let subject = if truncate {
                        console::truncate_str(&commit.subject, max_width, ELLIPSIS).to_string()
                    } else {
                        commit.subject.to_string()
                    };

                    let sub_spans = if let Some(pos) = state.search_matches[real_i].subject.clone()
                    {
                        highlighted_spans(
                            subject.into(),
                            pos,
                            self.ctx.color_theme.list_subject_fg,
                            Modifier::empty(),
                            &self.ctx.color_theme,
                            truncate,
                        )
                    } else {
                        vec![subject.fg(self.ctx.color_theme.list_subject_fg)]
                    };

                    spans.extend(sub_spans)
                }
                items.push(self.to_commit_list_item(display_i, spans, state));
                Self::insert_gap(&mut items, state, false, display_i);
            });
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
            .for_each(|(display_i, real_i, commit)| {
                let truncate = console::measure_text_width(&commit.author_name) > max_width;
                let name = if truncate {
                    console::truncate_str(&commit.author_name, max_width, ELLIPSIS).to_string()
                } else {
                    commit.author_name.to_string()
                };
                let spans = if let Some(pos) = state.search_matches[real_i].author_name.clone() {
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
            .for_each(|(display_i, real_i, commit)| {
                let hash = commit.commit_hash.as_short_hash();
                let spans = if let Some(pos) = state.search_matches[real_i].commit_hash.clone() {
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
            .for_each(|(display_i, _real_i, commit)| {
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

    /// Returns iterator of (display_idx, real_idx, &CommitInfo)
    /// display_idx: position on screen (0, 1, 2, ...)
    /// real_idx: actual index in commits Vec (for search_matches access)
    /// Skips the virtual row (if present and visible).
    fn rendering_commit_info_iter<'b>(
        &'b self,
        state: &'b CommitListState<'_>,
    ) -> impl Iterator<Item = (usize, usize, &'b CommitInfo<'b>)> {
        let has_filter = !state.filtered_indices.is_empty();
        let vr_offset = state.virtual_row_offset();
        let total_visible = state.height.min(state.total.saturating_sub(state.offset));
        let start = if state.offset == 0 { vr_offset } else { 0 };
        (start..total_visible).map(move |display_idx| {
            let visible_idx = state.offset + display_idx;
            let commit_visible_idx = visible_idx - vr_offset;
            let real_idx = if has_filter {
                state.filtered_indices[commit_visible_idx]
            } else {
                commit_visible_idx
            };
            (display_idx, real_idx, &state.commits[real_idx])
        })
    }

    fn rendering_commit_iter<'b>(
        &'b self,
        state: &'b CommitListState<'_>,
    ) -> impl Iterator<Item = (usize, usize, &'b Commit)> {
        self.rendering_commit_info_iter(state)
            .map(|(display_i, real_i, commit_info)| (display_i, real_i, commit_info.commit))
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
            let bg = if state.inline_detail_height > 0 {
                ratatui_color_to_rgb(self.ctx.color_theme.list_selected_bg)
            } else {
                self.ctx.color_theme.list_selected_bg
            };
            line = line.bg(bg).fg(self.ctx.color_theme.list_selected_fg);
        }
        ListItem::new(line)
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

    let head_bg = Color::Rgb(255, 140, 0);
    let is_head_branch = |n: &str| matches!(head, Head::Branch { name: hn } if hn == n);

    let ref_spans: Vec<(Vec<Span>, &String)> = refs
        .iter()
        .filter_map(|r| match r {
            Ref::Branch { name, .. } => {
                // 如果存在對應的 remote branch，隱藏本地分支
                let has_remote = refs.iter().any(|r| {
                    matches!(r, Ref::RemoteBranch { name: rn, .. } if rn.ends_with(&format!("/{}", name)))
                });
                if has_remote && show_remote_refs {
                    return None;
                }
                let is_head = is_head_branch(name);
                let fg = if is_head {
                    Color::White
                } else {
                    color_theme.list_ref_branch_fg
                };
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
                    spans = spans.into_iter().map(|s| s.bg(head_bg)).collect();
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
                            branch_span.fg(Color::White).bg(head_bg)
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
                let spans = refs_matches
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
                Some((spans, name))
            }
            Ref::Stash { .. } => None,
        })
        .collect();

    let mut spans = vec![Span::raw("(").fg(color_theme.list_ref_paren_fg).bold()];

    if let Head::Detached { target } = head {
        if commit_info.commit.commit_hash == *target {
            spans.push(Span::raw("HEAD").fg(color_theme.list_head_fg).bold());
            if !ref_spans.is_empty() {
                spans.push(Span::raw(", ").fg(color_theme.list_ref_paren_fg).bold());
            }
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;

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
}

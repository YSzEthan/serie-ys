use std::{cell::Cell, rc::Rc};

use ratatui::{layout::Rect, style::Color};
use rustc_hash::{FxHashMap, FxHashSet};
use tui_input::Input;

use crate::git::{CommitHash, Head, Ref, WorkingChanges};
use crate::graph::{CellWidthType, Graph, TextCell};

use super::search::{FilterState, SearchMatch, SearchState};
use super::{CommitInfo, FilteredIdx, RawCommitIdx, VisibleIdx};

/// 游標與 viewport 上下邊緣保持的最小距離；撞進這個 margin 時改由 offset 滾動。
const CURSOR_SCROLL_MARGIN: usize = 15;

#[derive(Debug)]
pub struct CommitListState<'a> {
    pub(super) commits: Vec<CommitInfo<'a>>,
    commit_hash_to_raw: FxHashMap<CommitHash, RawCommitIdx>,
    graph: Rc<Graph>,
    // 由主要 graph 與 filtered graph 共用：兩者都是從同一份
    // `GraphColorSet` / `Repository` 建出來的，所以只有一份，
    // 不是每個 graph 各配一份。
    graph_colors: Vec<Color>,
    pub(super) head_commit_hash: Option<CommitHash>,
    cell_width_type: CellWidthType,
    /// 緊湊模式：commit 文字貼齊該列 graph 實際畫到的最右邊，marker 欄與
    /// graph 右側留白都拿掉。跟 `cell_width_type` 一樣，每幀由
    /// `CommitList::render` 依 `area.width` 重新決定（見
    /// `super::layout::decide`），不是建構時凍結的值。
    compact: bool,
    /// 選取列的 graph 文字結束在第幾格（緊湊模式下 detail／refs／delete_ref
    /// 面板拿這個當左緣，取代非緊湊模式的 `graph_area_cell_width()`）。
    /// 跟 `cell_width_type`／`compact` 一樣每幀更新，非緊湊模式下不使用。
    selected_text_x: u16,
    pub(super) head: Head,

    // Filtered graph（remote-only commits 被隱藏時使用）
    filtered: Option<Rc<Graph>>,
    // Marker-overlay 的顏色對照表（commit_hash -> color），鍵的方式跟上面的
    // `graph_colors`（以 pos_x 為索引的調色盤）不同。雖然名稱相近，
    // 但不是同一個概念 -- 不要合併。
    filtered_graph_colors: Option<FxHashMap<CommitHash, Color>>,

    ref_name_to_commit_index_map: FxHashMap<String, RawCommitIdx>,

    pub(super) search_state: SearchState,
    pub(super) search_input: Input,
    pub(super) search_matches: Vec<SearchMatch>,

    // 最佳化：記住前一次搜尋，供增量搜尋使用
    pub(super) last_search_query: String,
    pub(super) last_matched_indices: Vec<RawCommitIdx>,
    pub(super) last_search_ignore_case: bool,
    pub(super) last_search_fuzzy: bool,

    // Filter 模式
    pub(super) filter_state: FilterState,
    pub(super) filter_input: Input,
    pub(super) filtered_indices: Vec<RawCommitIdx>,
    pub(super) text_filtered_indices: Vec<RawCommitIdx>,

    pub(super) selected: usize,
    pub(super) offset: usize,
    pub(super) total: usize,
    pub(super) height: usize,

    pub(super) inline_detail_height: u16,

    pub(super) show_remote_refs: bool,
    remote_only_commits: FxHashSet<CommitHash>,
    needs_graph_clear: bool,

    name_cell_width: u16,

    pub(super) default_ignore_case: bool,
    pub(super) default_fuzzy: bool,

    working_changes: Option<WorkingChanges>,

    pub(crate) selected_row_overflows: Cell<bool>,
}

impl<'a> CommitListState<'a> {
    pub fn new(
        commits: Vec<CommitInfo<'a>>,
        graph: Rc<Graph>,
        graph_colors: Vec<Color>,
        head_commit_hash: Option<CommitHash>,
        head: Head,
        ref_name_to_commit_index_map: FxHashMap<String, RawCommitIdx>,
        default_ignore_case: bool,
        default_fuzzy: bool,
        filtered: Option<Rc<Graph>>,
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
            // 佔位值：`CommitList::render` 在第一次繪製時就會透過
            // `set_layout` 依實際 `area.width` 覆寫，這裡的值只是讓 struct
            // 在那之前保持合法狀態。
            cell_width_type: CellWidthType::Double,
            compact: false,
            selected_text_x: 0,
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

    pub fn into_graph_parts(self) -> (Option<Rc<Graph>>, FxHashSet<CommitHash>) {
        (self.filtered, self.remote_only_commits)
    }

    /// `current_graph()` 本身的寬度，不含右側留白。不一定是 `self.graph`
    /// -- 走的是跟 `current_graph()` 本身一樣的 filtered/`show_remote_refs`
    /// fallback，所以永遠對得上實際被渲染的那個 graph。
    pub(super) fn graph_cell_width(&self) -> u16 {
        crate::graph::graph_cell_width(self.current_graph(), self.cell_width_type)
    }

    /// `graph_cell_width()` 加上右側留白（非緊湊模式下的版面才有這一格）。
    pub fn graph_area_cell_width(&self) -> u16 {
        self.graph_cell_width() + 1
    }

    pub(super) fn current_cell_count(&self) -> usize {
        self.current_graph().cell_count()
    }

    /// 每幀由 `CommitList::render` 呼叫，寫入依 `area.width` 重新決定的
    /// 寬度／緊湊設定。要在 `build_visible_rows`（它內部呼叫的
    /// `text_cells_for_hash`／`is_compact` 都讀這兩個欄位）之前呼叫。
    pub(super) fn set_layout(&mut self, cell_width_type: CellWidthType, compact: bool) {
        self.cell_width_type = cell_width_type;
        self.compact = compact;
    }

    /// 每幀由 `CommitList::render` 在 `build_visible_rows` 算出選取列的
    /// `text_x` 之後呼叫。
    pub(super) fn set_selected_text_x(&mut self, x: u16) {
        self.selected_text_x = x;
    }

    /// commit list 本身的左緣寬度：非緊湊模式假設 Marker 欄固定存在
    /// （`graph_area_cell_width() + 1`），緊湊模式沒有固定寬度，改用
    /// 選取列自己的 text_x。refs／delete_ref 的側欄面板定位用這個。
    /// detail.rs 的 inline detail 面板因為非緊湊分支要逐一檢查
    /// `ui_config.list.columns`（Marker 可能根本沒配置），走自己的
    /// `calc_graph_marker_width`，不能共用這個。
    pub fn panel_left_edge(&self) -> u16 {
        if self.compact {
            self.selected_text_x
        } else {
            self.graph_area_cell_width() + 1
        }
    }

    pub fn is_compact(&self) -> bool {
        self.compact
    }

    /// 緊湊模式下 detail／refs／delete_ref 面板拿這個當左緣，取代非緊湊
    /// 模式的 `graph_area_cell_width()`。
    pub fn selected_row_text_x(&self) -> u16 {
        self.selected_text_x
    }

    pub fn name_cell_width(&self) -> u16 {
        self.name_cell_width
    }

    pub fn set_inline_detail_height(&mut self, h: u16) {
        self.inline_detail_height = h;
    }

    /// 計算 inline detail 內容的 Rect（在 graph+marker 欄位右側）。
    /// `content_area` 是 commit list 的內容區域（header 下方）。
    /// `graph_marker_width` 是 graph + marker 欄位加總的寬度。
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

    /// 重建全新的 `CommitListState` 後，還原 remote-refs 顯示旗標
    /// （refresh path 用這個把使用者的 toggle 狀態帶到新的 App instance）。
    ///
    /// 約定：呼叫端負責完整的終端機重繪。
    /// refresh path 已經透過 `lib.rs` 裡的 `terminal.clear()` 做過這件事，
    /// 所以這個 setter 刻意 **不** 設定
    /// `needs_graph_clear` —— 設了會造成 double clear，多出一個空白畫面。
    ///
    /// 不要從互動式按鍵處理函式呼叫。那些場合請用 `toggle_remote_refs`，
    /// 它才擁有完整的 widget-local invalidation 約定。
    pub fn set_show_remote_refs(&mut self, show: bool) {
        if self.show_remote_refs == show {
            return;
        }
        self.show_remote_refs = show;
        // rebuild_filtered_indices 會把 selected/offset 夾進新的索引空間，
        // 所以呼叫端不必在意跟 reset_height 的先後順序。
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

    pub(super) fn virtual_row_offset(&self) -> usize {
        if self.has_virtual_row() {
            1
        } else {
            0
        }
    }

    pub fn is_virtual_row_selected(&self) -> bool {
        self.has_virtual_row() && self.offset + self.selected == 0
    }

    pub(super) fn first_visible_commit_hash(&self) -> Option<&CommitHash> {
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

    pub(super) fn commit(&self, idx: RawCommitIdx) -> &CommitInfo<'a> {
        &self.commits[idx.0]
    }

    pub(super) fn search_match(&self, idx: RawCommitIdx) -> &SearchMatch {
        &self.search_matches[idx.0]
    }

    pub(super) fn search_match_mut(&mut self, idx: RawCommitIdx) -> &mut SearchMatch {
        &mut self.search_matches[idx.0]
    }

    pub(super) fn raw_to_filtered(&self, raw: RawCommitIdx) -> Option<FilteredIdx> {
        resolve_raw_to_filtered(&self.filtered_indices, self.commits.len(), raw)
    }

    /// `None` 代表輸入的 `FilteredIdx` 越界。caller 不得 fallback 成 `RawCommitIdx(0)`：
    /// 合法對應只有「早退 / 游標不動」或 `debug_assert!`（render path invariant）。
    pub(super) fn filtered_to_raw(&self, f: FilteredIdx) -> Option<RawCommitIdx> {
        resolve_filtered_to_raw(&self.filtered_indices, self.commits.len(), f)
    }

    fn visible_to_filtered(&self, v: VisibleIdx) -> FilteredIdx {
        FilteredIdx(v.0.saturating_sub(self.virtual_row_offset()))
    }

    fn filtered_to_visible(&self, f: FilteredIdx) -> VisibleIdx {
        VisibleIdx(f.0 + self.virtual_row_offset())
    }

    pub(super) fn raw_to_visible(&self, raw: RawCommitIdx) -> Option<VisibleIdx> {
        self.raw_to_filtered(raw)
            .map(|f| self.filtered_to_visible(f))
    }

    fn current_visible(&self) -> VisibleIdx {
        VisibleIdx(self.offset + self.selected)
    }

    /// 唯一允許寫 `self.offset` / `self.selected` 的入口（相對位移除外）。
    pub(super) fn set_visible_selection(&mut self, target: VisibleIdx) {
        if let Some((offset, selected)) = compute_selection(target, self.total, self.height) {
            self.offset = offset;
            self.selected = selected;
        }
    }

    pub fn working_changes(&self) -> Option<&WorkingChanges> {
        self.working_changes.as_ref()
    }

    pub(super) fn rebuild_filtered_indices(&mut self) {
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
        let Some(parent) = self.selected_commit_parent_hash() else {
            return;
        };
        let Some(&raw) = self.commit_hash_to_raw.get(parent) else {
            return;
        };
        self.step_to_raw(raw);
    }

    /// 逐格把游標移到 `raw` 所在的可視列，沿用 select_next / select_prev 的
    /// scroll margin 行為。`raw` 被 filter 濾掉時靜默不動。迭代次數算好
    /// 才跑（而非以「游標到了沒」當終止條件），`height == 0` 時
    /// select_next / select_prev 各自是 no-op，迴圈跑完游標原地不動——
    /// 不需要額外的早退,也不會有選不到就空轉的風險。
    fn step_to_raw(&mut self, raw: RawCommitIdx) {
        let Some(target) = self.raw_to_visible(raw) else {
            return;
        };
        let current = self.current_visible().0;
        for _ in target.0..current {
            self.select_prev();
        }
        for _ in current..target.0 {
            self.select_next();
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

    /// 跳到目前 commit 的 child —— `select_parent()` 的反向。清單由新到舊
    /// （`--topo-order`，parent 不會出現在 child 之前），所以 child 必在
    /// 游標上方；從游標往上掃 filtered 座標，取第一個把目前 commit 列進
    /// `parent_commit_hashes`（不限 first-parent）的可視列。用「任一
    /// parent」而非「只認 first-parent」是刻意的：merge commit 對
    /// second-parent 那條線在主 graph 上也真的畫出來（見
    /// `graph::calc::calc_edges` 的 merge detour），只認 first-parent 會漏掉
    /// 游標正上方、畫面上看得到線的 merge commit。不追求跟 `select_parent()`
    /// 嚴格互逆——parent 是單值欄位、child 是多值關係，天生沒有唯一反向。
    /// 找不到就靜默不動。
    pub fn select_child(&mut self) {
        if let Some(raw) = self.selected_commit_child_raw() {
            self.step_to_raw(raw);
        }
    }

    fn selected_commit_child_raw(&self) -> Option<RawCommitIdx> {
        if self.total == 0 || self.is_virtual_row_selected() {
            return None;
        }
        let current = self.current_selected_raw();
        let hash = self.commit(current).commit_hash();
        let current_filtered = self.raw_to_filtered(current)?;
        (0..current_filtered.0)
            .rev()
            .filter_map(|i| self.filtered_to_raw(FilteredIdx(i)))
            .find(|&raw| self.commit(raw).commit.parent_commit_hashes.contains(hash))
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

    pub fn selected_commit_hash(&self) -> &CommitHash {
        // 虛擬行被選中時，退而求其次回傳第一個 commit hash 作為 fallback
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
        self.step_to_raw(raw);
    }

    fn current_graph(&self) -> &Graph {
        if !self.show_remote_refs {
            if let Some(ref f) = self.filtered {
                return f;
            }
        }
        &self.graph
    }

    pub(super) fn text_cells_for_hash(&self, hash: &CommitHash) -> Option<Vec<TextCell>> {
        crate::graph::text_cells(
            self.current_graph(),
            hash,
            &self.graph_colors,
            self.cell_width_type,
        )
    }

    pub(super) fn marker_color(&self, commit_info: &CommitInfo<'_>) -> Color {
        if !self.show_remote_refs {
            if let Some(ref colors) = self.filtered_graph_colors {
                if let Some(&color) = colors.get(commit_info.commit_hash()) {
                    return color;
                }
            }
        }
        commit_info.graph_color
    }
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
    use crate::git::Commit;

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

    // --- select_parent() / select_child() 回歸測試 ----------------------------

    fn commit_fixture(hash: &str, parents: &[&str]) -> Commit {
        Commit {
            commit_hash: hash.into(),
            parent_commit_hashes: parents.iter().copied().map(CommitHash::from).collect(),
            ..Default::default()
        }
    }

    /// `visible_raw` 指定哪些 raw index 留在 `filtered_indices` 內
    /// （其餘視為被 filter 藏起來），游標停在第一個可見列。
    fn build_state_visible_raws<'a>(
        commits: &'a [Commit],
        visible_raw: &[usize],
    ) -> CommitListState<'a> {
        let infos = commits
            .iter()
            .map(|c| CommitInfo::new(c, Vec::new(), Color::Reset))
            .collect();
        let graph = Graph {
            commit_hashes: Vec::new(),
            commit_pos_map: FxHashMap::default(),
            edges: Vec::new(),
            max_pos_x: 0,
        };
        let mut state = CommitListState::new(
            infos,
            Rc::new(graph),
            Vec::new(),
            None,
            Head::None,
            FxHashMap::default(),
            false,
            false,
            None,
            None,
            FxHashSet::default(),
            None,
        );
        state.filtered_indices = visible_raw.iter().copied().map(RawCommitIdx).collect();
        state.total = state.filtered_indices.len();
        state.reset_height(10);
        state.select_first();
        state
    }

    #[test]
    fn select_parent_hidden_by_filter_does_not_hang_and_leaves_cursor() {
        // 由新到舊：child(0) -> parent(1) -> grandparent(2)。filter 只留
        // child 跟 grandparent（raw 1 被藏起來）。
        let commits = vec![
            commit_fixture("child", &["parent"]),
            commit_fixture("parent", &["grandparent"]),
            commit_fixture("grandparent", &[]),
        ];
        let mut state = build_state_visible_raws(&commits, &[0, 2]);

        // 修正前：select_next() 在 filtered total=2 觸底後直接 return，
        // 但 while 迴圈比對的是全域 commit_hash_to_raw 找到的 hash，永遠
        // 走不到「parent」→ 無窮迴圈。這裡若卡住，測試本身就會逾時失敗。
        state.select_parent();

        assert_eq!(
            state.selected_commit_hash().as_str(),
            "child",
            "parent 被 filter 藏起來時，游標應該留在原地"
        );
    }

    #[test]
    fn select_parent_visible_in_filter_moves_cursor() {
        let commits = vec![
            commit_fixture("child", &["grandparent"]),
            commit_fixture("middle", &[]),
            commit_fixture("grandparent", &[]),
        ];
        let mut state = build_state_visible_raws(&commits, &[0, 2]);

        state.select_parent();

        assert_eq!(state.selected_commit_hash().as_str(), "grandparent");
    }

    #[test]
    fn select_child_moves_to_nearest_visible_child() {
        // 由新到舊：tip(0) -> merge(1) -> base(2)。hidden(不可見) 的 parent
        // 也是 merge，用來確認掃描不會誤選一個被 filter 藏起來的列。
        let commits = vec![
            commit_fixture("tip", &["merge"]),
            commit_fixture("hidden", &["merge"]),
            commit_fixture("merge", &["base"]),
            commit_fixture("base", &[]),
        ];
        let mut state = build_state_visible_raws(&commits, &[0, 2, 3]);
        state.select_parent(); // tip -> merge

        state.select_child();

        assert_eq!(state.selected_commit_hash().as_str(), "tip");
    }

    #[test]
    fn select_child_at_tip_does_not_move() {
        let commits = vec![commit_fixture("tip", &[]), commit_fixture("base", &[])];
        let mut state = build_state_visible_raws(&commits, &[0, 1]);

        state.select_child();

        assert_eq!(state.selected_commit_hash().as_str(), "tip");
    }

    #[test]
    fn select_child_no_visible_child_does_not_hang_and_leaves_cursor() {
        // base 唯一的 child 是 hidden(1)，被 filter 藏起來；tip(0) 跟 base
        // 沒有 parent/child 關係，純粹用來把 base 推離清單頂端。
        // 掃描範圍有界（0..current_filtered），找不到就回 None，不會像
        // 修正前的 select_parent() 那樣卡住。
        let commits = vec![
            commit_fixture("tip", &[]),
            commit_fixture("hidden", &["base"]),
            commit_fixture("base", &[]),
        ];
        let mut state = build_state_visible_raws(&commits, &[0, 2]);
        state.select_commit_hash(&CommitHash::from("base"));

        state.select_child();

        assert_eq!(state.selected_commit_hash().as_str(), "base");
    }
}

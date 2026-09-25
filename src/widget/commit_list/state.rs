use std::{cell::Cell, rc::Rc};

use ratatui::{layout::Rect, style::Color};
use rustc_hash::{FxHashMap, FxHashSet};
use tui_input::Input;

use crate::git::{CommitHash, Head, Ref, WorkingChanges};
use crate::graph::{CellWidthType, Graph, TextCell};
use crate::widget::scroll;

use super::search::{FilterState, MatchOptions, SearchMatch, SearchState};
use super::{ChildPickOption, CommitInfo, FilteredIdx, RawCommitIdx, VisibleIdx};

/// `CommitListState::select_child` 的結果。標 `#[must_use]`：忽略回傳值等於
/// 假裝分支點一定只有一個 child，`Ambiguous` 那份候選清單會被靜靜丟掉。
#[must_use]
#[derive(Debug)]
pub enum ChildJump {
    /// 目前 commit 沒有任何可視 child（已在最新的 commit，或被 filter 濾掉）。
    None,
    /// 剛好一個可視 child，游標已經跳過去了。
    Jumped,
    /// 兩個以上可視 child（目前 commit 是分支點），游標沒有動，呼叫端要開
    /// picker 讓使用者選要跳到哪一個。
    Ambiguous(Vec<ChildPickOption>),
}

/// `select_child` 候選項目的顯示標籤：tag 或（本地）branch 名稱 + commit
/// subject，subject 一定會帶——分支點的兩個 child 通常都不是任何 ref 的
/// tip，兩個候選都只剩短 hash 的話使用者根本分不出差異，等於沒解決問題。
///
/// 優先序刻意跟 `dispatch_checkout`（`src/view.rs`，local branch > tag）方向
/// 相反：這裡是 tag > branch，是使用者對這個功能明確要求的順序，不是筆誤，
/// 之後想把兩處統一時回來看這行。
fn child_pick_label(info: &CommitInfo) -> String {
    let refs = info.refs();
    let named = refs
        .iter()
        .find(|r| matches!(r, Ref::Tag { .. }))
        .or_else(|| refs.iter().find(|r| matches!(r, Ref::Branch { .. })));
    match named {
        Some(r) => format!("{}: {}", r.name(), info.subject()),
        None => info.subject().to_string(),
    }
}

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
    /// 目前生效的搜尋設定。刻意不放進 `SearchState::Searching`：套用之後
    /// （`Applied`）這組設定還要繼續驅動 refresh 還原，跟輸入模式無關。
    pub(super) search_options: MatchOptions,

    // 最佳化：記住前一次搜尋，供增量搜尋使用。整包存 `MatchOptions`（而非拆成
    // 散裝欄位）：`update_search_matches` 的 `settings_unchanged` 判斷式靠這個
    // 型別的 `PartialEq` 一次比較 ignore_case/fuzzy/target 三個維度，往後這個
    // struct 再加欄位，這裡結構上不可能漏比對。
    pub(super) last_search_query: String,
    pub(super) last_matched_indices: Vec<RawCommitIdx>,
    pub(super) last_search_options: MatchOptions,

    // Filter 模式
    pub(super) filter_state: FilterState,
    pub(super) filter_input: Input,
    /// 目前生效的 filter 設定，理由同 `search_options`。
    pub(super) filter_options: MatchOptions,
    pub(super) filtered_indices: Vec<RawCommitIdx>,
    pub(super) text_filtered_indices: Vec<RawCommitIdx>,

    pub(super) selected: usize,
    pub(super) offset: usize,
    pub(super) total: usize,
    pub(super) height: usize,
    /// 游標與清單上下緣至少保留的列數（`ui.list.scrolloff`，0 = 沒有邊距）。
    /// 實際生效值見 `effective_scrolloff()`——矮畫面會再夾一次，不能直接用
    /// 這個原始值算邊界。
    pub(super) scrolloff: usize,

    pub(super) inline_detail_height: u16,

    pub(super) show_remote_refs: bool,
    remote_only_commits: FxHashSet<CommitHash>,
    needs_graph_clear: bool,

    name_cell_width: u16,

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
        search_defaults: MatchOptions,
        filtered: Option<Rc<Graph>>,
        filtered_graph_colors: Option<FxHashMap<CommitHash, Color>>,
        remote_only_commits: FxHashSet<CommitHash>,
        working_changes: Option<WorkingChanges>,
        scrolloff: usize,
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
            search_options: search_defaults,
            last_search_query: String::new(),
            last_matched_indices: Vec::new(),
            // 初始值不影響正確性：`can_use_incremental` 有
            // `!last_search_query.is_empty()` 守衛，首次呼叫 `update_search_matches`
            // 時 `last_search_query` 必為空字串，一定會走全量掃描並覆寫這個值。
            last_search_options: search_defaults,
            filter_state: FilterState::Inactive,
            filter_input: Input::default(),
            filter_options: MatchOptions::FILTER_DEFAULT,
            filtered_indices: Vec::new(),
            text_filtered_indices: Vec::new(),
            selected: 0,
            offset: 0,
            total,
            height: 0,
            scrolloff,
            inline_detail_height: 0,
            show_remote_refs: true,
            remote_only_commits,
            needs_graph_clear: false,
            name_cell_width,
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
    // 不變式：`self.offset` 與 `self.selected` 只能透過 `place`（或呼叫它的
    // `set_visible_selection` / `select_visible_index`）寫入，避免
    // offset + selected 越過 `total`。唯一例外是 `rebuild_filtered_indices`
    // 開頭的歸零——`total` 可能因 filter 縮到 0，這時 `place` 是 no-op，
    // 要靠那兩行防止殘留舊值。

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

    /// 跳轉：把 `target` 放到距上緣 `effective_scrolloff()` 列的位置（傳給
    /// `place` 的 `prev_offset` 是 `usize::MAX`，一定會被夾到這個位置，等
    /// 同「捲到能看見 target 的最小 offset」）。`select_ref`／
    /// `select_commit_hash`／搜尋 n／N／filter 重建都走這裡。
    pub(super) fn set_visible_selection(&mut self, target: VisibleIdx) {
        self.place(target, usize::MAX);
    }

    /// 移動：以目前 `self.offset` 為捲動基準，只在 `target` 即將超出可視
    /// margin 範圍時才捲動一格。`select_next`／`select_prev`／`step_to_raw`
    /// 都走這裡，跟 `set_visible_selection` 的差異只在傳給 `place` 的
    /// `prev_offset`。
    fn select_visible_index(&mut self, target: VisibleIdx) {
        self.place(target, self.offset);
    }

    /// 唯一寫 `self.offset`／`self.selected` 的地方，見上方不變式註解。
    /// `prev_offset` 決定 `scroll::scrolled_offset` 的捲動基準：
    /// `usize::MAX` 得到「跳轉」語意（永遠會被夾到 margin 邊界），
    /// `self.offset` 得到「移動」語意（最小捲動）。`target >= self.total`
    /// 或 `height == 0` 時整個 no-op，維持 `selected < height` 的不變式。
    fn place(&mut self, target: VisibleIdx, prev_offset: usize) {
        if target.0 >= self.total || self.height == 0 {
            return;
        }
        self.offset = scroll::scrolled_offset(
            target.0,
            self.height,
            self.total,
            prev_offset,
            self.scrolloff,
        );
        self.selected = target.0 - self.offset;
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

    /// `self.scrolloff` 的實際生效值：矮畫面會再夾到 `(height-1)/2`，避免
    /// 上下邊距互相矛盾。
    fn effective_scrolloff(&self) -> usize {
        scroll::effective_scrolloff(self.scrolloff, self.height)
    }

    pub fn select_next(&mut self) {
        if self.total == 0 || self.height == 0 {
            return;
        }
        let cur = self.current_visible().0;
        if cur + 1 >= self.total {
            return;
        }
        self.select_visible_index(VisibleIdx(cur + 1));
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

    /// 把游標移到 `raw` 所在的可視列，沿用 `select_next`／`select_prev` 的
    /// scrolloff 手感——以目前 offset 為基準做最小捲動，不會像
    /// `set_visible_selection` 那樣把目標釘到距上緣 margin 列。`raw` 被
    /// filter 濾掉時靜默不動。
    fn step_to_raw(&mut self, raw: RawCommitIdx) {
        let Some(target) = self.raw_to_visible(raw) else {
            return;
        };
        self.select_visible_index(target);
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
    /// 游標上方；從游標往上掃 filtered 座標，收集把目前 commit 列進
    /// `parent_commit_hashes`（不限 first-parent）的可視列。用「任一
    /// parent」而非「只認 first-parent」是刻意的：merge commit 對
    /// second-parent 那條線在主 graph 上也真的畫出來（見
    /// `graph::calc::calc_edges` 的 merge detour），只認 first-parent 會漏掉
    /// 游標正上方、畫面上看得到線的 merge commit。剛好一個可視 child 才跳；
    /// 找到兩個以上代表目前 commit 是分支點，沒有依據替使用者猜要去哪一
    /// 條，交給 `Ambiguous` 的候選清單讓呼叫端開 picker 問使用者。不追求跟
    /// `select_parent()` 嚴格互逆——parent 是單值欄位、child 是多值關係，
    /// 天生沒有唯一反向。
    pub fn select_child(&mut self) -> ChildJump {
        let candidates = self.selected_commit_children_raw();
        match candidates.as_slice() {
            [] => ChildJump::None,
            [only] => {
                self.step_to_raw(*only);
                ChildJump::Jumped
            }
            _ => ChildJump::Ambiguous(
                candidates
                    .iter()
                    .map(|&raw| self.child_pick_option(raw))
                    .collect(),
            ),
        }
    }

    fn selected_commit_children_raw(&self) -> Vec<RawCommitIdx> {
        if self.total == 0 || self.is_virtual_row_selected() {
            return Vec::new();
        }
        let current = self.current_selected_raw();
        let hash = self.commit(current).commit_hash();
        let Some(current_filtered) = self.raw_to_filtered(current) else {
            return Vec::new();
        };
        (0..current_filtered.0)
            .rev()
            .filter_map(|i| self.filtered_to_raw(FilteredIdx(i)))
            .filter(|&raw| self.commit(raw).commit.parent_commit_hashes.contains(hash))
            .collect()
    }

    fn child_pick_option(&self, raw: RawCommitIdx) -> ChildPickOption {
        let info = self.commit(raw);
        ChildPickOption {
            label: child_pick_label(info),
            commit_hash: info.commit_hash().clone(),
        }
    }

    pub fn select_prev(&mut self) {
        if self.height == 0 {
            return;
        }
        let cur = self.current_visible().0;
        if cur == 0 {
            return;
        }
        self.select_visible_index(VisibleIdx(cur - 1));
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
        if self.height == 0 {
            return;
        }
        if self.offset + self.height < self.total {
            self.offset += 1;
            let margin = self.effective_scrolloff();
            self.selected = self.selected.saturating_sub(1).max(margin);
        }
    }

    pub fn scroll_up(&mut self) {
        if self.height == 0 {
            return;
        }
        if self.offset > 0 {
            self.offset -= 1;
            let margin = self.effective_scrolloff();
            self.selected = (self.selected + 1).min(self.height - 1 - margin);
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

    /// refresh 還原：把目前選中的 commit（`current_visible()`，不重新查
    /// hash——commit 若已經不在了，呼叫端的 `select_commit_hash` 早就沒有
    /// 移動游標，這裡只重新定位捲動視窗）放回螢幕上第 `row` 列。
    ///
    /// `prev_offset = target - row` 正是「offset 使得 selected == row」的
    /// 算式，交給 `place` 再過一次 `scroll::scrolled_offset` 的邊界檢查——
    /// `row` 落在 margin 帶內、或目標剛好在最後一頁時，會被自動修正，不
    /// 會像舊版 `scroll_up()` 迴圈那樣在最後一頁選到別的 commit（total=100,
    /// height=10, offset=90, row=9 這組輸入下，舊版會把 offset 拉到 81，
    /// 選取的 commit 就換了）。
    pub fn restore_selected_row(&mut self, row: usize) {
        let target = self.current_visible();
        let prev_offset = target.0.saturating_sub(row);
        self.place(target, prev_offset);
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

    /// 跟 `select_commit_hash` 的差異在捲動手感：這支走 `step_to_raw`，用
    /// 最小捲動比照上下移動的手感；`select_commit_hash` 走
    /// `set_visible_selection`，會把目標放到距上緣 `effective_scrolloff()`
    /// 列的位置。`select_head` 與 `select_child`（分支點選完 picker 之後）
    /// 都要跟一般移動手感一致，用這支；refresh 還原視角改用
    /// `restore_selected_row`，不要跟這兩支混用。
    pub fn step_to_commit_hash(&mut self, commit_hash: &CommitHash) {
        let Some(&raw) = self.commit_hash_to_raw.get(commit_hash) else {
            return;
        };
        self.step_to_raw(raw);
    }

    /// 把游標移到 HEAD 指向的 commit,畫面比照上下移動的最小捲動手感
    /// (不把 HEAD 硬拉到最上面)。HEAD 不存在或被 filter 濾掉時靜默不動。
    pub fn select_head(&mut self) {
        let Some(head) = self.head_commit_hash.clone() else {
            return;
        };
        self.step_to_commit_hash(&head);
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

/// Pure：`place(target, usize::MAX)` 的等價算法，只給測試用（不必真的建
/// 一個 `CommitListState` 就能驗證跳轉座標數學）。target 越界或 height=0
/// 回 None（caller 不動游標）。委派給 `scroll::scrolled_offset`，不另寫
/// 一套公式——`prev_offset = usize::MAX` 保證結果一定被夾到「距上緣
/// scrolloff 列」那個跳轉語意。
#[cfg(test)]
fn compute_selection(
    target: VisibleIdx,
    total: usize,
    height: usize,
    scrolloff: usize,
) -> Option<(usize, usize)> {
    if target.0 >= total || height == 0 {
        return None;
    }
    let offset = scroll::scrolled_offset(target.0, height, total, usize::MAX, scrolloff);
    let selected = target.0 - offset;
    debug_assert!(selected < height);
    Some((offset, selected))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{Commit, FileChange};

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
        let (offset, selected) = compute_selection(VisibleIdx(2), 10, 5, 0).unwrap();
        assert_eq!((offset, selected), (2, 0));
    }

    #[test]
    fn compute_selection_beyond_first_page_pins_max_offset() {
        // total=10, height=5, target=8 → offset pin 到 max_offset=5，selected=3
        let (offset, selected) = compute_selection(VisibleIdx(8), 10, 5, 0).unwrap();
        assert_eq!((offset, selected), (5, 3));
        assert!(offset + selected < 10);
        assert!(selected < 5);
    }

    #[test]
    fn compute_selection_total_le_height_uses_selected_only() {
        // total=3, height=10（height 比 total 大）→ 公式退化成 offset=0
        let (offset, selected) = compute_selection(VisibleIdx(2), 3, 10, 0).unwrap();
        assert_eq!((offset, selected), (0, 2));
    }

    #[test]
    fn compute_selection_out_of_range_returns_none() {
        // target >= total：不動游標（防呆原 panic 場景）
        assert!(compute_selection(VisibleIdx(10), 10, 5, 0).is_none());
        assert!(compute_selection(VisibleIdx(309), 234, 50, 0).is_none());
    }

    #[test]
    fn compute_selection_zero_height_returns_none() {
        // height=0（例如畫面尚未配置）→ 不動游標
        assert!(compute_selection(VisibleIdx(0), 10, 0, 0).is_none());
    }

    #[test]
    fn compute_selection_never_puts_cursor_off_screen() {
        // 窮舉：任何合法 target、任何 scrolloff 都應產生
        // offset + selected < total、selected < height。
        for total in 1..30 {
            for height in 1..20 {
                for scrolloff in [0, 1, 2, 5, 100] {
                    for t in 0..total {
                        let Some((offset, selected)) =
                            compute_selection(VisibleIdx(t), total, height, scrolloff)
                        else {
                            continue;
                        };
                        assert!(
                            offset + selected < total,
                            "invariant: offset+selected < total (t={t}, total={total}, h={height}, so={scrolloff})"
                        );
                        assert!(
                            selected < height,
                            "invariant: selected < height (t={t}, total={total}, h={height}, so={scrolloff})"
                        );
                    }
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
        let (offset, selected) = compute_selection(VisibleIdx(in_filter.0), 234, 50, 0).unwrap();
        assert!(offset + selected < 234);
        assert!(selected < 50);
    }

    // --- ui.list.scrolloff 回歸測試 -------------------------------------------
    // `current_list_status()` 回傳 `(selected, offset, height)`。

    /// `n` 個互不相關的 commit，全部可見，游標停在第一列，`height`／
    /// `scrolloff` 照呼叫端指定的值設定。
    fn scrolloff_fixture(
        commits: &[Commit],
        height: usize,
        scrolloff: usize,
    ) -> CommitListState<'_> {
        let visible: Vec<usize> = (0..commits.len()).collect();
        let mut state = build_state_visible_raws(commits, &visible);
        state.scrolloff = scrolloff;
        state.reset_height(height);
        state.select_first();
        state
    }

    fn commits_fixture(n: usize) -> Vec<Commit> {
        (0..n)
            .map(|i| commit_fixture(&format!("c{i}"), &[]))
            .collect()
    }

    #[test]
    fn scrolloff_keeps_context_for_selection_and_single_row_scrolling() {
        let commits = commits_fixture(12);
        let mut state = scrolloff_fixture(&commits, 6, 2);

        for _ in 0..4 {
            state.select_next();
        }
        assert_eq!(state.current_list_status(), (3, 1, 6));

        state.select_prev();
        state.select_prev();
        assert_eq!(state.current_list_status(), (2, 0, 6));

        state.set_visible_selection(VisibleIdx(7));
        assert_eq!(state.current_list_status(), (2, 5, 6));

        state.scroll_down();
        assert_eq!(state.current_list_status(), (2, 6, 6));
        state.scroll_up();
        assert_eq!(state.current_list_status(), (3, 5, 6));

        state.select_last();
        assert_eq!(state.current_list_status(), (5, 6, 6));
        state.select_prev();
        assert_eq!(state.current_list_status(), (4, 6, 6));
    }

    #[test]
    fn scrolloff_is_limited_by_list_height() {
        let commits = commits_fixture(8);
        let mut state = scrolloff_fixture(&commits, 2, 10);

        state.select_next();
        state.select_next();
        assert_eq!(state.current_list_status(), (1, 1, 2));

        state.set_visible_selection(VisibleIdx(6));
        assert_eq!(state.current_list_status(), (0, 6, 2));

        state.restore_selected_row(1);
        assert_eq!(state.current_list_status(), (1, 5, 2));
    }

    #[test]
    fn refresh_restores_selected_row_with_scrolloff() {
        let commits = commits_fixture(30);
        let mut state = scrolloff_fixture(&commits, 10, 2);

        state.set_visible_selection(VisibleIdx(15));
        assert_eq!(state.current_list_status(), (2, 13, 10));
        let target_hash = state.selected_commit_hash().clone();

        state.restore_selected_row(7);
        assert_eq!(state.current_list_status(), (7, 8, 10));
        assert_eq!(*state.selected_commit_hash(), target_hash);
    }

    /// refresh 還原在最後一頁不會換選到別的 commit——舊版 `scroll_up()`
    /// 迴圈在這個場景（offset=90, height=10, row=9）會把 offset 拉到 81，
    /// 選取的 commit 就換了。so=0／so=15 都要成立。
    #[test]
    fn restore_selected_row_on_last_page_keeps_same_commit() {
        for scrolloff in [0, 15] {
            let commits = commits_fixture(100);
            let mut state = scrolloff_fixture(&commits, 10, scrolloff);

            state.select_last();
            assert_eq!(state.current_list_status(), (9, 90, 10), "so={scrolloff}");
            let target_hash = state.selected_commit_hash().clone();

            state.restore_selected_row(9);
            assert_eq!(state.current_list_status(), (9, 90, 10), "so={scrolloff}");
            assert_eq!(*state.selected_commit_hash(), target_hash, "so={scrolloff}");
        }
    }

    #[test]
    fn restore_selected_row_clamps_row_to_height() {
        let commits = commits_fixture(30);
        let mut state = scrolloff_fixture(&commits, 10, 0);

        state.set_visible_selection(VisibleIdx(20));
        assert_eq!(state.current_list_status(), (0, 20, 10));

        state.restore_selected_row(50);
        assert_eq!(state.current_list_status(), (9, 11, 10));
    }

    #[test]
    fn restore_selected_row_inside_margin_is_pushed_out() {
        let commits = commits_fixture(30);
        let mut state = scrolloff_fixture(&commits, 10, 2);

        state.set_visible_selection(VisibleIdx(15));
        assert_eq!(state.current_list_status(), (2, 13, 10));

        state.restore_selected_row(0);
        assert_eq!(state.current_list_status(), (2, 13, 10));
    }

    #[test]
    fn default_scrolloff_15_in_tall_viewport() {
        let commits = commits_fixture(100);
        let mut state = scrolloff_fixture(&commits, 40, 15);

        for _ in 0..24 {
            state.select_next();
        }
        assert_eq!(state.current_list_status(), (24, 0, 40));
        state.select_next();
        assert_eq!(state.current_list_status(), (24, 1, 40));
    }

    #[test]
    fn step_to_commit_hash_scrolls_minimally_but_select_commit_hash_pins_at_margin() {
        let commits = commits_fixture(50);

        let mut stepped = scrolloff_fixture(&commits, 10, 2);
        stepped.step_to_commit_hash(&CommitHash::from("c20"));
        assert_eq!(stepped.current_list_status(), (7, 13, 10));
        stepped.step_to_commit_hash(&CommitHash::from("c16"));
        assert_eq!(stepped.current_list_status(), (3, 13, 10));

        let mut pinned = scrolloff_fixture(&commits, 10, 2);
        pinned.select_commit_hash(&CommitHash::from("c20"));
        assert_eq!(pinned.current_list_status(), (2, 18, 10));
    }

    #[test]
    fn scroll_down_is_noop_when_height_zero() {
        let commits = commits_fixture(10);
        let mut state = scrolloff_fixture(&commits, 10, 0);
        state.reset_height(0);

        state.scroll_down();

        assert_eq!(state.current_list_status(), (0, 0, 0));
    }

    /// shift-j 在游標本來就落在 margin 帶內（例如剛釘頂）時，不能讓
    /// `selected` 一路掉到 0 破壞邊距——`scroll_down` 的 `.max(margin)`
    /// 就是為了擋這個。
    #[test]
    fn scroll_down_does_not_collapse_margin_when_cursor_starts_inside_it() {
        let commits = commits_fixture(30);
        let mut state = scrolloff_fixture(&commits, 10, 2);

        state.scroll_down();

        assert!(state.current_list_status().0 >= 2);
    }

    #[test]
    fn virtual_row_at_visible_zero_respects_scrolloff() {
        let commits = commits_fixture(20);
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
        let working_changes = WorkingChanges {
            unstaged: vec![FileChange::Untracked {
                path: "new.txt".into(),
                stats: None,
            }],
            staged: Vec::new(),
        };
        let mut state = CommitListState::new(
            infos,
            Rc::new(graph),
            Vec::new(),
            None,
            Head::None,
            FxHashMap::default(),
            MatchOptions::default(),
            None,
            None,
            FxHashSet::default(),
            Some(working_changes),
            2,
        );
        state.reset_height(10);

        state.select_first();
        assert!(state.is_virtual_row_selected());
        assert_eq!(state.current_list_status(), (0, 0, 10));

        state.set_visible_selection(VisibleIdx(1));
        assert!(!state.is_virtual_row_selected());
        assert_eq!(state.current_list_status().1, 0);
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
            MatchOptions::default(),
            None,
            None,
            FxHashSet::default(),
            None,
            0,
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
    fn select_child_moves_to_sole_visible_child() {
        // 由新到舊：tip(0) -> merge(1) -> base(2)。hidden(不可見) 的 parent
        // 也是 merge，用來確認掃描不會誤選一個被 filter 藏起來的列
        // （所以 merge 的可視 child 只有 tip 一個，不算分支點）。
        let commits = vec![
            commit_fixture("tip", &["merge"]),
            commit_fixture("hidden", &["merge"]),
            commit_fixture("merge", &["base"]),
            commit_fixture("base", &[]),
        ];
        let mut state = build_state_visible_raws(&commits, &[0, 2, 3]);
        state.select_parent(); // tip -> merge

        let jump = state.select_child();

        assert!(matches!(jump, ChildJump::Jumped));
        assert_eq!(state.selected_commit_hash().as_str(), "tip");
    }

    #[test]
    fn select_child_multiple_visible_children_returns_ambiguous() {
        // fork(2) 有兩個可視 child：branchA(0)、branchB(1) —— 真正的分支
        // 點，沒有依據替使用者決定要跳去哪一條，回報候選讓呼叫端開 picker
        // 問使用者，游標本身不動。
        let commits = vec![
            commit_fixture("branchA", &["fork"]),
            commit_fixture("branchB", &["fork"]),
            commit_fixture("fork", &[]),
        ];
        let mut state = build_state_visible_raws(&commits, &[0, 1, 2]);
        state.select_commit_hash(&CommitHash::from("fork"));

        let jump = state.select_child();

        let ChildJump::Ambiguous(options) = jump else {
            panic!("expected Ambiguous, got {jump:?}");
        };
        // 掃描由游標往上、由近到遠：branchB（raw 1，緊鄰 fork）比 branchA
        // （raw 0，離 fork 較遠）先被收進候選清單。
        let hashes: Vec<&str> = options.iter().map(|o| o.commit_hash.as_str()).collect();
        assert_eq!(hashes, ["branchB", "branchA"]);
        assert_eq!(state.selected_commit_hash().as_str(), "fork");
    }

    #[test]
    fn select_child_at_tip_returns_none() {
        let commits = vec![commit_fixture("tip", &[]), commit_fixture("base", &[])];
        let mut state = build_state_visible_raws(&commits, &[0, 1]);

        let jump = state.select_child();

        assert!(matches!(jump, ChildJump::None));
        assert_eq!(state.selected_commit_hash().as_str(), "tip");
    }

    #[test]
    fn select_child_no_visible_child_returns_none_and_leaves_cursor() {
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

        let jump = state.select_child();

        assert!(matches!(jump, ChildJump::None));
        assert_eq!(state.selected_commit_hash().as_str(), "base");
    }

    #[test]
    fn child_pick_label_prefers_tag_over_branch_over_subject_only() {
        let commit = Commit {
            subject: "fix things".into(),
            ..Default::default()
        };
        let tag = Ref::Tag {
            name: "v1.0".into(),
            target: "deadbeef".into(),
        };
        let branch = Ref::Branch {
            name: "feature/x".into(),
            target: "deadbeef".into(),
        };

        let both = CommitInfo::new(&commit, vec![&branch, &tag], Color::Reset);
        assert_eq!(child_pick_label(&both), "v1.0: fix things");

        let branch_only = CommitInfo::new(&commit, vec![&branch], Color::Reset);
        assert_eq!(child_pick_label(&branch_only), "feature/x: fix things");

        let none = CommitInfo::new(&commit, vec![], Color::Reset);
        assert_eq!(child_pick_label(&none), "fix things");
    }
}

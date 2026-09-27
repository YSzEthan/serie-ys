use ratatui::crossterm::event::{Event, KeyEvent};
use rustc_hash::FxHashMap;
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

use crate::config::SearchTarget;
use crate::fuzzy::SearchMatcher;
use crate::git::Ref;

use super::state::CommitListState;
use super::{CommitInfo, MatchStep, RawCommitIdx, VisibleIdx};

/// 一組比對設定。search 與 filter 各自在 `CommitListState` 持有一份，跨
/// mode 轉換（`Searching` → `Applied`、filter 套用後仍生效）存活——套用後
/// 這組設定還要繼續驅動增量比對與 refresh 還原，跟「現在是不是在輸入」是
/// 兩件事，不該塞進只在輸入模式才存在的 enum variant。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MatchOptions {
    pub ignore_case: bool,
    pub fuzzy: bool,
    pub target: SearchTarget,
}

impl MatchOptions {
    /// Filter 模式預設用 fuzzy + 忽略大小寫，操作體驗比較好；target 維持 `All`，
    /// filter 沒有對應的 config 可以覆寫這個預設值（比照 ignore_case/fuzzy 對
    /// filter 的既有不對稱設計）。
    pub const FILTER_DEFAULT: MatchOptions = MatchOptions {
        ignore_case: true,
        fuzzy: true,
        target: SearchTarget::All,
    };

    /// 三個維度都講、永不省略——不會有「沒顯示 = 哪個狀態」的歧義。
    pub fn status_string(&self) -> String {
        let case = if self.ignore_case {
            "ignore-case"
        } else {
            "case-sensitive"
        };
        let matcher = if self.fuzzy { "fuzzy" } else { "substring" };
        format!("[{case}] [{matcher}] [target: {}]", self.target.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchState {
    Inactive,
    Searching {
        start_index: RawCommitIdx,
        match_index: usize,
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

    fn set_transient_message(&mut self, message: TransientMessage) {
        if let SearchState::Searching {
            transient_message, ..
        } = self
        {
            *transient_message = message;
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
    Target(SearchTarget),
}

impl TransientMessage {
    /// search／filter 兩邊的訊息文字完全相同，只有外層 state（`Searching`／
    /// `Filtering`）不同——這個 match 只該存在一份。
    fn text(self) -> Option<String> {
        match self {
            Self::None => None,
            Self::IgnoreCaseOn => Some("Ignore case: ON ".to_string()),
            Self::IgnoreCaseOff => Some("Ignore case: OFF".to_string()),
            Self::FuzzyOn => Some("Fuzzy match: ON ".to_string()),
            Self::FuzzyOff => Some("Fuzzy match: OFF".to_string()),
            Self::Target(target) => Some(format!("Target: {:<7}", target.as_str().to_uppercase())),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterState {
    Inactive,
    Filtering { transient_message: TransientMessage },
}

/// refresh 之後還原一次 search 或 filter 所需的全部輸入。兩者存的是同一個
/// 概念（餵給 `SearchMatcher::new` 的 query + 設定），共用一個型別。也是
/// `MatchSet::key` 的型別——`Default` 的空字串 query 正好對應「還沒算過」。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MatchQuery {
    pub query: String,
    pub options: MatchOptions,
}

/// 一列的 highlight 位置，只在 render 時對畫面上看得到的列建（見
/// `CommitListState::search_matcher`），不再是每個 commit 都存一份。
#[derive(Debug, Default, Clone)]
pub(super) struct SearchMatch {
    pub(super) refs: FxHashMap<String, SearchMatchPosition>,
    pub(super) subject: Option<SearchMatchPosition>,
    pub(super) author_name: Option<SearchMatchPosition>,
    pub(super) commit_hash: Option<SearchMatchPosition>,
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

    /// 這個欄位對應哪個 target。窮盡 match：加新的 `SearchField` variant 時這裡
    /// 會擋下編譯，不會像 `(SearchTarget, SearchField)` 配對 match 那樣，多了一個
    /// `_ => false` wildcard 靜默漏放行（新欄位永遠只在 target=All 時能被搜到，
    /// 而且不會有任何提示）。
    fn target(&self) -> SearchTarget {
        match self {
            Self::Subject(_) => SearchTarget::Subject,
            Self::AuthorName(_) => SearchTarget::Author,
            Self::CommitHash(_) => SearchTarget::Hash,
            Self::Ref(_) => SearchTarget::Ref,
        }
    }

    fn in_target(&self, target: SearchTarget) -> bool {
        target == SearchTarget::All || target == self.target()
    }
}

/// 搜尋與過濾看的所有欄位，全專案唯一一份清單 —— 加欄位只改這裡，`SearchMatch::new`
/// 與 `commit_quick_matches` 會自動跟上。兩邊各抄一份的話（包括 Stash 這條排除規則），
/// 分歧會讓某列算進 `hits` 卻標不出 highlight。
///
/// subject 排第一：`target == All` 時 `commit_quick_matches` 的 `any()` 靠這個順序
/// 短路；`target` 限定到單一欄位時，陣列裡另外兩個元素仍會先建好才被尾端的
/// `.filter()` 丟掉，成本可忽略（三個 `&str` 引用而已）。
fn search_fields<'a>(
    ci: &'a CommitInfo<'_>,
    target: SearchTarget,
) -> impl Iterator<Item = SearchField<'a>> {
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
    .filter(move |f| f.in_target(target))
}

impl SearchMatch {
    /// 收 `&SearchMatcher` 而不是 `(query, ignore_case, fuzzy)`：呼叫端在迴圈外就建好
    /// 一個，自己再建一次等於每個 commit 都重折一次 query、多配置一個 `String`。
    /// `pub(super)`：render 每幀替畫面上的列建，不再是 `CommitListState` 自己存一份。
    pub(super) fn new(ci: &CommitInfo<'_>, matcher: &SearchMatcher, target: SearchTarget) -> Self {
        let mut m = Self::default();
        for f in search_fields(ci, target) {
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

    pub(super) fn matched(&self) -> bool {
        !self.refs.is_empty()
            || self.subject.is_some()
            || self.author_name.is_some()
            || self.commit_hash.is_some()
    }
}

#[derive(Debug, Default, Clone)]
pub(super) struct SearchMatchPosition {
    pub(super) matched_indices: Vec<usize>,
}

impl SearchMatchPosition {
    pub(super) fn new(matched_indices: Vec<usize>) -> Self {
        Self { matched_indices }
    }
}

/// filter／search 的命中判斷只要 bool，`any()` 命中即停。
///
/// **別把它換成 `SearchMatch::new(..).matched()`** —— 後者不會短路，會對每個欄位
/// 算完整的 highlight 位置再全部丟掉。查詢 `a` 打在大 repo 上時幾乎每列都命中
/// subject，那是每次按鍵好幾倍的差距。search 與 filter 共用同一份，見
/// `MatchSet::update`。
fn commit_quick_matches(
    matcher: &SearchMatcher,
    commit_info: &CommitInfo<'_>,
    target: SearchTarget,
) -> bool {
    search_fields(commit_info, target).any(|f| matcher.matches(f.text()))
}

/// search 與 filter 共用：比對用的 `(query, options)` 連同算出來的命中清單
/// （raw index，恆遞增排序）。search 與 filter 各持一份。
///
/// **不變式**：`hits` 永遠等於 `key` 對目前 commits 的完整比對結果。重置一律用
/// `MatchSet::default()`，不提供只清 `hits` 的方法——否則下次打出延伸舊 query
/// 的字串時，增量路徑會從空集合出發，永遠零命中。
#[derive(Debug, Default, Clone)]
pub(super) struct MatchSet {
    pub(super) key: MatchQuery,
    pub(super) hits: Vec<RawCommitIdx>,
}

impl MatchSet {
    pub(super) fn total(&self) -> usize {
        self.hits.len()
    }

    /// `raw` 在命中清單裡的排名，從 1 起算；沒命中回 `None`。
    pub(super) fn rank_of(&self, raw: RawCommitIdx) -> Option<usize> {
        self.hits.binary_search(&raw).ok().map(|pos| pos + 1)
    }

    pub(super) fn contains(&self, raw: RawCommitIdx) -> bool {
        self.hits.binary_search(&raw).is_ok()
    }

    /// 沿 `step` 方向、`is_visible` 為 true 的下一個命中；從緊接在 `current`
    /// 之後（或之前）的位置開始找，不會選回 `current` 本身——即使它自己也是
    /// 命中，跟舊版逐列掃描（`while i != current.0`）同一個規則。全部命中都
    /// 不可見、或根本沒有命中時回 `None`。
    ///
    /// 只在 `hits` 內跳，不逐列掃 `commits`：最差 `hits.len()` 次
    /// `is_visible` 呼叫，取代舊版最差 `commits.len()` 次。
    pub(super) fn visible_hit_after(
        &self,
        current: RawCommitIdx,
        step: MatchStep,
        is_visible: impl Fn(RawCommitIdx) -> bool,
    ) -> Option<RawCommitIdx> {
        let len = self.hits.len();
        if len == 0 {
            return None;
        }
        // `current` 本身若也是命中，會佔掉 sorted `hits` 裡的一個位置——完整繞一圈
        // 一定會走回那個位置。步數上限扣掉它，才不會在「只剩 current 自己可見」時
        // 把它當成「下一個」選回去。
        let current_is_hit = self.hits.binary_search(&current).is_ok();
        let max_steps = if current_is_hit { len - 1 } else { len };
        if max_steps == 0 {
            return None;
        }
        let advance = |p: usize| match step {
            MatchStep::Next => (p + 1) % len,
            MatchStep::Prev => (p + len - 1) % len,
        };
        // 起點：sorted `hits` 裡緊接在 current 之後／之前的位置；`current`
        // 本身是不是命中不影響起點的選法（`partition_point` 用嚴格不等式）。
        let mut pos = match step {
            MatchStep::Next => self.hits.partition_point(|&r| r <= current) % len,
            MatchStep::Prev => {
                let p = self.hits.partition_point(|&r| r < current);
                (p + len - 1) % len
            }
        };
        for _ in 0..max_steps {
            let raw = self.hits[pos];
            if is_visible(raw) {
                return Some(raw);
            }
            pos = advance(pos);
        }
        None
    }

    /// 重算命中清單。`new_key.query` 是 `self.key.query` 的延伸、且設定沒變時，
    /// 只在舊命中裡篩選；否則對全部 commits 掃一遍。兩條路徑共用同一次
    /// `commit_quick_matches` 呼叫，比對結果不會分歧，`match` 發號（`rank_of`）
    /// 也就不必擔心兩條路徑各算一套。
    ///
    /// `self.key.query` 為空時不能走增量：`SearchMatcher::matches` 對空 query
    /// 固定回 false（也就是這裡的 `hits == []`），跟「一般子字串／子序列比對時
    /// 空字串比對一切」的數學語意不同，拿空 query 的零命中當「舊命中子集」的
    /// 起點會漏掉所有真正的命中。`self.key.query` 非空、`hits` 剛好是零命中
    /// （例如查 "zzz"）時則相反：延伸後保證仍是零命中，這條件本身就允許沿用
    /// （比全量重掃更快），不需要另外用 `!hits.is_empty()` 擋。
    pub(super) fn update(&mut self, commits: &[CommitInfo<'_>], new_key: MatchQuery) {
        if new_key.query.is_empty() {
            *self = MatchSet {
                key: new_key,
                hits: Vec::new(),
            };
            return;
        }
        let matcher = SearchMatcher::new(
            &new_key.query,
            new_key.options.ignore_case,
            new_key.options.fuzzy,
        );
        let can_use_incremental = new_key.options == self.key.options
            && !self.key.query.is_empty()
            && new_key.query.starts_with(&self.key.query);
        let candidates: Vec<RawCommitIdx> = if can_use_incremental {
            std::mem::take(&mut self.hits)
        } else {
            (0..commits.len()).map(RawCommitIdx).collect()
        };
        self.hits = candidates
            .into_iter()
            .filter(|raw| commit_quick_matches(&matcher, &commits[raw.0], new_key.options.target))
            .collect();
        self.key = new_key;
    }
}

impl<'a> CommitListState<'a> {
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

    pub fn search_state(&self) -> SearchState {
        self.search_state
    }

    pub fn search_options(&self) -> MatchOptions {
        self.search_options
    }

    /// 目前生效搜尋的 matcher／target，只在 `Searching`／`Applied` 且 query 非空
    /// 時有值。`render` 每幀呼叫一次，只替畫面上的列算 highlight（見
    /// `widget::commit_list::render::build_visible_rows`）。從 `self.search.key`
    /// 建，不從 `search_input`／`search_options` 建：`set_search_options` 在
    /// refresh 還原的中途只寫欄位、不重算，兩者在那個窗口內可能暫時不一致。
    pub(super) fn search_matcher(&self) -> Option<(SearchMatcher, SearchTarget)> {
        if matches!(self.search_state, SearchState::Inactive) || self.search.key.query.is_empty() {
            return None;
        }
        let options = self.search.key.options;
        Some((
            SearchMatcher::new(&self.search.key.query, options.ignore_case, options.fuzzy),
            options.target,
        ))
    }

    pub fn start_search(&mut self) {
        if let SearchState::Inactive | SearchState::Applied { .. } = self.search_state {
            self.search_state = SearchState::Searching {
                start_index: self.current_selected_raw(),
                match_index: 0,
                transient_message: TransientMessage::None,
            };
            self.search_input.reset();
            self.search = MatchSet::default();
        }
    }

    pub fn handle_search_input(&mut self, key: KeyEvent) {
        let SearchState::Searching { .. } = self.search_state else {
            return;
        };
        self.search_state
            .set_transient_message(TransientMessage::None);
        self.search_input.handle_event(&Event::Key(key));
        self.update_search_after_change();
    }

    pub fn apply_search(&mut self) {
        if let SearchState::Searching { match_index, .. } = self.search_state {
            if self.search_input.value().is_empty() {
                self.search_state = SearchState::Inactive;
            } else {
                self.search_state = SearchState::Applied {
                    match_index,
                    total_match: self.total_match(),
                };
            }
        }
    }

    fn total_match(&self) -> usize {
        self.search.total()
    }

    /// 目前游標所在列的排名（1 起算）；游標不在 match 上、或根本沒有可讀的選取列時
    /// 給 `0`，顯示成 `Match 0 of N`，按 GoToNext/GoToPrevious（預設 `]`/`[`）會
    /// 自然校正。
    ///
    /// 「沒有可讀的選取列」指 `total == 0`（例如 filter 零命中）或選在虛擬列：這兩
    /// 種情況 `current_selected_raw()` 回傳的是 fallback 值（第一個可見 commit），
    /// 不是使用者實際選取的 commit。
    fn current_match_index(&self) -> usize {
        if self.total == 0 || self.is_virtual_row_selected() {
            return 0;
        }
        self.search
            .rank_of(self.current_selected_raw())
            .unwrap_or(0)
    }

    /// 重算比對結果並重建 `Applied`。刻意不移動游標：
    /// `select_current_or_next_match_index` 會把目標放到距上緣 scrolloff
    /// 列的位置，這裡不要這個副作用。`restore_search` 與
    /// `update_search_after_change` 的 `Applied` 分支共用。
    fn reapply_search(&mut self) {
        self.update_search_matches();
        self.search_state = SearchState::Applied {
            match_index: self.current_match_index(),
            total_match: self.total_match(),
        };
    }

    /// refresh 之後還原一次已套用的 search。要在 selection 還原**之後**呼叫——它靠
    /// `current_selected_raw()` 判斷還原後游標停的 commit 是否仍是一個 match，見
    /// `CommitListState::reset_commit_list_with` 內的順序註記。不移動游標的理由見
    /// `reapply_search()`；這裡額外的後果是會把 selection 還原剛校正好的「使用者
    /// 原本在畫面第幾列」沖掉。
    ///
    /// `total_match` 算的是全體 commits，不理 filter——filter 與 search 同時還原時，
    /// 「Match a of b」裡可能有一部分是被 filter 藏起來、按 GoToNext/GoToPrevious
    /// 走不到的列（`select_match_in_direction` 用 `is_raw_visible` 跳過它們）。
    pub fn restore_search(&mut self, context: &MatchQuery) {
        self.search_input = Input::new(context.query.clone());
        self.search_options = context.options;
        self.reapply_search();
    }

    /// 只在 `Applied`（真的套用過、非輸入中）時回傳——refresh 中途取消一次沒套用
    /// 完的搜尋，沒有理由把它還原回來。
    pub fn search_refresh_context(&self) -> Option<MatchQuery> {
        if let SearchState::Applied { .. } = self.search_state {
            Some(MatchQuery {
                query: self.search_input.value().into(),
                options: self.search_options,
            })
        } else {
            None
        }
    }

    pub fn cancel_search(&mut self) {
        if let SearchState::Searching { .. } | SearchState::Applied { .. } = self.search_state {
            self.search_state = SearchState::Inactive;
            self.search_input.reset();
            self.search = MatchSet::default();
        }
    }

    /// search 的輸入按鍵與 `toggle_ignore_case`/`toggle_fuzzy`/`toggle_target` 共用，
    /// 形狀照抄 upstream `c4e771b` 的 `update_search_after_options_change()`，但
    /// `Applied` 分支**刻意不移動游標**（細節見 `reapply_search()` 的文件註解）——這點
    /// 跟 upstream 不同，是必要的偏離，不是漏改：upstream 在瀏覽模式 toggle 之後仍然
    /// 呼叫 `select_current_or_next_match_index`，會讓「切換一個選項」變成「清單自己
    /// 往下捲一大段」，本專案的 `restore_search()` 已經為了同一個理由拒絕過這個行為。
    fn update_search_after_change(&mut self) {
        match self.search_state {
            SearchState::Inactive => {}
            SearchState::Searching { start_index, .. } => {
                self.update_search_matches();
                self.select_current_or_next_match_index(start_index);
            }
            SearchState::Applied { .. } => self.reapply_search(),
        }
    }

    pub fn toggle_ignore_case(&mut self) {
        self.search_options.ignore_case = !self.search_options.ignore_case;
        self.search_state
            .set_transient_message(if self.search_options.ignore_case {
                TransientMessage::IgnoreCaseOn
            } else {
                TransientMessage::IgnoreCaseOff
            });
        self.update_search_after_change();
    }

    pub fn toggle_fuzzy(&mut self) {
        self.search_options.fuzzy = !self.search_options.fuzzy;
        self.search_state
            .set_transient_message(if self.search_options.fuzzy {
                TransientMessage::FuzzyOn
            } else {
                TransientMessage::FuzzyOff
            });
        self.update_search_after_change();
    }

    pub fn toggle_target(&mut self) {
        self.search_options.target = self.search_options.target.next();
        self.search_state
            .set_transient_message(TransientMessage::Target(self.search_options.target));
        self.update_search_after_change();
    }

    /// refresh 時無條件呼叫；有 active search 時 `restore_search` 會再用
    /// `context.options` 寫一次，兩者在 `From<&CommitListState>` 裡讀的是同一個
    /// `search_options`，值必然相同，先後順序不影響結果。
    ///
    /// 只寫欄位，不重算比對、不動 selection。不要從互動式按鍵處理函式呼叫，那些
    /// 場合請用 `toggle_ignore_case`/`toggle_fuzzy`/`toggle_target`。
    pub fn set_search_options(&mut self, options: MatchOptions) {
        self.search_options = options;
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
            let options = self.search_options.status_string();
            if total_match == 0 {
                let msg = format!("No matches found (query: \"{query}\") {options}");
                Some((msg, false))
            } else {
                let msg =
                    format!("Match {match_index} of {total_match} (query: \"{query}\") {options}");
                Some((msg, true))
            }
        } else {
            None
        }
    }

    pub fn search_query_cursor_position(&self) -> u16 {
        self.search_input.visual_cursor() as u16 + 1 // 加 1 是為了 "/"
    }

    pub fn transient_message_string(&self) -> Option<String> {
        if let SearchState::Searching {
            transient_message, ..
        } = self.search_state
        {
            transient_message.text()
        } else {
            None
        }
    }

    fn update_search_matches(&mut self) {
        let key = MatchQuery {
            query: self.search_input.value().to_string(),
            options: self.search_options,
        };
        self.search.update(&self.commits, key);
    }

    fn select_current_or_next_match_index(&mut self, current: RawCommitIdx) {
        if self.search.contains(current) && self.is_raw_visible(current) {
            self.select_raw(current);
            let mi = self.search.rank_of(current).unwrap_or(0);
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
        let Some(raw) = self
            .search
            .visible_hit_after(current, step, |raw| self.is_raw_visible(raw))
        else {
            return;
        };
        self.select_raw(raw);
        let mi = self.search.rank_of(raw).unwrap_or(0);
        self.search_state.update_match_index(mi);
    }

    fn is_raw_visible(&self, raw: RawCommitIdx) -> bool {
        self.raw_to_filtered(raw).is_some()
    }

    fn select_raw(&mut self, raw: RawCommitIdx) {
        if let Some(target) = self.raw_to_visible(raw) {
            self.set_visible_selection(target);
        }
    }

    // Filter 模式相關方法

    pub fn filter_state(&self) -> FilterState {
        self.filter_state
    }

    pub fn start_filter(&mut self) {
        if let FilterState::Inactive = self.filter_state {
            self.filter_options = MatchOptions::FILTER_DEFAULT;
            self.filter_state = FilterState::Filtering {
                transient_message: TransientMessage::None,
            };
            self.filter_input.reset();
            self.filtered_indices = None;
            self.update_filter_matches();
        }
    }

    pub fn handle_filter_input(&mut self, key: KeyEvent) {
        let FilterState::Filtering { .. } = self.filter_state else {
            return;
        };
        self.filter_state = FilterState::Filtering {
            transient_message: TransientMessage::None,
        };
        self.filter_input.handle_event(&Event::Key(key));
        self.update_filter_matches();
    }

    /// 沒有生效中的 filter 時是 no-op，游標不動——`filter_input` 為空且
    /// `Inactive` 就代表這裡本來就沒有 filter，`rebuild_filtered_indices` +
    /// `set_visible_selection(0)` 沒有東西可清，硬跑只會把游標無端拉到最上面。
    pub fn cancel_filter(&mut self) {
        if self.filter_state == FilterState::Inactive && self.filter_input.value().is_empty() {
            return;
        }
        self.filter_state = FilterState::Inactive;
        self.filter_input.reset();
        self.filter = MatchSet::default();
        self.rebuild_filtered_indices();
        self.set_visible_selection(VisibleIdx(0));
    }

    pub fn apply_filter(&mut self) {
        if let FilterState::Filtering { .. } = self.filter_state {
            self.filter_state = FilterState::Inactive;
            // 讓 filtered_indices 繼續生效
        }
    }

    /// refresh 之後還原一次已套用的 filter。要排在 `restore_search` 與 selection
    /// 還原之前呼叫——它會重建 `filtered_indices`（改變 `total`）並把游標壓到頂端，
    /// 見 `CommitListState::reset_commit_list_with` 內的順序註記。
    pub fn restore_filter(&mut self, context: &MatchQuery) {
        self.filter_input = Input::new(context.query.clone());
        self.filter_options = context.options;
        self.update_filter_matches();
    }

    /// filter 是否生效跟是否在輸入模式無關——`filter_input` 非空就代表清單正被
    /// 過濾，這正是 `rebuild_filtered_indices` 判斷 `has_text_filter` 的同一條件。
    pub fn filter_refresh_context(&self) -> Option<MatchQuery> {
        if self.filter_input.value().is_empty() {
            None
        } else {
            Some(MatchQuery {
                query: self.filter_input.value().into(),
                options: self.filter_options,
            })
        }
    }

    pub fn toggle_filter_ignore_case(&mut self) {
        let FilterState::Filtering { .. } = self.filter_state else {
            return;
        };
        self.filter_options.ignore_case = !self.filter_options.ignore_case;
        self.filter_state = FilterState::Filtering {
            transient_message: if self.filter_options.ignore_case {
                TransientMessage::IgnoreCaseOn
            } else {
                TransientMessage::IgnoreCaseOff
            },
        };
        self.update_filter_matches();
    }

    pub fn toggle_filter_fuzzy(&mut self) {
        let FilterState::Filtering { .. } = self.filter_state else {
            return;
        };
        self.filter_options.fuzzy = !self.filter_options.fuzzy;
        self.filter_state = FilterState::Filtering {
            transient_message: if self.filter_options.fuzzy {
                TransientMessage::FuzzyOn
            } else {
                TransientMessage::FuzzyOff
            },
        };
        self.update_filter_matches();
    }

    pub fn toggle_filter_target(&mut self) {
        let FilterState::Filtering { .. } = self.filter_state else {
            return;
        };
        self.filter_options.target = self.filter_options.target.next();
        self.filter_state = FilterState::Filtering {
            transient_message: TransientMessage::Target(self.filter_options.target),
        };
        self.update_filter_matches();
    }

    pub fn filter_query_string(&self) -> Option<String> {
        if let FilterState::Filtering { .. } = self.filter_state {
            Some(format!("filter: {}", self.filter_input.value()))
        } else {
            None
        }
    }

    pub fn filter_query_cursor_position(&self) -> u16 {
        // "filter: " 前綴佔 8 個字元
        8 + self.filter_input.visual_cursor() as u16
    }

    pub fn filter_transient_message_string(&self) -> Option<String> {
        if let FilterState::Filtering {
            transient_message, ..
        } = self.filter_state
        {
            transient_message.text()
        } else {
            None
        }
    }

    fn update_filter_matches(&mut self) {
        let key = MatchQuery {
            query: self.filter_input.value().to_string(),
            options: self.filter_options,
        };
        self.filter.update(&self.commits, key);
        self.rebuild_filtered_indices();
        self.set_visible_selection(VisibleIdx(0));
    }
}

#[cfg(test)]
mod tests {

    use crate::git::Commit;

    use super::*;

    fn commit_fixture() -> Commit {
        Commit {
            subject: "修正顯示問題".into(),
            author_name: "Alice".into(),
            commit_hash: "abc1234def".into(),
            ..Default::default()
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
        let info = CommitInfo::new(&c, vec![&branch, &stash]);

        let hit = |q: &str| {
            SearchMatch::new(
                &info,
                &SearchMatcher::new(q, false, false),
                SearchTarget::All,
            )
        };

        assert!(hit("修正").subject.is_some(), "subject");
        assert!(hit("Alice").author_name.is_some(), "author_name");
        assert!(hit("abc1234").commit_hash.is_some(), "commit_hash");
        assert!(hit("feature").refs.contains_key("feature/x"), "branch ref");

        // stash 的名稱與訊息都不該被搜到
        let m = hit("stash@");
        assert!(m.refs.is_empty() && !m.matched(), "stash 不該進搜尋範圍");
    }

    fn hash(n: usize) -> crate::git::CommitHash {
        format!("{n:040x}").as_str().into()
    }

    /// 忽略大小寫關閉、fuzzy 關閉、target 不限（`MatchOptions` 的零值）——本檔
    /// 絕大多數測試要的都是這組。
    fn exact(query: &str) -> MatchQuery {
        MatchQuery {
            query: query.into(),
            options: MatchOptions::default(),
        }
    }

    fn commits_with_subjects(subjects: &[&str]) -> Vec<Commit> {
        subjects
            .iter()
            .enumerate()
            .map(|(i, subject)| Commit {
                commit_hash: hash(i + 1),
                subject: (*subject).into(),
                ..Default::default()
            })
            .collect()
    }

    /// 用真實輸入路徑（`filter_input` → `update_filter_matches` →
    /// `rebuild_filtered_indices`）建 fixture，而非直接塞 `filtered_indices`——
    /// 否則 `restore_filter` 這種測的就是自己塞進去的值。
    fn with_commits<R>(commits: Vec<Commit>, f: impl FnOnce(&mut CommitListState<'_>) -> R) -> R {
        use std::rc::Rc;

        use rustc_hash::FxHashMap;

        use crate::git::Head;
        use crate::graph::Graph;

        let infos = commits
            .iter()
            .map(|c| CommitInfo::new(c, Vec::new()))
            .collect();
        let graph = Graph::from_materialized(commits.len(), Vec::new(), Vec::new());
        let mut state = CommitListState::new(
            infos,
            Rc::new(graph),
            Vec::new(),
            None,
            Head::None,
            FxHashMap::default(),
            MatchOptions::default(),
            None,
            crate::RemoteOnly::default(),
            None,
            0,
        );
        state.reset_height(10);
        f(&mut state)
    }

    fn with_state<R>(subjects: &[&str], f: impl FnOnce(&mut CommitListState<'_>) -> R) -> R {
        with_commits(commits_with_subjects(subjects), f)
    }

    #[test]
    fn restore_search_uses_restored_options_not_defaults() {
        with_state(&["FIX one", "fix two", "other"], |state| {
            // with_state 建立的初始 ignore_case 預設為 false（MatchOptions::default()），
            // 但還原的 context 要求 ignore_case = true —— 若 restore_search 誤用 default，
            // "FIX" 只會命中第一筆，不會命中第二筆。
            let context = MatchQuery {
                query: "fix".into(),
                options: MatchOptions {
                    ignore_case: true,
                    ..Default::default()
                },
            };
            state.restore_search(&context);

            let SearchState::Applied { total_match, .. } = state.search_state() else {
                panic!("expected Applied after restore_search");
            };
            assert_eq!(total_match, 2, "ignore_case 未依還原的設定重算");
        });
    }

    #[test]
    fn restore_search_keeps_selected_position_and_updates_match_index() {
        with_state(&["a match", "b match", "no hit"], |state| {
            state.select_commit_hash(&hash(2));
            let before = state.current_list_status();

            state.restore_search(&exact("match"));

            assert_eq!(
                state.current_list_status(),
                before,
                "restore_search 不該移動游標／捲動位置"
            );
            let SearchState::Applied { match_index, .. } = state.search_state() else {
                panic!("expected Applied after restore_search");
            };
            assert_eq!(match_index, 2, "目前選取的 commit 是第 2 個 match");
        });
    }

    #[test]
    fn restore_search_without_match_keeps_index_zero() {
        with_state(&["nothing here", "still nothing"], |state| {
            let before = state.current_list_status();

            state.restore_search(&exact("zzz"));

            assert_eq!(state.current_list_status(), before);
            let SearchState::Applied {
                match_index,
                total_match,
            } = state.search_state()
            else {
                panic!("expected Applied after restore_search");
            };
            assert_eq!((match_index, total_match), (0, 0));
        });
    }

    #[test]
    fn restore_filter_rebuilds_via_real_path() {
        with_state(&["keep me", "drop this", "keep too"], |state| {
            state.restore_filter(&exact("keep"));

            assert_eq!(state.total, 2, "只有兩筆含 keep 的 commit 該留下");
        });
    }

    #[test]
    fn restore_filter_hiding_selected_commit_leaves_cursor_at_top_no_panic() {
        with_state(&["alpha", "beta", "gamma"], |state| {
            state.select_commit_hash(&hash(2));

            state.restore_filter(&exact("alpha"));
            // 原本選的 "beta" 被 filter 藏起來，游標退回頂端（唯一剩下的那列）。
            state.select_commit_hash(&hash(2));

            assert_eq!(state.total, 1);
            let (selected, offset, _) = state.current_list_status();
            assert_eq!((selected, offset), (0, 0));
        });
    }

    #[test]
    fn restore_filter_zero_hits_guards_restore_search_from_bogus_index() {
        with_state(&["alpha", "beta"], |state| {
            state.restore_filter(&exact("no-such-term"));
            assert_eq!(state.total, 0);

            // total == 0 時 restore_search 不該去讀 current_selected_raw()。
            state.restore_search(&exact("alpha"));
            let SearchState::Applied { match_index, .. } = state.search_state() else {
                panic!("expected Applied after restore_search");
            };
            assert_eq!(match_index, 0);
        });
    }

    #[test]
    fn apply_filter_transitions_to_inactive_but_keeps_effect() {
        with_state(&["keep me", "drop this"], |state| {
            state.start_filter();
            state.filter_input = Input::new("keep".into());
            state.filter_options = MatchOptions::default();
            state.update_filter_matches();

            state.apply_filter();

            assert!(matches!(state.filter_state(), FilterState::Inactive));
            assert_eq!(state.total, 1, "apply 之後 filtered_indices 應該繼續生效");
        });
    }

    #[test]
    fn cancel_filter_without_active_filter_does_not_move_cursor() {
        with_state(&["a", "b", "c"], |state| {
            state.select_commit_hash(&hash(2));
            let before = state.current_list_status();

            state.cancel_filter();

            assert_eq!(
                before,
                state.current_list_status(),
                "沒有生效中的 filter 時，Esc 不該把游標拉回頂端"
            );
        });
    }

    #[test]
    fn toggle_ignore_case_works_from_browsing_mode_when_inactive() {
        with_state(&["FIX one", "fix two", "other"], |state| {
            assert_eq!(state.search_state(), SearchState::Inactive);

            state.toggle_ignore_case();

            assert!(state.search_options().ignore_case);
            assert_eq!(
                state.search_state(),
                SearchState::Inactive,
                "瀏覽模式下切換選項不該把狀態拉回 Searching"
            );
        });
    }

    #[test]
    fn toggle_after_applying_search_recalculates_total_match() {
        with_state(&["FIX one", "fix two", "other"], |state| {
            state.restore_search(&exact("fix"));
            let SearchState::Applied { total_match, .. } = state.search_state() else {
                panic!("expected Applied");
            };
            assert_eq!(total_match, 1, "ignore_case=false 時只有 'fix two' 命中");

            state.toggle_ignore_case();

            let SearchState::Applied { total_match, .. } = state.search_state() else {
                panic!("expected still Applied after toggling in browsing mode");
            };
            assert_eq!(total_match, 2, "toggle 後應該立刻重算，兩筆都命中");
        });
    }

    #[test]
    fn toggle_after_applying_search_does_not_move_cursor() {
        with_state(&["FIX one", "fix two", "other"], |state| {
            state.restore_search(&exact("fix"));
            let before = state.current_list_status();

            state.toggle_ignore_case();

            assert_eq!(
                state.current_list_status(),
                before,
                "瀏覽模式下 toggle 不該移動選取列／捲動位置"
            );
        });
    }

    #[test]
    fn toggle_after_zero_hit_filter_keeps_match_index_zero() {
        with_state(&["alpha", "beta"], |state| {
            state.restore_search(&exact("alpha"));
            state.restore_filter(&exact("no-such-term"));
            assert_eq!(state.total, 0);

            state.toggle_ignore_case();

            let SearchState::Applied { match_index, .. } = state.search_state() else {
                panic!("expected Applied after toggle");
            };
            assert_eq!(
                match_index, 0,
                "total == 0 時 current_match_index 的守衛要擋住"
            );
        });
    }

    /// `filtered_indices` 改成 `Option` 後，零命中是 `Some(空)`，不是「沒有
    /// filter」——`current_selected_raw` 得在 `Some(空)` 時明確 fallback 到 0，
    /// 不能真的去解一個不存在的 filtered idx。這裡直接走互動路徑（`/` 開搜尋、
    /// n／N 導航），不是只測 pure function。
    #[test]
    fn zero_hit_filter_then_start_search_and_navigate_does_not_panic() {
        with_state(&["alpha", "beta"], |state| {
            state.restore_filter(&exact("no-such-term"));
            assert_eq!(state.total, 0);

            state.start_search();
            let SearchState::Searching { start_index, .. } = state.search_state() else {
                panic!("expected Searching after start_search");
            };
            assert_eq!(start_index, RawCommitIdx(0));

            state.select_next_match();
            state.select_prev_match();
            assert_eq!(state.total, 0, "游標不動，filter 仍是零命中");
        });
    }

    /// `select_match_in_direction` 改成在 `MatchSet::hits` 裡二分搜尋＋循環走訪
    /// 後（取代逐列掃 `0..commits.len()`），這裡釘住最基本的行為：往下找到下一個
    /// match、往上找到上一個、繞過頭尾。
    #[test]
    fn select_next_match_and_prev_match_cycle_and_wrap() {
        with_state(&["alpha", "keep-one", "beta", "keep-two"], |state| {
            state.restore_search(&exact("keep"));
            state.select_first(); // 游標回到 raw 0（"alpha"，不是命中）

            state.select_next_match();
            assert_eq!(state.selected_commit_hash(), &hash(2), "往下第一個 match");

            state.select_next_match();
            assert_eq!(state.selected_commit_hash(), &hash(4), "往下第二個 match");

            state.select_next_match();
            assert_eq!(
                state.selected_commit_hash(),
                &hash(2),
                "繞回最前面那個 match"
            );

            state.select_prev_match();
            assert_eq!(
                state.selected_commit_hash(),
                &hash(4),
                "往上繞回最後一個 match"
            );
        });
    }

    /// search 與 filter 各自獨立：一個 raw 可能是搜尋命中，卻被 filter 藏起來。
    /// n／N 要跳過它，直接落在下一個「命中且可見」的列，不能卡在隱藏列上。
    #[test]
    fn select_next_match_skips_hits_hidden_by_filter() {
        with_state(&["aa keep", "bb keep", "aa keep2", "cc"], |state| {
            state.restore_search(&exact("keep"));
            // filter 只留含 "aa" 的兩筆：raw0（目前選取）跟 raw2；raw1 是搜尋命中，
            // 但被 filter 藏起來。
            state.restore_filter(&exact("aa"));
            assert_eq!(
                state.selected_commit_hash(),
                &hash(1),
                "restore_filter 後游標落在第一個可見列"
            );

            state.select_next_match();

            assert_eq!(
                state.selected_commit_hash(),
                &hash(3),
                "raw1（隱藏）被跳過，直接落在下一個可見命中 raw2"
            );
        });
    }

    /// 全部搜尋命中裡只有一個可見（其餘被 filter 藏起來），而游標剛好就停在它
    /// 上面：往下找不到「別的」可見命中，游標不該被拉回原地重選一次。
    #[test]
    fn select_next_match_does_not_reselect_current_as_only_visible_hit() {
        with_state(&["alpha", "keep-one", "beta"], |state| {
            state.restore_search(&exact("keep"));
            state.restore_filter(&exact("keep-one")); // 只留 raw1 自己
            assert_eq!(
                state.selected_commit_hash(),
                &hash(2),
                "唯一可見的一筆同時也是搜尋命中"
            );

            let before = state.current_list_status();
            state.select_next_match();

            assert_eq!(
                state.current_list_status(),
                before,
                "沒有別的可見命中，游標不該動"
            );
        });
    }

    #[test]
    fn start_search_reuses_browsing_mode_toggle_not_config_default() {
        with_state(&["FIX one", "fix two", "other"], |state| {
            // with_state 建立的初始 ignore_case 預設為 false（MatchOptions::default()）
            state.toggle_ignore_case(); // Inactive -> true，瀏覽模式下切換

            state.start_search();

            assert!(
                state.search_options().ignore_case,
                "start_search 不該把瀏覽模式切換過的選項重設回 config 預設值"
            );
        });
    }

    #[test]
    fn search_options_survive_full_cancel_and_restart_cycle() {
        with_state(&["FIX one", "fix two", "other"], |state| {
            state.start_search();
            state.toggle_fuzzy(); // 輸入模式中先開一次
            assert!(state.search_options().fuzzy);
            state.cancel_search(); // 回到 Inactive

            state.toggle_fuzzy(); // 瀏覽模式下關掉（issue #103 新能力）
            assert!(!state.search_options().fuzzy);
            state.toggle_fuzzy(); // 再開回來
            assert!(state.search_options().fuzzy);

            state.start_search(); // 重新按 `/`

            assert!(
                state.search_options().fuzzy,
                "瀏覽模式下切換過的選項要沿用到下一次搜尋"
            );
        });
    }

    #[test]
    fn status_string_covers_all_four_combinations() {
        let opts = |ignore_case, fuzzy| MatchOptions {
            ignore_case,
            fuzzy,
            ..Default::default()
        };
        assert_eq!(
            opts(false, false).status_string(),
            "[case-sensitive] [substring] [target: all]"
        );
        assert_eq!(
            opts(true, false).status_string(),
            "[ignore-case] [substring] [target: all]"
        );
        assert_eq!(
            opts(false, true).status_string(),
            "[case-sensitive] [fuzzy] [target: all]"
        );
        assert_eq!(
            opts(true, true).status_string(),
            "[ignore-case] [fuzzy] [target: all]"
        );
    }

    #[test]
    fn status_string_includes_target_segment() {
        let opts = MatchOptions {
            target: SearchTarget::Author,
            ..Default::default()
        };
        assert_eq!(
            opts.status_string(),
            "[case-sensitive] [substring] [target: author]"
        );
    }

    #[test]
    fn search_target_next_cycles_through_all_five_variants() {
        let mut t = SearchTarget::All;
        let mut seen = vec![t];
        for _ in 0..4 {
            t = t.next();
            seen.push(t);
        }
        assert_eq!(
            seen,
            vec![
                SearchTarget::All,
                SearchTarget::Subject,
                SearchTarget::Author,
                SearchTarget::Hash,
                SearchTarget::Ref,
            ]
        );
        assert_eq!(t.next(), SearchTarget::All, "繞一圈要回到 All");
    }

    #[test]
    fn matched_query_string_includes_options_after_query() {
        with_state(&["fix one", "fix two"], |state| {
            state.restore_search(&MatchQuery {
                query: "fix".into(),
                options: MatchOptions {
                    ignore_case: true,
                    ..Default::default()
                },
            });

            let (msg, matched) = state.matched_query_string().unwrap();
            assert!(matched);
            let query_pos = msg.find("(query: \"fix\")").expect("query segment present");
            let options_pos = msg.find("[ignore-case]").expect("options segment present");
            assert!(query_pos < options_pos, "選項摘要要排在 query 之後: {msg}");
        });
    }

    /// target 篩選：非目標欄位要維持 `None`/空 map，即使查詢字串本來能命中。
    #[test]
    fn search_match_respects_target_restriction() {
        let c = Commit {
            subject: "apple pie".into(),
            author_name: "apple".into(),
            commit_hash: "apple123".into(),
            ..Default::default()
        };
        let branch = Ref::Branch {
            name: "apple-branch".into(),
            target: "apple123".into(),
        };
        let info = CommitInfo::new(&c, vec![&branch]);
        let matcher = SearchMatcher::new("apple", false, false);

        let subject_only = SearchMatch::new(&info, &matcher, SearchTarget::Subject);
        assert!(subject_only.subject.is_some());
        assert!(subject_only.author_name.is_none());
        assert!(subject_only.commit_hash.is_none());
        assert!(subject_only.refs.is_empty());

        let author_only = SearchMatch::new(&info, &matcher, SearchTarget::Author);
        assert!(author_only.subject.is_none());
        assert!(author_only.author_name.is_some());

        let hash_only = SearchMatch::new(&info, &matcher, SearchTarget::Hash);
        assert!(hash_only.subject.is_none());
        assert!(hash_only.commit_hash.is_some());

        let ref_only = SearchMatch::new(&info, &matcher, SearchTarget::Ref);
        assert!(ref_only.subject.is_none());
        assert!(!ref_only.refs.is_empty());
    }

    /// target=Ref 且該 commit 完全沒有 ref：`search_fields` 產生空 iterator，
    /// 要確認回傳 `matched() == false` 而不是 panic 或誤判。
    #[test]
    fn search_match_target_ref_on_commit_without_refs_is_no_match() {
        let c = commit_fixture();
        let info = CommitInfo::new(&c, Vec::new());
        let matcher = SearchMatcher::new("anything", false, false);

        let m = SearchMatch::new(&info, &matcher, SearchTarget::Ref);
        assert!(!m.matched());
    }

    /// `Ref::Stash` 排除規則位於 target `.filter()` 的上游，只有在 target=Ref 時
    /// 才會顯形出問題，需要獨立測試覆蓋（既有測試只涵蓋 target=All）。
    #[test]
    fn search_match_target_ref_still_skips_stash() {
        let c = commit_fixture();
        let stash = Ref::Stash {
            name: "stash@{0}".into(),
            message: "wip".into(),
            target: "abc1234def".into(),
        };
        let info = CommitInfo::new(&c, vec![&stash]);
        let matcher = SearchMatcher::new("stash@", false, false);

        let m = SearchMatch::new(&info, &matcher, SearchTarget::Ref);
        assert!(!m.matched(), "target=Ref 時 stash 仍不該被搜到");
    }

    /// commit 1 只有 subject 命中「apple」、commit 2 只有 author 命中——用來分辨
    /// target 篩選到底有沒有真的生效，而不是巧合地兩邊都命中或都不命中。
    fn subject_author_crossed_commits() -> Vec<Commit> {
        vec![
            Commit {
                commit_hash: hash(1),
                subject: "apple pie".into(),
                author_name: "orange".into(),
                ..Default::default()
            },
            Commit {
                commit_hash: hash(2),
                subject: "banana bread".into(),
                author_name: "apple".into(),
                ..Default::default()
            },
        ]
    }

    /// 本次改動唯一的正確性風險點：`update_search_matches` 的增量搜尋快取判斷
    /// 必須把 target 算進「設定沒變」的比較，否則切換 target 後會誤用舊的候選
    /// 集合重新比對，漏掉真正該命中的列。
    #[test]
    fn toggling_target_does_not_reuse_stale_incremental_candidates() {
        with_commits(subject_author_crossed_commits(), |state| {
            state.restore_search(&MatchQuery {
                query: "apple".into(),
                options: MatchOptions {
                    target: SearchTarget::Subject,
                    ..Default::default()
                },
            });
            let SearchState::Applied { total_match, .. } = state.search_state() else {
                panic!("expected Applied");
            };
            assert_eq!(total_match, 1, "target=Subject 時只有第一筆命中");

            state.toggle_target(); // Subject -> Author

            let SearchState::Applied { total_match, .. } = state.search_state() else {
                panic!("expected still Applied after toggle_target");
            };
            assert_eq!(
                total_match, 1,
                "target=Author 時應該改命中第二筆；若誤用了 Subject 時期的增量候選集合會變成 0"
            );
        });
    }

    /// 只斷言 `total == 1` 不夠：Subject/Author 兩個 target 剛好都只命中一筆，
    /// 就算 `toggle_filter_target` 整個變成 no-op 這個數字也不會變。要驗證的是
    /// 「命中的是哪一筆」真的隨 target 換了。
    #[test]
    fn toggle_filter_target_changes_matched_commits() {
        with_commits(subject_author_crossed_commits(), |state| {
            state.start_filter();
            state.filter_input = Input::new("apple".into());
            state.filter_options.target = SearchTarget::Subject;
            state.update_filter_matches();
            assert_eq!(state.total, 1, "target=Subject 時只有第一筆命中");
            assert_eq!(state.selected_commit_hash(), &hash(1));

            state.toggle_filter_target(); // Subject -> Author
            assert_eq!(state.total, 1, "target=Author 時仍只有一筆命中");
            assert_eq!(
                state.selected_commit_hash(),
                &hash(2),
                "但應該改命中第二筆，不是繼續停在第一筆"
            );
        });
    }

    #[test]
    fn toggle_target_sets_transient_message_in_searching_mode() {
        with_state(&["a", "b"], |state| {
            state.start_search();
            state.toggle_target(); // All -> Subject

            assert_eq!(
                state.transient_message_string(),
                Some("Target: SUBJECT".to_string())
            );
        });
    }

    #[test]
    fn toggle_filter_target_sets_transient_message() {
        with_state(&["a", "b"], |state| {
            state.start_filter();
            state.toggle_filter_target(); // All -> Subject

            assert_eq!(
                state.filter_transient_message_string(),
                Some("Target: SUBJECT".to_string())
            );
        });
    }

    /// #122 手動量測：讀真實 repo，逐字打 "fix bug"，印出每一鍵耗時。
    /// 不在一般 `cargo test` 跑，用法：
    /// `SERIE_PERF_REPO=<repo path> cargo test --release perf_search -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn perf_search_keystrokes() {
        use std::path::Path;
        use std::rc::Rc;
        use std::time::Instant;

        use ratatui::crossterm::event::{KeyCode, KeyModifiers};

        use crate::git::{Repository, SortCommit};
        use crate::graph::Graph;

        let Ok(path) = std::env::var("SERIE_PERF_REPO") else {
            eprintln!("skip: 設定 SERIE_PERF_REPO=<repo path> 才會跑這個測試");
            return;
        };

        let load_start = Instant::now();
        let repository = Repository::load(Path::new(&path), SortCommit::Topological, None)
            .expect("load perf repo");
        eprintln!(
            "Repository::load: {:?} ({} commits)",
            load_start.elapsed(),
            repository.all_commits().len()
        );

        let build_start = Instant::now();
        let commits: Vec<CommitInfo> = repository
            .all_commits()
            .iter()
            .map(|c| CommitInfo::new(c, repository.refs(&c.commit_hash)))
            .collect();
        let commit_count = commits.len();
        let graph = Graph::from_materialized(commit_count, Vec::new(), Vec::new());
        let mut state = CommitListState::new(
            commits,
            Rc::new(graph),
            Vec::new(),
            None,
            crate::git::Head::None,
            FxHashMap::default(),
            MatchOptions::default(),
            None,
            crate::RemoteOnly::default(),
            None,
            0,
        );
        state.reset_height(50);
        eprintln!("CommitListState::new: {:?}", build_start.elapsed());

        state.start_search();
        for c in "fix bug".chars() {
            let key_start = Instant::now();
            state.handle_search_input(KeyEvent::new(KeyCode::Char(c), KeyModifiers::empty()));
            eprintln!("key {c:?}: {:?}", key_start.elapsed());
        }
    }
}

use ratatui::crossterm::event::{Event, KeyEvent};
use rustc_hash::FxHashMap;
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

use crate::fuzzy::SearchMatcher;
use crate::git::Ref;

use super::state::CommitListState;
use super::{CommitInfo, MatchStep, RawCommitIdx, VisibleIdx};

/// 一組比對設定。search 與 filter 各自在 `CommitListState` 持有一份，跨
/// mode 轉換（`Searching` → `Applied`、filter 套用後仍生效）存活——套用後
/// 這組設定還要繼續驅動增量比對與 refresh 還原，跟「現在是不是在輸入」是
/// 兩件事，不該塞進只在輸入模式才存在的 enum variant。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchOptions {
    pub ignore_case: bool,
    pub fuzzy: bool,
}

impl MatchOptions {
    /// Filter 模式預設用 fuzzy + 忽略大小寫，操作體驗比較好。
    pub const FILTER_DEFAULT: MatchOptions = MatchOptions {
        ignore_case: true,
        fuzzy: true,
    };
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterState {
    Inactive,
    Filtering { transient_message: TransientMessage },
}

/// refresh 之後還原一次 search 或 filter 所需的全部輸入。兩者存的是同一個
/// 概念（餵給 `SearchMatcher::new` 的 query + 設定），共用一個型別。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchQuery {
    pub query: String,
    pub options: MatchOptions,
}

#[derive(Debug, Default, Clone)]
pub(super) struct SearchMatch {
    pub(super) refs: FxHashMap<String, SearchMatchPosition>,
    pub(super) subject: Option<SearchMatchPosition>,
    pub(super) author_name: Option<SearchMatchPosition>,
    pub(super) commit_hash: Option<SearchMatchPosition>,
    match_index: usize, // 從 1 起算
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
pub(super) struct SearchMatchPosition {
    pub(super) matched_indices: Vec<usize>,
}

impl SearchMatchPosition {
    pub(super) fn new(matched_indices: Vec<usize>) -> Self {
        Self { matched_indices }
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

    pub fn start_search(&mut self) {
        if let SearchState::Inactive | SearchState::Applied { .. } = self.search_state {
            self.search_options = MatchOptions {
                ignore_case: self.default_ignore_case,
                fuzzy: self.default_fuzzy,
            };
            self.search_state = SearchState::Searching {
                start_index: self.current_selected_raw(),
                match_index: 0,
                transient_message: TransientMessage::None,
            };
            self.search_input.reset();
            self.clear_search_matches();
        }
    }

    pub fn handle_search_input(&mut self, key: KeyEvent) {
        let SearchState::Searching { start_index, .. } = self.search_state else {
            return;
        };
        self.search_state
            .set_transient_message(TransientMessage::None);
        self.search_input.handle_event(&Event::Key(key));
        self.update_search_matches();
        self.select_current_or_next_match_index(start_index);
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

    /// refresh 之後還原一次已套用的 search。要在 selection 還原**之後**呼叫——它靠
    /// `current_selected_raw()` 判斷還原後游標停的 commit 是否仍是一個 match，見
    /// `CommitListState::reset_commit_list_with` 內的順序註記。
    ///
    /// 刻意不移動游標：`select_current_or_next_match_index` 會把目標釘到 viewport
    /// 最上緣，把 selection 還原剛校正好的「使用者原本在畫面第幾列」整個沖掉。
    /// 找不到 match 時 `match_index` 停在 0（顯示成 `Match 0 of N`），按 `n`/`N`
    /// 會自然校正。
    ///
    /// `total_match` 算的是全體 commits，不理 filter——filter 與 search 同時還原時，
    /// 「Match a of b」裡可能有一部分是被 filter 藏起來、按 `n` 走不到的列
    /// （`select_match_in_direction` 用 `is_raw_visible` 跳過它們）。
    pub fn restore_search(&mut self, context: &MatchQuery) {
        self.search_input = Input::new(context.query.clone());
        self.search_options = context.options;
        self.update_search_matches();

        // total == 0（還原的 filter 零命中）或選到虛擬列時，current_selected_raw()
        // 會 fallback 成 RawCommitIdx(0)——不是使用者實際選取的 commit，不能拿來查
        // match_index。
        let match_index = if self.total > 0 && !self.is_virtual_row_selected() {
            let m = self.search_match(self.current_selected_raw());
            if m.matched() {
                m.match_index
            } else {
                0
            }
        } else {
            0
        };

        self.search_state = SearchState::Applied {
            match_index,
            total_match: self.search_matches.iter().filter(|m| m.matched()).count(),
        };
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
            self.clear_search_matches();
        }
    }

    pub fn toggle_ignore_case(&mut self) {
        let SearchState::Searching { start_index, .. } = self.search_state else {
            return;
        };
        self.search_options.ignore_case = !self.search_options.ignore_case;
        self.search_state
            .set_transient_message(if self.search_options.ignore_case {
                TransientMessage::IgnoreCaseOn
            } else {
                TransientMessage::IgnoreCaseOff
            });
        self.update_search_matches();
        self.select_current_or_next_match_index(start_index);
    }

    pub fn toggle_fuzzy(&mut self) {
        let SearchState::Searching { start_index, .. } = self.search_state else {
            return;
        };
        self.search_options.fuzzy = !self.search_options.fuzzy;
        self.search_state
            .set_transient_message(if self.search_options.fuzzy {
                TransientMessage::FuzzyOn
            } else {
                TransientMessage::FuzzyOff
            });
        self.update_search_matches();
        self.select_current_or_next_match_index(start_index);
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
        self.search_input.visual_cursor() as u16 + 1 // 加 1 是為了 "/"
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

    fn update_search_matches(&mut self) {
        let query = self.search_input.value().to_string();

        // query 為空時提早返回
        if query.is_empty() {
            self.clear_search_matches();
            self.last_search_query.clear();
            self.last_matched_indices.clear();
            return;
        }

        let MatchOptions { ignore_case, fuzzy } = self.search_options;
        let matcher = SearchMatcher::new(&query, ignore_case, fuzzy);

        // 判斷能不能用增量搜尋：
        // - 新 query 是舊 query 的延伸（使用者多打了幾個字）
        // - 搜尋設定沒變（ignore_case、fuzzy）
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
            self.filtered_indices.clear();
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
        self.text_filtered_indices.clear();
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

    fn update_filter_matches(&mut self) {
        let query = self.filter_input.value().to_string();

        self.text_filtered_indices.clear();

        if !query.is_empty() {
            let MatchOptions { ignore_case, fuzzy } = self.filter_options;
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

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

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

    fn hash(n: usize) -> crate::git::CommitHash {
        format!("{n:040x}").as_str().into()
    }

    /// 忽略大小寫關閉、fuzzy 關閉——本檔絕大多數測試要的都是這組。
    fn exact(query: &str) -> MatchQuery {
        MatchQuery {
            query: query.into(),
            options: MatchOptions {
                ignore_case: false,
                fuzzy: false,
            },
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
    fn with_state<R>(subjects: &[&str], f: impl FnOnce(&mut CommitListState<'_>) -> R) -> R {
        use std::rc::Rc;

        use rustc_hash::{FxHashMap, FxHashSet};

        use crate::git::Head;
        use crate::graph::Graph;

        let commits = commits_with_subjects(subjects);
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
        state.reset_height(10);
        f(&mut state)
    }

    #[test]
    fn restore_search_uses_restored_options_not_defaults() {
        with_state(&["FIX one", "fix two", "other"], |state| {
            // default_ignore_case = false（見 with_state），但還原的 context 要求
            // ignore_case = true —— 若 restore_search 誤用 default，"FIX" 只會命中
            // 第一筆，不會命中第二筆。
            let context = MatchQuery {
                query: "fix".into(),
                options: MatchOptions {
                    ignore_case: true,
                    fuzzy: false,
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
            state.filter_options = MatchOptions {
                ignore_case: false,
                fuzzy: false,
            };
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
}

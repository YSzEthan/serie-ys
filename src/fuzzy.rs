use std::borrow::Cow;
use std::sync::LazyLock;

use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};

static FUZZY_MATCHER: LazyLock<SkimMatcherV2> =
    LazyLock::new(|| SkimMatcherV2::default().respect_case());

/// 逐字元小寫化。`char` 數與原字串 1:1，所以 `fuzzy_indices` 回傳的 char 位置
/// 仍然對得回原字串 —— `str::to_lowercase()` 就不行，它是 full-Unicode 折疊，
/// `İ` 會變成兩個 char，位置整段錯開。
///
/// 也不能改用 `SkimMatcherV2::ignore_case()`：它只折 ASCII（`char_equal` 走
/// `eq_ignore_ascii_case`），而 query 在 `new` 裡是 full-Unicode 小寫化的，
/// 搜 `über` 會對不上 `ÜBER`。
///
/// 殘留差異：`İ` 這種小寫化後 char 數會變的字元，query 側（full）與這裡（逐字元）
/// 折出來的結果不同，fuzzy 模式下可能失配。極罕見，換來的是索引永遠對得回原字串。
fn fold_case(s: &str) -> String {
    s.chars()
        .map(|c| c.to_lowercase().next().unwrap_or(c))
        .collect()
}

pub(crate) struct SearchMatcher {
    query: String,
    ignore_case: bool,
    fuzzy: bool,
}

impl SearchMatcher {
    pub fn new(query: &str, ignore_case: bool, fuzzy: bool) -> Self {
        let query = if ignore_case {
            query.to_lowercase()
        } else {
            query.into()
        };
        Self {
            query,
            ignore_case,
            fuzzy,
        }
    }

    /// fuzzy 比對用的 haystack。大小寫敏感時原樣借用，不配置。
    fn haystack<'a>(&self, s: &'a str) -> Cow<'a, str> {
        if self.ignore_case {
            Cow::Owned(fold_case(s))
        } else {
            Cow::Borrowed(s)
        }
    }

    /// Quick check if string matches without computing match positions
    pub fn matches(&self, s: &str) -> bool {
        if self.query.is_empty() {
            return false;
        }
        if self.fuzzy {
            FUZZY_MATCHER
                .fuzzy_match(&self.haystack(s), &self.query)
                .is_some()
        } else if self.ignore_case {
            s.to_lowercase().contains(&self.query)
        } else {
            s.contains(&self.query)
        }
    }

    pub fn matched_position(&self, s: &str) -> Option<Vec<usize>> {
        if self.query.is_empty() {
            return None;
        }
        if self.fuzzy {
            // 位置是對折疊後的 haystack 取的，但那與 `s` 的 char 數 1:1，所以直接
            // 拿原字串換算 byte 位置。
            FUZZY_MATCHER
                .fuzzy_indices(&self.haystack(s), &self.query)
                .map(|(_, indices)| char_indices_to_byte_indices(s, indices))
        } else {
            let start = if self.ignore_case {
                s.to_lowercase().find(&self.query)
            } else {
                s.find(&self.query)
            }?;
            let end = start + self.query.len();
            // ignore_case 的位置是對 `to_lowercase()` 的結果取的，套回 `s` 未必落在
            // 字元邊界，下游 laurier 直接拿去切 byte 就會 panic。注意這個檢查只保證
            // 不 panic：小寫化改變 byte 長度時（`İ` 2 → 3），位置可能整段位移卻仍在
            // 合法邊界上，於是標到隔壁的字。
            if !s.is_char_boundary(start) || !s.is_char_boundary(end) {
                return None;
            }
            Some((start..end).collect())
        }
    }
}

/// `fuzzy_indices` 回傳的是 char 位置（skim 內部走 `chars().enumerate()`），但下游的
/// `laurier::highlight` 一律拿它去切 byte。純 ASCII 時兩者相等所以看不出來，中文或
/// emoji 一進來就會切在字元中間 panic。
///
/// 每個命中字元要展開成它佔用的**全部** byte：highlight 會把連續位置合併成 range，
/// 只給起始 byte 的話 3-byte 中文字會得到 `[start, start+1)`，照樣切壞。
fn char_indices_to_byte_indices(s: &str, char_indices: Vec<usize>) -> Vec<usize> {
    let bounds: Vec<(usize, usize)> = s
        .char_indices()
        .map(|(b, c)| (b, b + c.len_utf8()))
        .collect();
    char_indices
        .into_iter()
        .filter_map(|ci| bounds.get(ci).copied())
        .flat_map(|(start, end)| start..end)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回傳的每個位置都必須是合法的 char 邊界，否則 highlight 會 panic。
    fn assert_all_on_char_boundary(s: &str, positions: &[usize]) {
        for &p in positions {
            assert!(s.is_char_boundary(p), "位置 {p} 不在 {s:?} 的字元邊界上");
        }
    }

    #[test]
    fn fuzzy_positions_stay_on_char_boundaries_with_cjk() {
        let s = "修正顯示問題 fix";
        let positions = SearchMatcher::new("fix", false, true)
            .matched_position(s)
            .unwrap();
        assert_all_on_char_boundary(s, &positions);
        assert!(!positions.is_empty());
    }

    #[test]
    fn fuzzy_positions_cover_every_byte_of_a_multibyte_match() {
        let s = "🎉 add";
        // 命中 "🎉"（4 bytes）本身時，4 個 byte 都要在結果裡，否則 range 會切一半。
        let positions = SearchMatcher::new("🎉", false, true)
            .matched_position(s)
            .unwrap();
        assert_eq!(positions, vec![0, 1, 2, 3]);
    }

    #[test]
    fn ignore_case_fuzzy_indexes_against_the_original_string() {
        let s = "修正 MIXED Case";
        let positions = SearchMatcher::new("mixed", true, true)
            .matched_position(s)
            .unwrap();
        assert_all_on_char_boundary(s, &positions);
        // 索引落在原字串上，不是小寫化後的副本
        assert_eq!(&s[positions[0]..=positions[positions.len() - 1]], "MIXED");
    }

    #[test]
    fn ignore_case_folds_non_ascii() {
        // skim 的 ignore_case 只折 ASCII（char_equal 走 eq_ignore_ascii_case），
        // 而 query 是 full-Unicode 小寫化過的，兩邊得對得上才行。
        assert!(SearchMatcher::new("über", true, true).matches("ÜBER fix"));
        assert!(SearchMatcher::new("äpfel", true, true).matches("ÄPFEL"));
        assert!(SearchMatcher::new("über", true, true)
            .matched_position("ÜBER fix")
            .is_some());
    }

    #[test]
    fn substring_positions_that_would_split_a_char_are_dropped() {
        // "İ" 小寫化成 "i̇"（2 bytes → 3 bytes），"中" 在小寫字串裡的位置套回原字串
        // 會落在字元中間。標不出來可以接受，panic 不行。
        let s = "İ中文";
        let positions = SearchMatcher::new("中", true, false).matched_position(s);
        assert_eq!(positions, None);
    }

    #[test]
    fn substring_positions_are_byte_ranges() {
        let s = "🎉 add";
        let positions = SearchMatcher::new("add", false, false)
            .matched_position(s)
            .unwrap();
        assert_eq!(positions, vec![5, 6, 7]);
    }
}

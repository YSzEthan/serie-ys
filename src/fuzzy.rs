use std::borrow::Cow;
use std::sync::LazyLock;

use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};

static FUZZY_MATCHER: LazyLock<SkimMatcherV2> =
    LazyLock::new(|| SkimMatcherV2::default().respect_case());

/// 大小寫不敏感時，query 與 haystack 共用的折疊。逐字元小寫化，`char` 數與原字串
/// 1:1 —— 這是本檔案所有位置換算的前提，由 `fold_case_preserves_char_count` 守著。
///
/// 不能改用 `str::to_lowercase()`：它是 full-Unicode 折疊，`İ`（U+0130，整個
/// Unicode 唯一一個小寫化會展開成兩個 char 的字元）會讓 char 數與 byte 長度都變，
/// 位置就整段錯開。也不能交給 `SkimMatcherV2::ignore_case()`：它只折 ASCII
/// （`char_equal` 走 `eq_ignore_ascii_case`），搜 `über` 會對不上 `ÜBER`。
///
/// 與 full 折疊的兩處行為差異，都不值得為它們寫程式碼：
/// - `İstanbul` 搜 `istanbul` 由「不命中」變成命中 —— full 折疊得到的是
///   `i̇stanbul`（多一個 U+0307），本來就是假陰性。
/// - 希臘文 final sigma：`str::to_lowercase()` 實作了 Final_Sigma context rule，
///   `char::to_lowercase()` 沒有，所以 `ΟΔΟΣ` 折出 `οδοσ` 而非 `οδος`。搜尋時
///   query 帶 ς 的不再命中、帶 σ 的開始命中，兩個同樣罕見的情況對調。
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
        // 與 haystack 走同一個折疊，位置換算才對得起來。
        let query = if ignore_case {
            fold_case(query)
        } else {
            query.into()
        };
        Self {
            query,
            ignore_case,
            fuzzy,
        }
    }

    /// 比對用的 haystack。大小寫敏感時原樣借用，不配置。
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
        let haystack = self.haystack(s);
        if self.fuzzy {
            FUZZY_MATCHER.fuzzy_match(&haystack, &self.query).is_some()
        } else {
            haystack.contains(&self.query)
        }
    }

    pub fn matched_position(&self, s: &str) -> Option<Vec<usize>> {
        if self.query.is_empty() {
            return None;
        }
        // 兩個分支的位置都是對折疊後的 haystack 取的，而 haystack 與 `s` 的 char 數
        // 1:1，所以一律先換算成 char 位置、再攤回 `s` 的 byte 位置。
        let haystack = self.haystack(s);
        if self.fuzzy {
            FUZZY_MATCHER
                .fuzzy_indices(&haystack, &self.query)
                .map(|(_, indices)| char_indices_to_byte_indices(s, indices))
        } else {
            // substring 命中本來就是一段連續的 byte range，不必繞道
            // `char_indices_to_byte_indices` 去建整個字串的對照表 —— 這條路徑在
            // 每次按鍵、每個 commit 上都會跑（見 `commit_list::SearchMatch::new`）。
            let byte_pos = haystack.find(&self.query)?;
            let start_char = haystack[..byte_pos].chars().count();
            let query_chars = self.query.chars().count();
            // 兩行同形：都是「前 n 個 char 佔幾個 byte」。命中貼齊字串結尾時 `take`
            // 自然取不滿，不必為它多寫一條 fallback。
            let start: usize = s.chars().take(start_char).map(char::len_utf8).sum();
            let len: usize = s[start..]
                .chars()
                .take(query_chars)
                .map(char::len_utf8)
                .sum();
            Some((start..start + len).collect())
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

    /// full-Unicode 折疊會把 "İ" 變成 "i̇"（1 char → 2 char、2 bytes → 3 bytes），
    /// 命中位置就整段位移。兩組輸入釘的是同一個 bug 的兩種症狀：修好之前 `İ中文`
    /// 回 `None`（被邊界檢查丟掉），`İabcd` 靜默標出 `bcd`。後者更危險。
    #[test]
    fn substring_positions_survive_a_char_count_changing_prefix() {
        for (s, q) in [("İ中文", "中"), ("İabcd", "abc")] {
            let positions = SearchMatcher::new(q, true, false)
                .matched_position(s)
                .unwrap();
            assert_eq!(positions, vec![2, 3, 4], "{s:?}");
            assert_eq!(&s[2..5], q, "{s:?}");
        }
    }

    /// 兩者分歧就是「row 出現但 highlight 消失」—— 本輪修掉的正是這個裂縫，而它目前
    /// 只靠 skim 內部 `fuzzy_match` / `fuzzy_indices` 共用同一條實作撐著。
    #[test]
    fn matches_agrees_with_matched_position() {
        let queries = [
            ("abc", true, false),
            ("mixed", true, true),
            ("über", true, true),
            ("ADD", false, false),
            ("", true, true),
            ("zzz", true, true),
        ];
        for (q, ignore_case, fuzzy) in queries {
            let m = SearchMatcher::new(q, ignore_case, fuzzy);
            for s in [
                "İabcd",
                "İ中文",
                "修正 MIXED Case",
                "ÜBER fix",
                "🎉 add",
                "",
            ] {
                assert_eq!(
                    m.matches(s),
                    m.matched_position(s).is_some(),
                    "query={q:?} haystack={s:?}"
                );
            }
        }
    }

    /// 本檔案所有位置換算都靠這條不變式，`is_char_boundary` 那類事後防禦擋不住它被破壞。
    #[test]
    fn fold_case_preserves_char_count() {
        for s in ["İ", "İstanbul", "ΟΔΟΣ", "ǅ", "ﬁ", "🎉中文", "ÄPFEL"] {
            assert_eq!(fold_case(s).chars().count(), s.chars().count(), "{s:?}");
        }
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

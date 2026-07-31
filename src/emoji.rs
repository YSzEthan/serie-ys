//! GitHub emoji shortcode（`:tada:`）展開成實際 emoji 字元。
//!
//! gemoji 表裡有大量單雙字母的 shortcode（`x`、`o`、`ok`、`new`…），單純比對
//! `:name:` 會把 `sed 's:o:0:g'`、`arr[1:x:2]` 這類程式碼片語一起吃掉。GitHub 自己
//! 就是這樣，但它只影響渲染；這裡展開的結果會進到搜尋與剪貼簿，代價高得多，所以
//! 額外要求 shortcode 兩側緊鄰的 byte 不是識別字元。

use std::borrow::Cow;

/// 候選 shortcode 的長度上限，由 `max_shortcode_len_covers_gemoji` 釘住。
const MAX_SHORTCODE_LEN: usize = 64;

/// 把 `s` 裡的 GitHub emoji shortcode 展開。沒有任何 shortcode 時借用原字串。
pub fn expand(s: &str) -> Cow<'_, str> {
    let bytes = s.as_bytes();
    let mut out: Option<String> = None;
    let mut copied = 0;
    let mut i = 0;

    while let Some(open) = next_colon(bytes, i) {
        // 前界：`arr[1:x:2]` 的 `:x:` 前面貼著 `1`，不是 shortcode。
        if open > 0 && is_word_byte(bytes[open - 1]) {
            i = open + 1;
            continue;
        }
        let Some(close) = find_close(bytes, open) else {
            i = open + 1;
            continue;
        };
        // 後界：`sed 's:o:0:g'` 的 `:o:` 後面貼著 `0`，同理。
        if bytes.get(close + 1).is_some_and(|&b| is_word_byte(b)) {
            i = close;
            continue;
        }
        // open 與 close 都是 ASCII 冒號，中間由 find_close 保證全是 ASCII。
        let Some(emoji) = emojis::get_by_shortcode(&s[open + 1..close]) else {
            // 退回 close 而非 open + 1：中間的 byte 都不是冒號，重掃它們沒有意義。
            i = close;
            continue;
        };

        let buf = out.get_or_insert_with(|| String::with_capacity(s.len()));
        buf.push_str(&s[copied..open]);
        buf.push_str(emoji.as_str());
        copied = close + 1;
        i = close + 1;
    }

    match out {
        Some(mut buf) => {
            buf.push_str(&s[copied..]);
            Cow::Owned(buf)
        }
        None => Cow::Borrowed(s),
    }
}

fn next_colon(bytes: &[u8], from: usize) -> Option<usize> {
    bytes[from..]
        .iter()
        .position(|&b| b == b':')
        .map(|p| p + from)
}

/// 收尾冒號的位置。撞到別的非法字元、長度超過上限、或內容為空（`::`）都算失敗。
///
/// 冒號本身就不是 `is_shortcode_byte`，所以「合法字元連續段的長度」一個掃描就夠：
/// 段尾停在冒號才算命中。
fn find_close(bytes: &[u8], open: usize) -> Option<usize> {
    let limit = (open + 1 + MAX_SHORTCODE_LEN).min(bytes.len());
    let name = &bytes[open + 1..limit];
    let len = name.iter().position(|&b| !is_shortcode_byte(b))?;
    (len > 0 && name[len] == b':').then_some(open + 1 + len)
}

/// shortcode 內容允許的字元。`+`/`-` 是為了 `:+1:` 與 `:-1:`。
fn is_shortcode_byte(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'+' | b'-')
}

/// 緊鄰 shortcode 時會使其失效的字元。UTF-8 continuation byte（≥ 0x80）不算，
/// 所以 `修好了:tada:` 這種中文貼著寫的仍然展開。
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case(":tada:", "🎉")]
    #[case(":+1:", "👍")]
    #[case(":tada: 上線", "🎉 上線")]
    #[case("修好了:tada:", "修好了🎉")]
    #[case("**:tada:**", "**🎉**")]
    #[case("(:tada:)", "(🎉)")]
    #[case("| :x: | :ok: |", "| ❌ | 🆗 |")]
    #[case(":tada::tada:", "🎉🎉")]
    // 前一段查表未命中後，指標要能退回冒號繼續找下一段。
    #[case(":foo::tada:", ":foo:🎉")]
    fn expands(#[case] input: &str, #[case] want: &str) {
        assert_eq!(expand(input), want);
    }

    #[rstest]
    // 未知 shortcode
    #[case(":notanemoji:")]
    // 邊界規則擋下的程式碼片語
    #[case("sed 's:o:0:g'")]
    #[case("sed 's:new:old:'")]
    #[case("arr[1:x:2]")]
    #[case("a:ok:b")]
    #[case(":foo:tada:")]
    // 內容本身就不是 shortcode
    #[case("std::fmt")]
    #[case("12:30:45")]
    #[case("https://example.com")]
    #[case("feat: 修正顯示")]
    #[case(":TADA:")]
    // 只有開頭冒號、沒有收尾冒號
    #[case(":tada 沒有收尾")]
    #[case("::::")]
    fn leaves_alone(#[case] input: &str) {
        assert_eq!(expand(input), input);
    }

    #[test]
    fn borrows_when_nothing_to_expand() {
        assert!(matches!(expand("plain 沒有 shortcode"), Cow::Borrowed(_)));
        assert!(matches!(expand(":tada:"), Cow::Owned(_)));
    }

    #[test]
    fn multibyte_input_does_not_panic() {
        // 冒號夾著中文：find_close 撞到非 ASCII 就放棄，不會切在字元中間。
        assert_eq!(expand("修正:顯示問題:完成"), "修正:顯示問題:完成");
        assert_eq!(expand("🎉:tada:🎉"), "🎉🎉🎉");
    }

    /// 上限是猜出來的數字就會默默漏掉長 shortcode，直接對著資料表驗。
    #[test]
    fn max_shortcode_len_covers_gemoji() {
        let longest = emojis::iter()
            .flat_map(|e| e.shortcodes())
            .map(str::len)
            .max()
            .unwrap();
        assert!(
            longest <= MAX_SHORTCODE_LEN,
            "gemoji 最長 shortcode 為 {longest}，超過 MAX_SHORTCODE_LEN"
        );
    }
}

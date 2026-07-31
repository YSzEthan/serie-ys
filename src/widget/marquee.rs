//! Marquee scrolling helpers shared across list views.
//!
//! Frame-driven pause → slide → pause → slide-back cycle. Callers advance
//! `frame` once per animation tick (see `App::marquee_frame`).

use unicode_width::UnicodeWidthChar;

use crate::event::TICK_INTERVAL;

/// How long the marquee lingers at each endpoint before sliding.
const MARQUEE_PAUSE: std::time::Duration = std::time::Duration::from_secs(1);
/// `MARQUEE_PAUSE` in frame ticks. Recomputes if `TICK_INTERVAL` changes.
pub const PAUSE_TICKS: u64 = (MARQUEE_PAUSE.as_millis() / TICK_INTERVAL.as_millis()) as u64;

/// Result of a single marquee window slice.
pub struct MarqueeSlice {
    pub text: String,
    /// Set when a double-width char straddles the offset boundary and a space
    /// was prepended to keep alignment stable (prevents half-char jitter).
    pub prepended_space: bool,
    /// Byte offset into the original text where the slice starts.
    pub start_byte: usize,
    /// Byte offset into the original text where the slice ends.
    pub end_byte: usize,
}

/// 逐字元累加的顯示寬度，與 `scroll_window` 的切窗基準一致。
///
/// **決定要不要捲動的呼叫端必須用這個，不能用 `console::measure_text_width`。**
/// 後者是 str-level（認得 VS16 / ZWJ 序列），對 `⚠️`（U+26A0 U+FE0F）算 2 格而
/// 這裡算 1 格 —— 整張 gemoji 表有 452 個這種字元，而且正好是 `:warning:`
/// `:heart:` `:recycle:` 這批最常出現在 commit 訊息裡的。兩邊不一致時
/// `scroll_window` 會判定「放得下」而原文返回，呼叫端卻已經認定 overflow 進了
/// marquee 分支：marquee 永遠不動、尾巴看不到，`marquee_needed()` 又恆為真，
/// 於是每個 tick 都重繪一次整個畫面。
///
/// 逐字元會低估 VS16 序列一格，渲染時被裁掉；要根治得改用 grapheme 分段
/// （`unicode-segmentation` + 對每個 grapheme 取 `UnicodeWidthStr::width`），
/// 那是另一個尺寸的改動。
pub fn display_width(text: &str) -> usize {
    text.chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum()
}

/// Compute the visible slice of `text` at the current marquee `frame`.
/// `available` is the target width in terminal cells.
///
/// When the text fits within `available`, the full text is returned as-is.
pub fn scroll_window(text: &str, available: usize, frame: u64) -> MarqueeSlice {
    let char_info: Vec<(usize, usize)> = text
        .char_indices()
        .map(|(bi, c)| (bi, UnicodeWidthChar::width(c).unwrap_or(0)))
        .collect();
    // 走 `display_width` 而不是從 `char_info` 自己加總：兩份定義一旦漂移，
    // 呼叫端的 overflow 判定就會再次跟這裡對不上。
    let total_visual = display_width(text);
    if total_visual <= available {
        return MarqueeSlice {
            text: text.to_string(),
            prepended_space: false,
            start_byte: 0,
            end_byte: text.len(),
        };
    }

    let max_offset = (total_visual - available) as u64;
    let period = 2 * (PAUSE_TICKS + max_offset);
    let phase = frame % period;
    let offset_cells = if phase < PAUSE_TICKS {
        0
    } else if phase < PAUSE_TICKS + max_offset {
        phase - PAUSE_TICKS
    } else if phase < 2 * PAUSE_TICKS + max_offset {
        max_offset
    } else {
        period - phase
    } as usize;

    let mut skipped = 0usize;
    let mut start_idx = char_info.len();
    for (idx, &(_, w)) in char_info.iter().enumerate() {
        if skipped >= offset_cells {
            start_idx = idx;
            break;
        }
        skipped += w;
    }
    let first_byte = char_info
        .get(start_idx)
        .map(|&(bi, _)| bi)
        .unwrap_or(text.len());
    let prepended_space = skipped > offset_cells;

    let mut acc = if prepended_space { 1 } else { 0 };
    let mut end_idx = char_info.len();
    for (idx, &(_, w)) in char_info[start_idx..].iter().enumerate() {
        if acc + w > available {
            end_idx = start_idx + idx;
            break;
        }
        acc += w;
    }
    let end_byte = char_info
        .get(end_idx)
        .map(|&(bi, _)| bi)
        .unwrap_or(text.len());

    let slice = &text[first_byte..end_byte];
    let rendered = if prepended_space {
        format!(" {slice}")
    } else {
        slice.to_string()
    };

    MarqueeSlice {
        text: rendered,
        prepended_space,
        start_byte: first_byte,
        end_byte,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_available_returns_full() {
        let s = scroll_window("hello", 10, 0);
        assert_eq!(s.text, "hello");
        assert!(!s.prepended_space);
    }

    #[test]
    fn paused_at_start() {
        // 20 chars, available=10 → slide 10. Frame 0 → at start.
        let text = "abcdefghijklmnopqrst";
        let s = scroll_window(text, 10, 0);
        assert_eq!(s.text, "abcdefghij");
    }

    #[test]
    fn sliding_after_pause() {
        let text = "abcdefghijklmnopqrst";
        // phase = PAUSE_TICKS + 1 → offset 1
        let s = scroll_window(text, 10, PAUSE_TICKS + 1);
        assert_eq!(s.text, "bcdefghijk");
    }

    /// 呼叫端一旦改用 `console::measure_text_width` 判 overflow，這條就會紅。
    ///
    /// 它跟 `scroll_window` 對「放不放得下」的答案必須一致：不一致時呼叫端進了
    /// marquee 分支、`scroll_window` 卻原文返回，marquee 永遠不動而畫面每個 tick
    /// 重繪一次。`⚠️`（U+26A0 U+FE0F）是 gemoji 裡 452 個這種字元之一，而
    /// `:warning:` 正是最常出現在 commit 訊息裡的 shortcode 之一。
    #[test]
    fn display_width_agrees_with_scroll_window_on_variation_selectors() {
        for text in [
            "⚠️ fix the thing",
            "❤️ x",
            "♻️ refactor",
            "🎉 release",
            "中文 abc",
        ] {
            let w = display_width(text);
            assert!(
                scroll_window(text, w, 0).text == text,
                "{text:?}：display_width 說剛好放得下，scroll_window 卻要捲動"
            );
            if w > 0 {
                let slice = scroll_window(text, w - 1, PAUSE_TICKS + 1);
                assert!(
                    slice.text != text || slice.prepended_space,
                    "{text:?}：display_width 說放不下，scroll_window 卻原文返回"
                );
            }
        }
    }

    #[test]
    fn cjk_double_width_prepends_space() {
        // "中文" is 2 chars, each width 2 → total width 4
        // Plus "xxxxxx" → 10 width total; available=6 → slide 4.
        // Frame exactly when boundary crosses a double-width char start.
        let text = "中文xxxxxx";
        // offset=1 would land on mid of "中" — expect prepended space
        let s = scroll_window(text, 6, PAUSE_TICKS + 1);
        assert!(s.prepended_space);
        assert!(s.text.starts_with(' '));
    }
}

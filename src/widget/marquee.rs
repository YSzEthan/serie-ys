//! 各 list view 共用的 marquee 捲動輔助函式。
//!
//! 以 frame 驅動的暫停 → 滑動 → 暫停 → 滑回 循環。呼叫端每個動畫 tick
//! 推進一次 `frame`（見 `App::marquee_frame`）。

use std::ops::Range;

use ratatui::text::Span;
use unicode_width::UnicodeWidthChar;

use crate::event::TICK_INTERVAL;

/// marquee 在每個端點停留多久才開始滑動。
const MARQUEE_PAUSE: std::time::Duration = std::time::Duration::from_secs(1);
/// `MARQUEE_PAUSE` 換算成 frame tick 數。`TICK_INTERVAL` 改變時會重新計算。
pub const PAUSE_TICKS: u64 = (MARQUEE_PAUSE.as_millis() / TICK_INTERVAL.as_millis()) as u64;

/// 單次 marquee 視窗切片的結果。
pub struct MarqueeSlice {
    pub text: String,
    /// 當雙寬字元橫跨 offset 邊界、需要補一個空格來維持對齊穩定
    /// （避免半個字元的抖動）時設為 true。
    pub prepended_space: bool,
    /// 切片在原始文字中的起始 byte offset。
    pub start_byte: usize,
    /// 切片在原始文字中的結束 byte offset。
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

/// 計算 `text` 在目前 marquee `frame` 下可見的切片。
/// `available` 是以終端機格數表示的目標寬度。
///
/// 當文字寬度在 `available` 之內時，原樣回傳整段文字。
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

/// 依原字串（所有 span 內容串起來）的 byte 範圍切回多個 span，保留各自的
/// style，交集為空的 span 丟掉。範圍必須落在字元邊界（`scroll_window` 與
/// `split_at_width` 的回傳值都是）。
fn slice_spans(spans: &[Span<'_>], range: Range<usize>) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for span in spans {
        let text = span.content.as_ref();
        let end = start + text.len();
        let (lo, hi) = (range.start.max(start), range.end.min(end));
        if lo < hi {
            out.push(Span::styled(
                text[lo - start..hi - start].to_string(),
                span.style,
            ));
        }
        start = end;
    }
    out
}

fn joined(spans: &[Span<'_>]) -> String {
    spans.iter().map(|s| s.content.as_ref()).collect()
}

/// 多個 span 串起來的顯示寬度，基準同 `display_width`。
pub fn spans_display_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| display_width(&s.content)).sum()
}

/// 選取列用：整串 span 放不下 `available` 時依 `frame` 捲動，保留各段顏色。
pub fn scroll_spans(spans: &[Span<'_>], available: usize, frame: u64) -> Vec<Span<'static>> {
    let slice = scroll_window(&joined(spans), available, frame);
    let mut out = slice_spans(spans, slice.start_byte..slice.end_byte);
    if slice.prepended_space {
        out.insert(0, Span::raw(" "));
    }
    out
}

/// 非選取列用：放不下 `available` 時截到 `available - 1` 格再補 `…`，保留
/// 各段顏色。以字元為單位切（不是以 span），所以長 ref 名稱會截在名稱中間
/// 而不是整段消失。`available` 至少要 2（1 以下會比可用寬度還寬）。
pub fn truncate_spans(spans: &[Span<'_>], available: usize) -> Vec<Span<'static>> {
    let text = joined(spans);
    let (head, _) = crate::widget::split_at_width(&text, available.saturating_sub(1));
    let mut out = slice_spans(spans, 0..head.len());
    out.push(Span::raw(crate::widget::ELLIPSIS));
    out
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
        // 20 個字元，available=10 → 需要滑動 10 格。Frame 0 → 停在起始位置。
        let text = "abcdefghijklmnopqrst";
        let s = scroll_window(text, 10, 0);
        assert_eq!(s.text, "abcdefghij");
    }

    #[test]
    fn sliding_after_pause() {
        let text = "abcdefghijklmnopqrst";
        // phase = PAUSE_TICKS + 1 → offset 為 1
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
        // "中文" 是 2 個字元，各佔寬度 2 → 總寬度 4
        // 加上 "xxxxxx" → 總寬度 10；available=6 → 需要滑動 4 格。
        // 挑選邊界恰好跨過雙寬字元起點的 frame。
        let text = "中文xxxxxx";
        // offset=1 會落在 "中" 的中間 — 預期會補上前導空格
        let s = scroll_window(text, 6, PAUSE_TICKS + 1);
        assert!(s.prepended_space);
        assert!(s.text.starts_with(' '));
    }

    fn styled(text: &'static str, color: ratatui::style::Color) -> Span<'static> {
        Span::styled(text, ratatui::style::Style::default().fg(color))
    }

    #[test]
    fn slice_spans_keeps_styles_across_span_boundaries() {
        use ratatui::style::Color;
        let spans = [
            styled("(", Color::Red),
            styled("origin", Color::Blue),
            styled(")", Color::Red),
        ];
        let out = slice_spans(&spans, 0..4);
        assert_eq!(joined(&out), "(ori");
        assert_eq!(out[0].style.fg, Some(Color::Red));
        assert_eq!(out[1].style.fg, Some(Color::Blue));
        // 範圍完全落在第二段：第一、三段被丟掉
        let out = slice_spans(&spans, 2..5);
        assert_eq!(out.len(), 1);
        assert_eq!(joined(&out), "rig");
    }

    #[test]
    fn truncate_spans_cuts_inside_a_long_name_and_appends_ellipsis() {
        use ratatui::style::Color;
        let spans = [
            styled("(", Color::Red),
            styled("origin/feature-very-long", Color::Blue),
            styled(")", Color::Red),
        ];
        let out = truncate_spans(&spans, 12);
        assert_eq!(joined(&out), "(origin/fea…");
        assert!(spans_display_width(&out) <= 12);
        assert_eq!(out[1].style.fg, Some(Color::Blue));
    }

    #[test]
    fn truncate_spans_never_splits_a_double_width_char() {
        let spans = [Span::raw("分支名稱很長")];
        let out = truncate_spans(&spans, 6);
        // 5 格放得下兩個雙寬字（4 格）；第三個字會超過，所以不切半個
        assert_eq!(joined(&out), "分支…");
        assert!(spans_display_width(&out) <= 6);
    }

    #[test]
    fn scroll_spans_moves_the_window_and_keeps_colors() {
        use ratatui::style::Color;
        let spans = [
            styled("(", Color::Red),
            styled("origin/feature-very-long", Color::Blue),
            styled(")", Color::Red),
        ];
        let first = scroll_spans(&spans, 10, 0);
        assert_eq!(joined(&first), "(origin/fe");
        // 全長 26 格、視窗 10 格：捲動距離 16，frame = PAUSE + 16 停在尾端
        let last = scroll_spans(&spans, 10, PAUSE_TICKS + 16);
        assert!(joined(&last).ends_with(')'), "{}", joined(&last));
        assert_eq!(last.last().unwrap().style.fg, Some(Color::Red));
        assert!(spans_display_width(&last) <= 10);
    }

    #[test]
    fn scroll_spans_prepends_a_space_when_a_wide_char_straddles_the_edge() {
        let spans = [Span::raw("分支名稱很長很長")];
        for frame in PAUSE_TICKS..PAUSE_TICKS + 8 {
            let out = scroll_spans(&spans, 5, frame);
            assert!(
                spans_display_width(&out) <= 5,
                "frame {frame}: {}",
                joined(&out)
            );
        }
    }

    #[test]
    fn spans_display_width_agrees_with_scroll_window_on_variation_selectors() {
        // VS16 序列：呼叫端判定 overflow 的寬度必須跟 scroll_window 一致，
        // 否則 marquee 不動卻每個 tick 重繪
        let spans = [Span::raw("⚠️warn-ref")];
        let w = spans_display_width(&spans);
        assert_eq!(w, display_width("⚠️warn-ref"));
        let out = scroll_spans(&spans, w, 0);
        assert_eq!(joined(&out), "⚠️warn-ref");
    }
}

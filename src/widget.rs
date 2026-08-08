pub mod commit_detail;
pub mod commit_list;
pub mod marquee;
pub mod output_pane;
pub mod pending_overlay;
pub mod ref_list;

use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};

use crate::{color::ColorTheme, event::UserEvent, keybind::KeyBind};

/// 提示列的分隔符。寬度計算與實際渲染共用同一個常數。
const HINT_SEP: &str = "  ";

/// 截斷記號。`…`（U+2026）在 unicode-width 是 East Asian **Ambiguous** —— 一般
/// 終端算 1 格，但終端設成 CJK 寬度時佔 2 格。預留 2 格是刻意的，寧可少填一格
/// 也不要溢出被切掉。
const ELLIPSIS: &str = "…";
const ELLIPSIS_RESERVE: usize = 2;

/// 一組提示。多個 event 共同構成一個動作時（例如 ←→ 同為 toggle），
/// 各取其 display key 後用 `/` 串起來。
pub type HintSpec = (&'static [UserEvent], &'static str);

/// 建 `HintSpec` 的簡寫，比照 `view::help` 的 `fn b(...)`。
pub const fn h(events: &'static [UserEvent], desc: &'static str) -> HintSpec {
    (events, desc)
}

/// 把 `HintSpec` 解析成可渲染的 `(按鍵, 說明)`。
///
/// 一組裡**全部** event 都沒綁定才整組略過；只綁到一部分就顯示綁到的那些
/// —— 使用者解掉 `navigate_right` 之後，`h/l:commit` 會變成 `h:commit`，
/// 而不是整組消失讓他以為連 `h` 都沒了。
pub fn hint_pairs(keybind: &KeyBind, hints: &[HintSpec]) -> Vec<(String, &'static str)> {
    hints
        .iter()
        .filter_map(|(events, desc)| {
            let keys: Vec<String> = events
                .iter()
                .filter_map(|e| keybind.display_key(*e))
                .collect();
            (!keys.is_empty()).then(|| (keys.join("/"), *desc))
        })
        .collect()
}

/// `hint_pairs` + `hint_line` 是永遠成對出現的兩步驟（`key_fg` 幾乎都是
/// `theme.help_key_fg`）；這個組合入口收掉呼叫端重複的中繼變數。
/// `hint_pairs` / `hint_line` 仍保留給需要中間結果或不同 `key_fg` 的呼叫端
/// （wizard 沒有 `KeyBind`；`confirm_line` 要用 `status_interactive_fg`）。
pub fn keybind_hint_line(
    theme: &ColorTheme,
    keybind: &KeyBind,
    hints: &[HintSpec],
) -> Line<'static> {
    hint_line(theme, &hint_pairs(keybind, hints), theme.help_key_fg)
}

/// `key:desc  key:desc…` —— key 用 `key_fg`，冒號與 desc 用
/// `status_input_transient_fg`。
pub fn hint_line(theme: &ColorTheme, pairs: &[(String, &str)], key_fg: Color) -> Line<'static> {
    let desc_style = Style::default().fg(theme.status_input_transient_fg);
    let key_style = Style::default().fg(key_fg);

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(pairs.len() * 3);
    for (i, (key, desc)) in pairs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(HINT_SEP, desc_style));
        }
        spans.push(Span::styled(key.clone(), key_style));
        spans.push(Span::styled(format!(":{desc}"), desc_style));
    }
    Line::from(spans)
}

/// 把一行截到 `max_width` 格寬，有丟東西就補 `…`。
///
/// 以 span 為單位丟，不切 span 中間 —— 提示列的 span 是「一個按鍵」或
/// 「一段說明」，切一半只會產生看不懂的殘字，而且有切壞 CJK grapheme 的風險。
///
/// 原地改 `line.spans`，不重建整個 `Line` —— `Line` 除了 `spans` 還有
/// `style`／`alignment`，之前的寫法只手動搬 `style`（`max_width == 0` 那條
/// 分支甚至連 `style` 都沒搬），`alignment` 兩條路徑都會被弄丟。原地改就不
/// 用逐欄位搬，未來 `Line` 再加欄位也不會重蹈覆轍。
pub fn truncate_line(mut line: Line<'_>, max_width: usize) -> Line<'_> {
    if max_width == 0 {
        line.spans.clear();
        return line;
    }
    let total: usize = line.spans.iter().map(span_display_width).sum();
    if total <= max_width {
        return line;
    }

    let budget = max_width.saturating_sub(ELLIPSIS_RESERVE);
    let mut used = 0usize;
    let keep = line
        .spans
        .iter()
        .take_while(|s| {
            used += span_display_width(s);
            used <= budget
        })
        .count();
    line.spans.truncate(keep);
    line.spans.push(Span::raw(ELLIPSIS));
    line
}

fn span_display_width(span: &Span<'_>) -> usize {
    console::measure_text_width(span.content.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::ColorTheme;

    fn pairs(v: &[(&str, &'static str)]) -> Vec<(String, &'static str)> {
        v.iter().map(|(k, d)| ((*k).to_string(), *d)).collect()
    }

    #[test]
    fn hint_line_uses_colon_format() {
        let theme = ColorTheme::default();
        let line = hint_line(
            &theme,
            &pairs(&[("u", "pane"), ("j/k", "move")]),
            theme.help_key_fg,
        );
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "u:pane  j/k:move");
    }

    #[test]
    fn hint_pairs_joins_multiple_events_with_slash() {
        let keybind = KeyBind::new(None);
        let hints = [h(
            &[UserEvent::NavigateLeft, UserEvent::NavigateRight],
            "commit",
        )];
        assert_eq!(
            hint_pairs(&keybind, &hints),
            vec![("h/l".to_string(), "commit")]
        );
    }

    #[test]
    fn hint_pairs_skips_group_only_when_every_event_is_unbound() {
        let keybind = KeyBind::new(None);
        // UserCommand(1) 預設沒綁；單獨一組會被略過
        let only_unbound = [h(&[UserEvent::UserCommand(1)], "cmd")];
        assert!(hint_pairs(&keybind, &only_unbound).is_empty());

        // 混一個有綁的，就只顯示有綁的那個
        let partial = [h(
            &[UserEvent::UserCommand(1), UserEvent::Refresh],
            "refresh",
        )];
        assert_eq!(
            hint_pairs(&keybind, &partial),
            vec![("r".to_string(), "refresh")]
        );
    }

    #[test]
    fn truncate_keeps_everything_when_it_fits() {
        let theme = ColorTheme::default();
        let line = hint_line(&theme, &pairs(&[("u", "pane")]), theme.help_key_fg);
        let out = truncate_line(line, 80);
        let text: String = out.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "u:pane");
    }

    #[test]
    fn truncate_drops_whole_hints_and_appends_ellipsis() {
        let theme = ColorTheme::default();
        let line = hint_line(
            &theme,
            &pairs(&[("u", "pane"), ("j/k", "move"), ("r", "refresh")]),
            theme.help_key_fg,
        );
        let out = truncate_line(line, 12);
        let text: String = out.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.ends_with(ELLIPSIS), "應以省略號結尾：{text}");
        assert!(
            console::measure_text_width(&text) <= 12,
            "截斷後仍超寬：{text}"
        );
        // 沒有切在 span 中間：保留下來的是完整的 "u:pane"
        assert!(text.starts_with("u:pane"), "{text}");
    }

    #[test]
    fn truncate_with_tiny_width_does_not_panic() {
        let theme = ColorTheme::default();
        let line = hint_line(
            &theme,
            &pairs(&[("u", "pane"), ("j/k", "move")]),
            theme.help_key_fg,
        );
        for w in 0..4 {
            let _ = truncate_line(line.clone(), w);
        }
    }

    /// 之前的實作在 `max_width == 0` 時整個重建 `Line`，會弄丟呼叫端設在
    /// line 層級的 style（例如 `Line::raw(msg).fg(color)`）。原地 mutate
    /// 之後這個欄位要保留。
    #[test]
    fn truncate_preserves_line_style_even_at_zero_width() {
        use ratatui::style::Stylize;

        let line = Line::raw("hello").fg(ratatui::style::Color::Red);
        let out = truncate_line(line.clone(), 0);
        assert_eq!(out.style, line.style);
        assert!(out.spans.is_empty());
    }
}

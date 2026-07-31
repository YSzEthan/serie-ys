//! Light-weight markdown renderer for GitHub issue/PR bodies.
//!
//! Line-based, no dep on external crates. Supports: headings (1-3),
//! unordered/ordered lists, blockquote, hrule, fenced code, GFM tables laid
//! out to the available width, `**bold**`, `` `code` ``, `[text](url)` and
//! HTML entities. Link reference definitions, HTML comments and raw HTML are
//! dropped — bot comments (Vercel, CI) are full of them and they carry
//! nothing a terminal can show. `*italic*` is not handled.

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

const RULE_WIDTH: usize = 40;

pub fn render(body: &str, width: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let raw: Vec<&str> = body.lines().collect();
    let mut in_code = false;
    let mut in_html_comment = false;
    let mut i = 0;
    while i < raw.len() {
        let line = raw[i];

        // Fenced code block
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            out.push(rule_line(width));
            i += 1;
            continue;
        }
        if in_code {
            out.push(Line::styled(
                line.to_string(),
                Style::default().fg(Color::Gray),
            ));
            i += 1;
            continue;
        }

        // 以下的分支看到的都是展開後的內容。放在 code fence 分支之後，程式碼區塊
        // 才能像 GitHub 一樣保住 `:tada:` 原文。
        let expanded = crate::emoji::expand(line);
        let line = &*expanded;

        // HTML comments, tracked like code fences so they can span lines.
        // Vercel hides a base64 blob in one at the top of every comment.
        // Only recognised at the start of a line: mid-line comments are rare
        // enough that handling them would complicate every other branch.
        if in_html_comment {
            in_html_comment = !line.contains("-->");
            i += 1;
            continue;
        }
        if line.trim_start().starts_with("<!--") {
            in_html_comment = !line.contains("-->");
            i += 1;
            continue;
        }

        // Table (header + separator + rows)
        if is_table_row(line) && i + 1 < raw.len() && is_separator_row(raw[i + 1]) {
            let mut rows: Vec<Vec<String>> = Vec::new();
            rows.push(split_cells(line));
            i += 2;
            while i < raw.len() && is_table_row(raw[i]) {
                // 內文列是直接從 raw 取的，繞過了上面的行首展開，得自己來。
                rows.push(split_cells(&crate::emoji::expand(raw[i])));
                i += 1;
            }
            render_table(&mut out, &rows, width);
            continue;
        }

        // Invisible in rendered markdown; Vercel emits a huge base64 one.
        if is_link_reference_definition(line) {
            i += 1;
            continue;
        }

        // Block-level raw HTML (`<a href=…>`, `<picture>`, `</div>`).
        if is_html_block(line) {
            i += 1;
            continue;
        }

        let trimmed = line.trim_start();

        // Heading
        if let Some(rest) = trimmed.strip_prefix("### ") {
            out.push(Line::styled(
                rest.to_string(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ));
            i += 1;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("## ") {
            out.push(Line::styled(
                rest.to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ));
            i += 1;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("# ") {
            out.push(Line::styled(
                rest.to_string(),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
            i += 1;
            continue;
        }

        // Blockquote
        if let Some(rest) = trimmed.strip_prefix("> ") {
            out.push(Line::from(vec![
                Span::styled("│ ", Style::default().fg(Color::DarkGray)),
                Span::styled(rest.to_string(), Style::default().fg(Color::Gray)),
            ]));
            i += 1;
            continue;
        }

        // Horizontal rule
        if trimmed.starts_with("---") || trimmed.starts_with("___") || trimmed.starts_with("***") {
            let rest = trimmed.trim_matches(|c| c == '-' || c == '_' || c == '*');
            if rest.trim().is_empty() {
                out.push(rule_line(width));
                i += 1;
                continue;
            }
        }

        // Unordered list
        if let Some(rest) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            let indent = line.len() - trimmed.len();
            let mut spans: Vec<Span<'static>> = Vec::new();
            if indent > 0 {
                spans.push(Span::raw(" ".repeat(indent)));
            }
            spans.push(Span::styled("• ", Style::default().fg(Color::Yellow)));
            spans.extend(scan_inline(rest));
            out.push(Line::from(spans));
            i += 1;
            continue;
        }

        // Ordered list (`1. `, `23. `)
        if let Some((prefix, rest)) = split_ordered_list(trimmed) {
            let indent = line.len() - trimmed.len();
            let mut spans: Vec<Span<'static>> = Vec::new();
            if indent > 0 {
                spans.push(Span::raw(" ".repeat(indent)));
            }
            spans.push(Span::styled(
                format!("{prefix} "),
                Style::default().fg(Color::Yellow),
            ));
            spans.extend(scan_inline(rest));
            out.push(Line::from(spans));
            i += 1;
            continue;
        }

        // Plain line with inline scanning
        let spans = scan_inline(line);
        out.push(if spans.is_empty() {
            Line::raw(String::new())
        } else {
            Line::from(spans)
        });
        i += 1;
    }
    out
}

/// Section divider, clamped so it never wraps in a narrow preview.
/// Grey — dividers inside rendered markdown stay neutral.
pub(super) fn rule_line(width: usize) -> Line<'static> {
    rule_line_colored(width, Color::DarkGray)
}

/// [`rule_line`] in a caller-chosen colour, for dividers that separate
/// preview sections rather than markdown content.
pub(super) fn rule_line_colored(width: usize, color: Color) -> Line<'static> {
    let n = RULE_WIDTH.min(width);
    Line::styled("─".repeat(n), Style::default().fg(color))
}

/// `[label]: url` — invisible in rendered markdown.
///
/// A link destination is ASCII and contains no bare whitespace (CommonMark).
/// The ASCII half matters as much as the whitespace half: CJK prose such as
/// `[註]: 這是一段中文說明` has no spaces either, and dropping it would delete
/// real content. A definition carrying a quoted title is not recognised —
/// showing one is harmless, deleting prose is not.
fn is_link_reference_definition(s: &str) -> bool {
    let t = s.trim();
    let Some(rest) = t.strip_prefix('[') else {
        return false;
    };
    let Some(close) = rest.find("]:") else {
        return false;
    };
    let dest = rest[close + 2..].trim();
    !dest.is_empty() && dest.is_ascii() && !dest.contains(char::is_whitespace)
}

/// A line that is raw HTML rather than prose.
///
/// `<` alone is not enough: `<= 3 表示 ... > 0` is ordinary text. Requiring an
/// ASCII letter or `/` right after `<` is the cheap test that keeps prose.
fn is_html_block(s: &str) -> bool {
    let t = s.trim();
    let mut chars = t.chars();
    if chars.next() != Some('<') {
        return false;
    }
    let is_tag_start = matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '/');
    is_tag_start && t.contains('>')
}

fn is_table_row(s: &str) -> bool {
    let t = s.trim();
    t.starts_with('|') && t.matches('|').count() >= 2
}

fn is_separator_row(s: &str) -> bool {
    let t = s.trim();
    if !t.starts_with('|') {
        return false;
    }
    t.contains('-') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

fn split_cells(row: &str) -> Vec<String> {
    let t = row.trim().trim_start_matches('|').trim_end_matches('|');
    let mut cells = vec![String::new()];
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek() == Some(&'|') {
            cells.last_mut().unwrap().push('|');
            chars.next();
        } else if c == '|' {
            cells.push(String::new());
        } else {
            cells.last_mut().unwrap().push(c);
        }
    }
    for s in cells.iter_mut() {
        *s = s.trim().to_string();
    }
    cells
}

/// Gap between columns.
const COL_GAP: usize = 2;

fn render_table(out: &mut Vec<Line<'static>>, rows: &[Vec<String>], width: usize) {
    let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if cols == 0 {
        return;
    }

    // Cells become spans first: column widths must be measured on what is
    // actually drawn, not on the source markdown. A cell holding
    // `[name](https://…)` is a few characters wide once rendered, not a
    // hundred.
    let cells: Vec<Vec<Vec<Span<'static>>>> = rows
        .iter()
        .map(|row| {
            (0..cols)
                .map(|c| {
                    row.get(c)
                        .map(|s| trim_spans(scan_inline(s)))
                        .unwrap_or_default()
                })
                .collect()
        })
        .collect();

    let natural: Vec<usize> = (0..cols)
        .map(|c| {
            cells
                .iter()
                .map(|row| spans_width(&row[c]))
                .max()
                .unwrap_or(0)
        })
        .collect();

    let widths = fit_columns(&natural, width);

    for (ri, row) in cells.iter().enumerate() {
        let bold = ri == 0;
        // Wrap every cell first, then emit one output line per wrapped row.
        let wrapped: Vec<Vec<Vec<Span<'static>>>> = row
            .iter()
            .zip(&widths)
            .map(|(cell, &w)| wrap_cell(cell, w))
            .collect();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);

        for r in 0..height {
            let mut spans: Vec<Span<'static>> = Vec::new();
            for (c, (cell, &col_width)) in wrapped.iter().zip(&widths).enumerate() {
                // A cell with fewer wrapped rows than its neighbours is not a
                // special case — it just contributes no spans on this row.
                let part: &[Span<'static>] = cell.get(r).map_or(&[], Vec::as_slice);
                let used = spans_width(part);
                spans.extend(part.iter().map(|s| {
                    if bold {
                        Span::styled(s.content.to_string(), s.style.add_modifier(Modifier::BOLD))
                    } else {
                        s.clone()
                    }
                }));
                if c + 1 < cols {
                    let pad = col_width.saturating_sub(used) + COL_GAP;
                    spans.push(Span::raw(" ".repeat(pad)));
                }
            }
            out.push(Line::from(spans));
        }

        if ri == 0 {
            let total = widths.iter().sum::<usize>() + cols.saturating_sub(1) * COL_GAP;
            out.push(Line::styled(
                "─".repeat(total.min(width)),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(Span::width).sum()
}

/// Drop leading/trailing blanks left behind by removed markup.
///
/// `split_cells` trims the source text, but dropping an image from
/// `![Ready](…) [Ready](…)` re-introduces a leading space, which would shift
/// that column one cell right of its header.
fn trim_spans(mut spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
    while let Some(first) = spans.first() {
        let trimmed = first.content.trim_start().to_string();
        if trimmed.is_empty() {
            spans.remove(0);
        } else {
            spans[0] = Span::styled(trimmed, first.style);
            break;
        }
    }
    while let Some(last) = spans.last() {
        let trimmed = last.content.trim_end().to_string();
        if trimmed.is_empty() {
            spans.pop();
        } else {
            let i = spans.len() - 1;
            spans[i] = Span::styled(trimmed, last.style);
            break;
        }
    }
    spans
}

/// Distribute `width` across columns by equal shares with give-back.
///
/// Proportional-to-natural-width would be wrong: the column that blew the
/// budget (a bare URL, say) would claim most of the space and squeeze the
/// short, information-dense ones into two characters. Here every column that
/// fits inside an equal share takes only what it needs and returns the rest,
/// which is then re-divided among the columns still over budget.
fn fit_columns(natural: &[usize], width: usize) -> Vec<usize> {
    let cols = natural.len();
    let gaps = cols.saturating_sub(1) * COL_GAP;
    let avail = width.saturating_sub(gaps);
    // Overwhelmingly the common case: it already fits, so touch nothing.
    if cols == 0 || natural.iter().sum::<usize>() <= avail {
        return natural.to_vec();
    }

    let mut out = vec![0usize; cols];
    let mut settled = vec![false; cols];
    let mut remaining = avail;
    let mut unsettled = cols;

    // Each pass settles at least one column, so this terminates.
    while unsettled > 0 {
        let share = remaining / unsettled;
        let Some(idx) = (0..cols).find(|&c| !settled[c] && natural[c] <= share) else {
            // Nobody fits their share any more — split what is left evenly.
            for c in 0..cols {
                if !settled[c] {
                    out[c] = share.max(1);
                }
            }
            break;
        };
        out[idx] = natural[idx];
        settled[idx] = true;
        remaining -= natural[idx];
        unsettled -= 1;
    }
    out
}

/// Wrap one cell's spans to `width`, breaking mid-word.
///
/// Hard breaks are right here even though prose wants word breaks: cells hold
/// short phrases and URLs, and a URL has no spaces to break at.
fn wrap_cell(spans: &[Span<'static>], width: usize) -> Vec<Vec<Span<'static>>> {
    if width == 0 {
        return vec![Vec::new()];
    }
    let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
    let mut row: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;

    for span in spans {
        let mut rest: &str = span.content.as_ref();
        while !rest.is_empty() {
            let room = width - used;
            let (head, tail) = split_at_width(rest, room);
            if head.is_empty() {
                // Nothing fits in the remainder of this row — start a new one.
                rows.push(std::mem::take(&mut row));
                used = 0;
                continue;
            }
            row.push(Span::styled(head.to_string(), span.style));
            used += str_width(head);
            rest = tail;
            if used >= width && !rest.is_empty() {
                rows.push(std::mem::take(&mut row));
                used = 0;
            }
        }
    }
    if !row.is_empty() || rows.is_empty() {
        rows.push(row);
    }
    rows
}

fn str_width(s: &str) -> usize {
    Span::raw(s).width()
}

/// Split `s` at the last char boundary that keeps its width within `max`.
fn split_at_width(s: &str, max: usize) -> (&str, &str) {
    let mut w = 0usize;
    for (i, c) in s.char_indices() {
        let cw = str_width(c.encode_utf8(&mut [0u8; 4]));
        if w + cw > max {
            return s.split_at(i);
        }
        w += cw;
    }
    (s, "")
}

/// `1. rest` or `23. rest` → (prefix_with_dot, rest). None if not ordered list.
fn split_ordered_list(s: &str) -> Option<(&str, &str)> {
    let dot_pos = s.find('.')?;
    let (num, after) = s.split_at(dot_pos);
    if num.is_empty() || !num.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let rest = after.strip_prefix(". ")?;
    Some((&s[..=dot_pos], rest))
}

/// Split a line into styled spans, handling `**bold**`, `` `code` ``,
/// `[text](url)`, `![alt](url)` and HTML entities.
/// Non-matching text becomes a plain span. Unmatched markers stay as literal.
fn scan_inline(text: &str) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let bytes = text.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        // ![alt](url) — checked before `[`, otherwise the `!` lands in buf and
        // the image degrades into a link showing its alt text.
        if bytes[i] == b'!' && bytes.get(i + 1) == Some(&b'[') {
            if let Some((_, end)) = parse_link(text, i + 1) {
                i = end;
                continue;
            }
        }
        // [text](url) — the URL is unusable in a TUI, so only the label shows.
        if bytes[i] == b'[' {
            if let Some((label, end)) = parse_link(text, i) {
                if !buf.is_empty() {
                    spans.push(Span::raw(std::mem::take(&mut buf)));
                }
                spans.push(Span::styled(
                    label,
                    Style::default()
                        .fg(Color::Blue)
                        .add_modifier(Modifier::UNDERLINED),
                ));
                i = end;
                continue;
            }
        }
        // HTML entities — more common than raw tags in bot comments.
        if bytes[i] == b'&' {
            if let Some((decoded, end)) = parse_entity(text, i) {
                buf.push(decoded);
                i = end;
                continue;
            }
        }
        // **bold**
        if bytes[i] == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            if let Some(end) = find_close(text, i + 2, "**") {
                if !buf.is_empty() {
                    spans.push(Span::raw(std::mem::take(&mut buf)));
                }
                spans.push(Span::styled(
                    text[i + 2..end].to_string(),
                    Style::default().add_modifier(Modifier::BOLD),
                ));
                i = end + 2;
                continue;
            }
        }
        // `code`
        if bytes[i] == b'`' {
            if let Some(end) = find_close(text, i + 1, "`") {
                if !buf.is_empty() {
                    spans.push(Span::raw(std::mem::take(&mut buf)));
                }
                spans.push(Span::styled(
                    text[i + 1..end].to_string(),
                    Style::default().bg(Color::DarkGray).fg(Color::White),
                ));
                i = end + 1;
                continue;
            }
        }
        // Copy one UTF-8 scalar (not one byte) into buf to stay valid
        let ch_len = utf8_char_len(bytes[i]);
        buf.push_str(&text[i..i + ch_len]);
        i += ch_len;
    }
    if !buf.is_empty() {
        spans.push(Span::raw(buf));
    }
    spans
}

fn utf8_char_len(b: u8) -> usize {
    match b {
        0..=0x7F => 1,    // ASCII
        0x80..=0xBF => 1, // invalid continuation — advance safely
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

fn find_close(text: &str, from: usize, marker: &str) -> Option<usize> {
    text[from..].find(marker).map(|p| from + p)
}

/// Parse `[label](dest)` starting at the `[` in `open`.
/// Returns the label and the byte index just past the closing `)`.
fn parse_link(text: &str, open: usize) -> Option<(String, usize)> {
    let rest = text.get(open + 1..)?;
    let close = rest.find(']')?;
    if !rest[close + 1..].starts_with('(') {
        return None;
    }
    let paren = rest[close + 2..].find(')')?;
    let label = rest[..close].to_string();
    Some((label, open + 1 + close + 2 + paren + 1))
}

/// Decode the handful of entities GitHub actually emits.
/// Returns the character and the byte index just past the `;`.
fn parse_entity(text: &str, at: usize) -> Option<(char, usize)> {
    const ENTITIES: [(&str, char); 6] = [
        ("&nbsp;", ' '),
        ("&amp;", '&'),
        ("&lt;", '<'),
        ("&gt;", '>'),
        ("&quot;", '"'),
        ("&#39;", '\''),
    ];
    let rest = text.get(at..)?;
    ENTITIES
        .iter()
        .find(|(name, _)| rest.starts_with(name))
        .map(|(name, ch)| (*ch, at + name.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wide enough that nothing wraps — tests that care about width call
    /// `super::render` directly. Shadows the glob-imported `render`.
    fn render(body: &str) -> Vec<Line<'static>> {
        super::render(body, 80)
    }

    fn text_of(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn render_empty_body() {
        assert!(render("").is_empty());
    }

    #[test]
    fn render_heading() {
        let lines = render("## Hello");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].style.fg, Some(Color::Cyan));
        assert!(lines[0].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn render_list_nested_indent() {
        let lines = render("- a\n  - b");
        assert_eq!(lines.len(), 2);
        // first line: bullet + "a"
        assert!(lines[0]
            .spans
            .iter()
            .any(|s| s.content.as_ref().contains("•")));
        // nested line: leading spaces span
        assert!(lines[1]
            .spans
            .iter()
            .any(|s| s.content.as_ref().starts_with("  ")));
    }

    #[test]
    fn render_table_basic() {
        let lines = render("| A | B |\n|---|---|\n| 1 | 2 |");
        assert_eq!(lines.len(), 3); // header, separator, row
                                    // header first span bold
        assert!(lines[0].spans[0]
            .style
            .add_modifier
            .contains(Modifier::BOLD));
        // separator line is all ─
        assert!(lines[1]
            .spans
            .iter()
            .any(|s| s.content.as_ref().contains("─")));
    }

    #[test]
    fn emoji_shortcodes_expand_outside_code_fences() {
        let lines = render("# :tada: 標題\n- :bug: 項目\n```\n:tada: 原文\n```");
        assert_eq!(text_of(&lines[0]), "🎉 標題");
        assert_eq!(text_of(&lines[1]), "• 🐛 項目");
        // lines[2] 是 fence 的分隔線，lines[3] 才是程式碼內容
        assert_eq!(text_of(&lines[3]), ":tada: 原文");
    }

    #[test]
    fn emoji_shortcodes_expand_in_table_body_rows() {
        let lines = render("| :x: 表頭 | B |\n|---|---|\n| :ok: 內文 | 2 |");
        assert!(text_of(&lines[0]).contains('❌'));
        assert!(text_of(&lines[2]).contains('🆗'), "內文列也要展開");
    }

    #[test]
    fn render_table_escaped_pipe() {
        let lines = render(
            r#"| name | val |
|---|---|
| a\|b | 1 |"#,
        );
        // row has `a|b` in first cell (not split)
        let row = &lines[2];
        assert!(row.spans.iter().any(|s| s.content.as_ref() == "a|b"));
    }

    #[test]
    fn render_table_no_separator_is_plain() {
        let lines = render("| a | b |\njust text");
        // No separator → both lines plain, not table
        assert_eq!(lines.len(), 2);
        // First line should contain literal `|`
        assert!(lines[0]
            .spans
            .iter()
            .any(|s| s.content.as_ref().contains('|')));
    }

    #[test]
    fn render_code_fence() {
        let lines = render("```\nlet x = 1;\nlet y = 2;\n```");
        // rule + 2 code lines + rule = 4
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[1].style.fg, Some(Color::Gray));
        assert_eq!(lines[2].style.fg, Some(Color::Gray));
    }

    #[test]
    fn render_inline_bold_and_code() {
        let lines = render("**bold** and `code`");
        assert_eq!(lines.len(), 1);
        let spans = &lines[0].spans;
        // Find bold span
        let bold = spans
            .iter()
            .find(|s| s.style.add_modifier.contains(Modifier::BOLD));
        assert!(bold.is_some(), "should have a bold span");
        assert_eq!(bold.unwrap().content.as_ref(), "bold");
        // Find code span (dark bg)
        let code = spans.iter().find(|s| s.style.bg == Some(Color::DarkGray));
        assert!(code.is_some(), "should have a code span");
        assert_eq!(code.unwrap().content.as_ref(), "code");
    }

    // ── PR/bot comment noise ──

    #[test]
    fn drops_link_reference_definition() {
        let lines = render("[vc]: #bDxmwiKVhzWA8lnKsChqdkPRvpRGYhaFCXP1WC1h+T4=");
        assert!(lines.is_empty(), "got {lines:?}");
    }

    #[test]
    fn keeps_bracketed_prose_with_spaces_after_colon() {
        // A link destination cannot contain bare whitespace, so this is prose.
        let lines = render("[註]: 這是一段中文說明");
        assert_eq!(lines.len(), 1);
        assert!(lines[0]
            .spans
            .iter()
            .any(|s| s.content.contains("中文說明")));
    }

    #[test]
    fn drops_html_comment_spanning_lines() {
        let lines = render("before\n<!-- hidden\nstill hidden\n-->\nafter");
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(
            text.contains("before") && text.contains("after"),
            "got {text:?}"
        );
        assert!(!text.contains("hidden"), "got {text:?}");
    }

    #[test]
    fn drops_raw_html_block() {
        let lines = render("<a href=\"https://x\"><picture><source media=\"(x)\">");
        assert!(lines.is_empty(), "got {lines:?}");
    }

    #[test]
    fn keeps_prose_that_merely_starts_with_angle_bracket() {
        // `<` followed by a non-letter is arithmetic, not a tag.
        let lines = render("<= 3 表示可讀 > 0");
        assert_eq!(lines.len(), 1);
        assert!(lines[0]
            .spans
            .iter()
            .any(|s| s.content.contains("表示可讀")));
    }

    #[test]
    fn decodes_html_entities() {
        let lines = render("a &amp; b &lt;c&gt; &quot;d&quot; &#39;e&#39;");
        let text: String = lines[0]
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(text, "a & b <c> \"d\" 'e'");
    }

    #[test]
    fn link_keeps_only_its_label() {
        let lines = render("see [scanoo-web](https://vercel.com/scanoo/very/long/url) now");
        let text: String = lines[0]
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(text, "see scanoo-web now");
    }

    #[test]
    fn image_is_dropped_including_alt_text() {
        // The `!` must be consumed with the image, not left behind.
        let lines = render("x ![Ready](https://vercel.com/static/ready.svg) y");
        let text: String = lines[0]
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(text, "x  y");
    }

    // ── table layout ──

    #[test]
    fn table_cell_parses_inline_markup() {
        let lines = render("| a | b |\n|---|---|\n| **LINE Console** | x |");
        let row = &lines[2];
        let bold = row
            .spans
            .iter()
            .find(|s| s.style.add_modifier.contains(Modifier::BOLD));
        assert!(bold.is_some(), "cell markup must be parsed: {row:?}");
        assert!(!row.spans.iter().any(|s| s.content.contains('*')));
    }

    #[test]
    fn table_never_exceeds_available_width() {
        let body = "| Project | Deployment | Updated |\n|---|---|---|\n\
                    | [scanoo-web](https://vercel.com/scanoo-projects/scanoo-web) \
                    | [Ready](https://vercel.com/scanoo/6sZmUsHXUE4ErPKEDRwvJPVq8Kq9) \
                    | Jul 27, 2026 6:12am |";
        for width in [20usize, 34, 46, 80] {
            for line in super::render(body, width) {
                assert!(
                    line.width() <= width,
                    "width {width}: line is {} wide: {line:?}",
                    line.width()
                );
            }
        }
    }

    #[test]
    fn narrow_table_keeps_short_columns_intact() {
        // Give-back allocation: the long column absorbs the squeeze, the short
        // ones still get their natural width.
        let natural = vec![4, 60, 6];
        let widths = fit_columns(&natural, 40);
        assert_eq!(widths[0], 4);
        assert_eq!(widths[2], 6);
        assert!(widths[1] > 0 && widths[1] < 60);
        assert!(widths.iter().sum::<usize>() + 2 * COL_GAP <= 40);
    }

    #[test]
    fn table_that_fits_is_left_at_natural_width() {
        let natural = vec![4, 6, 8];
        assert_eq!(fit_columns(&natural, 80), natural);
    }

    #[test]
    fn long_cell_wraps_instead_of_truncating() {
        let body = "| k | v |\n|---|---|\n| a | 這是一段很長的中文說明不應該被截斷 |";
        let lines = super::render(body, 30);
        // The cell wraps, so join across rows and drop the padding before
        // checking that nothing was cut.
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect::<String>()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(
            text.contains("不應該被截斷"),
            "content must survive: {text:?}"
        );
    }

    #[test]
    fn dropped_image_does_not_shift_its_column() {
        // Vercel writes `![Ready](icon.svg) [Ready](url)` in status cells;
        // removing the image must not leave the label indented one cell.
        let lines = render("| a | b |\n|---|---|\n| x | ![i](u.svg) Ready |");
        let row = &lines[2];
        assert!(
            row.spans.iter().any(|s| s.content.as_ref() == "Ready"),
            "cell must not keep a leading space: {row:?}"
        );
    }

    #[test]
    fn rule_line_respects_narrow_width() {
        let lines = super::render("---", 10);
        assert_eq!(lines[0].width(), 10);
    }

    /// The comment shape that motivated all of the above: a Vercel deployment
    /// bot post, at the real preview width of a 80-column terminal.
    #[test]
    fn vercel_comment_renders_without_noise() {
        let body = concat!(
            "[vc]: #bDxmwiKVhzWA8lnKsChqdkPRvpRGYhaFCXP1WC1h+T4=:eyJpc21vbml0b3JpbmciOnRydWV9\n",
            "The latest updates on your projects. ",
            "Learn more about [Vercel for GitHub](https://vercel.link/github-learn-more).\n",
            "\n",
            "| Project | Deployment | Updated (UTC) |\n",
            "| :--- | :----- | :------ |\n",
            "| [scanoo-web](https://vercel.com/scanoo-projects/scanoo-web) ",
            "| ![Ready](https://vercel.com/static/status/ready.svg) ",
            "[Ready](https://vercel.com/scanoo-projects/scanoo-web/6sZmUsHXUE4ErPKEDRwvJPVq8Kq9) ",
            "| Jul 27, 2026 6:12am |\n",
            "\n",
            r#"<a href="https://vercel.com/vercel-agent/request-review?pr=811" rel="noreferrer">"#,
            r#"<picture><source media="(prefers-color-scheme: dark)" srcset="x.svg"></picture></a>"#,
            "\n",
        );
        let width = 46; // 80-column terminal → 60% preview minus padding
        let lines = super::render(body, width);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();

        assert!(!text.contains("eyJpc21"), "base64 blob leaked: {text:?}");
        assert!(!text.contains("[vc]"), "ref definition leaked: {text:?}");
        assert!(!text.contains("<picture"), "raw HTML leaked: {text:?}");
        assert!(!text.contains("href="), "raw HTML leaked: {text:?}");
        assert!(!text.contains("https://"), "bare URL leaked: {text:?}");
        // Labels survive.
        assert!(text.contains("scanoo-web"), "got: {text:?}");
        assert!(text.contains("Vercel for GitHub"), "got: {text:?}");

        // Only table rows must fit the width: prose is wrapped by `Paragraph`
        // (word-aware), while a wrapped table row would lose its alignment.
        let table_row = lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content.contains("scanoo-web")))
            .expect("table row present");
        assert!(
            table_row.width() <= width,
            "table row is {} wide: {table_row:?}",
            table_row.width()
        );
    }
}

//! Light-weight markdown renderer for GitHub issue/PR bodies.
//!
//! Line-based, no dep on external crates. Supports: headings (1-3),
//! unordered/ordered lists, blockquote, hrule, fenced code, GFM-ish tables,
//! plus two inline patterns: `**bold**` and `` `code` ``. Link / image / HTML
//! are rendered as literal text.

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

const RULE_WIDTH: usize = 40;

pub fn render(body: &str) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let raw: Vec<&str> = body.lines().collect();
    let mut in_code = false;
    let mut i = 0;
    while i < raw.len() {
        let line = raw[i];

        // Fenced code block
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            out.push(rule_line());
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

        // Table (header + separator + rows)
        if is_table_row(line) && i + 1 < raw.len() && is_separator_row(raw[i + 1]) {
            let mut rows: Vec<Vec<String>> = Vec::new();
            rows.push(split_cells(line));
            i += 2;
            while i < raw.len() && is_table_row(raw[i]) {
                rows.push(split_cells(raw[i]));
                i += 1;
            }
            render_table(&mut out, &rows);
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
                out.push(rule_line());
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

fn rule_line() -> Line<'static> {
    Line::styled("─".repeat(RULE_WIDTH), Style::default().fg(Color::DarkGray))
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

fn render_table(out: &mut Vec<Line<'static>>, rows: &[Vec<String>]) {
    let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if cols == 0 {
        return;
    }
    let widths: Vec<usize> = (0..cols)
        .map(|c| {
            rows.iter()
                .map(|r| {
                    r.get(c)
                        .map(|s| console::measure_text_width(s))
                        .unwrap_or(0)
                })
                .max()
                .unwrap_or(0)
        })
        .collect();

    for (ri, row) in rows.iter().enumerate() {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let cell_style = if ri == 0 {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        for (c, &col_width) in widths.iter().enumerate() {
            let cell = row.get(c).map(String::as_str).unwrap_or("");
            let pad = col_width.saturating_sub(console::measure_text_width(cell));
            spans.push(Span::styled(cell.to_string(), cell_style));
            if c + 1 < cols {
                let mut sep = " ".repeat(pad);
                sep.push_str("  ");
                spans.push(Span::raw(sep));
            }
        }
        out.push(Line::from(spans));
        if ri == 0 {
            let total = widths.iter().sum::<usize>() + cols.saturating_sub(1) * 2;
            out.push(Line::styled(
                "─".repeat(total),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }
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

/// Split a line into styled spans, handling `**bold**` and `` `code` ``.
/// Non-matching text becomes a plain span. Unmatched markers stay as literal.
fn scan_inline(text: &str) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let bytes = text.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}

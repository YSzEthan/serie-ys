//! GitHub issue/PR 內文用的輕量 markdown 渲染器。
//!
//! 以行為單位處理，不依賴外部 crate。支援：標題（1-3 級）、
//! 無序／有序列表、引言區塊、水平線、fenced code、依可用寬度排版的
//! GFM 表格、`**bold**`、`` `code` ``、`[text](url)` 以及
//! HTML 實體。連結參考定義、HTML 註解與 HTML 標籤一律捨棄（標籤是行內
//! 剝除，剝完什麼都不剩的行整行丟掉）——bot 留言（Vercel、CI）充斥這些
//! 東西，而它們在終端機裡什麼都顯示不出來。`*italic*` 則沒有處理。

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::widget::{split_at_width, str_width};

const RULE_WIDTH: usize = 40;

pub fn render(body: &str, width: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let raw: Vec<&str> = body.lines().collect();
    let mut in_code = false;
    let mut in_html_comment = false;
    let mut i = 0;
    while i < raw.len() {
        let line = raw[i];

        // Fenced code 區塊
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

        // HTML 註解，用和 code fence 一樣的方式追蹤，才能跨行。
        // Vercel 每則留言開頭都會用它藏一段 base64 資料。
        // 只在行首才視為註解：行中出現註解的情況很罕見，
        // 為此把其他每個分支都弄複雜並不划算。
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

        // 表格（表頭 + 分隔線 + 內文列）
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

        // 渲染後的 markdown 看不到；Vercel 會產生一大段 base64。
        if is_link_reference_definition(line) {
            i += 1;
            continue;
        }

        let trimmed = line.trim_start();

        // 標題
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

        // 引言區塊
        if let Some(rest) = trimmed.strip_prefix("> ") {
            out.push(Line::from(vec![
                Span::styled("│ ", Style::default().fg(Color::DarkGray)),
                Span::styled(rest.to_string(), Style::default().fg(Color::Gray)),
            ]));
            i += 1;
            continue;
        }

        // 水平線
        if trimmed.starts_with("---") || trimmed.starts_with("___") || trimmed.starts_with("***") {
            let rest = trimmed.trim_matches(|c| c == '-' || c == '_' || c == '*');
            if rest.trim().is_empty() {
                out.push(rule_line(width));
                i += 1;
                continue;
            }
        }

        // 無序列表
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

        // 有序列表（`1. `、`23. `）
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

        // 一般文字行，做行內掃描。掃完一無所剩、原始行卻不是空白，代表整行
        // 都是被剝掉的標記（bot 留言常見的整行 `<a><picture>…`）或一張圖片——
        // 連空行都不要留，否則 Vercel 那種留言會被撐出一堆空隙。
        let spans = scan_inline(line);
        if spans.is_empty() && !line.trim().is_empty() {
            i += 1;
            continue;
        }
        out.push(if spans.is_empty() {
            Line::raw(String::new())
        } else {
            Line::from(spans)
        });
        i += 1;
    }
    out
}

/// 區段分隔線，寬度會被限制住，在窄預覽視窗裡也不會換行。
/// 用灰色——渲染出的 markdown 內部分隔線維持中性色調。
pub(super) fn rule_line(width: usize) -> Line<'static> {
    rule_line_colored(width, Color::DarkGray)
}

/// 顏色可由呼叫端指定的 [`rule_line`]，用於分隔預覽區段，
/// 而非 markdown 內容本身的分隔線。
pub(super) fn rule_line_colored(width: usize, color: Color) -> Line<'static> {
    let n = RULE_WIDTH.min(width);
    Line::styled("─".repeat(n), Style::default().fg(color))
}

/// `[label]: url`——渲染後的 markdown 看不到。
///
/// 連結目的地依 CommonMark 規範必須是 ASCII 且不含空白字元。
/// ASCII 這個條件和空白字元一樣重要：像「`[註]: 這是一段中文說明`」
/// 這種中日韓文字同樣不含空白，若拿掉判斷會刪掉真正的內容。
/// 帶引號標題的定義不會被辨識出來——多顯示一行無傷大雅，
/// 誤刪文字內容才是問題。
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

/// 從 `open` 所在的 `<` 開始解析一個 HTML 標籤，回傳緊接在 `>` 之後的
/// byte 索引。不是標籤則回傳 None。
///
/// 光看 `<` 不夠，要求後面緊接 ASCII 字母或 `/`：`<= 3 表示 ... > 0` 只是
/// 普通文字。另外要排掉 CommonMark autolink（`<https://x>`、`<a@b.com>`）——
/// GitHub 會把它渲染成連結，刪掉等於吞掉一整條網址。判準是「裡面沒有空白、
/// 而且有 `:` 或 `@`」：真正的標籤只要帶屬性就一定有空白（`<a href="…">`），
/// 不帶屬性的（`<sup>`、`</a>`）則兩個字元都不會出現。
///
/// 屬性值裡含 `>`、標籤跨行都不處理——GitHub 與 bot 的實際輸出都不長那樣。
///
/// 回傳緊接在 `>` 之後的 byte 索引，以及標籤名與屬性之間的原文（`inner`）——
/// 呼叫端要判斷是不是 `<br>` 時直接重用這段，不必再從頭剝一次角括號。
fn parse_html_tag(text: &str, open: usize) -> Option<(usize, &str)> {
    let rest = text.get(open + 1..)?;
    let first = rest.chars().next()?;
    if !(first.is_ascii_alphabetic() || first == '/') {
        return None;
    }
    let close = rest.find('>')?;
    let inner = &rest[..close];
    if !inner.contains(char::is_whitespace) && (inner.contains(':') || inner.contains('@')) {
        return None;
    }
    Some((open + 1 + close + 1, inner))
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

/// 欄與欄之間的間距。
const COL_GAP: usize = 2;

fn render_table(out: &mut Vec<Line<'static>>, rows: &[Vec<String>], width: usize) {
    let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if cols == 0 {
        return;
    }

    // 先把儲存格轉成 span：欄寬要以實際畫出來的內容為準來量測，
    // 而不是原始 markdown 文字。像 `[name](https://…)` 這種儲存格，
    // 渲染後只有幾個字元寬，不是上百字元。
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
        // 先把每個儲存格換行處理好，再依換行後的每一列各輸出一行。
        let wrapped: Vec<Vec<Vec<Span<'static>>>> = row
            .iter()
            .zip(&widths)
            .map(|(cell, &w)| wrap_cell(cell, w))
            .collect();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);

        for r in 0..height {
            let mut spans: Vec<Span<'static>> = Vec::new();
            for (c, (cell, &col_width)) in wrapped.iter().zip(&widths).enumerate() {
                // 換行後列數比鄰居少的儲存格不是特例——
                // 它只是在這一列不貢獻任何 span 而已。
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

/// 去除因刪除標記語法而殘留下來的前後空白。
///
/// `split_cells` 會裁掉原始文字的空白，但從 `![Ready](…) [Ready](…)`
/// 中拿掉圖片後又會多出一個開頭空白，導致該欄整個相對表頭
/// 往右偏移一格。
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

/// 以「均分＋歸還多餘額度」的方式把 `width` 分配到各欄。
///
/// 按自然寬度比例分配是錯的：超支的那一欄（例如一段裸網址）
/// 會佔走大部分空間，把資訊密度高的短欄擠到只剩兩個字元。
/// 這裡的做法是：只要某欄在均分額度內就夠用，它就只拿自己需要的，
/// 剩下的額度歸還，再重新分給還超支的欄位。
fn fit_columns(natural: &[usize], width: usize) -> Vec<usize> {
    let cols = natural.len();
    let gaps = cols.saturating_sub(1) * COL_GAP;
    let avail = width.saturating_sub(gaps);
    // 絕大多數情況：本來就放得下，什麼都不用動。
    if cols == 0 || natural.iter().sum::<usize>() <= avail {
        return natural.to_vec();
    }

    let mut out = vec![0usize; cols];
    let mut settled = vec![false; cols];
    let mut remaining = avail;
    let mut unsettled = cols;

    // 每一輪至少會確定一欄，所以一定會結束。
    while unsettled > 0 {
        let share = remaining / unsettled;
        let Some(idx) = (0..cols).find(|&c| !settled[c] && natural[c] <= share) else {
            // 沒有誰的均分額度還夠用了——把剩下的平均分掉。
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

/// 把一個儲存格的 span 依 `width` 換行，允許在字詞中間強制斷行。
///
/// 一般文字希望在詞與詞之間斷行，但這裡用強制斷行是對的：
/// 儲存格裡放的是短句和網址，而網址根本沒有空白可以斷。
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
                // 這一列剩下的空間已經放不下任何東西——開始新的一列。
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

/// `1. rest` 或 `23. rest` → (含句點的前綴, rest)。不是有序列表則回傳 None。
fn split_ordered_list(s: &str) -> Option<(&str, &str)> {
    let dot_pos = s.find('.')?;
    let (num, after) = s.split_at(dot_pos);
    if num.is_empty() || !num.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let rest = after.strip_prefix(". ")?;
    Some((&s[..=dot_pos], rest))
}

/// 把一行文字拆成帶樣式的 span，處理 `**bold**`、`` `code` ``、
/// `[text](url)`、`![alt](url)`、HTML 標籤與 HTML 實體。
/// 沒對上的文字變成一般 span。沒對上的標記符號原樣保留。
fn scan_inline(text: &str) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let bytes = text.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        // ![alt](url) ——要先於 `[` 判斷，否則 `!` 會落進 buf，
        // 圖片就退化成顯示 alt 文字的連結。
        if bytes[i] == b'!' && bytes.get(i + 1) == Some(&b'[') {
            if let Some((_, end)) = parse_link(text, i + 1) {
                i = end;
                continue;
            }
        }
        // [text](url) ——網址在 TUI 裡沒用，所以只顯示標籤文字。
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
        // 行內 HTML 標籤。Vercel 新版留言把 `<a><sup><img/></sup></a>` 塞進
        // 表格儲存格，那條路徑只會走到這裡，行首判斷攔不到。
        if bytes[i] == b'<' {
            if let Some((end, inner)) = parse_html_tag(text, i) {
                // `<br>`／`<br/>`／`<br />`：GitHub 表格儲存格靠它分行，剝掉
                // 標籤時得留一個空白，否則 `foo<br>bar` 會黏成 `foobar`。
                if inner
                    .trim_end_matches('/')
                    .trim()
                    .eq_ignore_ascii_case("br")
                {
                    buf.push(' ');
                }
                i = end;
                continue;
            }
        }
        // HTML 實體——在 bot 留言裡比原始標籤更常見。
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
        // 一次複製一個 UTF-8 scalar（不是一個 byte）到 buf，維持字串合法性
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
        0x80..=0xBF => 1, // 不合法的接續位元組——安全地往前推進
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

fn find_close(text: &str, from: usize, marker: &str) -> Option<usize> {
    text[from..].find(marker).map(|p| from + p)
}

/// 從 `open` 所在的 `[` 開始解析 `[label](dest)`。
/// 回傳標籤文字，以及緊接在結尾 `)` 之後的 byte 索引。
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

/// 解碼 GitHub 實際會用到的少數幾種 HTML 實體。
/// 回傳解碼後的字元，以及緊接在 `;` 之後的 byte 索引。
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

    /// 寬度夠大，內容不會換行——需要測試寬度的測試會直接呼叫
    /// `super::render`。這裡會遮蔽掉透過 glob import 進來的 `render`。
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
        // 第一行：項目符號 + "a"
        assert!(lines[0]
            .spans
            .iter()
            .any(|s| s.content.as_ref().contains("•")));
        // 巢狀行：開頭空白的 span
        assert!(lines[1]
            .spans
            .iter()
            .any(|s| s.content.as_ref().starts_with("  ")));
    }

    #[test]
    fn render_table_basic() {
        let lines = render("| A | B |\n|---|---|\n| 1 | 2 |");
        assert_eq!(lines.len(), 3); // 表頭、分隔線、內文列
                                    // 表頭第一個 span 要是粗體
        assert!(lines[0].spans[0]
            .style
            .add_modifier
            .contains(Modifier::BOLD));
        // 分隔線整行都是 ─
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
        // 內文列第一欄要是 `a|b`（沒有被拆開）
        let row = &lines[2];
        assert!(row.spans.iter().any(|s| s.content.as_ref() == "a|b"));
    }

    #[test]
    fn render_table_no_separator_is_plain() {
        let lines = render("| a | b |\njust text");
        // 沒有分隔線 → 兩行都當作一般文字，不是表格
        assert_eq!(lines.len(), 2);
        // 第一行應該要保留原本的 `|` 字元
        assert!(lines[0]
            .spans
            .iter()
            .any(|s| s.content.as_ref().contains('|')));
    }

    #[test]
    fn render_code_fence() {
        let lines = render("```\nlet x = 1;\nlet y = 2;\n```");
        // 分隔線 + 2 行程式碼 + 分隔線 = 4
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[1].style.fg, Some(Color::Gray));
        assert_eq!(lines[2].style.fg, Some(Color::Gray));
    }

    #[test]
    fn render_inline_bold_and_code() {
        let lines = render("**bold** and `code`");
        assert_eq!(lines.len(), 1);
        let spans = &lines[0].spans;
        // 找出粗體 span
        let bold = spans
            .iter()
            .find(|s| s.style.add_modifier.contains(Modifier::BOLD));
        assert!(bold.is_some(), "should have a bold span");
        assert_eq!(bold.unwrap().content.as_ref(), "bold");
        // 找出 code span（深色背景）
        let code = spans.iter().find(|s| s.style.bg == Some(Color::DarkGray));
        assert!(code.is_some(), "should have a code span");
        assert_eq!(code.unwrap().content.as_ref(), "code");
    }

    // ── PR／bot 留言的雜訊 ──

    #[test]
    fn drops_link_reference_definition() {
        let lines = render("[vc]: #bDxmwiKVhzWA8lnKsChqdkPRvpRGYhaFCXP1WC1h+T4=");
        assert!(lines.is_empty(), "got {lines:?}");
    }

    #[test]
    fn keeps_bracketed_prose_with_spaces_after_colon() {
        // 連結目的地不能包含空白字元，所以這是一般文字。
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
        // `<` 後面接非字母字元是算式，不是標籤。
        let lines = render("<= 3 表示可讀 > 0");
        assert_eq!(lines.len(), 1);
        assert!(lines[0]
            .spans
            .iter()
            .any(|s| s.content.contains("表示可讀")));
    }

    #[test]
    fn strips_inline_html_tags() {
        let lines = render("前<b>粗</b>後<img src=\"x.svg\" alt=\"\" />尾");
        assert_eq!(text_of(&lines[0]), "前粗後尾");
    }

    #[test]
    fn br_becomes_a_space() {
        // GitHub 的表格儲存格靠 `<br>` 分行，直接刪掉會把兩段文字黏在一起。
        for body in ["a<br>b", "a<br/>b", "a<BR />b"] {
            assert_eq!(text_of(&render(body)[0]), "a b", "{body}");
        }
    }

    #[test]
    fn keeps_autolinks() {
        // CommonMark autolink，GitHub 會渲染成連結——不是可以吃掉的標籤。
        let lines = render("see <https://example.com> and <a@b.com>");
        assert_eq!(
            text_of(&lines[0]),
            "see <https://example.com> and <a@b.com>"
        );
    }

    #[test]
    fn generic_args_are_eaten_like_github_does() {
        // `<String>` 在 GitHub 上也會被當標籤吃掉（沒加反引號的代價）。
        // 刻意跟網頁行為一致，而不是自己發明一套。
        assert_eq!(text_of(&render("Vec<String> 很長")[0]), "Vec 很長");
    }

    #[test]
    fn keeps_inline_less_than_in_prose() {
        // `<` 後面接非字母字元是算式，行中出現時跟行首一樣要留著。
        let lines = render("條件是 a <= b 而且 c > d");
        assert_eq!(text_of(&lines[0]), "條件是 a <= b 而且 c > d");
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
        // `!` 必須跟圖片一起被吃掉，不能留下來。
        let lines = render("x ![Ready](https://vercel.com/static/ready.svg) y");
        let text: String = lines[0]
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(text, "x  y");
    }

    // ── 表格排版 ──

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
        // 歸還多餘額度的分配方式：長欄吸收被壓縮的部分，
        // 短欄仍然能拿到它們的自然寬度。
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
        // 儲存格會換行，所以先把各列文字接起來、去掉補的空白，
        // 再檢查內容有沒有被截斷。
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
        // Vercel 在狀態欄裡會寫 `![Ready](icon.svg) [Ready](url)`；
        // 拿掉圖片後不能讓標籤文字多縮一格。
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

    /// 促成以上所有測試的留言型態：一則 Vercel 部署 bot 貼文，
    /// 用 80 欄終端機實際預覽時的寬度來測試。
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
        let width = 46; // 80 欄終端機 → 60% 預覽寬度扣掉留白
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
        // 標籤文字要保留下來。
        assert!(text.contains("scanoo-web"), "got: {text:?}");
        assert!(text.contains("Vercel for GitHub"), "got: {text:?}");

        // 只有表格列一定要符合寬度限制：一般文字會交給 `Paragraph`
        // 換行（會顧及字詞邊界），但表格列換行的話會破壞對齊。
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

    /// Vercel 後來改了留言格式，把專案圖示的 `<a><sup><img/></sup></a>` 直接
    /// 塞進表格儲存格。這條路徑只走 `scan_inline`，不經過任何行首判斷——
    /// 內容取自 scanoo-tw/scanoo-web#886 的實際留言。
    #[test]
    fn vercel_comment_with_inline_html_in_cells() {
        let body = concat!(
            "| Project | Deployment | Actions | Updated (UTC) |\n",
            "| :--- | :----- | :------ | :------ |\n",
            "| <a href=\"https://vercel.com/scanoo-projects/scanoo-web\"><sup>",
            "<img src=\"https://vercel.com/api/www/avatar?projectId=prj_NEmNjGM9",
            "&teamId=team_bm5Pz0v7wQhfmBkYpNmGGDpM&s=32\" width=\"16\" height=\"16\" ",
            "align=\"middle\" alt=\"\" /></sup></a> ",
            "[scanoo-web](https://vercel.com/scanoo-projects/scanoo-web) ",
            "| ![Ready](https://vercel.com/static/status/ready.svg) ",
            "[Ready](https://vercel.com/scanoo-projects/scanoo-web/ELwvvDg97wLQ) ",
            "| [Preview](https://scanoo-web-git-issue-878-scanoo-projects.vercel.app) ",
            "| Aug 9, 2026 12:24pm |\n",
        );
        let width = 46;
        let lines = super::render(body, width);
        let text: String = lines.iter().map(|l| text_of(l)).collect();

        for leaked in ["<a", "href=", "<img", "<sup", "https://", "width="] {
            assert!(!text.contains(leaked), "{leaked} leaked: {text:?}");
        }
        for kept in ["scanoo-web", "Ready", "Preview"] {
            assert!(text.contains(kept), "{kept} missing: {text:?}");
        }

        // 拿掉圖示後，第一欄不能多出一個開頭空白把整欄往右推。
        let row = lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content.contains("scanoo-web")))
            .expect("table row present");
        assert_eq!(row.spans[0].content.as_ref(), "scanoo-web", "{row:?}");
        assert!(row.width() <= width, "row is {} wide: {row:?}", row.width());
    }
}

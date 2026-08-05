use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Padding, Paragraph, Wrap},
    Frame,
};

use crate::github::{GhIssue, GhPullRequest};

use super::{
    preview::{borrow_lines, RowData},
    GitHubFocus, GitHubTab, GitHubView, LoadState,
};

/// List row cell layout: paragraph padding (1) + indicator (2).
/// Keep in sync with `render_issue_line` / `render_pr_line` — if the indicator
/// width or the Paragraph padding changes, adjust this constant too.
const LIST_LINK_COL_OFFSET: u16 = 3;

impl<'a> GitHubView<'a> {
    pub fn render(&mut self, f: &mut Frame, area: Rect, marquee_frame: u64) {
        self.height = area.height as usize;
        // Render is the single source of truth for overflow — reset at entry
        // so focuses that skip render_list (CheckboxEdit, Prompt) auto-clear.
        self.selected_row_overflows.set(false);

        // ── 三區 split：頂部 tab/prompt + 下半 list|preview ──
        let [header_area, content_area] =
            Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).areas(area);

        self.render_header(f, header_area);

        // ── Loading / 錯誤提示 ──
        if self.current_list_len() == 0 {
            let (text, color) = match &self.load_state {
                LoadState::Loading => ("Loading GitHub data...".to_string(), Color::DarkGray),
                LoadState::Error(err) => (err.clone(), Color::Red),
                LoadState::Idle => ("No items".to_string(), Color::DarkGray),
            };
            render_centered_message(f, content_area, text, color);
            return;
        }

        let [list_area, preview_area] =
            Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
                .areas(content_area);

        self.render_list(f, list_area, marquee_frame);
        self.render_preview(f, preview_area);

        // ── Flash message ──
        if let Some((ref msg, is_error)) = self.flash_message {
            let color = if is_error {
                Color::Red
            } else {
                Color::DarkGray
            };
            let flash_area = Rect::new(
                content_area.x,
                content_area.bottom().saturating_sub(1),
                content_area.width,
                1,
            );
            let flash = Paragraph::new(Line::from(Span::styled(
                msg.clone(),
                Style::default().fg(color),
            )))
            .alignment(ratatui::layout::Alignment::Center);
            f.render_widget(flash, flash_area);
        }
    }

    fn render_header(&self, f: &mut Frame, area: Rect) {
        let filter_label = self.state_filter.as_str();
        let count = self.current_list_len();
        let issues_label = format!(" Issues ({}) ", self.issues.len());
        let prs_label = format!(" PRs ({}) ", self.pull_requests.len());

        let tab_line = Line::from(vec![
            Span::styled(
                issues_label,
                if self.active_tab == GitHubTab::Issues {
                    Style::default().fg(Color::Black).bg(Color::Cyan).bold()
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ),
            Span::raw(" "),
            Span::styled(
                prs_label,
                if self.active_tab == GitHubTab::PullRequests {
                    Style::default().fg(Color::Black).bg(Color::Cyan).bold()
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ),
            Span::raw("  "),
            Span::styled(
                format!("[{filter_label}]"),
                Style::default().fg(Color::DarkGray),
            ),
            if self.has_active_filter() {
                Span::styled(
                    format!("  {count} matched"),
                    Style::default().fg(Color::DarkGray),
                )
            } else {
                Span::raw("")
            },
            if matches!(self.load_state, LoadState::Loading) {
                Span::styled("  ⟳ 重新抓取中…", Style::default().fg(Color::Yellow))
            } else {
                Span::raw("")
            },
        ]);

        // Prompt input line
        let prompt_color = if self.focus == GitHubFocus::Prompt {
            Color::Cyan
        } else {
            Color::DarkGray
        };
        let prompt_prefix = Span::styled("> ", Style::default().fg(prompt_color));
        let prompt_value = Span::raw(self.search_input.value().to_string());
        let prompt_line = Line::from(vec![
            Span::raw("  "), // left padding
            prompt_prefix,
            prompt_value,
        ]);

        let [tab_area, prompt_area] =
            Layout::vertical([Constraint::Length(2), Constraint::Length(1)]).areas(area);

        f.render_widget(
            Paragraph::new(tab_line).block(Block::default().padding(Padding::new(2, 2, 1, 0))),
            tab_area,
        );

        f.render_widget(Paragraph::new(prompt_line), prompt_area);

        // Show cursor in prompt when focused
        if self.focus == GitHubFocus::Prompt {
            let cursor_x = prompt_area.x + 2 /* pad */ + 2 /* "> " */ + self.search_input.visual_cursor() as u16;
            f.set_cursor_position((cursor_x, prompt_area.y));
        }
    }

    fn render_list(&self, f: &mut Frame, area: Rect, marquee_frame: u64) {
        // 內層 Paragraph 的 Padding 不覆蓋最左欄，會留下 list view text-mode
        // graph 的字元；先 Clear 整個 list area 擋住殘留。
        f.render_widget(Clear, area);

        let list_border_color = if self.focus == GitHubFocus::List {
            Color::Cyan
        } else {
            Color::DarkGray
        };
        let block = Block::default()
            .borders(Borders::RIGHT)
            .border_style(Style::default().fg(list_border_color));
        let inner = block.inner(area);
        f.render_widget(block, area);

        let has_next = self.current_has_next_cursor();
        // Reserve one row for the load-more indicator when there's a next page
        let visible_height = if has_next {
            (inner.height as usize).saturating_sub(1)
        } else {
            inner.height as usize
        };

        let rows = self.current_viewport_rows(visible_height, inner.width, marquee_frame);
        let mut lines: Vec<Line<'static>> = rows.iter().map(|r| r.line.clone()).collect();

        if has_next {
            let hint = if self.loading_more {
                " Loading more…"
            } else {
                " ↓ more"
            };
            lines.push(Line::styled(hint, Style::default().fg(Color::DarkGray)));
        }

        let list_paragraph =
            Paragraph::new(lines).block(Block::default().padding(Padding::horizontal(1)));
        f.render_widget(list_paragraph, inner);

        // OSC 8 overlay on `#N` for each visible row. Cell layout lives in
        // `LIST_LINK_COL_OFFSET` — keep in sync with the indicator + padding.
        // tmux DCS passthrough loses cursor positioning, so the host terminal
        // renders the label at an arbitrary column — skip overlay inside tmux.
        if crate::external::is_tmux() {
            return;
        }
        let buf = f.buffer_mut();
        let x = inner.left().saturating_add(LIST_LINK_COL_OFFSET);
        if x >= inner.right() {
            return;
        }
        let remaining = inner.right() - x;
        for (i, row) in rows.iter().enumerate() {
            if row.url.is_empty() {
                continue;
            }
            let y = inner.top() + i as u16;
            if y >= inner.bottom() {
                break;
            }
            let label = format!("#{}", row.number);
            let label_width = console::measure_text_width(&label) as u16;
            // Too narrow to fit the whole `#N` — skip overlay (partial hyperlink is worse than none)
            if label_width > remaining {
                continue;
            }
            let payload = crate::external::format_osc8_hyperlink(&row.url, &label);
            buf[(x, y)].set_symbol(&payload);
            for j in 1..label_width {
                buf[(x + j, y)].set_skip(true);
            }
        }
    }

    fn labels_pad_width_for_tab(&self) -> usize {
        match self.active_tab {
            GitHubTab::Issues => self
                .issues
                .iter()
                .map(|i| labels_display_width(&i.labels))
                .max()
                .unwrap_or(0),
            GitHubTab::PullRequests => self
                .pull_requests
                .iter()
                .map(|p| labels_display_width(&p.labels))
                .max()
                .unwrap_or(0),
        }
    }

    fn current_viewport_rows(
        &self,
        visible_height: usize,
        inner_width: u16,
        marquee_frame: u64,
    ) -> Vec<RowData> {
        let pad = self.labels_pad_width_for_tab();
        // Paragraph has Padding::horizontal(1) inside → inner content width is -2.
        let content_width = inner_width.saturating_sub(2) as usize;
        let mut rows = Vec::with_capacity(visible_height);
        let mut overflow = false;

        let make_issue = |issue: &GhIssue, vis_i: usize| -> (RowData, bool) {
            let is_selected = vis_i == self.selected_index;
            let frame = is_selected.then_some(marquee_frame);
            let (line, did_scroll) =
                render_issue_line(issue, is_selected, pad, content_width, frame);
            (
                RowData {
                    line,
                    url: issue.url.clone(),
                    number: issue.number,
                },
                did_scroll,
            )
        };
        let make_pr = |pr: &GhPullRequest, vis_i: usize| -> (RowData, bool) {
            let is_selected = vis_i == self.selected_index;
            let frame = is_selected.then_some(marquee_frame);
            let (line, did_scroll) = render_pr_line(pr, is_selected, pad, content_width, frame);
            (
                RowData {
                    line,
                    url: pr.url.clone(),
                    number: pr.number,
                },
                did_scroll,
            )
        };

        if !self.has_active_filter() {
            match self.active_tab {
                GitHubTab::Issues => {
                    for (i, issue) in self
                        .issues
                        .iter()
                        .enumerate()
                        .skip(self.offset)
                        .take(visible_height)
                    {
                        let (row, ovf) = make_issue(issue, i);
                        overflow |= ovf;
                        rows.push(row);
                    }
                }
                GitHubTab::PullRequests => {
                    for (i, pr) in self
                        .pull_requests
                        .iter()
                        .enumerate()
                        .skip(self.offset)
                        .take(visible_height)
                    {
                        let (row, ovf) = make_pr(pr, i);
                        overflow |= ovf;
                        rows.push(row);
                    }
                }
            }
        } else {
            let indices = self.current_filtered_indices();
            match self.active_tab {
                GitHubTab::Issues => {
                    for (vis_i, &data_i) in indices
                        .iter()
                        .enumerate()
                        .skip(self.offset)
                        .take(visible_height)
                    {
                        let (row, ovf) = make_issue(&self.issues[data_i], vis_i);
                        overflow |= ovf;
                        rows.push(row);
                    }
                }
                GitHubTab::PullRequests => {
                    for (vis_i, &data_i) in indices
                        .iter()
                        .enumerate()
                        .skip(self.offset)
                        .take(visible_height)
                    {
                        let (row, ovf) = make_pr(&self.pull_requests[data_i], vis_i);
                        overflow |= ovf;
                        rows.push(row);
                    }
                }
            }
        }
        self.selected_row_overflows.set(overflow);
        rows
    }

    fn render_preview(&mut self, f: &mut Frame, area: Rect) {
        if self.focus == GitHubFocus::CheckboxEdit {
            self.render_checkbox_preview(f, area);
            return;
        }

        let block = Block::default().padding(Padding::horizontal(1));
        let inner = block.inner(area);
        f.render_widget(block, area);

        // Render is the source of truth for the preview's usable height; the
        // scroll handlers read it back rather than re-deriving it from `height`.
        self.preview_height = inner.height as usize;

        // `preview_input(&self)` 借走整個 self，borrow checker 看不出它沒碰
        // preview_cache，所以無法同時 &mut 它。先剝離成 local 讓兩個借用
        // 不重疊，順序不能反過來。還原要留在下面幾個 early return 之前——
        // 挪到函式尾端的話，return 會把 cache 丟掉，preview 靜默變空白。
        let mut cache = std::mem::take(&mut self.preview_cache);
        let visual_len = cache.get_or_build(&self.preview_input(inner.width));
        self.preview_cache = cache;

        self.last_preview_len = visual_len;
        // Clamp preview_offset to avoid scrolling past content. Both sides are
        // visual (post-wrap) lines — `Paragraph::scroll` skips wrapped lines,
        // not source lines. The `u16` bound belongs here too, so state and
        // screen cannot disagree about where the bottom is.
        let max_offset = visual_len
            .saturating_sub(inner.height as usize)
            .min(u16::MAX as usize);
        self.preview_offset = self.preview_offset.min(max_offset);
        let scroll = self.preview_offset as u16;

        let paragraph = Paragraph::new(borrow_lines(self.preview_cache.lines()))
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0));
        f.render_widget(paragraph, inner);

        // Overlay `#N` cells with OSC 8 hyperlinks. Must run after Paragraph
        // render so we overwrite the pre-drawn plain `#N` glyph.
        // tmux DCS passthrough loses cursor positioning, so the host terminal
        // renders the label at an arbitrary column — skip overlay inside tmux.
        if crate::external::is_tmux() {
            return;
        }
        // The header's position is only trustworthy before scrolling: the
        // scroll offset counts wrapped lines, not source lines, so the header
        // itself scrolls off screen the moment `scroll != 0`. Restoring a
        // hyperlink after that needs links attached to spans rather than a
        // stored coordinate.
        if scroll != 0 || inner.height == 0 {
            return;
        }
        let Some(ov) = self.preview_cache.overlay() else {
            return;
        };
        let (x, y) = (inner.left(), inner.top());
        if x >= inner.right() {
            return;
        }
        let payload = crate::external::format_osc8_hyperlink(&ov.url, &ov.label);
        let label_width = console::measure_text_width(&ov.label) as u16;
        let buf = f.buffer_mut();
        buf[(x, y)].set_symbol(&payload);
        let remaining = inner.right() - x;
        for i in 1..label_width.min(remaining) {
            buf[(x + i, y)].set_skip(true);
        }
    }

    fn render_checkbox_preview(&self, f: &mut Frame, area: Rect) {
        let Some(ref panel) = self.task_panel else {
            return;
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow))
            .title(" Tasks (editing) ")
            .padding(Padding::horizontal(1));
        let inner = block.inner(area);
        f.render_widget(block, area);

        // Available height minus footer line
        let content_height = inner.height.saturating_sub(1) as usize;

        // Scroll offset for long task lists
        let offset = if panel.selected >= content_height {
            panel.selected - content_height + 1
        } else {
            0
        };

        let mut lines: Vec<Line> = panel
            .items
            .iter()
            .enumerate()
            .skip(offset)
            .take(content_height)
            .map(|(i, item)| {
                let selected = i == panel.selected;
                let indicator = if selected { "▸ " } else { "  " };
                let checkbox = if item.checked { "☑ " } else { "☐ " };
                let checkbox_color = if item.checked {
                    Color::Green
                } else {
                    Color::DarkGray
                };
                let label_style = if selected {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };

                Line::from(vec![
                    Span::styled(indicator.to_string(), label_style),
                    Span::styled(checkbox.to_string(), Style::default().fg(checkbox_color)),
                    Span::styled(item.label.clone(), label_style),
                ])
            })
            .collect();

        // Footer
        lines.push(Line::from(Span::styled(
            " h/l:toggle  Enter:submit  Esc:cancel",
            Style::default().fg(Color::DarkGray),
        )));

        let paragraph = Paragraph::new(lines);
        f.render_widget(paragraph, inner);
    }
}

// ── 渲染輔助函數 ──

fn render_centered_message(f: &mut Frame, list_area: Rect, text: String, color: Color) {
    let msg = Paragraph::new(Line::from(Span::styled(text, Style::default().fg(color))))
        .alignment(ratatui::layout::Alignment::Center)
        .block(Block::default().padding(Padding::vertical(list_area.height.saturating_sub(1) / 2)));
    f.render_widget(msg, list_area);
}

fn hex_to_color(hex: &str) -> Color {
    let hex = hex.trim_start_matches('#');
    if hex.len() == 6 {
        let r = u8::from_str_radix(&hex[0..2], 16);
        let g = u8::from_str_radix(&hex[2..4], 16);
        let b = u8::from_str_radix(&hex[4..6], 16);
        if let (Ok(r), Ok(g), Ok(b)) = (r, g, b) {
            return Color::Rgb(r, g, b);
        }
    }
    Color::Yellow
}

pub(super) fn state_color(state: &str) -> Color {
    match state {
        "OPEN" => Color::Green,
        "CLOSED" => Color::Red,
        "MERGED" => Color::Magenta,
        _ => Color::Gray,
    }
}

pub(super) fn label_spans(labels: &[crate::github::GhLabel]) -> Vec<Span<'static>> {
    if labels.is_empty() {
        return vec![];
    }
    let mut spans = vec![Span::raw(" [")];
    for (i, label) in labels.iter().enumerate() {
        let color = label
            .color
            .as_deref()
            .map(hex_to_color)
            .unwrap_or(Color::Yellow);
        spans.push(Span::styled(label.name.clone(), Style::default().fg(color)));
        if i < labels.len() - 1 {
            spans.push(Span::raw(", "));
        }
    }
    spans.push(Span::raw("]"));
    spans
}

/// Returns `(line, scrolled)`. `scrolled=true` means the title+author tail
/// got a marquee treatment due to overflow — caller keeps the ticker alive.
fn render_issue_line(
    issue: &GhIssue,
    selected: bool,
    labels_pad_width: usize,
    content_width: usize,
    marquee_frame: Option<u64>,
) -> (Line<'static>, bool) {
    let indicator = if selected { "▸ " } else { "  " };
    let state_color = match issue.state.as_str() {
        "OPEN" => Color::Green,
        "CLOSED" => Color::Red,
        _ => Color::Gray,
    };
    let style = if selected {
        Style::default().fg(Color::Cyan).bold()
    } else {
        Style::default()
    };

    let mut spans = vec![
        Span::styled(indicator.to_string(), style),
        Span::styled(
            format!("#{:<5} ", issue.number),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("{:<6}", issue.state.to_lowercase()),
            Style::default().fg(state_color),
        ),
    ];
    spans.extend(label_spans(&issue.labels));
    let used = labels_display_width(&issue.labels);
    if labels_pad_width > used {
        spans.push(Span::raw(" ".repeat(labels_pad_width - used)));
    }
    spans.push(Span::raw(" "));

    let tail = format!("{}  @{}", issue.title, issue.author.login);
    // 2 (indicator) + 7 (#N block `#XXXXX `) + 6 (state `{:<6}`) + labels_pad + 1 (space)
    let prefix_width = 2 + 7 + 6 + labels_pad_width + 1;
    let (tail_spans, scrolled) = tail_spans(
        &tail,
        content_width.saturating_sub(prefix_width),
        marquee_frame,
        style,
    );
    spans.extend(tail_spans);
    (Line::from(spans), scrolled)
}

/// Render `title  @author` (or similar) either truncated/untouched when not
/// overflowing, or scrolled via marquee when selected + overflow + frame.
fn tail_spans(
    tail: &str,
    available: usize,
    marquee_frame: Option<u64>,
    style_title: Style,
) -> (Vec<Span<'static>>, bool) {
    // 寬度基準必須跟下面的 `scroll_window` 一致，理由見 `marquee::display_width`。
    let tail_width = crate::widget::marquee::display_width(tail);
    if available == 0 {
        return (vec![], false);
    }
    if tail_width > available {
        if let Some(frame) = marquee_frame {
            let slice = crate::widget::marquee::scroll_window(tail, available, frame);
            return (vec![Span::styled(slice.text, style_title)], true);
        }
        // Non-selected overflow row: truncate with ellipsis
        let truncated = console::truncate_str(tail, available, "…").to_string();
        return (vec![Span::styled(truncated, style_title)], false);
    }
    (vec![Span::styled(tail.to_string(), style_title)], false)
}

fn render_pr_line(
    pr: &GhPullRequest,
    selected: bool,
    labels_pad_width: usize,
    content_width: usize,
    marquee_frame: Option<u64>,
) -> (Line<'static>, bool) {
    let indicator = if selected { "▸ " } else { "  " };
    let (state_color, state_label) = if pr.is_draft {
        (Color::Gray, "draft".to_string())
    } else {
        let color = match pr.state.as_str() {
            "OPEN" => Color::Green,
            "CLOSED" => Color::Red,
            "MERGED" => Color::Magenta,
            _ => Color::Gray,
        };
        (color, pr.state.to_lowercase())
    };
    let style = if selected {
        Style::default().fg(Color::Cyan).bold()
    } else {
        Style::default()
    };

    let mut spans = vec![
        Span::styled(indicator.to_string(), style),
        Span::styled(
            format!("#{:<5} ", pr.number),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("{state_label:<6}"),
            Style::default().fg(state_color),
        ),
    ];
    spans.extend(label_spans(&pr.labels));
    let used = labels_display_width(&pr.labels);
    if labels_pad_width > used {
        spans.push(Span::raw(" ".repeat(labels_pad_width - used)));
    }
    spans.push(Span::raw(" "));

    let tail = format!("{}  ← {}  @{}", pr.title, pr.head_ref_name, pr.author.login);
    let prefix_width = 2 + 7 + 6 + labels_pad_width + 1;
    let (tail_spans, scrolled) = tail_spans(
        &tail,
        content_width.saturating_sub(prefix_width),
        marquee_frame,
        style,
    );
    spans.extend(tail_spans);
    (Line::from(spans), scrolled)
}

/// Sum of the visible cells occupied by `label_spans(labels)`: `" [a, b]"`.
fn labels_display_width(labels: &[crate::github::GhLabel]) -> usize {
    if labels.is_empty() {
        return 0;
    }
    let names: usize = labels
        .iter()
        .map(|l| console::measure_text_width(&l.name))
        .sum();
    let seps = labels.len().saturating_sub(1) * 2; // ", "
                                                   // " [" + names + seps + "]"
    3 + names + seps
}

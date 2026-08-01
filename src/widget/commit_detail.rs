use std::rc::Rc;

use chrono::{DateTime, FixedOffset};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    symbols::border,
    text::{Line, Span},
    widgets::{Block, Borders, Padding, Paragraph, StatefulWidget, Widget},
};
use unicode_width::UnicodeWidthChar;

use crate::{
    app::AppContext,
    color::ColorTheme,
    git::{Commit, FileChange, Ref, WorkingChanges},
    graph::GlyphSet,
};

const ICON_FILE: &str = "\u{f0214} ";
const ICON_FOLDER: &str = "\u{f0770} ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailPane {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy)]
enum LineMode {
    /// Render with marquee + body wrap at this width.
    Render(usize),
    /// Measure logical line count: no marquee scroll, no body wrap.
    Measure,
}

#[derive(Debug, Default)]
pub struct CommitDetailState {
    left_offset: usize,
    right_offset: usize,
    active_pane: Option<DetailPane>,
    /// 上次 render 時 subject 是否超過 marquee 可用寬度。App tick 迴圈讀這個
    /// 決定要不要繼續推進 marquee_frame。
    subject_overflows: std::cell::Cell<bool>,
}

impl CommitDetailState {
    pub fn scroll_down(&mut self) {
        match self.active_pane() {
            DetailPane::Left => self.left_offset = self.left_offset.saturating_add(1),
            DetailPane::Right => self.right_offset = self.right_offset.saturating_add(1),
        }
    }

    pub fn scroll_up(&mut self) {
        match self.active_pane() {
            DetailPane::Left => self.left_offset = self.left_offset.saturating_sub(1),
            DetailPane::Right => self.right_offset = self.right_offset.saturating_sub(1),
        }
    }

    pub fn select_first(&mut self) {
        self.left_offset = 0;
        self.right_offset = 0;
    }

    fn clamp_offsets(&mut self, left_len: usize, right_len: usize, inner_height: usize) {
        self.left_offset = self.left_offset.min(left_len.saturating_sub(inner_height));
        self.right_offset = self
            .right_offset
            .min(right_len.saturating_sub(inner_height));
    }

    pub fn active_pane(&self) -> DetailPane {
        self.active_pane.unwrap_or(DetailPane::Left)
    }

    pub fn toggle_pane(&mut self) {
        self.active_pane = Some(match self.active_pane() {
            DetailPane::Left => DetailPane::Right,
            DetailPane::Right => DetailPane::Left,
        });
    }

    pub fn subject_overflows(&self) -> bool {
        self.subject_overflows.get()
    }
}

pub struct CommitDetail<'a> {
    commit: &'a Commit,
    changes: &'a Vec<FileChange>,
    refs: &'a Vec<Ref>,
    ctx: Rc<AppContext>,
    marquee_frame: u64,
}

impl<'a> CommitDetail<'a> {
    pub fn new(
        commit: &'a Commit,
        changes: &'a Vec<FileChange>,
        refs: &'a Vec<Ref>,
        ctx: Rc<AppContext>,
        marquee_frame: u64,
    ) -> Self {
        Self {
            commit,
            changes,
            refs,
            ctx,
            marquee_frame,
        }
    }
}

impl StatefulWidget for CommitDetail<'_> {
    type State = CommitDetailState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let [left_area, divider_area, right_area] = Layout::horizontal([
            Constraint::Percentage(50),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .areas(area);

        let active = state.active_pane();
        let left_active = active == DetailPane::Left;
        let right_active = active == DetailPane::Right;

        let available = left_area.width.saturating_sub(2) as usize;
        let right_available = right_area.width.saturating_sub(2) as usize;
        // 寬度基準必須跟下面的 `scroll_window` 一致，理由見 `marquee::display_width`。
        state
            .subject_overflows
            .set(crate::widget::marquee::display_width(&self.commit.subject) > available);
        let left_lines = self.info_lines(LineMode::Render(available));
        let right_lines: Vec<Line> = self
            .changes_lines()
            .into_iter()
            .flat_map(|l| wrap_line_spans(l, right_available))
            .collect();

        let glyphs = GlyphSet::from_style(self.ctx.graph_style);
        let block = detail_block(self.ctx.color_theme.divider_fg, glyphs);
        let inner_h = block.inner(area).height as usize;
        state.clamp_offsets(left_lines.len(), right_lines.len(), inner_h);

        let left_lines: Vec<Line> = left_lines.into_iter().skip(state.left_offset).collect();
        let right_lines: Vec<Line> = right_lines.into_iter().skip(state.right_offset).collect();
        let left_lines = if left_active {
            left_lines
        } else {
            dim_lines(left_lines)
        };
        let right_lines = if right_active {
            right_lines
        } else {
            dim_lines(right_lines)
        };

        let left_paragraph = Paragraph::new(left_lines)
            .style(Style::default().fg(self.ctx.color_theme.fg))
            .block(block.clone());
        left_paragraph.render(left_area, buf);

        // Render vertical divider
        render_vertical_divider(divider_area, buf, self.ctx.color_theme.divider_fg, glyphs);

        let right_paragraph = Paragraph::new(right_lines)
            .style(Style::default().fg(self.ctx.color_theme.fg))
            .block(block);
        right_paragraph.render(right_area, buf);
    }
}

impl CommitDetail<'_> {
    pub fn content_height(&self) -> u16 {
        let left = self.info_lines(LineMode::Measure).len();
        let right = self.changes_lines().len();
        (left.max(right) + 2) as u16 // +2 for top/bottom borders
    }

    fn info_lines(&self, mode: LineMode) -> Vec<Line<'_>> {
        fn push_wrapped<'a>(lines: &mut Vec<Line<'a>>, line: Line<'a>, wrap_at: Option<usize>) {
            match wrap_at {
                Some(w) => lines.extend(wrap_line_spans(line, w)),
                None => lines.push(line),
            }
        }

        let (marquee_width, wrap_at) = match mode {
            LineMode::Render(w) => (w, Some(w)),
            LineMode::Measure => (usize::MAX, None),
        };
        let mut lines: Vec<Line> = Vec::new();

        // Author
        push_wrapped(
            &mut lines,
            Line::from(vec![
                Span::styled(
                    "Author: ",
                    Style::default().fg(self.ctx.color_theme.detail_label_fg),
                ),
                self.commit
                    .author_name
                    .as_str()
                    .fg(self.ctx.color_theme.detail_name_fg),
                " <".into(),
                self.commit
                    .author_email
                    .as_str()
                    .fg(self.ctx.color_theme.detail_email_fg),
                ">".into(),
            ]),
            wrap_at,
        );
        push_wrapped(
            &mut lines,
            Line::from(vec![
                Span::raw("        "),
                Span::styled(
                    self.format_date(&self.commit.author_date),
                    Style::default().fg(self.ctx.color_theme.detail_date_fg),
                ),
            ]),
            wrap_at,
        );

        if is_author_committer_different(self.commit) {
            push_wrapped(
                &mut lines,
                Line::from(vec![
                    Span::styled(
                        "Committer: ",
                        Style::default().fg(self.ctx.color_theme.detail_label_fg),
                    ),
                    self.commit
                        .committer_name
                        .as_str()
                        .fg(self.ctx.color_theme.detail_name_fg),
                    " <".into(),
                    self.commit
                        .committer_email
                        .as_str()
                        .fg(self.ctx.color_theme.detail_email_fg),
                    ">".into(),
                ]),
                wrap_at,
            );
            push_wrapped(
                &mut lines,
                Line::from(vec![
                    Span::raw("           "),
                    Span::styled(
                        self.format_date(&self.commit.committer_date),
                        Style::default().fg(self.ctx.color_theme.detail_date_fg),
                    ),
                ]),
                wrap_at,
            );
        }

        // SHA
        push_wrapped(
            &mut lines,
            Line::from(vec![
                Span::styled(
                    "Commit: ",
                    Style::default().fg(self.ctx.color_theme.detail_label_fg),
                ),
                self.commit
                    .commit_hash
                    .as_str()
                    .fg(self.ctx.color_theme.detail_hash_fg),
            ]),
            wrap_at,
        );

        // Parents
        if has_parent(self.commit) {
            let mut spans: Vec<Span> = vec![Span::styled(
                "Parents: ",
                Style::default().fg(self.ctx.color_theme.detail_label_fg),
            )];
            let parents = &self.commit.parent_commit_hashes;
            for (i, hash) in parents.iter().enumerate() {
                spans.push(hash.as_short_hash().fg(self.ctx.color_theme.detail_hash_fg));
                if i < parents.len() - 1 {
                    spans.push(Span::raw(" "));
                }
            }
            push_wrapped(&mut lines, Line::from(spans), wrap_at);
        }

        // Refs
        if has_refs(self.refs) {
            push_wrapped(
                &mut lines,
                Line::from(vec![
                    Span::styled(
                        "Refs: ",
                        Style::default().fg(self.ctx.color_theme.detail_label_fg),
                    ),
                    self.refs_span(),
                ]),
                wrap_at,
            );
        }

        // Divider + commit message. Subject is marquee-trimmed to `marquee_width`,
        // so wrapping is a no-op; push raw to avoid the wrap-vs-no-wrap branch.
        lines.push(Line::raw(""));
        let subject_slice = crate::widget::marquee::scroll_window(
            &self.commit.subject,
            marquee_width,
            self.marquee_frame,
        );
        lines.push(Line::from(Span::raw(subject_slice.text).bold()));

        if !self.commit.body.is_empty() {
            lines.push(Line::raw(""));
            for body_line in self.commit.body.lines() {
                match wrap_at {
                    Some(w) => lines.extend(wrap_to_width(body_line, w).into_iter().map(Line::raw)),
                    None => lines.push(Line::raw(body_line)),
                }
            }
        }

        lines
    }

    fn format_date(&self, date: &DateTime<FixedOffset>) -> String {
        if self.ctx.ui_config.detail.date_local {
            let local = date.with_timezone(&chrono::Local);
            local
                .format(&self.ctx.ui_config.detail.date_format)
                .to_string()
        } else {
            date.format(&self.ctx.ui_config.detail.date_format)
                .to_string()
        }
    }

    fn refs_span(&self) -> Span<'_> {
        let names: Vec<String> = self
            .refs
            .iter()
            .filter_map(|r| match r {
                Ref::Branch { name, .. } => Some(name.clone()),
                Ref::RemoteBranch { name, .. } => Some(name.clone()),
                Ref::Tag { name, .. } => Some(name.clone()),
                Ref::Stash { .. } => None,
            })
            .collect();
        Span::styled(
            names.join(", "),
            Style::default()
                .fg(self.ctx.color_theme.detail_ref_branch_fg)
                .add_modifier(Modifier::BOLD),
        )
    }

    fn changes_lines(&self) -> Vec<Line<'_>> {
        build_tree_lines(self.changes, &self.ctx.color_theme)
    }
}

fn dim_color(color: Color) -> Color {
    match color {
        Color::Rgb(r, g, b) => {
            // Blend toward dark pink: average with (140,110,120) then darken
            let r = ((r as u16 + 140) / 3) as u8;
            let g = ((g as u16 + 110) / 3) as u8;
            let b = ((b as u16 + 120) / 3) as u8;
            Color::Rgb(r, g, b)
        }
        Color::Red => Color::Rgb(140, 90, 100),
        Color::Green => Color::Rgb(100, 130, 110),
        Color::Blue => Color::Rgb(100, 100, 140),
        Color::Yellow => Color::Rgb(140, 130, 100),
        Color::Cyan => Color::Rgb(100, 130, 140),
        Color::Magenta => Color::Rgb(130, 100, 130),
        Color::White | Color::Reset => Color::Rgb(130, 115, 120),
        Color::Gray => Color::Rgb(110, 100, 105),
        Color::DarkGray => Color::Rgb(85, 78, 82),
        other => other,
    }
}

fn dim_lines(lines: Vec<Line<'_>>) -> Vec<Line<'_>> {
    lines
        .into_iter()
        .map(|line| {
            let spans: Vec<Span> = line
                .spans
                .into_iter()
                .map(|span| {
                    let mut style = span.style;
                    if let Some(fg) = style.fg {
                        style.fg = Some(dim_color(fg));
                    } else {
                        style.fg = Some(Color::DarkGray);
                    }
                    Span::styled(span.content, style)
                })
                .collect();
            Line::from(spans)
        })
        .collect()
}

fn render_vertical_divider(area: Rect, buf: &mut Buffer, fg: Color, glyphs: GlyphSet) {
    let style = Style::default().fg(fg);
    for y in area.top()..area.bottom() {
        buf[(area.left(), y)]
            .set_symbol(glyphs.vert)
            .set_style(style);
    }
}

fn detail_block(divider_fg: Color, glyphs: GlyphSet) -> Block<'static> {
    Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_set(border::Set {
            horizontal_top: glyphs.horiz,
            horizontal_bottom: glyphs.horiz,
            ..border::PLAIN
        })
        .style(Style::default().fg(divider_fg))
        .padding(Padding::new(1, 1, 0, 0))
}

fn is_author_committer_different(commit: &Commit) -> bool {
    commit.author_name != commit.committer_name
        || commit.author_email != commit.committer_email
        || commit.author_date != commit.committer_date
}

fn has_parent(commit: &Commit) -> bool {
    !commit.parent_commit_hashes.is_empty()
}

fn has_refs(refs: &[Ref]) -> bool {
    refs.iter().any(|r| {
        matches!(
            r,
            Ref::Branch { .. } | Ref::RemoteBranch { .. } | Ref::Tag { .. }
        )
    })
}

pub struct WorkingChangesDetail<'a> {
    working_changes: &'a WorkingChanges,
    ctx: Rc<AppContext>,
}

impl<'a> WorkingChangesDetail<'a> {
    pub fn new(working_changes: &'a WorkingChanges, ctx: Rc<AppContext>) -> Self {
        Self {
            working_changes,
            ctx,
        }
    }
}

impl StatefulWidget for WorkingChangesDetail<'_> {
    type State = CommitDetailState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let [left_area, divider_area, right_area] = Layout::horizontal([
            Constraint::Percentage(50),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .areas(area);

        let active = state.active_pane();
        let left_active = active == DetailPane::Left;
        let right_active = active == DetailPane::Right;

        let right_available = right_area.width.saturating_sub(2) as usize;
        let left_lines = self.info_lines();
        let right_lines: Vec<Line> = self
            .file_lines()
            .into_iter()
            .flat_map(|l| wrap_line_spans(l, right_available))
            .collect();

        let glyphs = GlyphSet::from_style(self.ctx.graph_style);
        let block = detail_block(self.ctx.color_theme.divider_fg, glyphs);
        let inner_h = block.inner(area).height as usize;
        state.clamp_offsets(left_lines.len(), right_lines.len(), inner_h);

        let left_lines: Vec<Line> = left_lines.into_iter().skip(state.left_offset).collect();
        let right_lines: Vec<Line> = right_lines.into_iter().skip(state.right_offset).collect();
        let left_lines = if left_active {
            left_lines
        } else {
            dim_lines(left_lines)
        };
        let right_lines = if right_active {
            right_lines
        } else {
            dim_lines(right_lines)
        };

        let left_paragraph = Paragraph::new(left_lines)
            .style(Style::default().fg(self.ctx.color_theme.fg))
            .block(block.clone());
        left_paragraph.render(left_area, buf);

        // Render vertical divider
        render_vertical_divider(divider_area, buf, self.ctx.color_theme.divider_fg, glyphs);

        let right_paragraph = Paragraph::new(right_lines)
            .style(Style::default().fg(self.ctx.color_theme.fg))
            .block(block);
        right_paragraph.render(right_area, buf);
    }
}

impl WorkingChangesDetail<'_> {
    pub fn content_height(&self) -> u16 {
        let left = self.info_lines().len();
        let right = self.file_lines().len();
        (left.max(right) + 2) as u16 // +2 for top/bottom borders
    }

    fn info_lines(&self) -> Vec<Line<'_>> {
        let mut lines: Vec<Line> = Vec::new();

        lines.push(
            Line::from("Uncommitted Changes")
                .style(Style::default().fg(self.ctx.color_theme.fg).bold()),
        );
        lines.push(Line::raw(""));

        if !self.working_changes.staged.is_empty() {
            lines.push(
                Line::from(format!(
                    "Staged Changes ({})",
                    self.working_changes.staged.len()
                ))
                .style(Style::default().fg(self.ctx.color_theme.fg).bold()),
            );
        }

        if !self.working_changes.unstaged.is_empty() {
            lines.push(
                Line::from(format!(
                    "Unstaged Changes ({})",
                    self.working_changes.unstaged.len()
                ))
                .style(Style::default().fg(self.ctx.color_theme.fg).bold()),
            );
        }

        lines
    }

    fn file_lines(&self) -> Vec<Line<'_>> {
        let mut lines: Vec<Line> = Vec::new();

        if !self.working_changes.staged.is_empty() {
            lines.push(
                Line::from("Staged:").style(Style::default().fg(self.ctx.color_theme.fg).bold()),
            );
            lines.extend(build_tree_lines(
                &self.working_changes.staged,
                &self.ctx.color_theme,
            ));
            lines.push(Line::raw(""));
        }

        if !self.working_changes.unstaged.is_empty() {
            lines.push(
                Line::from("Unstaged:").style(Style::default().fg(self.ctx.color_theme.fg).bold()),
            );
            lines.extend(build_tree_lines(
                &self.working_changes.unstaged,
                &self.ctx.color_theme,
            ));
        }

        lines
    }
}

struct FileTreeNode<'a> {
    name: String,
    change: Option<&'a FileChange>,
    children: Vec<FileTreeNode<'a>>,
}

fn build_file_tree<'a>(changes: &'a [FileChange]) -> Vec<FileTreeNode<'a>> {
    let mut root: Vec<FileTreeNode<'a>> = Vec::new();

    for change in changes {
        let path = change.path();
        let parts: Vec<&str> = path.split('/').collect();
        insert_into_tree(&mut root, &parts, change);
    }

    collapse_single_dirs(&mut root);
    sort_tree(&mut root);

    root
}

fn insert_into_tree<'a>(nodes: &mut Vec<FileTreeNode<'a>>, parts: &[&str], change: &'a FileChange) {
    if parts.len() == 1 {
        // Leaf file node
        nodes.push(FileTreeNode {
            name: parts[0].to_string(),
            change: Some(change),
            children: Vec::new(),
        });
        return;
    }

    // Find or create directory node
    let dir_name = parts[0];
    let existing = nodes
        .iter_mut()
        .find(|n| n.change.is_none() && n.name == dir_name);

    if let Some(dir_node) = existing {
        insert_into_tree(&mut dir_node.children, &parts[1..], change);
    } else {
        let mut dir_node = FileTreeNode {
            name: dir_name.to_string(),
            change: None,
            children: Vec::new(),
        };
        insert_into_tree(&mut dir_node.children, &parts[1..], change);
        nodes.push(dir_node);
    }
}

fn collapse_single_dirs(nodes: &mut Vec<FileTreeNode<'_>>) {
    for node in nodes.iter_mut() {
        if node.change.is_none() {
            // Collapse single-child directory chains
            while node.children.len() == 1 && node.children[0].change.is_none() {
                let child = node.children.remove(0);
                node.name = format!("{}/{}", node.name, child.name);
                node.children = child.children;
            }
            collapse_single_dirs(&mut node.children);
        }
    }
}

fn sort_tree(nodes: &mut Vec<FileTreeNode<'_>>) {
    // Directories first, then files, each sorted alphabetically
    nodes.sort_by(|a, b| {
        let a_is_dir = a.change.is_none();
        let b_is_dir = b.change.is_none();
        b_is_dir.cmp(&a_is_dir).then(a.name.cmp(&b.name))
    });
    for node in nodes.iter_mut() {
        if node.change.is_none() {
            sort_tree(&mut node.children);
        }
    }
}

fn flatten_tree_to_lines(
    nodes: Vec<FileTreeNode<'_>>,
    depth: usize,
    color_theme: &ColorTheme,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let indent = "  ".repeat(depth);

    for node in nodes {
        if let Some(change) = node.change {
            // File node
            let color = match change {
                FileChange::Add { .. } => color_theme.detail_file_change_add_fg,
                FileChange::Modify { .. } => color_theme.detail_file_change_modify_fg,
                FileChange::Delete { .. } => color_theme.detail_file_change_delete_fg,
            };

            let mut spans: Vec<Span> = vec![
                indent.clone().into(),
                Span::styled(ICON_FILE, Style::default().fg(ratatui::style::Color::Gray)),
                Span::styled(node.name, Style::default().fg(color)),
            ];

            if let Some((add, del)) = change.stats() {
                spans.push("  （".into());
                spans.push(Span::styled(
                    format!("+{add}"),
                    Style::default().fg(color_theme.detail_file_change_add_fg),
                ));
                spans.push(" | ".into());
                spans.push(Span::styled(
                    format!("-{del}"),
                    Style::default().fg(color_theme.detail_file_change_delete_fg),
                ));
                spans.push("）".into());
            }

            lines.push(Line::from(spans));
        } else {
            // Directory node
            lines.push(Line::from(vec![
                indent.clone().into(),
                Span::styled(
                    ICON_FOLDER,
                    Style::default().fg(ratatui::style::Color::Gray),
                ),
                node.name.into(),
            ]));
            lines.extend(flatten_tree_to_lines(node.children, depth + 1, color_theme));
        }
    }

    lines
}

fn build_tree_lines<'a>(
    changes: &'a [FileChange],
    color_theme: &'a ColorTheme,
) -> Vec<Line<'static>> {
    let tree = build_file_tree(changes);
    flatten_tree_to_lines(tree, 0, color_theme)
}

fn wrap_to_width(text: &str, width: usize) -> Vec<String> {
    debug_assert!(width > 0, "wrap_to_width requires non-zero width");
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_w = 0usize;
    for c in text.chars() {
        let cw = UnicodeWidthChar::width(c).unwrap_or(0);
        if cw > 0 && current_w + cw > width && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
            current_w = 0;
        }
        current.push(c);
        current_w += cw;
    }
    // Always emit at least one line; preserves blank body lines (paragraph breaks).
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

fn span_width(span: &Span<'_>) -> usize {
    span.content
        .chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum()
}

fn split_span_at_width<'a>(span: Span<'a>, max_w: usize) -> (Span<'a>, Option<Span<'a>>) {
    let style = span.style;
    let s: &str = span.content.as_ref();
    let mut split_byte = s.len();
    let mut acc_w = 0usize;
    for (i, c) in s.char_indices() {
        let cw = UnicodeWidthChar::width(c).unwrap_or(0);
        if cw > 0 && acc_w + cw > max_w {
            split_byte = i;
            break;
        }
        acc_w += cw;
    }
    let head = Span::styled(s[..split_byte].to_string(), style);
    let tail = if split_byte == s.len() {
        None
    } else {
        Some(Span::styled(s[split_byte..].to_string(), style))
    };
    (head, tail)
}

/// Wrap a multi-span line at `width` cells, preserving each span's style and `line.style`.
/// Continuation lines are flush-left (no indent alignment).
/// Contract: `width == 0` or empty `line.spans` returns `vec![line]` unchanged — these are
/// real states under aggressive terminal resize / blank lines, not caller bugs.
fn wrap_line_spans<'a>(line: Line<'a>, width: usize) -> Vec<Line<'a>> {
    if width == 0 || line.spans.is_empty() || line.width() <= width {
        return vec![line];
    }
    let line_style = line.style;
    let mut result: Vec<Line<'a>> = Vec::new();
    let mut current: Vec<Span<'a>> = Vec::new();
    let mut current_w = 0usize;
    for span in line.spans {
        let mut remaining = Some(span);
        while let Some(s) = remaining.take() {
            let s_w = span_width(&s);
            if current_w + s_w <= width {
                current.push(s);
                current_w += s_w;
                continue;
            }
            let (head, tail) = split_span_at_width(s, width - current_w);
            if !head.content.is_empty() {
                current.push(head);
            }
            let mut l = Line::from(std::mem::take(&mut current));
            l.style = line_style;
            result.push(l);
            current_w = 0;
            remaining = tail;
        }
    }
    if !current.is_empty() || result.is_empty() {
        let mut l = Line::from(current);
        l.style = line_style;
        result.push(l);
    }
    result
}

#[cfg(test)]
mod tests {
    use ratatui::{
        style::{Color, Modifier, Style},
        text::{Line, Span},
    };

    use super::{wrap_line_spans, wrap_to_width};

    #[test]
    fn wraps_ascii() {
        assert_eq!(wrap_to_width("abcdef", 3), vec!["abc", "def"]);
    }

    #[test]
    fn wraps_cjk_double_width() {
        assert_eq!(
            wrap_to_width("中文中文中文", 4),
            vec!["中文", "中文", "中文"]
        );
    }

    #[test]
    fn wraps_mixed_width() {
        assert_eq!(wrap_to_width("a中b", 2), vec!["a", "中", "b"]);
    }

    #[test]
    fn empty_input_returns_single_blank_line() {
        assert_eq!(wrap_to_width("", 80), vec![String::new()]);
    }

    #[test]
    fn wrap_lines_single_span_short() {
        let line = Line::from(Span::raw("hi"));
        assert_eq!(wrap_line_spans(line, 10).len(), 1);
    }

    #[test]
    fn wrap_lines_single_span_split() {
        let line = Line::from(Span::raw("abcdef"));
        let r = wrap_line_spans(line, 3);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].spans[0].content, "abc");
        assert_eq!(r[1].spans[0].content, "def");
    }

    #[test]
    fn wrap_lines_preserves_span_style_across_split() {
        let red = Style::default().fg(Color::Red);
        let line = Line::from(Span::styled("abcdef", red));
        let r = wrap_line_spans(line, 3);
        assert_eq!(r[0].spans[0].style, red);
        assert_eq!(r[1].spans[0].style, red);
    }

    #[test]
    fn wrap_lines_preserves_line_style() {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let mut line = Line::from(Span::raw("abcdef"));
        line.style = bold;
        let r = wrap_line_spans(line, 3);
        assert_eq!(r[0].style, bold);
        assert_eq!(r[1].style, bold);
    }

    #[test]
    fn wrap_lines_splits_across_multiple_spans() {
        let line = Line::from(vec![Span::raw("AB"), Span::raw("CDE")]);
        let r = wrap_line_spans(line, 3);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].spans.len(), 2);
        assert_eq!(r[0].spans[0].content, "AB");
        assert_eq!(r[0].spans[1].content, "C");
        assert_eq!(r[1].spans[0].content, "DE");
    }

    #[test]
    fn wrap_lines_cjk_double_width() {
        let line = Line::from(Span::raw("中文中文"));
        let r = wrap_line_spans(line, 4);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].spans[0].content, "中文");
        assert_eq!(r[1].spans[0].content, "中文");
    }

    #[test]
    fn wrap_lines_combining_mark_stays_with_base() {
        let line = Line::from(Span::raw("e\u{0301}fg"));
        let r = wrap_line_spans(line, 2);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].spans[0].content, "e\u{0301}f");
        assert_eq!(r[1].spans[0].content, "g");
    }

    #[test]
    fn wrap_lines_empty_returns_self() {
        let line = Line::from(Vec::<Span>::new());
        let r = wrap_line_spans(line, 10);
        assert_eq!(r.len(), 1);
        assert!(r[0].spans.is_empty());
    }

    #[test]
    fn wrap_lines_zero_width_returns_self() {
        let line = Line::from(Span::raw("anything"));
        let r = wrap_line_spans(line, 0);
        assert_eq!(r.len(), 1);
    }
}

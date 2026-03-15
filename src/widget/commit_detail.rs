use std::rc::Rc;

use chrono::{DateTime, FixedOffset};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Padding, Paragraph, StatefulWidget, Widget},
};

use crate::{
    app::AppContext,
    color::ColorTheme,
    git::{Commit, FileChange, Ref, WorkingChanges},
};

const ICON_FILE: &str = "\u{f0214} ";
const ICON_FOLDER: &str = "\u{f0770} ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailPane {
    Left,
    Right,
}

#[derive(Debug, Default)]
pub struct CommitDetailState {
    height: usize,
    left_offset: usize,
    right_offset: usize,
    active_pane: Option<DetailPane>,
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

    pub fn scroll_page_down(&mut self) {
        match self.active_pane() {
            DetailPane::Left => self.left_offset = self.left_offset.saturating_add(self.height),
            DetailPane::Right => self.right_offset = self.right_offset.saturating_add(self.height),
        }
    }

    pub fn scroll_page_up(&mut self) {
        match self.active_pane() {
            DetailPane::Left => self.left_offset = self.left_offset.saturating_sub(self.height),
            DetailPane::Right => self.right_offset = self.right_offset.saturating_sub(self.height),
        }
    }

    pub fn scroll_half_page_down(&mut self) {
        match self.active_pane() {
            DetailPane::Left => self.left_offset = self.left_offset.saturating_add(self.height / 2),
            DetailPane::Right => {
                self.right_offset = self.right_offset.saturating_add(self.height / 2)
            }
        }
    }

    pub fn scroll_half_page_up(&mut self) {
        match self.active_pane() {
            DetailPane::Left => self.left_offset = self.left_offset.saturating_sub(self.height / 2),
            DetailPane::Right => {
                self.right_offset = self.right_offset.saturating_sub(self.height / 2)
            }
        }
    }

    pub fn select_first(&mut self) {
        self.left_offset = 0;
        self.right_offset = 0;
    }

    pub fn select_last(&mut self) {
        match self.active_pane() {
            DetailPane::Left => self.left_offset = usize::MAX,
            DetailPane::Right => self.right_offset = usize::MAX,
        }
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
}

pub struct CommitDetail<'a> {
    commit: &'a Commit,
    changes: &'a Vec<FileChange>,
    refs: &'a Vec<Ref>,
    ctx: Rc<AppContext>,
}

impl<'a> CommitDetail<'a> {
    pub fn new(
        commit: &'a Commit,
        changes: &'a Vec<FileChange>,
        refs: &'a Vec<Ref>,
        ctx: Rc<AppContext>,
    ) -> Self {
        Self {
            commit,
            changes,
            refs,
            ctx,
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

        let left_lines = self.info_lines();
        let right_lines = self.changes_lines();

        let area_height = area.height as usize;
        state.height = area_height;
        state.left_offset = state
            .left_offset
            .min(left_lines.len().saturating_sub(area_height));
        state.right_offset = state
            .right_offset
            .min(right_lines.len().saturating_sub(area_height));

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
            .block(
                Block::default()
                    .borders(Borders::TOP | Borders::BOTTOM)
                    .style(Style::default().fg(self.ctx.color_theme.divider_fg))
                    .padding(Padding::new(1, 1, 0, 0)),
            );
        left_paragraph.render(left_area, buf);

        // Render vertical divider
        render_vertical_divider(divider_area, buf, self.ctx.color_theme.divider_fg);

        let right_paragraph = Paragraph::new(right_lines)
            .style(Style::default().fg(self.ctx.color_theme.fg))
            .block(
                Block::default()
                    .borders(Borders::TOP | Borders::BOTTOM)
                    .style(Style::default().fg(self.ctx.color_theme.divider_fg))
                    .padding(Padding::new(1, 1, 0, 0)),
            );
        right_paragraph.render(right_area, buf);
    }
}

impl CommitDetail<'_> {
    pub fn content_height(&self) -> u16 {
        let left = self.info_lines().len();
        let right = self.changes_lines().len();
        (left.max(right) + 2) as u16 // +2 for top/bottom borders
    }

    fn info_lines(&self) -> Vec<Line<'_>> {
        let mut lines: Vec<Line> = Vec::new();

        // Author
        lines.push(Line::from(vec![
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
        ]));
        lines.push(Line::from(vec![
            Span::raw("        "),
            Span::styled(
                self.format_date(&self.commit.author_date),
                Style::default().fg(self.ctx.color_theme.detail_date_fg),
            ),
        ]));

        if is_author_committer_different(self.commit) {
            lines.push(Line::from(vec![
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
            ]));
            lines.push(Line::from(vec![
                Span::raw("           "),
                Span::styled(
                    self.format_date(&self.commit.committer_date),
                    Style::default().fg(self.ctx.color_theme.detail_date_fg),
                ),
            ]));
        }

        // SHA
        lines.push(Line::from(vec![
            Span::styled(
                "Commit: ",
                Style::default().fg(self.ctx.color_theme.detail_label_fg),
            ),
            self.commit
                .commit_hash
                .as_str()
                .fg(self.ctx.color_theme.detail_hash_fg),
        ]));

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
            lines.push(Line::from(spans));
        }

        // Refs
        if has_refs(self.refs) {
            lines.push(Line::from(vec![
                Span::styled(
                    "Refs: ",
                    Style::default().fg(self.ctx.color_theme.detail_label_fg),
                ),
                self.refs_span(),
            ]));
        }

        // Divider + commit message
        lines.push(Line::raw(""));
        lines.push(Line::from(self.commit.subject.as_str().bold()));

        if !self.commit.body.is_empty() {
            lines.push(Line::raw(""));
            lines.extend(self.commit.body.lines().map(Line::raw));
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

fn render_vertical_divider(area: Rect, buf: &mut Buffer, fg: Color) {
    let style = Style::default().fg(fg);
    for y in area.top()..area.bottom() {
        buf[(area.left(), y)].set_symbol("│").set_style(style);
    }
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

        let left_lines = self.info_lines();
        let right_lines = self.file_lines();

        let area_height = area.height as usize;
        state.height = area_height;
        state.left_offset = state
            .left_offset
            .min(left_lines.len().saturating_sub(area_height));
        state.right_offset = state
            .right_offset
            .min(right_lines.len().saturating_sub(area_height));

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
            .block(
                Block::default()
                    .borders(Borders::TOP | Borders::BOTTOM)
                    .style(Style::default().fg(self.ctx.color_theme.divider_fg))
                    .padding(Padding::new(1, 1, 0, 0)),
            );
        left_paragraph.render(left_area, buf);

        // Render vertical divider
        render_vertical_divider(divider_area, buf, self.ctx.color_theme.divider_fg);

        let right_paragraph = Paragraph::new(right_lines)
            .style(Style::default().fg(self.ctx.color_theme.fg))
            .block(
                Block::default()
                    .borders(Borders::TOP | Borders::BOTTOM)
                    .style(Style::default().fg(self.ctx.color_theme.divider_fg))
                    .padding(Padding::new(1, 1, 0, 0)),
            );
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

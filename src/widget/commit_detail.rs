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
    git::{Commit, CommitHash, DiffTarget, FileChange, Ref, WorkingChanges},
    graph::GlyphSet,
};

const ICON_FILE: &str = "\u{f0214} ";
const ICON_FOLDER: &str = "\u{f0770} ";

/// 游標與檔案樹視窗上/下緣保持的邊距列數。
const FILE_TREE_SCROLLOFF: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DetailPane {
    #[default]
    Info,
    Files,
}

#[derive(Debug, Clone, Copy)]
enum LineMode {
    /// 以此寬度渲染，含 marquee 捲動與內文換行。
    Render(usize),
    /// 只算邏輯行數：不做 marquee 捲動，也不做內文換行。
    Measure,
}

/// 檔案樹的一列。`file` 為 `None` 代表目錄／區段標題／空行，游標不會停在上面。
#[derive(Debug, Clone)]
pub struct TreeRow {
    pub line: Line<'static>,
    pub file: Option<DiffTarget>,
}

#[derive(Debug, Default)]
pub struct CommitDetailState {
    left_offset: usize,
    /// Files pane：檔案樹視窗的頂端 row index。唯一驅動來源是游標 + scrolloff，
    /// 沒有獨立捲動檔案樹的按鍵。
    right_offset: usize,
    active_pane: DetailPane,
    file_cursor: Option<usize>,
    /// 上一幀 render 量到的檔案樹可視高度，`move_file_cursor`／`resync_files_window`
    /// 算 scrolloff 要用。
    files_window_height: usize,
    /// 上次 render 時 subject 是否超過 marquee 可用寬度。App tick 迴圈讀這個
    /// 決定要不要繼續推進 marquee_frame。
    subject_overflows: std::cell::Cell<bool>,
}

impl CommitDetailState {
    pub fn active_pane(&self) -> DetailPane {
        self.active_pane
    }

    pub fn toggle_pane(&mut self) {
        self.active_pane = match self.active_pane {
            DetailPane::Info => DetailPane::Files,
            DetailPane::Files => DetailPane::Info,
        };
    }

    pub fn scroll_info_down(&mut self) {
        self.left_offset = self.left_offset.saturating_add(1);
    }

    pub fn scroll_info_up(&mut self) {
        self.left_offset = self.left_offset.saturating_sub(1);
    }

    pub fn file_cursor(&self) -> Option<usize> {
        self.file_cursor
    }

    pub fn selected_file<'r>(&self, rows: &'r [TreeRow]) -> Option<&'r DiffTarget> {
        self.file_cursor
            .and_then(|i| rows.get(i))
            .and_then(|r| r.file.as_ref())
    }

    /// 內容切換（換 commit、切到 Working Changes）時呼叫：左側捲動歸零、
    /// 游標移到第一個檔案列（沒有檔案就是 `None`）。
    pub fn reset(&mut self, rows: &[TreeRow]) {
        self.left_offset = 0;
        self.right_offset = 0;
        self.file_cursor = rows.iter().position(|r| r.file.is_some());
    }

    /// 游標移到下一個檔案列（跳過目錄／標題／空行）。回傳 true 代表游標真的
    /// 移動了，呼叫端據此決定要不要重新載入 diff。
    pub fn move_file_cursor_down(&mut self, rows: &[TreeRow]) -> bool {
        self.move_file_cursor(rows, 1)
    }

    pub fn move_file_cursor_up(&mut self, rows: &[TreeRow]) -> bool {
        self.move_file_cursor(rows, -1)
    }

    fn move_file_cursor(&mut self, rows: &[TreeRow], dir: isize) -> bool {
        let Some(start) = self.file_cursor else {
            return false;
        };
        let mut i = start as isize;
        loop {
            i += dir;
            if i < 0 || i as usize >= rows.len() {
                return false;
            }
            if rows[i as usize].file.is_some() {
                self.file_cursor = Some(i as usize);
                self.right_offset = scrolled_offset(
                    i as usize,
                    self.files_window_height,
                    rows.len(),
                    self.right_offset,
                );
                return true;
            }
        }
    }

    fn set_files_window_height(&mut self, h: usize) {
        self.files_window_height = h;
    }

    fn files_window_top(&self) -> usize {
        self.right_offset
    }

    /// render 時的安全網：終端機縮放讓視窗高度變化時，重新套用 scrolloff
    /// 確保游標仍在可視範圍內。與游標移動共用同一個 `scrolled_offset`，
    /// 不分裂成兩處各自實作。
    fn resync_files_window(&mut self, rows_len: usize) {
        if let Some(cursor) = self.file_cursor {
            self.right_offset = scrolled_offset(
                cursor,
                self.files_window_height,
                rows_len,
                self.right_offset,
            );
        }
    }

    fn clamp_left_offset(&mut self, left_len: usize, inner_height: usize) {
        self.left_offset = self.left_offset.min(left_len.saturating_sub(inner_height));
    }

    pub fn subject_overflows(&self) -> bool {
        self.subject_overflows.get()
    }
}

/// 標準 vim 風格 scrolloff：游標與視窗上/下緣保持 `FILE_TREE_SCROLLOFF` 列邊距，
/// 但清單頭/尾不強制留邊（沒有更多內容可留，標準 clamp）。這是檔案樹視窗位置
/// 的唯一計算點——游標移動與 resize 後的安全網都呼叫這個。
fn scrolled_offset(
    cursor: usize,
    window_height: usize,
    rows_len: usize,
    prev_offset: usize,
) -> usize {
    if window_height == 0 {
        return 0;
    }
    let max_offset = rows_len.saturating_sub(window_height);
    if max_offset == 0 {
        return 0;
    }

    let scrolloff = FILE_TREE_SCROLLOFF.min(window_height.saturating_sub(1) / 2);
    let min_offset_for_cursor = cursor.saturating_sub(window_height - 1 - scrolloff);
    let max_offset_for_cursor = cursor.saturating_sub(scrolloff).min(max_offset);

    let mut offset = prev_offset.min(max_offset);
    if offset > max_offset_for_cursor {
        offset = max_offset_for_cursor;
    }
    if offset < min_offset_for_cursor {
        offset = min_offset_for_cursor.min(max_offset);
    }
    offset
}

pub struct CommitDetail<'a> {
    commit: &'a Commit,
    rows: &'a [TreeRow],
    refs: &'a Vec<Ref>,
    ctx: Rc<AppContext>,
    marquee_frame: u64,
}

impl<'a> CommitDetail<'a> {
    pub fn new(
        commit: &'a Commit,
        rows: &'a [TreeRow],
        refs: &'a Vec<Ref>,
        ctx: Rc<AppContext>,
        marquee_frame: u64,
    ) -> Self {
        Self {
            commit,
            rows,
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

        let left_active = state.active_pane() == DetailPane::Info;

        let available = left_area.width.saturating_sub(2) as usize;
        // 寬度基準必須跟下面的 `scroll_window` 一致，理由見 `marquee::display_width`。
        state
            .subject_overflows
            .set(crate::widget::marquee::display_width(&self.commit.subject) > available);
        let left_lines = self.info_lines(LineMode::Render(available));

        let glyphs = GlyphSet::from_style(self.ctx.graph_style);
        let block = detail_block(self.ctx.color_theme.divider_fg, glyphs);
        let inner_h = block.inner(area).height as usize;

        state.clamp_left_offset(left_lines.len(), inner_h);
        state.set_files_window_height(inner_h);
        state.resync_files_window(self.rows.len());

        let left_lines: Vec<Line> = left_lines.into_iter().skip(state.left_offset).collect();
        let left_lines = if left_active {
            left_lines
        } else {
            dim_lines(left_lines)
        };

        let left_paragraph = Paragraph::new(left_lines)
            .style(Style::default().fg(self.ctx.color_theme.fg))
            .block(block);
        left_paragraph.render(left_area, buf);

        // 渲染垂直分隔線
        render_vertical_divider(divider_area, buf, self.ctx.color_theme.divider_fg, glyphs);

        render_file_tree_pane(self.rows, right_area, buf, state, &self.ctx);
    }
}

impl CommitDetail<'_> {
    pub fn content_height(&self) -> u16 {
        let left = self.info_lines(LineMode::Measure).len();
        let right = self.rows.len();
        (left.max(right) + 2) as u16 // +2 為上／下邊框
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

        // 作者
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

        // 父提交
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

        // 分隔線 + commit 訊息。Subject 已用 marquee 裁到 `marquee_width`，
        // 所以換行處理形同無操作；直接 push 原始內容以避免換行／不換行的分支判斷。
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
}

/// 檔案樹右側 pane 的共用渲染邏輯：截斷＋補白、選取列 highlight、捲動視窗、
/// 非 focus 時 dim。`CommitDetail` 與 `WorkingChangesDetail` 共用同一份，
/// 避免同樣的邏輯抄兩次。`right_available`／`right_active`／`block` 都能從
/// `area`／`state`／`ctx` 推導，不需要呼叫端各自算好再傳進來。
fn render_file_tree_pane(
    rows: &[TreeRow],
    area: Rect,
    buf: &mut Buffer,
    state: &CommitDetailState,
    ctx: &AppContext,
) {
    let right_available = area.width.saturating_sub(2) as usize;
    let right_active = state.active_pane() == DetailPane::Files;
    let glyphs = GlyphSet::from_style(ctx.graph_style);
    let block = detail_block(ctx.color_theme.divider_fg, glyphs);

    // 先 skip 視窗外的 row 再截斷／highlight，畫面外的列不用白做一次
    // clone＋補白。
    let right_lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(state.files_window_top())
        .map(|(i, row)| {
            let line = truncate_line_to_width(row.line.clone(), right_available);
            if right_active && Some(i) == state.file_cursor() {
                highlight_line(line, ctx.color_theme.list_selected_bg)
            } else {
                line
            }
        })
        .collect();
    let right_lines = if right_active {
        right_lines
    } else {
        dim_lines(right_lines)
    };

    let right_paragraph = Paragraph::new(right_lines)
        .style(Style::default().fg(ctx.color_theme.fg))
        .block(block);
    right_paragraph.render(area, buf);
}

fn dim_color(color: Color) -> Color {
    match color {
        Color::Rgb(r, g, b) => {
            // 往暗粉紅色調混：先跟 (140,110,120) 取平均，再壓暗
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
    staged_count: usize,
    unstaged_count: usize,
    rows: &'a [TreeRow],
    ctx: Rc<AppContext>,
}

impl<'a> WorkingChangesDetail<'a> {
    pub fn new(
        staged_count: usize,
        unstaged_count: usize,
        rows: &'a [TreeRow],
        ctx: Rc<AppContext>,
    ) -> Self {
        Self {
            staged_count,
            unstaged_count,
            rows,
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

        let left_active = state.active_pane() == DetailPane::Info;

        let left_lines = self.info_lines();

        let glyphs = GlyphSet::from_style(self.ctx.graph_style);
        let block = detail_block(self.ctx.color_theme.divider_fg, glyphs);
        let inner_h = block.inner(area).height as usize;

        state.clamp_left_offset(left_lines.len(), inner_h);
        state.set_files_window_height(inner_h);
        state.resync_files_window(self.rows.len());

        let left_lines: Vec<Line> = left_lines.into_iter().skip(state.left_offset).collect();
        let left_lines = if left_active {
            left_lines
        } else {
            dim_lines(left_lines)
        };

        let left_paragraph = Paragraph::new(left_lines)
            .style(Style::default().fg(self.ctx.color_theme.fg))
            .block(block);
        left_paragraph.render(left_area, buf);

        // 渲染垂直分隔線
        render_vertical_divider(divider_area, buf, self.ctx.color_theme.divider_fg, glyphs);

        render_file_tree_pane(self.rows, right_area, buf, state, &self.ctx);
    }
}

impl WorkingChangesDetail<'_> {
    pub fn content_height(&self) -> u16 {
        let left = self.info_lines().len();
        let right = self.rows.len();
        (left.max(right) + 2) as u16 // +2 為上／下邊框
    }

    fn info_lines(&self) -> Vec<Line<'_>> {
        let mut lines: Vec<Line> = Vec::new();

        lines.push(
            Line::from("Uncommitted Changes")
                .style(Style::default().fg(self.ctx.color_theme.fg).bold()),
        );
        lines.push(Line::raw(""));

        if self.staged_count > 0 {
            lines.push(
                Line::from(format!("Staged Changes ({})", self.staged_count))
                    .style(Style::default().fg(self.ctx.color_theme.fg).bold()),
            );
        }

        if self.unstaged_count > 0 {
            lines.push(
                Line::from(format!("Unstaged Changes ({})", self.unstaged_count))
                    .style(Style::default().fg(self.ctx.color_theme.fg).bold()),
            );
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
        // 檔案葉節點
        nodes.push(FileTreeNode {
            name: parts[0].to_string(),
            change: Some(change),
            children: Vec::new(),
        });
        return;
    }

    // 尋找或建立目錄節點
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
            // 摺疊只有單一子節點的目錄鏈
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
    // 目錄排前面，接著是檔案，各自依字母順序排序
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
    to_target: &dyn Fn(&FileChange) -> DiffTarget,
) -> Vec<TreeRow> {
    let mut rows = Vec::new();
    let indent = "  ".repeat(depth);

    for node in nodes {
        if let Some(change) = node.change {
            // 檔案節點
            let color = match change {
                FileChange::Add { .. } => color_theme.detail_file_change_add_fg,
                FileChange::Modify { .. } => color_theme.detail_file_change_modify_fg,
                FileChange::Delete { .. } => color_theme.detail_file_change_delete_fg,
                FileChange::Untracked { .. } => color_theme.detail_file_change_add_fg,
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

            rows.push(TreeRow {
                line: Line::from(spans),
                file: Some(to_target(change)),
            });
        } else {
            // 目錄節點
            rows.push(TreeRow {
                line: Line::from(vec![
                    indent.clone().into(),
                    Span::styled(
                        ICON_FOLDER,
                        Style::default().fg(ratatui::style::Color::Gray),
                    ),
                    node.name.into(),
                ]),
                file: None,
            });
            rows.extend(flatten_tree_to_lines(
                node.children,
                depth + 1,
                color_theme,
                to_target,
            ));
        }
    }

    rows
}

fn build_tree_lines(
    changes: &[FileChange],
    color_theme: &ColorTheme,
    to_target: &dyn Fn(&FileChange) -> DiffTarget,
) -> Vec<TreeRow> {
    let tree = build_file_tree(changes);
    flatten_tree_to_lines(tree, 0, color_theme, to_target)
}

/// commit 的檔案樹 rows，每一列都指向同一個 commit hash 的單檔 diff。
pub fn build_commit_tree_rows(
    changes: &[FileChange],
    hash: &CommitHash,
    color_theme: &ColorTheme,
) -> Vec<TreeRow> {
    build_tree_lines(changes, color_theme, &|change| DiffTarget::Commit {
        hash: hash.clone(),
        path: change.path().to_string(),
    })
}

/// Working Changes 的檔案樹 rows：staged／unstaged 分兩段各自的標題列，
/// unstaged 段裡的 untracked 檔案另外路由到 `DiffTarget::Untracked`。
pub fn build_working_changes_tree_rows(
    working_changes: &WorkingChanges,
    color_theme: &ColorTheme,
) -> Vec<TreeRow> {
    let mut rows = Vec::new();

    if !working_changes.staged.is_empty() {
        rows.push(section_header_row("Staged:", color_theme));
        rows.extend(build_tree_lines(
            &working_changes.staged,
            color_theme,
            &|change| DiffTarget::Staged {
                path: change.path().to_string(),
            },
        ));
        rows.push(TreeRow {
            line: Line::raw(""),
            file: None,
        });
    }

    if !working_changes.unstaged.is_empty() {
        rows.push(section_header_row("Unstaged:", color_theme));
        rows.extend(build_tree_lines(
            &working_changes.unstaged,
            color_theme,
            &|change| match change {
                FileChange::Untracked { path, .. } => DiffTarget::Untracked { path: path.clone() },
                _ => DiffTarget::Unstaged {
                    path: change.path().to_string(),
                },
            },
        ));
    }

    rows
}

fn section_header_row(text: &str, color_theme: &ColorTheme) -> TreeRow {
    TreeRow {
        line: Line::from(text.to_string()).style(Style::default().fg(color_theme.fg).bold()),
        file: None,
    }
}

/// 契約與 `wrap_line_spans` 一致：`width == 0` 原樣回傳，這是終端機被縮到極窄時
/// 會真實發生的狀態（`available = left_area.width - 2` 會算出 0），不是呼叫端的錯誤。
/// 原本這裡有個 `debug_assert!(width > 0)`，跟下面自己的 graceful 處理互相矛盾，
/// 而且會讓 debug build 在把視窗拖到很窄時直接 panic。
fn wrap_to_width(text: &str, width: usize) -> Vec<String> {
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
    // 一定至少輸出一行；讓 body 裡的空白行（段落分隔）得以保留。
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

/// 以 `width` 格寬換行一個多 span 的 line，保留每個 span 的樣式與 `line.style`。
/// 續行一律靠左對齊（不做縮排對齊）。
/// 契約：`width == 0` 或 `line.spans` 為空時原樣回傳 `vec![line]` —— 這是終端機
/// 劇烈縮放／空白行時會真實發生的狀態，不是呼叫端的錯誤。
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

/// 把一行截到 `width` 格寬，超出時補上省略號；不足 `width` 時補空白到剛好
/// `width` —— 這樣選取列的 highlight 背景才能鋪滿整列，而不只是蓋在文字底下
/// （`Line.style` 只會 patch 到既有文字，不會鋪滿整個 row）。
fn truncate_line_to_width(line: Line<'static>, width: usize) -> Line<'static> {
    if width == 0 {
        return Line::from(Vec::<Span<'static>>::new());
    }
    let line_style = line.style;
    let total_w: usize = line.spans.iter().map(span_width).sum();

    if total_w <= width {
        let mut spans = line.spans;
        let pad = width - total_w;
        if pad > 0 {
            spans.push(Span::raw(" ".repeat(pad)));
        }
        let mut l = Line::from(spans);
        l.style = line_style;
        return l;
    }

    // 省略號佔 1 格，內容截到 width - 1；重用 `split_span_at_width` 處理
    // CJK 雙寬與 combining mark。
    let content_w = width.saturating_sub(1);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut acc_w = 0usize;
    for span in line.spans {
        if acc_w >= content_w {
            break;
        }
        let remaining = content_w - acc_w;
        let sw = span_width(&span);
        if sw <= remaining {
            acc_w += sw;
            spans.push(span);
        } else {
            let (head, _) = split_span_at_width(span, remaining);
            acc_w += span_width(&head);
            if !head.content.is_empty() {
                spans.push(head);
            }
            break;
        }
    }
    spans.push(Span::raw("…"));
    acc_w += 1;
    if acc_w < width {
        spans.push(Span::raw(" ".repeat(width - acc_w)));
    }
    let mut l = Line::from(spans);
    l.style = line_style;
    l
}

/// 把 `bg` patch 到每個 span，搭配 `truncate_line_to_width` 補的空白 span
/// 才能讓選取列的背景色鋪滿整列寬度。
fn highlight_line(mut line: Line<'static>, bg: Color) -> Line<'static> {
    for span in &mut line.spans {
        span.style.bg = Some(bg);
    }
    line
}

#[cfg(test)]
mod tests {
    use ratatui::{
        style::{Color, Modifier, Style},
        text::{Line, Span},
    };

    use super::{
        build_commit_tree_rows, truncate_line_to_width, wrap_line_spans, wrap_to_width, FileChange,
    };
    use crate::{color::ColorTheme, git::CommitHash};

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

    #[test]
    fn truncate_pads_short_line_to_full_width() {
        let line = Line::from(Span::raw("hi"));
        let r = truncate_line_to_width(line, 10);
        let w: usize = r.spans.iter().map(|s| s.content.chars().count()).sum();
        assert_eq!(w, 10);
    }

    #[test]
    fn truncate_long_line_ends_with_ellipsis_and_fills_width() {
        let line = Line::from(Span::raw("this is a very long file name.rs"));
        let r = truncate_line_to_width(line, 10);
        let total: String = r.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(total.ends_with('…'));
        let w: usize = r.spans.iter().map(|s| s.content.chars().count()).sum();
        assert_eq!(w, 10);
    }

    #[test]
    fn truncate_cjk_does_not_split_wide_char() {
        let line = Line::from(Span::raw("中文中文中文中文"));
        let r = truncate_line_to_width(line, 5);
        let w: usize = r
            .spans
            .iter()
            .map(|s| {
                s.content
                    .chars()
                    .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(0))
                    .sum::<usize>()
            })
            .sum();
        assert_eq!(w, 5);
    }

    #[test]
    fn build_commit_tree_rows_directory_rows_have_no_file_target() {
        let changes = vec![FileChange::Modify {
            path: "src/app.rs".into(),
            stats: None,
        }];
        let hash: CommitHash = "abc123".into();
        let theme = ColorTheme::default();
        let rows = build_commit_tree_rows(&changes, &hash, &theme);

        assert!(rows.iter().any(|r| r.file.is_none())); // 目錄列
        let file_row = rows.iter().find(|r| r.file.is_some()).unwrap();
        match file_row.file.as_ref().unwrap() {
            crate::git::DiffTarget::Commit { hash: h, path } => {
                assert_eq!(h, &hash);
                assert_eq!(path, "src/app.rs"); // 完整路徑，不是摺疊後的葉節點名
            }
            other => panic!("unexpected target: {other:?}"),
        }
    }

    #[test]
    fn build_working_changes_tree_rows_routes_untracked_separately() {
        use crate::git::WorkingChanges;

        let wc = WorkingChanges {
            staged: vec![],
            unstaged: vec![FileChange::Untracked {
                path: "new_file.txt".into(),
                stats: None,
            }],
        };
        let theme = ColorTheme::default();
        let rows = super::build_working_changes_tree_rows(&wc, &theme);
        let file_row = rows.iter().find(|r| r.file.is_some()).unwrap();
        match file_row.file.as_ref().unwrap() {
            crate::git::DiffTarget::Untracked { path } => assert_eq!(path, "new_file.txt"),
            other => panic!("unexpected target: {other:?}"),
        }
    }
}

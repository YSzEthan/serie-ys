use std::rc::Rc;

use laurier::highlight::highlight_matched_text;
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{List, ListItem, Paragraph, StatefulWidget, Widget},
};
use rustc_hash::FxHashMap;

use crate::{
    app::AppContext,
    color::{ratatui_color_to_rgb, ColorTheme},
    config::UserListColumnType,
    git::{Commit, CommitHash, Head, Ref},
    graph::{Glyph, GlyphSet, TextCell},
};

use super::search::SearchMatchPosition;
use super::state::CommitListState;
use super::{CommitInfo, FilteredIdx, RawCommitIdx};

const ELLIPSIS: &str = "...";
const VIRTUAL_ROW_COLOR: Color = Color::Gray;

pub struct CommitList<'a> {
    ctx: Rc<AppContext>,
    marquee_frame: u64,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a> CommitList<'a> {
    pub fn new(ctx: Rc<AppContext>, marquee_frame: u64) -> Self {
        Self {
            ctx,
            marquee_frame,
            _marker: std::marker::PhantomData,
        }
    }
}

impl<'a> StatefulWidget for CommitList<'a> {
    type State = CommitListState<'a>;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        if area.height < 2 {
            return;
        }

        let [header_area, content_area] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);

        self.update_state(content_area, state);

        let name_width = if state.name_cell_width() > 0 {
            state.name_cell_width()
        } else {
            self.ctx.ui_config.list.name_width
        };
        let constraints = calc_cell_widths(
            area.width,
            self.ctx.ui_config.list.subject_min_width,
            state.graph_area_cell_width(),
            name_width,
            self.ctx.ui_config.list.date_width,
            &self.ctx.ui_config.list.columns,
        );

        let header_chunks = Layout::horizontal(constraints.clone()).split(header_area);
        let header_style = Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD);
        for (i, col) in self.ctx.ui_config.list.columns.iter().enumerate() {
            let title = match col {
                UserListColumnType::Graph => "Graph",
                UserListColumnType::Marker => "",
                UserListColumnType::Subject => "Description",
                UserListColumnType::Name => "Author",
                UserListColumnType::Hash => "Commit",
                UserListColumnType::Date => "Date",
            };
            if !title.is_empty() {
                Paragraph::new(Line::from(vec![
                    Span::raw(" "),
                    Span::styled(title, header_style),
                ]))
                .render(header_chunks[i], buf);
            }
        }

        let content_chunks = Layout::horizontal(constraints).split(content_area);

        for (i, col) in self.ctx.ui_config.list.columns.iter().enumerate() {
            match col {
                UserListColumnType::Graph => {
                    self.render_graph(buf, content_chunks[i], state);
                }
                UserListColumnType::Marker => {
                    self.render_marker(buf, content_chunks[i], state);
                }
                UserListColumnType::Subject => {
                    self.render_subject(buf, content_chunks[i], state);
                }
                UserListColumnType::Name => {
                    self.render_name(buf, content_chunks[i], state);
                }
                UserListColumnType::Hash => {
                    self.render_hash(buf, content_chunks[i], state);
                }
                UserListColumnType::Date => {
                    self.render_date(buf, content_chunks[i], state);
                }
            }
        }
    }
}

impl CommitList<'_> {
    fn update_state(&self, area: Rect, state: &mut CommitListState<'_>) {
        state.height = (area.height as usize).saturating_sub(state.inline_detail_height as usize);

        if state.total > state.height && state.total - state.height < state.offset {
            let diff = state.offset - (state.total - state.height);
            state.selected += diff;
            state.offset -= diff;
        }
        if state.selected >= state.height {
            let diff = state.selected - state.height + 1;
            state.selected -= diff;
            state.offset += diff;
        }
        // 可見列的 text cell 是由 render_graph 透過 rendering_commit_info_iter()
        // 隨需計算的，這也是「哪些列可見」的唯一真相來源 —— 不需要另外的
        // preload pass。
    }

    /// GraphStyle -> GlyphSet 選擇的唯一入口。
    fn glyphs(&self) -> GlyphSet {
        GlyphSet::from_style(self.ctx.graph_style)
    }

    fn render_graph(&self, buf: &mut Buffer, area: Rect, state: &CommitListState<'_>) {
        if area.is_empty() {
            return;
        }
        let gap = state.inline_detail_height;
        let head_hash = state.head_commit_hash.as_ref();
        let selected_bg = ratatui_color_to_rgb(self.ctx.color_theme.list_selected_bg);

        let head_col = head_hash.and_then(|h| self.graph_text_head_col(state, h));
        let virtual_row_visible = state.has_virtual_row() && state.offset == 0;

        if virtual_row_visible {
            let y = area.top();
            // ◯ fallback 次序：HEAD column → 第一個可見 commit 的 dot column → 0
            let col = head_col.unwrap_or_else(|| {
                state
                    .first_visible_commit_hash()
                    .and_then(|h| self.graph_text_head_col(state, h))
                    .unwrap_or(0)
            });
            self.put_text_cell(buf, area, y, col, Glyph::HeadDot, VIRTUAL_ROW_COLOR);
            if state.selected == 0 {
                apply_row_bg(buf, area, y, selected_bg);
            }
        }

        let head_line_col = head_col.filter(|_| virtual_row_visible);
        let mut seen_head = false;
        for (display_i, _, commit_info) in self.rendering_commit_info_iter(state) {
            let y_offset = if gap > 0 && display_i > state.selected {
                gap
            } else {
                0
            };
            let y = area.top() + display_i as u16 + y_offset;
            if y >= area.bottom() {
                continue;
            }
            let hash = &commit_info.commit.commit_hash;
            // 這裡的 `None` 現在只代表一種情況：`hash` 不在
            // `current_graph().commit_pos_map` 裡 —— 也就是 graph 跟
            // commit list 不同步了。因為 text cell 是隨需計算的，已經沒有
            // 「還沒 preload」這種情況存在了。
            let Some(cells) = state.text_cells_for_hash(hash) else {
                continue;
            };
            let is_head = head_hash == Some(hash);
            let is_selected = display_i == state.selected;
            self.put_text_cells(buf, area, y, &cells, is_head);

            if !seen_head {
                if is_head {
                    seen_head = true;
                } else if let Some(hc) = head_line_col {
                    if cells.get(hc).is_some_and(|c| c.glyph == Glyph::Blank) {
                        self.put_text_cell(buf, area, y, hc, Glyph::Vert, VIRTUAL_ROW_COLOR);
                    }
                }
            }

            if is_selected {
                apply_row_bg(buf, area, y, selected_bg);
            }
        }

        // Spacer rows（inline detail 的間隔列）：在每個有效欄畫上 `│`。
        if gap > 0 {
            let spacer_hash = if state.is_virtual_row_selected() {
                state.first_visible_commit_hash().cloned()
            } else {
                Some(
                    state
                        .commit(state.current_selected_raw())
                        .commit
                        .commit_hash
                        .clone(),
                )
            };
            if let Some(hash) = spacer_hash {
                if let Some(cells) = state.text_cells_for_hash(&hash) {
                    let gray = state.is_virtual_row_selected();
                    for gap_row in 0..gap {
                        let y = area.top() + state.selected as u16 + 1 + gap_row;
                        if y >= area.bottom() {
                            break;
                        }
                        self.put_text_spacer(buf, area, y, &cells, gray);
                    }
                }
            }
        }
    }

    /// 回傳 `hash` 在目前 graph 上的 text-graph 欄位（以 cell 為單位，不是
    /// char），不存在則回傳 None。
    fn graph_text_head_col(&self, state: &CommitListState<'_>, hash: &CommitHash) -> Option<usize> {
        let cells = state.text_cells_for_hash(hash)?;
        cells.iter().position(|c| c.glyph.is_dot())
    }

    fn put_text_cells(
        &self,
        buf: &mut Buffer,
        area: Rect,
        y: u16,
        cells: &[TextCell],
        is_head: bool,
    ) {
        let glyphs = self.glyphs();
        for (i, cell) in cells.iter().enumerate() {
            let x = area.left() + i as u16;
            if x >= area.right() {
                break;
            }
            let (glyph, bold) = if is_head && cell.glyph == Glyph::CommitDot {
                (Glyph::HeadDot, true)
            } else {
                (cell.glyph, false)
            };
            let s = glyphs.resolve(glyph);
            let mut style = Style::default().fg(cell.color);
            if bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            buf[(x, y)].set_symbol(s).set_style(style);
        }
    }

    fn put_text_spacer(
        &self,
        buf: &mut Buffer,
        area: Rect,
        y: u16,
        cells: &[TextCell],
        gray: bool,
    ) {
        let glyphs = self.glyphs();
        for (i, cell) in cells.iter().enumerate() {
            let x = area.left() + i as u16;
            if x >= area.right() {
                break;
            }
            // 哪幾欄要把線接下去是 `Glyph` 該回答的事，不是這個迴圈 ——
            // 見 `Glyph::extends_downward`。至於這一欄究竟有沒有被合併成
            // junction，那是 `Column::can_merge` 的判斷，多色欄位下取決於
            // `graph.color.branches`。
            if !cell.glyph.extends_downward() {
                continue;
            }
            let color = if gray { VIRTUAL_ROW_COLOR } else { cell.color };
            let s = glyphs.resolve(Glyph::Vert);
            buf[(x, y)]
                .set_symbol(s)
                .set_style(Style::default().fg(color));
        }
    }

    fn put_text_cell(
        &self,
        buf: &mut Buffer,
        area: Rect,
        y: u16,
        col: usize,
        glyph: Glyph,
        color: Color,
    ) {
        let x = area.left() + col as u16;
        if x >= area.right() {
            return;
        }
        let s = self.glyphs().resolve(glyph);
        buf[(x, y)]
            .set_symbol(s)
            .set_style(Style::default().fg(color));
    }

    fn render_marker(&self, buf: &mut Buffer, area: Rect, state: &CommitListState<'_>) {
        if area.is_empty() {
            return;
        }
        let gap = state.inline_detail_height;
        let vert = self.glyphs().vert;
        let mut items: Vec<ListItem> = Vec::new();
        if state.has_virtual_row() && state.offset == 0 {
            let mut line = Line::from(vert.fg(Color::Gray));
            if state.selected == 0 {
                line = line
                    .bg(ratatui_color_to_rgb(self.ctx.color_theme.list_selected_bg))
                    .fg(Color::Gray);
            }
            items.push(ListItem::new(line));
            // 當 virtual row 被選中時，插入 marker 的間隔
            if gap > 0 && state.selected == 0 {
                for _ in 0..gap {
                    items.push(ListItem::new(vert.fg(Color::Gray)));
                }
            }
        }
        self.rendering_commit_info_iter(state)
            .for_each(|(display_i, _, commit_info)| {
                let color = state.marker_color(commit_info);
                let mut line = Line::from(vert.fg(color));
                if display_i == state.selected {
                    line = line.bg(ratatui_color_to_rgb(self.ctx.color_theme.list_selected_bg));
                }
                items.push(ListItem::new(line));
                if gap > 0 && display_i == state.selected && !state.is_virtual_row_selected() {
                    let sel_color = state.marker_color(state.commit(state.current_selected_raw()));
                    for _ in 0..gap {
                        items.push(ListItem::new(vert.fg(sel_color)));
                    }
                }
            });
        Widget::render(List::new(items), area, buf)
    }

    fn insert_gap<'b>(
        items: &mut Vec<ListItem<'b>>,
        state: &CommitListState<'_>,
        is_virtual: bool,
        display_i: usize,
    ) {
        let gap = state.inline_detail_height;
        if gap == 0 {
            return;
        }
        let should_insert = if is_virtual {
            state.is_virtual_row_selected()
        } else {
            display_i == state.selected && !state.is_virtual_row_selected()
        };
        if should_insert {
            for _ in 0..gap {
                items.push(ListItem::new(Line::raw("")));
            }
        }
    }

    fn render_subject(&self, buf: &mut Buffer, area: Rect, state: &CommitListState<'_>) {
        let max_width = (area.width as usize).saturating_sub(2);
        if area.is_empty() || max_width == 0 {
            state.selected_row_overflows.set(false);
            return;
        }
        let mut items: Vec<ListItem> = Vec::new();
        let mut any_selected_overflow = false;
        let marquee_frame = self.marquee_frame;
        let selected = state.selected;
        // Virtual row（虛擬列）
        if state.has_virtual_row() && state.offset == 0 {
            let count = state.working_changes().map_or(0, |wc| wc.file_count());
            let text = format!("Uncommitted Changes ({count})");
            let spans = vec![Span::styled(
                text,
                Style::default()
                    .fg(VIRTUAL_ROW_COLOR)
                    .add_modifier(Modifier::ITALIC),
            )];
            items.push(self.to_commit_list_item(0, spans, state));
            Self::insert_gap(&mut items, state, true, 0);
        }
        self.rendering_commit_info_iter(state)
            .for_each(|(display_i, raw, commit_info)| {
                let mut spans = refs_spans(
                    commit_info,
                    &state.head,
                    &state.search_match(raw).refs,
                    &self.ctx.color_theme,
                    state.show_remote_refs,
                );
                let ref_spans_width: usize = spans.iter().map(|s| s.width()).sum();
                let avail = max_width.saturating_sub(ref_spans_width);
                let commit = &commit_info.commit;
                if avail > ELLIPSIS.len() {
                    // byte-len 是視覺寬度的下界（ASCII 相等、非 ASCII byte 更多），
                    // 用它先短路大多數「明顯放得下」的 row，省一次寬度計算。
                    // 寬度基準必須跟 `scroll_window` 一致，理由見 `marquee::display_width`。
                    let overflow = commit.subject.len() > avail
                        && crate::widget::marquee::display_width(&commit.subject) > avail;
                    let is_selected = display_i == selected;
                    let search_pos = state.search_match(raw).subject.as_ref();
                    let sub_spans = if is_selected && overflow {
                        any_selected_overflow = true;
                        marquee_subject_spans(
                            &commit.subject,
                            avail,
                            marquee_frame,
                            search_pos,
                            &self.ctx.color_theme,
                        )
                    } else {
                        let subject = if overflow {
                            console::truncate_str(&commit.subject, avail, ELLIPSIS).to_string()
                        } else {
                            commit.subject.to_string()
                        };
                        if let Some(pos) = search_pos {
                            highlighted_spans(
                                subject.into(),
                                pos.clone(),
                                self.ctx.color_theme.list_subject_fg,
                                Modifier::empty(),
                                &self.ctx.color_theme,
                                overflow,
                            )
                        } else {
                            vec![subject.fg(self.ctx.color_theme.list_subject_fg)]
                        }
                    };
                    spans.extend(sub_spans);
                }
                items.push(self.to_commit_list_item(display_i, spans, state));
                Self::insert_gap(&mut items, state, false, display_i);
            });
        state.selected_row_overflows.set(any_selected_overflow);
        Widget::render(List::new(items), area, buf);
    }

    fn render_name(&self, buf: &mut Buffer, area: Rect, state: &CommitListState<'_>) {
        let max_width = (area.width as usize).saturating_sub(2);
        if area.is_empty() || max_width == 0 {
            return;
        }
        let mut items: Vec<ListItem> = Vec::new();
        if state.has_virtual_row() && state.offset == 0 {
            items.push(self.to_commit_list_item(0, vec!["-".fg(VIRTUAL_ROW_COLOR)], state));
            Self::insert_gap(&mut items, state, true, 0);
        }
        self.rendering_commit_iter(state)
            .for_each(|(display_i, raw, commit)| {
                let truncate = console::measure_text_width(&commit.author_name) > max_width;
                let name = if truncate {
                    console::truncate_str(&commit.author_name, max_width, ELLIPSIS).to_string()
                } else {
                    commit.author_name.to_string()
                };
                let spans = if let Some(pos) = state.search_match(raw).author_name.clone() {
                    highlighted_spans(
                        name.into(),
                        pos,
                        self.ctx.color_theme.list_name_fg,
                        Modifier::empty(),
                        &self.ctx.color_theme,
                        truncate,
                    )
                } else {
                    vec![name.fg(self.ctx.color_theme.list_name_fg)]
                };
                items.push(self.to_commit_list_item(display_i, spans, state));
                Self::insert_gap(&mut items, state, false, display_i);
            });
        Widget::render(List::new(items), area, buf);
    }

    fn render_hash(&self, buf: &mut Buffer, area: Rect, state: &CommitListState<'_>) {
        if area.is_empty() {
            return;
        }
        let mut items: Vec<ListItem> = Vec::new();
        if state.has_virtual_row() && state.offset == 0 {
            items.push(self.to_commit_list_item(0, vec!["-".fg(VIRTUAL_ROW_COLOR)], state));
            Self::insert_gap(&mut items, state, true, 0);
        }
        self.rendering_commit_iter(state)
            .for_each(|(display_i, raw, commit)| {
                let hash = commit.commit_hash.as_short_hash();
                let spans = if let Some(pos) = state.search_match(raw).commit_hash.clone() {
                    highlighted_spans(
                        hash.into(),
                        pos,
                        self.ctx.color_theme.list_hash_fg,
                        Modifier::empty(),
                        &self.ctx.color_theme,
                        false,
                    )
                } else {
                    vec![hash.fg(self.ctx.color_theme.list_hash_fg)]
                };
                items.push(self.to_commit_list_item(display_i, spans, state));
                Self::insert_gap(&mut items, state, false, display_i);
            });
        Widget::render(List::new(items), area, buf);
    }

    fn render_date(&self, buf: &mut Buffer, area: Rect, state: &CommitListState<'_>) {
        if area.is_empty() {
            return;
        }
        let mut items: Vec<ListItem> = Vec::new();
        if state.has_virtual_row() && state.offset == 0 {
            items.push(self.to_commit_list_item(0, vec!["-".fg(VIRTUAL_ROW_COLOR)], state));
            Self::insert_gap(&mut items, state, true, 0);
        }
        self.rendering_commit_iter(state)
            .for_each(|(display_i, _raw, commit)| {
                let date = &commit.author_date;
                let date_str = if self.ctx.ui_config.list.date_local {
                    let local = date.with_timezone(&chrono::Local);
                    local
                        .format(&self.ctx.ui_config.list.date_format)
                        .to_string()
                } else {
                    date.format(&self.ctx.ui_config.list.date_format)
                        .to_string()
                };
                items.push(self.to_commit_list_item(
                    display_i,
                    vec![date_str.fg(self.ctx.color_theme.list_date_fg)],
                    state,
                ));
                Self::insert_gap(&mut items, state, false, display_i);
            });
        Widget::render(List::new(items), area, buf);
    }

    /// 回傳 (display_idx, raw_idx, &CommitInfo) 的 iterator
    /// display_idx：畫面上的位置（0, 1, 2, ...）
    /// raw_idx：commits Vec 裡的實際索引（供存取 search_matches 用）
    /// 會跳過 virtual row（如果存在且可見的話）。
    fn rendering_commit_info_iter<'b>(
        &'b self,
        state: &'b CommitListState<'_>,
    ) -> impl Iterator<Item = (usize, RawCommitIdx, &'b CommitInfo<'b>)> {
        let vr_offset = state.virtual_row_offset();
        let total_visible = state.height.min(state.total.saturating_sub(state.offset));
        let start = if state.offset == 0 { vr_offset } else { 0 };
        (start..total_visible).filter_map(move |display_idx| {
            let visible_idx = state.offset + display_idx;
            let filtered = FilteredIdx(visible_idx - vr_offset);
            let raw = state.filtered_to_raw(filtered)?;
            Some((display_idx, raw, state.commit(raw)))
        })
    }

    fn rendering_commit_iter<'b>(
        &'b self,
        state: &'b CommitListState<'_>,
    ) -> impl Iterator<Item = (usize, RawCommitIdx, &'b Commit)> {
        self.rendering_commit_info_iter(state)
            .map(|(display_i, raw, commit_info)| (display_i, raw, commit_info.commit))
    }

    fn to_commit_list_item<'a, 'b>(
        &'b self,
        i: usize,
        spans: Vec<Span<'a>>,
        state: &'b CommitListState<'_>,
    ) -> ListItem<'a> {
        let mut spans = spans;
        spans.insert(0, Span::raw(" "));
        spans.push(Span::raw(" "));
        let mut line = Line::from(spans);
        if i == state.selected {
            line = line
                .bg(ratatui_color_to_rgb(self.ctx.color_theme.list_selected_bg))
                .fg(self.ctx.color_theme.list_selected_fg);
        }
        ListItem::new(line)
    }
}

fn apply_row_bg(buf: &mut Buffer, area: Rect, y: u16, bg: Color) {
    // 只覆寫 bg 這個 channel，讓先前寫進 graph cell 的 fg/modifier 得以保留。
    for x in area.left()..area.right() {
        buf[(x, y)].set_bg(bg);
    }
}

fn refs_spans<'a>(
    commit_info: &'a CommitInfo<'_>,
    head: &'a Head,
    refs_matches: &'a FxHashMap<String, SearchMatchPosition>,
    color_theme: &'a ColorTheme,
    show_remote_refs: bool,
) -> Vec<Span<'a>> {
    let refs = &commit_info.refs;

    if refs.len() == 1 {
        if let Ref::Stash { name, .. } = refs[0] {
            return vec![
                Span::raw(name.clone())
                    .fg(color_theme.list_ref_stash_fg)
                    .bold(),
                Span::raw(" "),
            ];
        }
    }

    let is_head_branch = |n: &str| matches!(head, Head::Branch { name: hn } if hn == n);
    // tag arm 高亮條件：detached HEAD 指向此 commit。
    let is_head_detached_here = matches!(
        head,
        Head::Detached { target } if commit_info.commit.commit_hash == *target,
    );

    let ref_spans: Vec<(Vec<Span>, &String)> = refs
        .iter()
        .filter_map(|r| match r {
            Ref::Branch { name, .. } => {
                // 如果存在對應的 remote branch，隱藏本地分支（HEAD branch 也適用：
                // 此時 RemoteBranch arm 內會把對應的 dev 部分高亮表達 HEAD）。
                let has_remote = refs.iter().any(|r| {
                    matches!(r, Ref::RemoteBranch { name: rn, .. } if rn.ends_with(&format!("/{name}")))
                });
                if has_remote && show_remote_refs {
                    return None;
                }
                let is_head = is_head_branch(name);
                let fg = color_theme.list_ref_branch_fg;
                let mut spans = refs_matches
                    .get(name)
                    .map(|pos| {
                        highlighted_spans(
                            name.into(),
                            pos.clone(),
                            fg,
                            Modifier::BOLD,
                            color_theme,
                            false,
                        )
                    })
                    .unwrap_or_else(|| vec![Span::raw(name).fg(fg).bold()]);
                if is_head {
                    spans = highlight_as_head(spans, color_theme);
                }
                Some((spans, name))
            }
            Ref::RemoteBranch { name, .. } => {
                if !show_remote_refs {
                    return None;
                }
                // 三段分色：remote(紅) + /(paren色) + branch_name
                // 有對應本地分支 → branch_name 綠色，否則紅色
                let spans = if let Some(slash_pos) = name.find('/') {
                    let remote_part = &name[..slash_pos];
                    let branch_part = &name[slash_pos + 1..];
                    let has_local = refs.iter().any(|r| {
                        matches!(r, Ref::Branch { name: ln, .. } if ln == branch_part)
                    });
                    if has_local {
                        let is_head = is_head_branch(branch_part);
                        let mut branch_span = Span::raw(branch_part.to_string()).bold();
                        branch_span = if is_head {
                            branch_span
                                .fg(Color::Black)
                                .bg(color_theme.list_head_fg)
                        } else {
                            branch_span.fg(color_theme.list_ref_branch_fg)
                        };
                        vec![
                            Span::raw(remote_part.to_string())
                                .fg(color_theme.list_ref_remote_branch_fg)
                                .bold(),
                            Span::raw("/")
                                .fg(color_theme.list_ref_paren_fg)
                                .bold(),
                            branch_span,
                        ]
                    } else {
                        vec![Span::raw(name)
                            .fg(color_theme.list_ref_remote_branch_fg)
                            .bold()]
                    }
                } else {
                    vec![Span::raw(name)
                        .fg(color_theme.list_ref_remote_branch_fg)
                        .bold()]
                };
                Some((spans, name))
            }
            Ref::Tag { name, .. } => {
                let fg = color_theme.list_ref_tag_fg;
                let mut spans = refs_matches
                    .get(name)
                    .map(|pos| {
                        highlighted_spans(
                            name.into(),
                            pos.clone(),
                            fg,
                            Modifier::BOLD,
                            color_theme,
                            false,
                        )
                    })
                    .unwrap_or_else(|| vec![Span::raw(name).fg(fg).bold()]);
                if is_head_detached_here {
                    spans = highlight_as_head(spans, color_theme);
                }
                Some((spans, name))
            }
            Ref::Stash { .. } => None,
        })
        .collect();

    let mut spans = vec![Span::raw("(").fg(color_theme.list_ref_paren_fg).bold()];

    // HEAD（含 detached）由 graph 上的空心圓表達，文字不再顯示。

    let refs_len = ref_spans.len();
    for (i, ss) in ref_spans.into_iter().enumerate() {
        let (ref_spans, _ref_name) = ss;
        spans.extend(ref_spans);
        if i < refs_len - 1 {
            spans.push(Span::raw(", ").fg(color_theme.list_ref_paren_fg).bold());
        }
    }

    spans.push(Span::raw(") ").fg(color_theme.list_ref_paren_fg).bold());

    if spans.len() == 2 {
        spans.clear(); // 只剩下 "(" 和 ")"，所以清空
    }

    spans
}

fn highlight_as_head<'a>(spans: Vec<Span<'a>>, color_theme: &ColorTheme) -> Vec<Span<'a>> {
    spans
        .into_iter()
        .map(|s| s.fg(Color::Black).bg(color_theme.list_head_fg))
        .collect()
}

/// 回傳 marquee 視窗內的 subject spans。Scroll offset 由
/// `crate::widget::marquee::scroll_window` 處理；這邊只負責 search highlight。
fn marquee_subject_spans(
    subject: &str,
    available: usize,
    marquee_frame: u64,
    search_pos: Option<&SearchMatchPosition>,
    color_theme: &ColorTheme,
) -> Vec<Span<'static>> {
    let slice = crate::widget::marquee::scroll_window(subject, available, marquee_frame);

    if let Some(pos) = search_pos {
        let shift = if slice.prepended_space { 1 } else { 0 };
        let translated: Vec<usize> = pos
            .matched_indices
            .iter()
            .copied()
            .filter(|&bi| bi >= slice.start_byte && bi < slice.end_byte)
            .map(|bi| bi - slice.start_byte + shift)
            .collect();
        highlighted_spans(
            slice.text.into(),
            SearchMatchPosition::new(translated),
            color_theme.list_subject_fg,
            Modifier::empty(),
            color_theme,
            false,
        )
    } else {
        vec![slice.text.fg(color_theme.list_subject_fg)]
    }
}

fn highlighted_spans(
    s: Span<'_>,
    pos: SearchMatchPosition,
    base_fg: Color,
    base_modifier: Modifier,
    color_theme: &ColorTheme,
    truncate: bool,
) -> Vec<Span<'static>> {
    let mut hm = highlight_matched_text(vec![s])
        .matched_indices(pos.matched_indices)
        .not_matched_style(Style::default().fg(base_fg).add_modifier(base_modifier))
        .matched_style(
            Style::default()
                .fg(color_theme.list_match_fg)
                .bg(color_theme.list_match_bg)
                .add_modifier(base_modifier),
        );
    if truncate {
        hm = hm.ellipsis(ELLIPSIS);
    }
    hm.into_spans()
}

fn calc_cell_widths(
    area_width: u16,
    subject_min_width: u16,
    graph_width: u16,
    name_width: u16,
    date_width: u16,
    columns: &[UserListColumnType],
) -> Vec<Constraint> {
    let pad = 2;
    let (
        mut graph_cell_width,
        mut marker_cell_width,
        mut name_cell_width,
        mut hash_cell_width,
        mut date_cell_width,
    ) = (0, 0, 0, 0, 0);

    for col in columns {
        match col {
            UserListColumnType::Graph => {
                graph_cell_width = graph_width;
            }
            UserListColumnType::Marker => {
                marker_cell_width = 1;
            }
            UserListColumnType::Name => {
                name_cell_width = name_width + pad;
            }
            UserListColumnType::Hash => {
                hash_cell_width = 7 + pad;
            }
            UserListColumnType::Date => {
                date_cell_width = date_width + pad;
            }
            UserListColumnType::Subject => {}
        }
    }

    let mut total_width = graph_cell_width
        + marker_cell_width
        + hash_cell_width
        + name_cell_width
        + date_cell_width
        + subject_min_width;

    if total_width > area_width {
        total_width = total_width.saturating_sub(name_cell_width);
        name_cell_width = 0;
    }
    if total_width > area_width {
        total_width = total_width.saturating_sub(date_cell_width);
        date_cell_width = 0;
    }
    if total_width > area_width {
        hash_cell_width = 0;
    }

    let mut constraints = Vec::new();
    for col in columns {
        match col {
            UserListColumnType::Graph => {
                constraints.push(Constraint::Length(graph_cell_width));
            }
            UserListColumnType::Marker => {
                constraints.push(Constraint::Length(marker_cell_width));
            }
            UserListColumnType::Subject => {
                constraints.push(Constraint::Min(0));
            }
            UserListColumnType::Name => {
                constraints.push(Constraint::Length(name_cell_width));
            }
            UserListColumnType::Hash => {
                constraints.push(Constraint::Length(hash_cell_width));
            }
            UserListColumnType::Date => {
                constraints.push(Constraint::Length(date_cell_width));
            }
        }
    }
    constraints
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- render_graph_text --------------------------------------------
    //
    // CommitList 直接實作 StatefulWidget，所以不需要 Terminal /
    // TestBackend —— Buffer::empty() + render() 就夠了。
    // Fixture／常數放在自己的子 module 裡，避免跟 `mod tests` 其他部分
    // 的命名衝突。
    mod render_graph_tests {
        use super::*;
        use crate::{
            color::GraphColorSet,
            config::{CoreConfig, GraphColorConfig, UiConfig},
            git::{FileChange, WorkingChanges},
            graph::{CellWidthType, Edge, EdgeType, Graph, GraphStyle},
            keybind::KeyBind,
        };
        use rustc_hash::FxHashSet;

        const TERM_W: u16 = 80;

        const RED: Color = Color::Rgb(0xFF, 0x00, 0x00);
        const GREEN: Color = Color::Rgb(0x00, 0xFF, 0x00);
        const BLUE: Color = Color::Rgb(0x00, 0x00, 0xFF);
        // ratatui_color_to_rgb(ColorTheme::default().list_selected_bg) == DarkGray
        const SELECTED_BG: Color = Color::Rgb(80, 80, 80);

        fn test_ctx_styled(graph_style: GraphStyle) -> Rc<AppContext> {
            Rc::new(AppContext {
                keybind: KeyBind::new(None),
                core_config: CoreConfig::default(),
                ui_config: UiConfig::default(),
                color_theme: ColorTheme::default(),
                graph_style,
            })
        }

        fn test_graph_color_set() -> GraphColorSet {
            GraphColorSet::new(&GraphColorConfig {
                branches: vec!["#FF0000".into(), "#00FF00".into(), "#0000FF".into()],
            })
        }

        /// Commit hash 必須 >= 7 個字元：`render_hash` 用的是 `as_short_hash()`
        /// （`[0..7]`），太短會 panic。
        fn text_graph_commits() -> Vec<Commit> {
            [
                ("aaaaaaa", "first"),
                ("bbbbbbb", "second"),
                ("ccccccc", "third"),
            ]
            .into_iter()
            .map(|(hash, subject)| Commit {
                commit_hash: hash.into(),
                subject: subject.into(),
                author_name: "alice".into(),
                ..Default::default()
            })
            .collect()
        }

        /// 測試 1-4 共用的 3-commit graph。
        ///
        /// c0 的 dot 刻意放在 pos_x=1，而不是 pos_x=0：`render_graph_text` 的
        /// virtual-row 欄位 fallback 順序是 `head_col -> 第一個可見 commit 的
        /// dot 欄 -> 字面值 0`。如果 HEAD 的 dot 就在欄位 0，這三條路徑會全部
        /// 重合，即使有 regression 把欄位寫死成 0，也會在 virtual-row 測試裡
        /// 悄悄溜過去而不被發現。
        fn text_graph(commits: &[Commit]) -> Graph {
            graph_fixture(
                commits,
                [(1, 0), (0, 1), (2, 2)],
                vec![
                    vec![
                        Edge::new(EdgeType::Vertical, 0, 0),
                        Edge::new(EdgeType::Horizontal, 2, 2),
                    ],
                    vec![
                        Edge::new(EdgeType::Horizontal, 1, 2),
                        Edge::new(EdgeType::LeftTop, 2, 2),
                    ],
                    vec![Edge::new(EdgeType::Vertical, 0, 0)],
                ],
            )
        }

        /// 這三個 fixture 的差異只在於 dot 的位置，以及每一列帶了哪些
        /// edge。
        fn graph_fixture(
            commits: &[Commit],
            positions: [(usize, usize); 3],
            edges: Vec<Vec<Edge>>,
        ) -> Graph {
            Graph {
                commit_hashes: commits.iter().map(|c| c.commit_hash.clone()).collect(),
                commit_pos_map: commits
                    .iter()
                    .map(|c| c.commit_hash.clone())
                    .zip(positions)
                    .collect(),
                edges,
                max_pos_x: 2,
            }
        }

        /// 刻意跟 `text_graph` 用不同形狀（線性、三個 commit 都在 pos_x=0，
        /// 沒有合併），這樣 `filtered_graph_manager_fills_text_cells` 才能
        /// 判斷 `current_graph()` 是不是真的選到了這個 graph 而非 primary
        /// 那個 —— 如果兩個 fixture 渲染結果一樣，一個永遠回傳 primary
        /// graph 的 `current_graph()` 也會通過測試而不被發現。
        fn text_graph_filtered(commits: &[Commit]) -> Graph {
            graph_fixture(
                commits,
                [(0, 0), (0, 1), (0, 2)],
                vec![
                    vec![Edge::new(EdgeType::Vertical, 0, 0)],
                    vec![Edge::new(EdgeType::Vertical, 0, 0)],
                    vec![],
                ],
            )
        }

        /// 這裡每一欄都同時帶了兩條 edge，這是 `text_graph` 從來不會發生的
        /// 情況 —— 它的三列把每條 edge 各放在自己的欄位，所以在 `Single`
        /// 之下沒有任何一格會被共用。沒有這個 fixture，widget 這條路徑
        /// （`text_cells_for_hash` -> `put_text_cells` -> `GlyphSet::resolve`）
        /// 就完全沒有涵蓋到 junction glyph：
        /// `render_graph_single_width_folds_cells_and_sizes_column_correctly`
        /// 在 issue #29 修好前後會渲染出一樣的結果。
        fn text_graph_colliding(commits: &[Commit]) -> Graph {
            graph_fixture(
                commits,
                [(2, 0), (2, 1), (0, 2)],
                vec![
                    // 欄 0 和 1：一條垂直線被一段水平線穿過 -> `┼`
                    vec![
                        Edge::new(EdgeType::Vertical, 0, 0),
                        Edge::new(EdgeType::Horizontal, 0, 2),
                        Edge::new(EdgeType::Vertical, 1, 1),
                        Edge::new(EdgeType::Horizontal, 1, 2),
                    ],
                    // 欄 0：一個指向下方的 branch 端點，被水平線穿過
                    // -> `┬`（不是 `┼`：這裡沒有東西往上延續）
                    // 欄 1：轉角遇上水平線 -> `┴`
                    vec![
                        Edge::new(EdgeType::Down, 0, 0),
                        Edge::new(EdgeType::Horizontal, 0, 2),
                        Edge::new(EdgeType::RightBottom, 1, 1),
                        Edge::new(EdgeType::Horizontal, 1, 2),
                    ],
                    vec![Edge::new(EdgeType::Vertical, 1, 1)],
                ],
            )
        }

        /// `text_graph_colliding`裡的每次碰撞都是兩條*不同*的線擠在同一欄，
        /// 所以 `Double` 永遠不會在那裡合併。這裡改成每一列的欄 0 是同一條
        /// 線內部的碰撞，合併不會損失任何顏色，`Double` 就會把它們聯集起來。
        /// 第 1 列的欄 0 刻意設計成往上收掉的形狀（`┴`），這是 spacer 測試
        /// 需要的。
        ///
        /// 欄 1 則刻意保留成兩條線的碰撞，讓一個 fixture 就能並排涵蓋兩種
        /// 分支。沒有這個 fixture，`double_cells` 的合併分支在 widget
        /// 層級就完全沒有測試涵蓋到。
        fn text_graph_colliding_same_line(commits: &[Commit]) -> Graph {
            graph_fixture(
                commits,
                [(2, 0), (2, 1), (0, 2)],
                vec![
                    // 欄 0：同一條線的轉角加上它自己往下的延續 -> `┤`。
                    // 欄 1：兩條線，其中只有一條會消失 -> 維持 `│`，旁邊配上
                    // `─`。
                    vec![
                        Edge::new(EdgeType::RightBottom, 0, 0),
                        Edge::new(EdgeType::Down, 0, 0),
                        Edge::new(EdgeType::Vertical, 1, 1),
                        Edge::new(EdgeType::Horizontal, 1, 2),
                    ],
                    // 欄 0：同一條線的轉角被它自己的水平線段穿過
                    // -> `┴`，收尾是往上的。
                    vec![
                        Edge::new(EdgeType::RightBottom, 0, 0),
                        Edge::new(EdgeType::Horizontal, 0, 0),
                        Edge::new(EdgeType::Vertical, 1, 1),
                    ],
                    vec![Edge::new(EdgeType::Vertical, 1, 1)],
                ],
            )
        }

        struct Opts {
            /// `commits` 的索引，會餵給 `CommitListState` 共用的
            /// `head_commit_hash`（驅動 `put_text_cells` 裡的 `is_head`）。
            /// 跟 `CommitListState` 自己的 `head: Head` 欄位不同，後者只影響
            /// ref 的渲染，跟這些測試無關。
            head_hash: Option<usize>,
            working_changes: bool,
            inline_detail_height: u16,
            /// 為 true 時會另外建一個第二個 `Graph` fixture（跟 primary 同
            /// 形狀），透過 `filtered` 和 `set_show_remote_refs(false)`
            /// 讓渲染走它這條路。這是 `render_graph` 的 filtered 分支唯一
            /// 會被跑到的路徑 —— 沒有它，那個分支的測試涵蓋率就是零。
            filtered: bool,
            cell_width_type: CellWidthType,
        }

        impl Default for Opts {
            fn default() -> Self {
                Self {
                    head_hash: Some(0),
                    working_changes: false,
                    inline_detail_height: 0,
                    filtered: false,
                    cell_width_type: CellWidthType::Double,
                }
            }
        }

        fn build_state(commits: &[Commit], graph: Graph, opts: Opts) -> CommitListState<'_> {
            let head_hash = opts.head_hash.map(|i| commits[i].commit_hash.clone());
            let graph_colors: Vec<Color> = test_graph_color_set()
                .colors
                .iter()
                .map(|c| c.to_ratatui_color())
                .collect();
            let infos = commits
                .iter()
                .map(|c| CommitInfo::new(c, Vec::new(), Color::Reset))
                .collect();
            let working = opts.working_changes.then(|| WorkingChanges {
                staged: vec![FileChange::Modify {
                    path: "src/a.rs".into(),
                    stats: None,
                }],
                unstaged: Vec::new(),
            });
            let filtered = opts.filtered.then(|| Rc::new(text_graph_filtered(commits)));
            let mut state = CommitListState::new(
                infos,
                Rc::new(graph),
                graph_colors,
                head_hash,
                opts.cell_width_type,
                Head::None,
                FxHashMap::default(),
                false,
                false,
                filtered,
                None,
                FxHashSet::default(),
                working,
            );
            if opts.filtered {
                state.set_show_remote_refs(false);
            }
            state.set_inline_detail_height(opts.inline_detail_height);
            state
        }

        fn render_commit_list(state: &mut CommitListState<'_>, height: u16) -> Buffer {
            render_commit_list_styled(state, height, GraphStyle::Rounded)
        }

        fn render_commit_list_styled(
            state: &mut CommitListState<'_>,
            height: u16,
            graph_style: GraphStyle,
        ) -> Buffer {
            let ctx = test_ctx_styled(graph_style);
            assert!(
                matches!(
                    ctx.ui_config.list.columns.first(),
                    Some(UserListColumnType::Graph)
                ),
                "fixture assumes Graph is the first column"
            );
            let area = Rect::new(0, 0, TERM_W, height);
            let mut buf = Buffer::empty(area);
            CommitList::new(ctx, 0).render(area, &mut buf, state);
            buf
        }

        /// 只取 Graph 欄（x 在 0..6 範圍），`rows` 裡每一列各回傳一個字串。
        fn graph_rows(buf: &Buffer, rows: std::ops::RangeInclusive<u16>) -> Vec<String> {
            graph_rows_width(buf, rows, 6)
        }

        /// 跟 `graph_rows` 一樣，但用於比每個 double-width 測試固定用的
        /// `0..6` 更窄或更寬的 graph 欄 —— single-width fixture 需要，
        /// 因為那裡的欄寬是 3 個 cell。
        fn graph_rows_width(
            buf: &Buffer,
            rows: std::ops::RangeInclusive<u16>,
            width: u16,
        ) -> Vec<String> {
            rows.map(|y| (0..width).map(|x| buf[(x, y)].symbol()).collect())
                .collect()
        }

        #[test]
        fn render_graph_draws_cells_below_header() {
            let commits = text_graph_commits();
            let mut state = build_state(&commits, text_graph(&commits), Opts::default());
            let buf = render_commit_list(&mut state, 10);

            assert_eq!(
                graph_rows(&buf, 1..=3),
                ["│ ◯ ──", "● ──╭─", "│   ● "],
                "row 1..3 are c0(HEAD)/c1/c2; row 0 is the header"
            );

            // c0（HEAD，被選中）：dot 欄是綠色（pos_x=1），欄 0 的 Vertical
            // edge 是紅色（line 0），Horizontal edge 是藍色（line 2）——
            // 顏色跟的是 associated_line_pos_x，不是 pos_x。
            assert_eq!(buf[(0, 1)].fg, RED);
            assert_eq!(buf[(2, 1)].fg, GREEN);
            assert_eq!(buf[(4, 1)].fg, BLUE);
            assert!(buf[(2, 1)].modifier.contains(Modifier::BOLD));
            for x in 0..7 {
                assert_eq!(buf[(x, 1)].bg, SELECTED_BG, "selected row bg at x={x}");
            }

            // c1（未被選中）：沒有覆寫 bg。
            assert_ne!(buf[(0, 2)].bg, SELECTED_BG);
        }

        #[test]
        fn render_graph_single_width_folds_cells_and_sizes_column_correctly() {
            // #21：這個 bug 是 `graph_area_cell_width()` 跟 `build_text_cells`
            // 對「一個 `Single` graph 欄需要幾格」意見不一致。Snapshot
            // 測試抓不到這個問題 —— 它們只看得到 cell 的內容，看不到 widget
            // 實際配置的欄寬。這裡是唯一能觀察到欄寬的地方。
            let commits = text_graph_commits();
            let mut state = build_state(
                &commits,
                text_graph(&commits),
                Opts {
                    cell_width_type: CellWidthType::Single,
                    ..Default::default()
                },
            );
            let buf = render_commit_list(&mut state, 10);

            assert_eq!(
                graph_rows_width(&buf, 1..=3, 3),
                ["│◯─", "●─╭", "│ ●"],
                "one edge per column: each cell resolves to that edge's own glyph"
            );

            // Marker 欄緊接在 graph 欄之後（寬度是 cell_count(3) + 1 個
            // pad = 4）。如果 `graph_area_cell_width()` 還是照 `Double`
            // 的大小算（7），這個 `│` 會落在比這裡再往右一格的位置。
            assert_eq!(buf[(4, 1)].symbol(), "│");
        }

        /// Issue #29：唯一驗證 junction glyph 真的畫到畫面上的 end-to-end
        /// 檢查。修好之前，這些欄位會渲染成 `│`/`╯`，整段水平線都被吃掉。
        #[test]
        fn render_graph_single_width_draws_junctions_where_edges_collide() {
            let commits = text_graph_commits();
            let mut state = build_state(
                &commits,
                text_graph_colliding(&commits),
                Opts {
                    cell_width_type: CellWidthType::Single,
                    ..Default::default()
                },
            );
            let buf = render_commit_list(&mut state, 10);

            assert_eq!(
                graph_rows_width(&buf, 1..=3, 3),
                ["┼┼◯", "┬┴●", "●│ "],
                "colliding edges combine instead of the loser vanishing"
            );
        }

        /// 同一個 fixture 換成 `Double`。這裡每次碰撞都是兩條不同的線，
        /// 合併會抹掉一種顏色，所以這些欄位維持贏者全拿 —— 跟前面的
        /// `Single` 不一樣，`Single` 沒有第二個半格可以退讓，永遠都會聯集。
        #[test]
        fn render_graph_double_width_keeps_winner_when_lines_differ() {
            let commits = text_graph_commits();
            let mut state = build_state(&commits, text_graph_colliding(&commits), Opts::default());
            let buf = render_commit_list(&mut state, 10);

            assert_eq!(
                graph_rows_width(&buf, 1..=3, 6),
                ["│─│─◯ ", "│─╯─● ", "● │   "],
                "multi-coloured columns give each half-cell to the winning edge"
            );
        }

        /// 同一條線內部的碰撞：沒有東西要抹掉，所以 `Double` 會把它們聯集，
        /// 符號的兩半跟 `Single` 畫出來的一致。
        #[test]
        fn render_graph_double_width_unions_same_line_collisions() {
            let commits = text_graph_commits();
            let mut state = build_state(
                &commits,
                text_graph_colliding_same_line(&commits),
                Opts::default(),
            );
            let buf = render_commit_list(&mut state, 10);

            assert_eq!(
                graph_rows_width(&buf, 1..=3, 6),
                ["┤ │─◯ ", "┴─│ ● ", "● │   "],
                "single-coloured columns merge, the two-line column doesn't"
            );
        }

        #[test]
        fn render_graph_uses_ascii_style() {
            // 只有這個測試會走 AppContext -> glyphs() -> put_text_cell 這條路；
            // tests/graph.rs 是直接呼叫 GlyphSet::resolve()，從不會跑到這條
            // 串接，所以這是「-s ascii 真的會改變畫面內容」唯一的 end-to-end
            // 涵蓋。
            let commits = text_graph_commits();
            let mut state = build_state(&commits, text_graph(&commits), Opts::default());
            let buf = render_commit_list_styled(&mut state, 10, GraphStyle::Ascii);

            assert_eq!(
                graph_rows(&buf, 1..=3),
                ["| o --", "* --+-", "|   * "],
                "ascii style substitutes every box-drawing glyph 1:1"
            );

            // Marker 欄緊接在 graph 欄之後（寬度 7 = graph_area_cell_width
            // 的 `w + 1`），而且共用同一個 `self.glyphs()` 呼叫，所以風格
            // 也必須跟著切換。
            assert_eq!(buf[(7, 1)].symbol(), "|");
        }

        #[test]
        fn head_commit_dot_is_hollow_and_bold() {
            let commits = text_graph_commits();

            let mut with_head = build_state(&commits, text_graph(&commits), Opts::default());
            let buf_a = render_commit_list(&mut with_head, 10);
            assert_eq!(buf_a[(2, 1)].symbol(), "◯");
            assert!(buf_a[(2, 1)].modifier.contains(Modifier::BOLD));

            let mut without_head = build_state(
                &commits,
                text_graph(&commits),
                Opts {
                    head_hash: None,
                    ..Default::default()
                },
            );
            let buf_b = render_commit_list(&mut without_head, 10);
            assert_eq!(buf_b[(2, 1)].symbol(), "●");
            assert!(!buf_b[(2, 1)].modifier.contains(Modifier::BOLD));
            // 顏色不受是否為 head 影響。
            assert_eq!(buf_b[(2, 1)].fg, GREEN);

            // c1/c2 在兩種狀態下都不是 HEAD：一律是實心 dot，不會加粗。
            for buf in [&buf_a, &buf_b] {
                assert_eq!(buf[(0, 2)].symbol(), "●");
                assert!(!buf[(0, 2)].modifier.contains(Modifier::BOLD));
                assert_eq!(buf[(4, 3)].symbol(), "●");
                assert!(!buf[(4, 3)].modifier.contains(Modifier::BOLD));
            }
        }

        #[test]
        fn spacer_row_extends_only_vertical_columns() {
            let commits = text_graph_commits();
            let mut state = build_state(
                &commits,
                text_graph(&commits),
                Opts {
                    inline_detail_height: 2,
                    ..Default::default()
                },
            );
            let buf = render_commit_list(&mut state, 10);

            // 選中列是 c0（pos_y=0）：cells 是 [Vert, ' ', Dot, ' ', Horiz, Horiz]。
            // 只有 Vertical（idx0）與 Dot（idx2）會往下延伸 ——
            // Horizontal 那一對（idx4/5）絕對不可以。
            for y in [2u16, 3u16] {
                assert_eq!(buf[(0, y)].symbol(), "│", "vertical edge extends at y={y}");
                assert_eq!(buf[(0, y)].fg, RED);
                assert_eq!(buf[(2, y)].symbol(), "│", "dot column extends at y={y}");
                assert_eq!(buf[(2, y)].fg, GREEN);
                assert_ne!(
                    buf[(4, y)].symbol(),
                    "─",
                    "TEXT_HORIZ must not extend into the spacer row at y={y}"
                );
                assert_eq!(buf[(4, y)].symbol(), " ");
            }

            // c1/c2 因為這 2 列的間隔而被往下推。
            assert_eq!(graph_rows(&buf, 4..=5), ["● ──╭─", "│   ● "]);
        }

        /// 一條往上收掉的線，底下沒有東西可以接，而且不論這一欄最後變成哪個
        /// 字元都一樣。兩個 fixture 是同一個「轉角撞水平線」的形狀，只是收斂
        /// 方式不同：`Double` 合併了那一欄就是 `┴`，保留 winner 就是 `╯`。
        /// issue #31 的 bug 是後者以前照樣往下延伸，於是兩條相撞的 edge 是什麼
        /// 顏色，決定了 spacer row 那一格有沒有線。
        ///
        /// `text_graph` 兩種都碰不到（每欄只有一條 edge，永遠不會相撞），
        /// 所以只有這兩個相撞 fixture 有覆蓋到。
        #[test]
        fn spacer_row_stops_wherever_the_column_ends_going_up() {
            let commits = text_graph_commits();

            let spacer_symbols = |graph: Graph| {
                let mut state = build_state(
                    &commits,
                    graph,
                    Opts {
                        inline_detail_height: 1,
                        ..Default::default()
                    },
                );
                // 在 render 設定過 `height` 之前，`select_next` 是 no-op，
                // 所以第一次 render 純粹是為了讓第二次落在第 1 列，
                // 其 spacer 才會落在 y=3。
                render_commit_list(&mut state, 10);
                state.select_next();
                let buf = render_commit_list(&mut state, 10);
                [0u16, 2, 4].map(|x| buf[(x, 3u16)].symbol().to_string())
            };

            // 第 1 列是 `┴─│ ●`：被合併的那一欄往上收掉。
            assert_eq!(
                spacer_symbols(text_graph_colliding_same_line(&commits)),
                [" ", "│", "│"],
                "┴ does not extend downward, the vertical and the dot do"
            );
            // 第 1 列是 `│─╯─●`：同一個形狀沒被合併就是 `╯`，跟 `┴` 一樣是往上
            // 收掉。x=0 那一欄是 `Down` 被 `Horizontal` 穿過，所以它會延伸。
            assert_eq!(
                spacer_symbols(text_graph_colliding(&commits)),
                ["│", " ", "│"],
                "an unmerged corner stops too"
            );
        }

        #[test]
        fn virtual_row_draws_gray_head_dot_at_top() {
            let commits = text_graph_commits();
            let mut state = build_state(
                &commits,
                text_graph(&commits),
                Opts {
                    working_changes: true,
                    inline_detail_height: 1,
                    ..Default::default()
                },
            );
            let buf = render_commit_list(&mut state, 10);

            // Virtual row 在 buffer 第 1 列，dot 在 HEAD 的欄位（idx2，不是 0）。
            assert_eq!(buf[(2, 1)].symbol(), "◯");
            assert_eq!(
                buf[(2, 1)].fg,
                Color::Gray,
                "VIRTUAL_ROW_COLOR, not theme-converted"
            );
            assert!(!buf[(2, 1)].modifier.contains(Modifier::BOLD));
            for x in 0..7 {
                assert_eq!(
                    buf[(x, 1)].bg,
                    SELECTED_BG,
                    "virtual row is selected by default"
                );
            }

            // 灰色 spacer row（virtual row 被選中 + gap=1）先出現，在第 2 列
            // —— `y_offset` 會在 spacer 本身之前，把每個排在 `state.selected`
            // （0，virtual row）之後的 commit 往下推 `gap` 格。凡是有重繪的
            // cell 都必須用 VIRTUAL_ROW_COLOR，不能用自己的顏色。
            let spacer_y = 2u16;
            assert_eq!(buf[(0, spacer_y)].symbol(), "│");
            assert_eq!(buf[(0, spacer_y)].fg, Color::Gray);
            assert_eq!(buf[(2, spacer_y)].symbol(), "│");
            assert_eq!(buf[(2, spacer_y)].fg, Color::Gray);

            // c0 自己（粗體、有顏色）的 HEAD dot，被 spacer 往下推到第 3 列。
            assert_eq!(buf[(2, 3)].symbol(), "◯");
            assert!(buf[(2, 3)].modifier.contains(Modifier::BOLD));
            assert_eq!(buf[(2, 3)].fg, GREEN);

            // c1/c2 因同樣的 gap 而往下移。
            assert_eq!(graph_rows(&buf, 4..=5), ["● ──╭─", "│   ● "]);
        }

        #[test]
        fn head_upward_connector_line_fills_gap() {
            // 專用的 2-commit graph，重複使用 `text_graph_commits()` 的前兩個：
            // c0 完全沒有 edge（它的 column-0 cell 是空的），c1（HEAD）
            // 落在欄位 0。virtual row 的 dot 會落在 HEAD 的欄位上；
            // render_graph_text 必須在 c0 的空白 cell 上打出一個灰色的
            // 連接線，這條線看起來才會是連續的。
            let all_commits = text_graph_commits();
            let commits = &all_commits[..2];
            let h = |i: usize| commits[i].commit_hash.clone();
            let graph = Graph {
                commit_hashes: commits.iter().map(|c| c.commit_hash.clone()).collect(),
                commit_pos_map: [(h(0), (1, 0)), (h(1), (0, 1))].into_iter().collect(),
                edges: vec![vec![], vec![]],
                max_pos_x: 1,
            };
            let mut state = build_state(
                commits,
                graph,
                Opts {
                    head_hash: Some(1),
                    working_changes: true,
                    ..Default::default()
                },
            );
            let buf = render_commit_list(&mut state, 10);

            // Virtual row 的 dot 在 HEAD 的欄位（c1 在 pos_x=0 -> idx 0）。
            assert_eq!(buf[(0, 1)].symbol(), "◯");
            // c0 這一列在欄位 0 沒有 edge，所以沒有連接線的話這個 cell 會是
            // 空白的。它必須被填上灰色的 `│`。
            assert_eq!(buf[(0, 2)].symbol(), "│");
            assert_eq!(buf[(0, 2)].fg, Color::Gray);
            // c1（HEAD）本身，不受連接線邏輯影響。
            assert_eq!(buf[(0, 3)].symbol(), "◯");
            assert!(buf[(0, 3)].modifier.contains(Modifier::BOLD));
        }

        #[test]
        fn filtered_graph_manager_fills_text_cells() {
            // 只要 `show_remote_refs` 關閉且存在 filtered graph（「隱藏
            // remote-only commit」這條路），`render_graph` 的 `current_graph()`
            // 就會選用 `filtered` 而不是 `graph`。整個測試套件裡沒有其他地方
            // 會把 `filtered` 設成 true，所以沒有這個測試的話，filtered
            // 分支可能悄悄不再渲染（例如 `current_graph()` 退化成永遠回傳
            // primary graph）也不會有測試發現。
            //
            // `text_graph_filtered` 刻意跟 `text_graph`（primary fixture）
            // 用不同形狀 —— 如果兩者渲染結果一樣，一個完全忽略 `filtered`
            // 的 `current_graph()` 依然會通過這個斷言而不被發現。
            let commits = text_graph_commits();
            let mut state = build_state(
                &commits,
                text_graph(&commits),
                Opts {
                    filtered: true,
                    ..Default::default()
                },
            );
            assert!(!state.show_remote_refs());
            let buf = render_commit_list(&mut state, 10);

            assert_eq!(
                graph_rows(&buf, 1..=3),
                ["◯     ", "●     ", "●     "],
                "must render text_graph_filtered's shape, not the primary graph's"
            );
        }

        #[test]
        fn graph_area_cell_width_reflects_current_graph_not_primary() {
            // `graph_area_cell_width()` 跟 `current_graph()` 共用同一套
            // `show_remote_refs` / `filtered` fallback，兩者對「哪個 graph
            // 是目前的」必須永遠一致。上面用到的 `text_graph_filtered` 剛好
            // 跟 primary fixture 共用 `max_pos_x: 2`，所以一個悄悄退回
            // primary graph 的 `graph_area_cell_width()` 在那裡會算出一樣
            // 的寬度而不被發現。這個測試的 filtered graph 刻意用了不同的
            // `max_pos_x`，純粹是為了驗證寬度計算 —— 不涉及渲染。
            let primary = Graph {
                commit_hashes: Vec::new(),
                commit_pos_map: FxHashMap::default(),
                edges: Vec::new(),
                max_pos_x: 5, // double 寬度：(5+1)*2 + 1 個 pad = 13
            };
            let filtered = Graph {
                commit_hashes: Vec::new(),
                commit_pos_map: FxHashMap::default(),
                edges: Vec::new(),
                max_pos_x: 0, // double 寬度：(0+1)*2 + 1 個 pad = 3
            };
            let mut state = CommitListState::new(
                Vec::new(),
                Rc::new(primary),
                Vec::new(),
                None,
                CellWidthType::Double,
                Head::None,
                FxHashMap::default(),
                false,
                false,
                Some(Rc::new(filtered)),
                None,
                FxHashSet::default(),
                None,
            );
            state.set_show_remote_refs(false);
            assert_eq!(
                state.graph_area_cell_width(),
                3,
                "must use the filtered graph's width (3), not the primary's (13)"
            );
        }
    }

    #[test]
    fn test_calc_cell_widths_all_columns() {
        let area_width = 80;
        let subject_min_width = 20;
        let graph_width = 6;
        let name_width = 10;
        let date_width = 15;
        let columns = vec![
            UserListColumnType::Graph,
            UserListColumnType::Marker,
            UserListColumnType::Subject,
            UserListColumnType::Date,
            UserListColumnType::Name,
            UserListColumnType::Hash,
        ];

        let actual = calc_cell_widths(
            area_width,
            subject_min_width,
            graph_width,
            name_width,
            date_width,
            &columns,
        );

        let expected = vec![
            Constraint::Length(6),  // Graph
            Constraint::Length(1),  // Marker
            Constraint::Min(0),     // Subject
            Constraint::Length(17), // Date (15 + 2 pad)
            Constraint::Length(12), // Name (10 + 2 pad)
            Constraint::Length(9),  // Hash (7 + 2 pad)
        ];
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_calc_cell_width_all_columns_small_area_remove_name_date_hash() {
        let area_width = 30;
        let subject_min_width = 20;
        let graph_width = 6;
        let name_width = 10;
        let date_width = 15;
        let columns = vec![
            UserListColumnType::Graph,
            UserListColumnType::Marker,
            UserListColumnType::Subject,
            UserListColumnType::Date,
            UserListColumnType::Name,
            UserListColumnType::Hash,
        ];

        let actual = calc_cell_widths(
            area_width,
            subject_min_width,
            graph_width,
            name_width,
            date_width,
            &columns,
        );

        // Graph + Marker + Subject + Hash = 6 + 1 + 20 + 9 = 36 > 30
        // => 移除 Name、Date 和 Hash
        let expected = vec![
            Constraint::Length(6), // Graph
            Constraint::Length(1), // Marker
            Constraint::Min(0),    // Subject
            Constraint::Length(0), // Date 已移除
            Constraint::Length(0), // Name 已移除
            Constraint::Length(0), // Hash 已移除
        ];
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_calc_cell_width_all_columns_small_area_remove_name_date() {
        let area_width = 40;
        let subject_min_width = 20;
        let graph_width = 6;
        let name_width = 10;
        let date_width = 15;
        let columns = vec![
            UserListColumnType::Graph,
            UserListColumnType::Marker,
            UserListColumnType::Subject,
            UserListColumnType::Date,
            UserListColumnType::Name,
            UserListColumnType::Hash,
        ];

        let actual = calc_cell_widths(
            area_width,
            subject_min_width,
            graph_width,
            name_width,
            date_width,
            &columns,
        );

        // Graph + Marker + Subject + Hash = 6 + 1 + 20 + 9 = 36
        // Graph + Marker + Subject + Date + Hash = 6 + 1 + 20 + 17 + 9 = 53 > 40
        // => 移除 Name 和 Date
        let expected = vec![
            Constraint::Length(6), // Graph
            Constraint::Length(1), // Marker
            Constraint::Min(0),    // Subject
            Constraint::Length(0), // Date 已移除
            Constraint::Length(0), // Name 已移除
            Constraint::Length(9), // Hash (7 + 2 pad)
        ];
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_calc_cell_width_all_columns_small_area_remove_name() {
        let area_width = 60;
        let subject_min_width = 20;
        let graph_width = 6;
        let name_width = 10;
        let date_width = 15;
        let columns = vec![
            UserListColumnType::Graph,
            UserListColumnType::Marker,
            UserListColumnType::Subject,
            UserListColumnType::Date,
            UserListColumnType::Name,
            UserListColumnType::Hash,
        ];

        let actual = calc_cell_widths(
            area_width,
            subject_min_width,
            graph_width,
            name_width,
            date_width,
            &columns,
        );

        // Graph + Marker + Subject + Date + Hash = 6 + 1 + 20 + 17 + 9 = 53 <= 60
        // Graph + Marker + Subject + Name + Date + Hash = 6 + 1 + 20 + 12 + 17 + 9 = 65 > 60
        // => 移除 Name
        let expected = vec![
            Constraint::Length(6),  // Graph
            Constraint::Length(1),  // Marker
            Constraint::Min(0),     // Subject
            Constraint::Length(17), // Date (15 + 2 pad)
            Constraint::Length(0),  // Name 已移除
            Constraint::Length(9),  // Hash (7 + 2 pad)
        ];
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_calc_cell_width_columns_order() {
        let area_width = 80;
        let subject_min_width = 20;
        let graph_width = 6;
        let name_width = 10;
        let date_width = 15;
        let columns = vec![
            UserListColumnType::Date,
            UserListColumnType::Subject,
            UserListColumnType::Hash,
            UserListColumnType::Graph,
        ];

        let actual = calc_cell_widths(
            area_width,
            subject_min_width,
            graph_width,
            name_width,
            date_width,
            &columns,
        );

        let expected = vec![
            Constraint::Length(17), // Date (15 + 2 pad)
            Constraint::Min(0),     // Subject
            Constraint::Length(9),  // Hash (7 + 2 pad)
            Constraint::Length(6),  // Graph
        ];
        assert_eq!(actual, expected);
    }
}

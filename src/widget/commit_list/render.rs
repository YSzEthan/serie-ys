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
        // Visible-row text cells are computed on demand by render_graph via
        // rendering_commit_info_iter(), which is the single source of truth
        // for "what rows are visible" -- no separate preload pass needed.
    }

    /// Single seam for GraphStyle -> GlyphSet selection.
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
            // `None` here now means only one thing: `hash` isn't in
            // `current_graph().commit_pos_map` -- i.e. the graph and the
            // commit list have desynced. There's no "not preloaded yet"
            // case anymore since text cells are computed on demand.
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

        // Spacer rows (inline detail gap): draw `│` at each active column.
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

    /// Returns the text-graph column (in cells, not chars) of `hash` on the
    /// current graph, or None if missing.
    fn graph_text_head_col(&self, state: &CommitListState<'_>, hash: &CommitHash) -> Option<usize> {
        let cells = state.text_cells_for_hash(hash)?;
        cells
            .iter()
            .position(|c| matches!(c.glyph, Glyph::CommitDot | Glyph::HeadDot))
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
            // Only redraw `│` where something reaches downward. Of the
            // junctions that means `┬├┤┼`; `┴` is excluded, since drawing a
            // line beneath "this one ends going up" would be plainly wrong.
            // `╰`/`╯` also reach upward yet have always been listed -- this
            // list has always been looser than
            // `EdgeType::has_downward_continuation`, and stays that way here.
            //
            // So the same column can answer differently depending on width:
            // a `RightBottom` crossed by a `Horizontal` is `╯` under
            // `DoubleL` (extends) and `┴` under `DoubleF`/`Single` (does
            // not). The stricter answer is the correct one; the loose `╯`
            // survives because tightening it is a separate change from #30.
            let draw_vertical = matches!(
                cell.glyph,
                Glyph::CommitDot
                    | Glyph::Vert
                    | Glyph::HeadDot
                    | Glyph::CornerTL
                    | Glyph::CornerTR
                    | Glyph::CornerBL
                    | Glyph::CornerBR
                    | Glyph::TeeDown
                    | Glyph::TeeRight
                    | Glyph::TeeLeft
                    | Glyph::Cross
            );
            if !draw_vertical {
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
            // Insert marker gap when virtual row is selected
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
        // Virtual row
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

    /// Returns iterator of (display_idx, raw_idx, &CommitInfo)
    /// display_idx: position on screen (0, 1, 2, ...)
    /// raw_idx: actual index in commits Vec (for search_matches access)
    /// Skips the virtual row (if present and visible).
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
    // Overwrite only the bg channel so previously-written fg/modifier on the
    // graph cells survives.
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
        spans.clear(); // contains only "(" and ")", so clear it
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
    // CommitList implements StatefulWidget directly, so no Terminal /
    // TestBackend is needed — Buffer::empty() + render() is enough.
    // Fixtures/constants live in their own submodule so they don't collide
    // with the rest of `mod tests`.
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

        /// Commit hash must be >= 7 chars: `render_hash` uses `as_short_hash()`
        /// ([0..7]); a shorter hash panics.
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

        /// 3-commit graph shared by tests 1-4.
        ///
        /// c0's dot is deliberately at pos_x=1, NOT pos_x=0: `render_graph_text`'s
        /// virtual-row column fallback is `head_col -> first-visible-commit's dot
        /// col -> literal 0`. If HEAD's dot sat at column 0, all three paths would
        /// coincide and a regression that hardcodes column 0 would slip past the
        /// virtual-row test undetected.
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

        /// The three fixtures differ only in where the dots sit and which
        /// edges each row carries.
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

        /// Deliberately different shape from `text_graph` (linear, all three
        /// commits at pos_x=0, no merge) so `filtered_graph_manager_fills_text_cells`
        /// can tell whether `current_graph()` actually picked this graph
        /// instead of the primary one -- if both fixtures rendered
        /// identically, a `current_graph()` that always returns the primary
        /// graph would pass the test undetected.
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

        /// Every column here carries two edges at once, which `text_graph`
        /// never does -- its three rows put each edge in its own column, so
        /// under `Single` nothing ever shares a cell. Without this fixture
        /// the widget path (`text_cells_for_hash` -> `put_text_cells` ->
        /// `GlyphSet::resolve`) would have no coverage of junction glyphs at
        /// all: `render_graph_single_width_folds_cells_and_sizes_column_correctly`
        /// renders identically before and after issue #29.
        fn text_graph_colliding(commits: &[Commit]) -> Graph {
            graph_fixture(
                commits,
                [(2, 0), (2, 1), (0, 2)],
                vec![
                    // cols 0 and 1: a vertical crossed by a horizontal run -> `┼`
                    vec![
                        Edge::new(EdgeType::Vertical, 0, 0),
                        Edge::new(EdgeType::Horizontal, 0, 2),
                        Edge::new(EdgeType::Vertical, 1, 1),
                        Edge::new(EdgeType::Horizontal, 1, 2),
                    ],
                    // col 0: a branch tip pointing down, crossed horizontally
                    // -> `┬` (NOT `┼`: nothing continues upward here)
                    // col 1: corner meeting a horizontal run -> `┴`
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

        struct Opts {
            /// Index into `commits`, fed to `CommitListState`'s shared
            /// `head_commit_hash` (drives `is_head` in `put_text_cells`).
            /// Distinct from `CommitListState`'s own `head: Head` field, which
            /// only affects ref rendering and doesn't matter for these tests.
            head_hash: Option<usize>,
            working_changes: bool,
            inline_detail_height: u16,
            /// When true, builds a second `Graph` fixture (same shape as the
            /// primary) and routes rendering through it via `filtered`
            /// and `set_show_remote_refs(false)`. This is the only path
            /// `render_graph`'s filtered branch exercises -- without it,
            /// that branch has zero test coverage.
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
                    cell_width_type: CellWidthType::DoubleF,
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

        /// Graph column only (x in 0..6), one string per row in `rows`.
        fn graph_rows(buf: &Buffer, rows: std::ops::RangeInclusive<u16>) -> Vec<String> {
            graph_rows_width(buf, rows, 6)
        }

        /// Same as `graph_rows`, but for a graph column narrower/wider than
        /// the fixed `0..6` every double-width test uses -- needed for
        /// single-width fixtures, where the column is 3 cells wide.
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

            // c0 (HEAD, selected): dot col is green (pos_x=1), the Vertical
            // edge at col 0 is red (line 0), the Horizontal edges are blue
            // (line 2) -- colors follow associated_line_pos_x, not pos_x.
            assert_eq!(buf[(0, 1)].fg, RED);
            assert_eq!(buf[(2, 1)].fg, GREEN);
            assert_eq!(buf[(4, 1)].fg, BLUE);
            assert!(buf[(2, 1)].modifier.contains(Modifier::BOLD));
            for x in 0..7 {
                assert_eq!(buf[(x, 1)].bg, SELECTED_BG, "selected row bg at x={x}");
            }

            // c1 (not selected): no bg override.
            assert_ne!(buf[(0, 2)].bg, SELECTED_BG);
        }

        #[test]
        fn render_graph_single_width_folds_cells_and_sizes_column_correctly() {
            // #21: the bug was `graph_area_cell_width()` and `build_text_cells`
            // disagreeing on how many cells a `Single` graph column needs.
            // Snapshot tests can't catch that -- they only see the cell
            // contents, never the column width the widget actually allocated.
            // This is the one place that width is observable.
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

            // Marker column starts right after the graph column (width
            // cell_count(3) + 1 pad = 4). If `graph_area_cell_width()` were
            // still sized for `Double` (7), this `│` would land one cell
            // further right than here.
            assert_eq!(buf[(4, 1)].symbol(), "│");
        }

        /// Issue #29: the only end-to-end check that junction glyphs reach
        /// the screen. Before the fix these columns rendered as `│`/`╯` with
        /// the horizontal runs swallowed whole.
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

        /// The same fixture under `DoubleF`: the symbol half carries the
        /// union, so it comes out as the `Single` rendering with a connector
        /// cell spliced in after each column.
        #[test]
        fn render_graph_double_f_width_unions_colliding_edges() {
            let commits = text_graph_commits();
            let mut state = build_state(&commits, text_graph_colliding(&commits), Opts::default());
            let buf = render_commit_list(&mut state, 10);

            assert_eq!(
                graph_rows_width(&buf, 1..=3, 6),
                ["┼─┼─◯ ", "┬─┴─● ", "● │   "],
                "double-f's symbol half draws the same junction single does"
            );
        }

        /// The same fixture under `DoubleL`, which is why that width still
        /// exists: this is what double drew before issue #30. Row 2 is the
        /// defect -- col 1 shows `╯` and the `Horizontal` crossing it is
        /// gone, with no trace in the connector either.
        #[test]
        fn render_graph_double_l_width_keeps_the_pre_30_rendering() {
            let commits = text_graph_commits();
            let mut state = build_state(
                &commits,
                text_graph_colliding(&commits),
                Opts {
                    cell_width_type: CellWidthType::DoubleL,
                    ..Default::default()
                },
            );
            let buf = render_commit_list(&mut state, 10);

            assert_eq!(
                graph_rows_width(&buf, 1..=3, 6),
                ["│─│─◯ ", "│─╯─● ", "● │   "],
                "double-l still gives each half-cell to the winning edge"
            );
        }

        #[test]
        fn render_graph_uses_ascii_style() {
            // Only this test goes through AppContext -> glyphs() -> put_text_cell;
            // tests/graph.rs calls GlyphSet::resolve() directly and never
            // exercises that wiring, so this is the sole end-to-end coverage
            // for "-s ascii actually changes what's on screen".
            let commits = text_graph_commits();
            let mut state = build_state(&commits, text_graph(&commits), Opts::default());
            let buf = render_commit_list_styled(&mut state, 10, GraphStyle::Ascii);

            assert_eq!(
                graph_rows(&buf, 1..=3),
                ["| o --", "* --+-", "|   * "],
                "ascii style substitutes every box-drawing glyph 1:1"
            );

            // Marker column sits right after the graph column (width 7 =
            // graph_area_cell_width's `w + 1`) and shares the same
            // `self.glyphs()` call, so it must switch style too.
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
            // Color is unaffected by head/non-head.
            assert_eq!(buf_b[(2, 1)].fg, GREEN);

            // c1/c2 are never HEAD in either state: plain dot, never bold.
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

            // Selected row is c0 (pos_y=0): cells are [Vert, ' ', Dot, ' ', Horiz, Horiz].
            // Only the Vertical (idx0) and the Dot (idx2) are in put_text_spacer's
            // whitelist -- the Horizontal pair (idx4/5) must NOT extend down.
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

            // c1/c2 pushed down by the 2-row gap.
            assert_eq!(graph_rows(&buf, 4..=5), ["● ──╭─", "│   ● "]);
        }

        /// `┴` is deliberately absent from `put_text_spacer`'s whitelist: a
        /// line that ends going up has nothing to continue below it.
        /// `DoubleF` is the first double width that can produce one, so this
        /// is a visible behaviour change and not just a new character --
        /// the very same column under `DoubleL` is `╯`, which *is*
        /// whitelisted and does extend downward.
        ///
        /// `text_graph` can't reach this (one edge per column, never a
        /// junction), so the colliding fixture is the only way to cover it.
        #[test]
        fn spacer_row_stops_at_tee_up_under_double_f() {
            let commits = text_graph_commits();

            let spacer_symbols = |width: CellWidthType| {
                let mut state = build_state(
                    &commits,
                    text_graph_colliding(&commits),
                    Opts {
                        inline_detail_height: 1,
                        cell_width_type: width,
                        ..Default::default()
                    },
                );
                // `select_next` is a no-op until a render has set `height`,
                // so the first pass is only there to make the second one
                // land on row 1 -- `┬─┴─●` under double-f, `│─╯─●` under
                // double-l. Its spacer then sits at y=3.
                render_commit_list(&mut state, 10);
                state.select_next();
                let buf = render_commit_list(&mut state, 10);
                [0u16, 2, 4].map(|x| buf[(x, 3u16)].symbol().to_string())
            };

            assert_eq!(
                spacer_symbols(CellWidthType::DoubleF),
                ["│", " ", "│"],
                "┬ and the dot extend downward, ┴ does not"
            );
            assert_eq!(
                spacer_symbols(CellWidthType::DoubleL),
                ["│", "│", "│"],
                "the same column is ╯ under double-l, which does extend"
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

            // Virtual row at buffer row 1, dot at HEAD's column (idx2, not 0).
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

            // Gray spacer row (virtual row selected + gap=1) comes first, at
            // row 2 -- `y_offset` pushes every commit past `state.selected`
            // (0, the virtual row) down by `gap` before the spacer itself.
            // Cells that redraw at all must use VIRTUAL_ROW_COLOR, not their
            // own color.
            let spacer_y = 2u16;
            assert_eq!(buf[(0, spacer_y)].symbol(), "│");
            assert_eq!(buf[(0, spacer_y)].fg, Color::Gray);
            assert_eq!(buf[(2, spacer_y)].symbol(), "│");
            assert_eq!(buf[(2, spacer_y)].fg, Color::Gray);

            // c0's own (bold, colored) HEAD dot, pushed down past the spacer
            // to row 3.
            assert_eq!(buf[(2, 3)].symbol(), "◯");
            assert!(buf[(2, 3)].modifier.contains(Modifier::BOLD));
            assert_eq!(buf[(2, 3)].fg, GREEN);

            // c1/c2 shift down by the same gap.
            assert_eq!(graph_rows(&buf, 4..=5), ["● ──╭─", "│   ● "]);
        }

        #[test]
        fn head_upward_connector_line_fills_gap() {
            // Dedicated 2-commit graph reusing the first two of
            // `text_graph_commits()`: c0 has NO edges at all (its column-0
            // cell is blank), c1 (HEAD) sits at column 0. The virtual row's
            // dot lands on HEAD's column; render_graph_text must punch a gray
            // connector through c0's blank cell so the line reads continuous.
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

            // Virtual row dot at HEAD's column (c1 is at pos_x=0 -> idx 0).
            assert_eq!(buf[(0, 1)].symbol(), "◯");
            // c0's row has no edge at column 0, so without the connector this
            // cell would be blank. It must be filled with a gray `│`.
            assert_eq!(buf[(0, 2)].symbol(), "│");
            assert_eq!(buf[(0, 2)].fg, Color::Gray);
            // c1 (HEAD) itself, unaffected by the connector logic.
            assert_eq!(buf[(0, 3)].symbol(), "◯");
            assert!(buf[(0, 3)].modifier.contains(Modifier::BOLD));
        }

        #[test]
        fn filtered_graph_manager_fills_text_cells() {
            // `render_graph`'s `current_graph()` picks `filtered`
            // instead of `graph` whenever `show_remote_refs` is off and a
            // filtered graph exists (the "hide remote-only commits" path).
            // Nothing else in this suite ever sets `filtered: true`, so
            // without this test the filtered branch could silently stop
            // rendering (e.g. `current_graph()` regressing to always return
            // the primary graph) and no test would notice.
            //
            // `text_graph_filtered` is deliberately a different shape from
            // `text_graph` (the primary fixture) -- if both rendered
            // identically, a `current_graph()` that ignores `filtered`
            // entirely would still pass this assertion undetected.
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
            // `graph_area_cell_width()` and `current_graph()` share the same
            // `show_remote_refs` / `filtered` fallback and must always agree
            // on which graph is "current". `text_graph_filtered` (used just
            // above) happens to share `max_pos_x: 2` with the primary
            // fixture, so a `graph_area_cell_width()` that silently fell
            // back to the primary graph would produce the same width there
            // and go undetected. This test's filtered graph has a
            // deliberately different `max_pos_x`, purely to exercise the
            // width computation -- no rendering involved.
            let primary = Graph {
                commit_hashes: Vec::new(),
                commit_pos_map: FxHashMap::default(),
                edges: Vec::new(),
                max_pos_x: 5, // double width: (5+1)*2 + 1 pad = 13
            };
            let filtered = Graph {
                commit_hashes: Vec::new(),
                commit_pos_map: FxHashMap::default(),
                edges: Vec::new(),
                max_pos_x: 0, // double width: (0+1)*2 + 1 pad = 3
            };
            let mut state = CommitListState::new(
                Vec::new(),
                Rc::new(primary),
                Vec::new(),
                None,
                CellWidthType::DoubleF,
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
        // => Name, Date, and Hash are removed
        let expected = vec![
            Constraint::Length(6), // Graph
            Constraint::Length(1), // Marker
            Constraint::Min(0),    // Subject
            Constraint::Length(0), // Date removed
            Constraint::Length(0), // Name removed
            Constraint::Length(0), // Hash removed
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
        // => Name and Date are removed
        let expected = vec![
            Constraint::Length(6), // Graph
            Constraint::Length(1), // Marker
            Constraint::Min(0),    // Subject
            Constraint::Length(0), // Date removed
            Constraint::Length(0), // Name removed
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
        // => Name is removed
        let expected = vec![
            Constraint::Length(6),  // Graph
            Constraint::Length(1),  // Marker
            Constraint::Min(0),     // Subject
            Constraint::Length(17), // Date (15 + 2 pad)
            Constraint::Length(0),  // Name removed
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

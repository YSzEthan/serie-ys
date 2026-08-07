use std::rc::Rc;

use laurier::highlight::highlight_matched_text;
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Paragraph, StatefulWidget, Widget},
};
use rustc_hash::FxHashMap;

use crate::{
    app::AppContext,
    color::{ratatui_color_to_rgb, ColorTheme},
    config::UserListColumnType,
    git::{CommitHash, Head, Ref},
    graph::{Glyph, GlyphSet, TextCell},
};

use super::layout;
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
        let columns = &self.ctx.ui_config.list.columns;

        // 寬度／緊湊每幀從 `content_area.width` 決定（不是啟動時凍結）——
        // 兩者跟著 resize、refs 側欄開合自動反應，`-c auto` 也完全不用付
        // `terminal::size()` 的 I/O 成本。決定完先寫回 state，
        // `build_visible_rows` 內部的 `text_cells_for_hash` 才會用到
        // 正確的寬度。
        let (cell_width_type, compact) = layout::decide(
            columns,
            state.current_cell_count(),
            content_area.width,
            self.ctx.graph_width,
            self.ctx.compact,
            self.ctx.ui_config.list.subject_min_width,
            name_width,
            self.ctx.ui_config.list.date_width,
        );
        state.set_layout(cell_width_type, compact);

        // 六個欄位共用同一份列表 —— `text_cells_for_hash` 不會被重複呼叫，
        // 緊湊模式的 `text_x` 也只有一份算法，不會有 graph 跟 subject
        // 各算各的漂移風險。
        let rows = self.build_visible_rows(state);
        let selected_text_x = rows
            .iter()
            .find(|r| r.is_selected)
            .map(|r| r.text_x)
            .unwrap_or(0);

        let constraints = layout::calc_cell_widths(
            area.width,
            self.ctx.ui_config.list.subject_min_width,
            state.graph_cell_width(),
            name_width,
            self.ctx.ui_config.list.date_width,
            columns,
            compact,
        );

        let header_chunks = Layout::horizontal(constraints.clone()).split(header_area);
        let header_style = Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD);
        for (i, col) in columns.iter().enumerate() {
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

        for (i, col) in columns.iter().enumerate() {
            match col {
                UserListColumnType::Graph => {
                    self.render_graph(buf, content_chunks[i], state, &rows);
                }
                UserListColumnType::Marker => {
                    self.render_marker(buf, content_chunks[i], state, &rows);
                }
                UserListColumnType::Subject => {
                    if compact {
                        // Graph 自己的欄位寬度是 0（見
                        // `layout::calc_cell_widths`），跟 Subject 共用這塊
                        // Rect —— 每列的文字才能貼齊「這一列自己」的 graph
                        // 終點，而不是全域最深的那一欄。
                        self.render_graph(buf, content_chunks[i], state, &rows);
                    }
                    self.render_subject(buf, content_chunks[i], state, &rows);
                }
                UserListColumnType::Name => {
                    self.render_name(buf, content_chunks[i], state, &rows);
                }
                UserListColumnType::Hash => {
                    self.render_hash(buf, content_chunks[i], state, &rows);
                }
                UserListColumnType::Date => {
                    self.render_date(buf, content_chunks[i], state, &rows);
                }
            }
        }

        // `rows` 借了 `state.commits`，`&rows` 的最後一次使用在上面那個
        // for 迴圈 —— 這裡才能重新可變借用 `state` 寫回選取列的 `text_x`。
        state.set_selected_text_x(selected_text_x);
    }
}

/// 一幀之內、一列的所有版面事實。`text_cells_for_hash` 是隨需計算的，
/// 由六個 render_* 各自呼叫就會變成一列算好幾次 —— 而且緊湊模式下大家
/// 都要用同一個 `text_x`，各算各的遲早漂移。所以在 `build_visible_rows`
/// 算一次，之後每個 render_* 都只是讀。
struct VisibleRow<'b> {
    /// 相對 `content_area.top()` 的列位移（已經算進 inline detail 的 gap）。
    y: u16,
    is_selected: bool,
    /// 相對 `area.left()`：這一列圖形實際延伸到第幾格。非緊湊模式恆為 0；
    /// 緊湊模式下用來決定文字從哪裡開始（`draw_row_line`）以及
    /// `render_graph` 在 compact 分支要接手畫多寬。
    text_x: u16,
    content: RowContent<'b>,
    graph: RowGraph,
}

enum RowContent<'b> {
    Virtual,
    Commit {
        raw: RawCommitIdx,
        info: &'b CommitInfo<'b>,
    },
}

enum RowGraph {
    /// virtual row：只有一顆 dot，畫在 HEAD 欄位（fallback 次序見
    /// `build_visible_rows`）。
    Dot(usize),
    Cells {
        cells: Vec<TextCell>,
        is_head: bool,
        /// 排在 HEAD 前面、virtual row 又可見時，HEAD 欄位上合成的向上
        /// 連接線（原本 `render_graph` 的 `head_line_col` 那段邏輯）。
        synthetic_connector: Option<usize>,
    },
}

impl RowGraph {
    /// 這一列圖形實際延伸到第幾格（不含）—— 緊湊模式下 `text_x` 的來源。
    /// 用 `cells` 本身最後一個非 Blank 格是不夠的：合成的連接線畫在
    /// Blank 格上，藏在 `cells` 的內容之外，兩者要取 max。
    fn extent(&self) -> u16 {
        match self {
            RowGraph::Dot(col) => *col as u16 + 1,
            RowGraph::Cells {
                cells,
                synthetic_connector,
                ..
            } => {
                let cells_end = cells
                    .iter()
                    .rposition(|c| c.glyph != Glyph::Blank)
                    .map(|i| i as u16 + 1)
                    .unwrap_or(0);
                let connector_end = synthetic_connector.map(|c| c as u16 + 1).unwrap_or(0);
                cells_end.max(connector_end)
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
        // 可見列的 text cell 是由 build_visible_rows 透過 rendering_commit_info_iter()
        // 隨需計算的，這也是「哪些列可見」的唯一真相來源 —— 不需要另外的
        // preload pass。
    }

    /// GraphStyle -> GlyphSet 選擇的唯一入口。
    fn glyphs(&self) -> GlyphSet {
        GlyphSet::from_style(self.ctx.graph_style)
    }

    /// 六個欄位共用的一次性列表計算：virtual row（若可見）+ 每個可見
    /// commit，含 gap（inline detail 間隔列）造成的垂直位移、緊湊模式的
    /// `text_x`。呼叫前 `state.set_layout` 必須已經跑過，否則
    /// `text_cells_for_hash`／`is_compact` 用到的還是上一幀的值。
    fn build_visible_rows<'b>(&'b self, state: &'b CommitListState<'_>) -> Vec<VisibleRow<'b>> {
        let compact = state.is_compact();
        let gap = state.inline_detail_height;
        let head_hash = state.head_commit_hash.as_ref();
        let head_col = head_hash.and_then(|h| self.graph_text_head_col(state, h));
        let virtual_row_visible = state.has_virtual_row() && state.offset == 0;
        let head_line_col = head_col.filter(|_| virtual_row_visible);

        let mut rows = Vec::new();

        if virtual_row_visible {
            // ◯ fallback 次序：HEAD column → 第一個可見 commit 的 dot column → 0
            let dot_col = head_col.unwrap_or_else(|| {
                state
                    .first_visible_commit_hash()
                    .and_then(|h| self.graph_text_head_col(state, h))
                    .unwrap_or(0)
            });
            let graph = RowGraph::Dot(dot_col);
            let text_x = if compact { graph.extent() } else { 0 };
            rows.push(VisibleRow {
                y: 0,
                is_selected: state.selected == 0,
                text_x,
                content: RowContent::Virtual,
                graph,
            });
        }

        let mut seen_head = false;
        for (display_i, raw, info) in self.rendering_commit_info_iter(state) {
            let hash = &info.commit.commit_hash;
            // 這裡的 `None` 只代表一種情況：`hash` 不在
            // `current_graph().commit_pos_map` 裡 —— 也就是 graph 跟
            // commit list 不同步了。因為 text cell 是隨需計算的，已經沒有
            // 「還沒 preload」這種情況存在了。
            let Some(cells) = state.text_cells_for_hash(hash) else {
                continue;
            };
            let is_head = head_hash == Some(hash);
            let synthetic_connector = if !seen_head && !is_head {
                head_line_col.filter(|&hc| cells.get(hc).is_some_and(|c| c.glyph == Glyph::Blank))
            } else {
                None
            };
            if is_head {
                seen_head = true;
            }

            let graph = RowGraph::Cells {
                cells,
                is_head,
                synthetic_connector,
            };
            let text_x = if compact { graph.extent() } else { 0 };
            let y_offset = if gap > 0 && display_i > state.selected {
                gap
            } else {
                0
            };
            rows.push(VisibleRow {
                y: display_i as u16 + y_offset,
                is_selected: display_i == state.selected,
                text_x,
                content: RowContent::Commit { raw, info },
                graph,
            });
        }

        rows
    }

    fn render_graph(
        &self,
        buf: &mut Buffer,
        area: Rect,
        state: &CommitListState<'_>,
        rows: &[VisibleRow<'_>],
    ) {
        if area.is_empty() {
            return;
        }
        let selected_bg = ratatui_color_to_rgb(self.ctx.color_theme.list_selected_bg);
        for row in rows {
            let y = area.top() + row.y;
            if y >= area.bottom() {
                continue;
            }
            match &row.graph {
                RowGraph::Dot(col) => {
                    self.put_text_cell(buf, area, y, *col, Glyph::HeadDot, VIRTUAL_ROW_COLOR);
                }
                RowGraph::Cells {
                    cells,
                    is_head,
                    synthetic_connector,
                } => {
                    self.put_text_cells(buf, area, y, cells, *is_head);
                    if let Some(hc) = synthetic_connector {
                        self.put_text_cell(buf, area, y, *hc, Glyph::Vert, VIRTUAL_ROW_COLOR);
                    }
                }
            }
            if row.is_selected {
                apply_row_bg(buf, area, y, selected_bg);
            }
        }
        self.draw_graph_spacer(buf, area, state);
    }

    /// Spacer rows（inline detail 的間隔列）：把選取列的線往下延伸接住。
    /// gap 一定緊接在選取列後面，跟 `rows` 的內容無關，所以獨立算，不用
    /// 塞進 `VisibleRow`。
    fn draw_graph_spacer(&self, buf: &mut Buffer, area: Rect, state: &CommitListState<'_>) {
        let gap = state.inline_detail_height;
        if gap == 0 {
            return;
        }
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
        let Some(hash) = spacer_hash else {
            return;
        };
        let Some(cells) = state.text_cells_for_hash(&hash) else {
            return;
        };
        let gray = state.is_virtual_row_selected();
        for gap_row in 0..gap {
            let y = area.top() + state.selected as u16 + 1 + gap_row;
            if y >= area.bottom() {
                break;
            }
            self.put_text_spacer(buf, area, y, &cells, gray);
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

    fn render_marker(
        &self,
        buf: &mut Buffer,
        area: Rect,
        state: &CommitListState<'_>,
        rows: &[VisibleRow<'_>],
    ) {
        if area.is_empty() {
            return;
        }
        let vert = self.glyphs().vert;
        let selected_bg = ratatui_color_to_rgb(self.ctx.color_theme.list_selected_bg);
        for row in rows {
            let y = area.top() + row.y;
            if y >= area.bottom() {
                continue;
            }
            let color = match &row.content {
                RowContent::Virtual => VIRTUAL_ROW_COLOR,
                RowContent::Commit { info, .. } => state.marker_color(info),
            };
            buf[(area.left(), y)]
                .set_symbol(vert)
                .set_style(Style::default().fg(color));
            if row.is_selected {
                apply_row_bg(buf, area, y, selected_bg);
            }
        }
        self.draw_marker_spacer(buf, area, state);
    }

    /// Marker 欄在 spacer rows（inline detail 間隔列）上也要延續同一條
    /// `│`，顏色跟 `draw_graph_spacer` 一樣取自選取列。
    fn draw_marker_spacer(&self, buf: &mut Buffer, area: Rect, state: &CommitListState<'_>) {
        let gap = state.inline_detail_height;
        if gap == 0 {
            return;
        }
        let vert = self.glyphs().vert;
        let color = if state.is_virtual_row_selected() {
            VIRTUAL_ROW_COLOR
        } else {
            state.marker_color(state.commit(state.current_selected_raw()))
        };
        for gap_row in 0..gap {
            let y = area.top() + state.selected as u16 + 1 + gap_row;
            if y >= area.bottom() {
                break;
            }
            buf[(area.left(), y)]
                .set_symbol(vert)
                .set_style(Style::default().fg(color));
        }
    }

    fn render_subject(
        &self,
        buf: &mut Buffer,
        area: Rect,
        state: &CommitListState<'_>,
        rows: &[VisibleRow<'_>],
    ) {
        if area.is_empty() {
            state.selected_row_overflows.set(false);
            return;
        }
        let mut any_selected_overflow = false;
        let marquee_frame = self.marquee_frame;

        for row in rows {
            let sub_width = area.width.saturating_sub(row.text_x);
            let max_width = (sub_width as usize).saturating_sub(2);
            match &row.content {
                RowContent::Virtual => {
                    let count = state.working_changes().map_or(0, |wc| wc.file_count());
                    let text = format!("Uncommitted Changes ({count})");
                    let spans = vec![Span::styled(
                        text,
                        Style::default()
                            .fg(VIRTUAL_ROW_COLOR)
                            .add_modifier(Modifier::ITALIC),
                    )];
                    self.draw_row_line(buf, area, row, row.text_x, spans);
                }
                RowContent::Commit { raw, info } => {
                    let mut spans = refs_spans(
                        info,
                        &state.head,
                        &state.search_match(*raw).refs,
                        &self.ctx.color_theme,
                        state.show_remote_refs,
                    );
                    let ref_spans_width: usize = spans.iter().map(|s| s.width()).sum();
                    let avail = max_width.saturating_sub(ref_spans_width);
                    let commit = &info.commit;
                    if avail > ELLIPSIS.len() {
                        // byte-len 是視覺寬度的下界（ASCII 相等、非 ASCII byte 更多），
                        // 用它先短路大多數「明顯放得下」的 row，省一次寬度計算。
                        // 寬度基準必須跟 `scroll_window` 一致，理由見 `marquee::display_width`。
                        let overflow = commit.subject.len() > avail
                            && crate::widget::marquee::display_width(&commit.subject) > avail;
                        let search_pos = state.search_match(*raw).subject.as_ref();
                        let sub_spans = if row.is_selected && overflow {
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
                    self.draw_row_line(buf, area, row, row.text_x, spans);
                }
            }
        }
        state.selected_row_overflows.set(any_selected_overflow);
    }

    fn render_name(
        &self,
        buf: &mut Buffer,
        area: Rect,
        state: &CommitListState<'_>,
        rows: &[VisibleRow<'_>],
    ) {
        if area.is_empty() {
            return;
        }
        // Name 是獨立欄，不跟 Graph 共用 Rect，`max_width` 不看
        // `row.text_x`（那是給 Subject 用的，見 draw_row_line 的註解）。
        let max_width = (area.width as usize).saturating_sub(2);
        for row in rows {
            let spans = match &row.content {
                RowContent::Virtual => vec!["-".fg(VIRTUAL_ROW_COLOR)],
                RowContent::Commit { raw, info } => {
                    let commit = info.commit;
                    let truncate = console::measure_text_width(&commit.author_name) > max_width;
                    let name = if truncate {
                        console::truncate_str(&commit.author_name, max_width, ELLIPSIS).to_string()
                    } else {
                        commit.author_name.to_string()
                    };
                    if let Some(pos) = state.search_match(*raw).author_name.clone() {
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
                    }
                }
            };
            self.draw_row_line(buf, area, row, 0, spans);
        }
    }

    fn render_hash(
        &self,
        buf: &mut Buffer,
        area: Rect,
        state: &CommitListState<'_>,
        rows: &[VisibleRow<'_>],
    ) {
        if area.is_empty() {
            return;
        }
        for row in rows {
            let spans = match &row.content {
                RowContent::Virtual => vec!["-".fg(VIRTUAL_ROW_COLOR)],
                RowContent::Commit { raw, info } => {
                    let hash = info.commit.commit_hash.as_short_hash();
                    if let Some(pos) = state.search_match(*raw).commit_hash.clone() {
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
                    }
                }
            };
            self.draw_row_line(buf, area, row, 0, spans);
        }
    }

    fn render_date(
        &self,
        buf: &mut Buffer,
        area: Rect,
        _state: &CommitListState<'_>,
        rows: &[VisibleRow<'_>],
    ) {
        if area.is_empty() {
            return;
        }
        for row in rows {
            let spans = match &row.content {
                RowContent::Virtual => vec!["-".fg(VIRTUAL_ROW_COLOR)],
                RowContent::Commit { info, .. } => {
                    let date = &info.commit.author_date;
                    let date_str = if self.ctx.ui_config.list.date_local {
                        let local = date.with_timezone(&chrono::Local);
                        local
                            .format(&self.ctx.ui_config.list.date_format)
                            .to_string()
                    } else {
                        date.format(&self.ctx.ui_config.list.date_format)
                            .to_string()
                    };
                    vec![date_str.fg(self.ctx.color_theme.list_date_fg)]
                }
            };
            self.draw_row_line(buf, area, row, 0, spans);
        }
    }

    /// Subject／Name／Hash／Date 共用：把一列 spans 畫進「起點 =
    /// `area.left() + row.text_x`」的子 Rect。非緊湊模式 `text_x` 恆為
    /// 0，子 Rect 就是整塊 `area`，跟過去用 `List` 逐項渲染逐格等價
    /// ——`List`／`ListItem` 的 style 都是 all-`None` 的 patch，沒設
    /// `highlight_style`／`highlight_spacing`，本來就等於直接畫。
    /// 緊湊模式下 graph 跟 subject 共用同一塊 Rect，兩個 writer 的範圍
    /// 由呼叫端傳進來的 `text_x` 保證不重疊，不用依賴繪製順序。`text_x`
    /// 是獨立參數而不是直接讀 `row.text_x`：Date／Name／Hash 有自己獨立
    /// 的欄位 Rect，跟 Graph 共用的只有 Subject，這幾欄永遠傳 0——用
    /// `row.text_x`（那是「這一列 graph 延伸到哪」）當它們的偏移量會把
    /// 這些欄位的內容跟著 graph 深度往右推，在自己的 Rect 裡留下一段沒
    /// 畫到 bg 的空隙。
    fn draw_row_line(
        &self,
        buf: &mut Buffer,
        area: Rect,
        row: &VisibleRow<'_>,
        text_x: u16,
        spans: Vec<Span<'_>>,
    ) {
        let y = area.top() + row.y;
        if y >= area.bottom() {
            return;
        }
        let x = area.left() + text_x;
        if x >= area.right() {
            return;
        }
        let mut spans = spans;
        spans.insert(0, Span::raw(" "));
        spans.push(Span::raw(" "));
        let mut line = Line::from(spans);
        if row.is_selected {
            line = line
                .bg(ratatui_color_to_rgb(self.ctx.color_theme.list_selected_bg))
                .fg(self.ctx.color_theme.list_selected_fg);
        }
        let sub = Rect::new(x, y, area.right() - x, 1);
        Widget::render(line, sub, buf);
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

#[cfg(test)]
mod tests {
    use super::layout::calc_cell_widths;
    use super::*;
    use crate::{git::Commit, CompactType, GraphWidthType};

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

        /// `graph_width` 明確指定（不是 `Auto`）：這些是逐格斷言的低階渲染
        /// 測試，寬度必須由測試自己決定，不能讓 `layout::decide` 依
        /// `TERM_W`／`cell_count` 現算 —— 不然要 `Single` 的兩個測試會被
        /// 悄悄決回 `Double`。`compact` 固定 `Off`：這批測試都是緊湊模式
        /// 重構前就存在的基準，不該被緊湊邏輯影響。
        fn test_ctx_styled(graph_style: GraphStyle, graph_width: GraphWidthType) -> Rc<AppContext> {
            Rc::new(AppContext {
                keybind: KeyBind::new(None),
                core_config: CoreConfig::default(),
                ui_config: UiConfig::default(),
                color_theme: ColorTheme::default(),
                graph_style,
                graph_width: Some(graph_width),
                compact: Some(CompactType::Off),
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
        }

        impl Default for Opts {
            fn default() -> Self {
                Self {
                    head_hash: Some(0),
                    working_changes: false,
                    inline_detail_height: 0,
                    filtered: false,
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
            render_commit_list_styled(state, height, GraphStyle::Rounded, GraphWidthType::Double)
        }

        fn render_commit_list_styled(
            state: &mut CommitListState<'_>,
            height: u16,
            graph_style: GraphStyle,
            graph_width: GraphWidthType,
        ) -> Buffer {
            let ctx = test_ctx_styled(graph_style, graph_width);
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

        /// 跟 `render_commit_list_styled` 一樣，但強制 `-c on`（`ctx.compact`）
        /// ——這些測試要的是「緊湊模式本身對不對」，不是「auto 會不會選到
        /// 緊湊」（那是 `layout::decide` 自己的測試範圍）。
        fn render_commit_list_compact(
            state: &mut CommitListState<'_>,
            height: u16,
            graph_width: GraphWidthType,
        ) -> Buffer {
            let ctx = Rc::new(AppContext {
                keybind: KeyBind::new(None),
                core_config: CoreConfig::default(),
                ui_config: UiConfig::default(),
                color_theme: ColorTheme::default(),
                graph_style: GraphStyle::Rounded,
                graph_width: Some(graph_width),
                compact: Some(CompactType::On),
            });
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

        /// 整列（graph 到 Commit hash 全部欄位）dump 成一個字串，右側補齊
        /// trailing space 一起保留（不 trim）—— 這是緊湊模式重構的等價性
        /// 安全網：只要這個字串一個字元不動，整個版面（marker 位置、
        /// Description/Date/Author/Commit 的 x 座標）就沒有變。
        fn full_row(buf: &Buffer, y: u16) -> String {
            (0..TERM_W).map(|x| buf[(x, y)].symbol()).collect()
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

        /// 緊湊模式重構（`VisibleRow` 統一六欄、`List` 換成逐列 `Line`）的
        /// 等價性安全網：釘住*今天*完整版面（header + 3 個 commit 列）逐字元
        /// 不動。之後每一步重構都拿這個當驗收，一格都不能變。
        #[test]
        fn full_layout_snapshot_is_stable_before_compact_mode() {
            let commits = text_graph_commits();
            let mut state = build_state(&commits, text_graph(&commits), Opts::default());
            let buf = render_commit_list(&mut state, 10);

            assert_eq!(
                full_row(&buf, 0),
                " Graph   Description                                 Date        Author Commit  ",
                "header row"
            );
            assert_eq!(
                full_row(&buf, 1),
                "│ ◯ ── │ first                                       1970-01-01  alice  aaaaaaa ",
                "c0 row"
            );
            assert_eq!(
                full_row(&buf, 2),
                "● ──╭─ │ second                                      1970-01-01  alice  bbbbbbb ",
                "c1 row"
            );
            assert_eq!(
                full_row(&buf, 3),
                "│   ●  │ third                                       1970-01-01  alice  ccccccc ",
                "c2 row"
            );
        }

        #[test]
        fn render_graph_single_width_folds_cells_and_sizes_column_correctly() {
            // #21：這個 bug 是 `graph_area_cell_width()` 跟 `build_text_cells`
            // 對「一個 `Single` graph 欄需要幾格」意見不一致。Snapshot
            // 測試抓不到這個問題 —— 它們只看得到 cell 的內容，看不到 widget
            // 實際配置的欄寬。這裡是唯一能觀察到欄寬的地方。
            let commits = text_graph_commits();
            let mut state = build_state(&commits, text_graph(&commits), Opts::default());
            let buf = render_commit_list_styled(
                &mut state,
                10,
                GraphStyle::Rounded,
                GraphWidthType::Single,
            );

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
            let mut state = build_state(&commits, text_graph_colliding(&commits), Opts::default());
            let buf = render_commit_list_styled(
                &mut state,
                10,
                GraphStyle::Rounded,
                GraphWidthType::Single,
            );

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
            let buf = render_commit_list_styled(
                &mut state,
                10,
                GraphStyle::Ascii,
                GraphWidthType::Double,
            );

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
                Head::None,
                FxHashMap::default(),
                false,
                false,
                Some(Rc::new(filtered)),
                None,
                FxHashSet::default(),
                None,
            );
            state.set_layout(CellWidthType::Double, false);
            state.set_show_remote_refs(false);
            assert_eq!(
                state.graph_area_cell_width(),
                3,
                "must use the filtered graph's width (3), not the primary's (13)"
            );
        }

        // ---- 緊湊模式 --------------------------------------------------

        /// `text_graph` 三列的深度只差 1 格（3 vs 2），差異弱到連方向接反
        /// 都測不出來。這個 fixture 刻意拉開：c0/c2 只有一顆 dot（深度 1），
        /// c1 前面接了一條橫跨 4 欄的線（深度 5 = cell_count），足以證明
        /// 「每列各自貼齊」不是「整欄一起左移」的巧合。
        fn staggered_depth_graph(commits: &[Commit]) -> Graph {
            Graph {
                commit_hashes: commits.iter().map(|c| c.commit_hash.clone()).collect(),
                commit_pos_map: commits
                    .iter()
                    .map(|c| c.commit_hash.clone())
                    .zip([(0, 0), (4, 1), (0, 2)])
                    .collect(),
                edges: vec![
                    vec![],
                    vec![
                        Edge::new(EdgeType::Horizontal, 0, 0),
                        Edge::new(EdgeType::Horizontal, 1, 1),
                        Edge::new(EdgeType::Horizontal, 2, 2),
                        Edge::new(EdgeType::Horizontal, 3, 3),
                    ],
                    vec![],
                ],
                max_pos_x: 4,
            }
        }

        #[test]
        fn compact_row_text_starts_after_that_rows_last_glyph() {
            let commits = text_graph_commits();
            let mut state = build_state(&commits, staggered_depth_graph(&commits), Opts::default());
            let buf = render_commit_list_compact(&mut state, 10, GraphWidthType::Single);

            // c0/c2：只有一顆 dot 在欄 0，text_x=1，前導空白在 x=1，
            // subject 的第一個字在 x=2。
            assert_eq!(
                buf[(2, 1)].symbol(),
                "f",
                "first 的 f，貼在 c0 自己的 dot 後面"
            );
            assert_eq!(
                buf[(2, 3)].symbol(),
                "t",
                "third 的 t，貼在 c2 自己的 dot 後面"
            );
            // c1：橫線一路接到 dot（欄 4），cell_count=5，深度打滿，
            // text_x=5，跟非緊湊模式的位置一樣。
            assert_eq!(
                buf[(6, 2)].symbol(),
                "s",
                "second 的 s，這一列深度打滿，位置沒被緊湊模式改變"
            );
        }

        #[test]
        fn compact_falls_back_when_subject_does_not_follow_graph() {
            // Subject 排在 Graph 前面：compact_possible 回 false，緊湊
            // 模式整個不生效，版面要跟非緊湊逐格一樣。
            let commits = text_graph_commits();
            let mut state = build_state(&commits, staggered_depth_graph(&commits), Opts::default());
            let mut ui_config = UiConfig::default();
            ui_config.list.columns = vec![UserListColumnType::Subject, UserListColumnType::Graph];
            let ctx = Rc::new(AppContext {
                keybind: KeyBind::new(None),
                core_config: CoreConfig::default(),
                ui_config,
                color_theme: ColorTheme::default(),
                graph_style: GraphStyle::Rounded,
                graph_width: Some(GraphWidthType::Single),
                compact: Some(CompactType::On),
            });
            let area = Rect::new(0, 0, TERM_W, 10);
            let mut buf = Buffer::empty(area);
            CommitList::new(ctx, 0).render(area, &mut buf, &mut state);

            assert!(
                !state.is_compact(),
                "columns 排不出「Graph 緊接 Subject」，decide() 要直接關掉緊湊"
            );
        }

        #[test]
        fn compact_removes_the_marker_column() {
            let commits = text_graph_commits();
            let mut state = build_state(&commits, text_graph(&commits), Opts::default());
            let buf = render_commit_list_compact(&mut state, 10, GraphWidthType::Double);

            // 非緊湊模式下這一格是 marker 的 `│`（見
            // render_graph_uses_ascii_style 的同一個位置）；緊湊模式下
            // c0 這一列深度打滿（text_x=6），這格改成 subject 的第一個字。
            assert_eq!(
                buf[(7, 1)].symbol(),
                "f",
                "marker 不見了，x=7 變成 subject 的 f（first）"
            );
        }

        #[test]
        fn compact_virtual_row_text_follows_the_dot() {
            let commits = text_graph_commits();
            let mut state = build_state(
                &commits,
                text_graph(&commits),
                Opts {
                    working_changes: true,
                    ..Default::default()
                },
            );
            let buf = render_commit_list_compact(&mut state, 10, GraphWidthType::Double);

            // virtual row 的 dot 畫在 cell index 2（見
            // virtual_row_draws_gray_head_dot_at_top 對同一個 fixture 的
            // 斷言），`RowGraph::Dot` 存的就是這個 cell index，text_x =
            // 2+1 = 3；"Uncommitted..." 的 U 貼在 x=4。
            assert_eq!(buf[(4, 1)].symbol(), "U");
        }

        #[test]
        fn compact_spacer_rows_keep_their_vertical_lines() {
            let commits = text_graph_commits();
            let mut state = build_state(
                &commits,
                text_graph_colliding_same_line(&commits),
                Opts {
                    inline_detail_height: 1,
                    ..Default::default()
                },
            );
            let buf = render_commit_list_compact(&mut state, 10, GraphWidthType::Double);

            // 跟 spacer_row_extends_only_vertical_columns 同一個 fixture／
            // 斷言，只是加上緊湊模式 —— spacer 是獨立算的
            // （draw_graph_spacer），不吃 VisibleRow 的 text_x，緊湊與否
            // 不該讓它消失。
            assert_eq!(buf[(2, 2)].symbol(), "│");
        }

        /// HEAD 不是第一列時，`render_graph` 會在排在它前面、`cells[hc]`
        /// 是 Blank 的列上合成一條向上連接線（`hc` = HEAD 自己的 dot
        /// 欄）。`text_x` 若只看 `cells` 本身最後一個非 Blank 格（不管
        /// 這條合成線），會算出太小的值，讓 subject 的文字直接畫過去、
        /// 蓋掉這條線 —— 這是 `RowGraph::extent()` 要 `max(cells_end,
        /// connector_end)` 的原因。
        fn head_not_first_graph(commits: &[Commit]) -> Graph {
            Graph {
                commit_hashes: commits.iter().map(|c| c.commit_hash.clone()).collect(),
                commit_pos_map: commits
                    .iter()
                    .map(|c| c.commit_hash.clone())
                    .zip([(0, 0), (3, 1), (0, 2)])
                    .collect(),
                edges: vec![vec![], vec![], vec![]],
                max_pos_x: 3,
            }
        }

        #[test]
        fn compact_text_does_not_cover_the_synthesized_head_connector() {
            let commits = text_graph_commits();
            let mut state = build_state(
                &commits,
                head_not_first_graph(&commits),
                Opts {
                    head_hash: Some(1), // c1 是 HEAD，排在 c0 後面（顯示上 c0 在它上面）
                    working_changes: true,
                    ..Default::default()
                },
            );
            let buf = render_commit_list_compact(&mut state, 10, GraphWidthType::Double);

            // c0（row0，y=2）自己只有一顆 dot 在欄 0；HEAD（c1，pos_x=3）
            // 的 dot 欄是 double-width 的 cell index 6。c0 那一列在欄 6
            // 是 Blank，所以會合成一條灰色 │。若 text_x 沒把它算進去，
            // 這格會被 subject 的文字蓋掉。
            assert_eq!(
                buf[(6, 2)].symbol(),
                "│",
                "HEAD 上方那條合成連接線沒被緊湊模式的文字蓋掉"
            );
            assert_eq!(buf[(6, 2)].fg, Color::Gray, "VIRTUAL_ROW_COLOR");
        }

        #[test]
        fn compact_selected_row_keeps_graph_colors_under_the_selection_bg() {
            let commits = text_graph_commits();

            let mut reference = build_state(&commits, text_graph(&commits), Opts::default());
            reference.selected = 2; // c2：唯一在 text_graph 裡有非零深度差的列
            let reference_buf = render_commit_list(&mut reference, 10);
            let expected_dot_fg = reference_buf[(4, 3)].fg;

            let mut state = build_state(&commits, text_graph(&commits), Opts::default());
            state.selected = 2;
            let buf = render_commit_list_compact(&mut state, 10, GraphWidthType::Double);

            assert_eq!(
                buf[(4, 3)].fg,
                expected_dot_fg,
                "graph 的分支色不會被 subject 那個 Line 的 fg patch 蓋掉 —— \
                 兩個 writer 的 Rect 由 text_x 保證不重疊，不是靠繪製順序撐著"
            );
            for x in 0..TERM_W {
                assert_eq!(
                    buf[(x, 3)].bg,
                    SELECTED_BG,
                    "選取列整列 bg 都要覆蓋到，x={x}"
                );
            }
        }
    }

    #[test]
    fn test_calc_cell_widths_all_columns() {
        let area_width = 80;
        let subject_min_width = 20;
        // calc_cell_widths 現在自己加右側留白（見 layout::required_width），
        // 輸入是不含 padding 的 graph_cell_width，5 + 1 = 原本斷言的 6。
        let graph_width = 5;
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
            false,
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
        // calc_cell_widths 現在自己加右側留白（見 layout::required_width），
        // 輸入是不含 padding 的 graph_cell_width，5 + 1 = 原本斷言的 6。
        let graph_width = 5;
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
            false,
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
        // calc_cell_widths 現在自己加右側留白（見 layout::required_width），
        // 輸入是不含 padding 的 graph_cell_width，5 + 1 = 原本斷言的 6。
        let graph_width = 5;
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
            false,
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
        // calc_cell_widths 現在自己加右側留白（見 layout::required_width），
        // 輸入是不含 padding 的 graph_cell_width，5 + 1 = 原本斷言的 6。
        let graph_width = 5;
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
            false,
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
        // calc_cell_widths 現在自己加右側留白（見 layout::required_width），
        // 輸入是不含 padding 的 graph_cell_width，5 + 1 = 原本斷言的 6。
        let graph_width = 5;
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
            false,
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

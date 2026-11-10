use std::{borrow::Cow, num::NonZeroU16};

use ratatui::{
    buffer::{Buffer, CellDiffOption},
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Padding, Paragraph, Wrap},
    Frame,
};
use rust_i18n::t;

use crate::{
    color::{MUTED_FG, TEXT_COLOR},
    github::{CiState, GhIssue, GhPullRequest},
    widget::truncate_line,
};

use super::{
    branches, ci,
    preview::{borrow_lines, RowData},
    GitHubFocus, GitHubTab, GitHubView, LabelMode, LoadState,
};

/// 列表列的欄位配置：paragraph padding (1) + indicator (2)。
/// 要跟 `render_issue_line` / `render_pr_line` 保持同步——如果 indicator
/// 寬度或 Paragraph padding 改了，這個常數也要跟著調整。
const LIST_LINK_COL_OFFSET: u16 = 3;

/// 在 `(x, y)` 寫入 OSC 8 超連結，後面 `width - 1` 格標成 Skip。
///
/// 第一格必須設 `ForcedWidth`：Ratatui 的 diff 會用 symbol 的顯示寬度決定
/// 要跳過幾格，而整串 escape（含 URL）會被算成幾十格寬，diff 就跳過 label
/// 之後那些格子不更新，舊畫面（Branches 卡片、graph 圖片）會殘留在畫面上。
fn set_osc8_link(buf: &mut Buffer, x: u16, y: u16, url: &str, label: &str, width: u16) {
    let Some(forced) = NonZeroU16::new(width) else {
        return;
    };
    buf[(x, y)]
        .set_symbol(&crate::external::format_osc8_hyperlink(url, label))
        .set_diff_option(CellDiffOption::ForcedWidth(forced));
    for i in 1..width {
        buf[(x + i, y)].set_diff_option(CellDiffOption::Skip);
    }
}

impl<'a> GitHubView<'a> {
    pub fn render(&mut self, f: &mut Frame, area: Rect, marquee_frame: u64) {
        self.height = area.height as usize;
        self.width = area.width as usize;
        // 視窗縮放會改變卡片欄數與可見列數，先把選取重新捲進畫面
        if self.active_tab.is_card_tab() {
            self.adjust_scroll();
        }
        // render 是溢出狀態的唯一真相來源——在進入時重置，讓跳過 render_list
        // 的 focus（CheckboxEdit、Prompt）能自動清除。
        self.selected_row_overflows.set(false);

        // ── 三區 split：頂部 tab/prompt + 下半 list|preview ──
        let [header_area, content_area] =
            Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).areas(area);

        self.render_header(f, header_area);

        // ── Loading / 錯誤提示 ──
        if self.current_list_len() == 0 {
            let (text, color) = match self.list_load_state() {
                LoadState::Loading => (t!("github.render.loading").into_owned(), MUTED_FG),
                LoadState::Error(err) => (err.clone(), Color::Red),
                LoadState::Idle => (t!("github.render.empty").into_owned(), MUTED_FG),
            };
            render_centered_message(f, content_area, text, color);
            return;
        }

        if self.active_tab == GitHubTab::Branches {
            self.render_branches(f, content_area);
        } else if self.active_tab == GitHubTab::Ci {
            self.render_ci(f, content_area);
        } else if self.is_narrow() {
            // 窄版單欄：焦點在 preview（含 checkbox 編輯）就畫 preview，其餘畫清單
            if matches!(self.focus, GitHubFocus::Preview | GitHubFocus::CheckboxEdit) {
                self.render_preview(f, content_area);
            } else {
                self.render_list(f, content_area, marquee_frame);
            }
        } else {
            let [list_area, preview_area] =
                Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
                    .areas(content_area);

            self.render_list(f, list_area, marquee_frame);
            self.render_preview(f, preview_area);
        }

        // ── 提示訊息 ──
        if let Some((ref msg, is_error)) = self.flash_message {
            let color = if is_error { Color::Red } else { TEXT_COLOR };
            let flash_area = Rect::new(
                content_area.x,
                content_area.bottom().saturating_sub(1),
                content_area.width,
                1,
            );
            let flash = Paragraph::new(truncate_line(
                Line::from(Span::styled(msg.as_str(), Style::default().fg(color))),
                content_area.width as usize,
            ))
            .alignment(ratatui::layout::Alignment::Center);
            f.render_widget(flash, flash_area);
        }
    }

    fn render_header(&self, f: &mut Frame, area: Rect) {
        let narrow = self.is_narrow();
        let pending_ci = self
            .ci
            .iter()
            .filter(|e| e.state() == CiState::Pending)
            .count();
        // 窄版用短標籤（4 個分頁都要看得到），其餘寬度留給分頁本身
        let (issues_label, prs_label, branches_label, ci_label) = if narrow {
            (
                t!("github.render.tab_issues_short", n = self.issues.len()),
                t!("github.render.tab_prs_short", n = self.pull_requests.len()),
                t!("github.render.tab_branches_short", n = self.branches.len()),
                t!("github.render.tab_ci_short", n = pending_ci),
            )
        } else {
            (
                t!("github.render.tab_issues", n = self.issues.len()),
                t!("github.render.tab_prs", n = self.pull_requests.len()),
                t!("github.render.tab_branches", n = self.branches.len()),
                t!("github.render.tab_ci", n = pending_ci),
            )
        };
        let tab_style = |tab: GitHubTab| {
            if self.active_tab == tab {
                Style::default().fg(Color::Black).bg(Color::Cyan).bold()
            } else {
                Style::default().fg(MUTED_FG)
            }
        };

        let mut tab_spans = vec![
            Span::styled(issues_label, tab_style(GitHubTab::Issues)),
            Span::raw(" "),
            Span::styled(prs_label, tab_style(GitHubTab::PullRequests)),
            Span::raw(" "),
            Span::styled(branches_label, tab_style(GitHubTab::Branches)),
            Span::raw(" "),
            Span::styled(ci_label, tab_style(GitHubTab::Ci)),
        ];
        // 篩選標籤、符合數、重新載入提示在窄版放不下，篩選狀態看 prompt 列即可
        if !narrow {
            let filter_label = self.state_filter.label();
            let count = self.current_list_len();
            tab_spans.extend([
                Span::raw("  "),
                // state filter 只管 issue／PR；Branches 分頁沒有 state，只顯示作者篩選
                Span::styled(
                    match self.active_tab {
                        GitHubTab::Branches => match self.current_author() {
                            Some(author) => format!("[@{author}]"),
                            None => t!("github.render.all_authors").into_owned(),
                        },
                        GitHubTab::Ci => String::new(),
                        GitHubTab::Issues | GitHubTab::PullRequests => {
                            match self.current_author() {
                                Some(author) => format!("\u{f0232} {filter_label} [@{author}]"),
                                None => format!("\u{f0232} {filter_label}"),
                            }
                        }
                    },
                    Style::default().fg(MUTED_FG),
                ),
            ]);
            if self.has_active_filter() {
                tab_spans.push(Span::styled(
                    t!("github.render.matched", n = count),
                    Style::default().fg(MUTED_FG),
                ));
            }
            if matches!(self.list_load_state(), LoadState::Loading) {
                tab_spans.push(Span::styled(
                    t!("github.render.refetching"),
                    Style::default().fg(Color::Yellow),
                ));
            }
        }
        let tab_line = Line::from(tab_spans);

        // Prompt 輸入列
        let prompt_color = if self.focus == GitHubFocus::Prompt {
            Color::Cyan
        } else {
            MUTED_FG
        };
        let prompt_prefix = Span::styled("\u{f002} ", Style::default().fg(prompt_color));
        let prompt_value = Span::raw(self.search_input.value().to_string());
        let prompt_line = Line::from(vec![
            Span::raw("  "), // 左側留白
            prompt_prefix,
            prompt_value,
        ]);

        let [tab_area, prompt_area] =
            Layout::vertical([Constraint::Length(2), Constraint::Length(1)]).areas(area);

        let pad_x = if narrow { 0 } else { 2 };
        f.render_widget(
            Paragraph::new(tab_line)
                .block(Block::default().padding(Padding::new(pad_x, pad_x, 1, 0))),
            tab_area,
        );

        f.render_widget(Paragraph::new(prompt_line), prompt_area);

        // focus 在 prompt 上時顯示游標
        if self.focus == GitHubFocus::Prompt {
            let cursor_x = prompt_area.x + 2 /* 留白 */ + 2 /* "\u{f002} " */ + self.search_input.visual_cursor() as u16;
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
            MUTED_FG
        };
        let block = Block::default()
            .borders(Borders::RIGHT)
            .border_style(Style::default().fg(list_border_color));
        let inner = block.inner(area);
        f.render_widget(block, area);

        let has_next = self.current_has_next_cursor();
        // 有下一頁時保留一列給 load-more 指示器
        let visible_height = if has_next {
            (inner.height as usize).saturating_sub(1)
        } else {
            inner.height as usize
        };

        let rows = self.current_viewport_rows(visible_height, inner.width, marquee_frame);
        let mut lines: Vec<Line<'static>> = rows.iter().map(|r| r.line.clone()).collect();

        if has_next {
            let hint = if self.loading_more {
                t!("github.render.loading_more")
            } else {
                t!("github.render.more")
            };
            lines.push(Line::styled(hint, Style::default().fg(MUTED_FG)));
        }

        let list_paragraph =
            Paragraph::new(lines).block(Block::default().padding(Padding::horizontal(1)));
        f.render_widget(list_paragraph, inner);

        // 對每個可視列的 `#N` 疊加 OSC 8。cell 版面配置定義在
        // `LIST_LINK_COL_OFFSET`——要跟 indicator + padding 保持同步。
        //
        // OSC 8 只用在「畫面上那段文字本身不可能是網址」的地方——`#123`
        // 沒有終端能猜出它是連結，只能靠 OSC 8。這裡能安全疊加是因為底稿
        // 已經畫了同寬度的純文字 `#N`：tmux 的 DCS passthrough 會遺失游標
        // 定位，導致 host 終端機把 label 畫到任意欄位，但在 tmux 裡跳過疊加
        // 時看到的仍是正確的純文字，不會露出殘骸（跟狀態列那則通知改印純
        // URL 是同一個判準，見 `app.rs::open_url`）。
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
            // 寬度不足以放下完整的 `#N`——跳過疊加（部分超連結比沒有更糟）
            if label_width > remaining {
                continue;
            }
            set_osc8_link(buf, x, y, &row.url, &label, label_width);
        }
    }

    fn labels_pad_width_for_tab(&self, mode: LabelMode) -> usize {
        match self.active_tab {
            GitHubTab::Issues => self
                .issues
                .iter()
                .map(|i| labels_display_width(&i.labels, mode))
                .max()
                .unwrap_or(0),
            GitHubTab::PullRequests => self
                .pull_requests
                .iter()
                .map(|p| labels_display_width(&p.labels, mode))
                .max()
                .unwrap_or(0),
            GitHubTab::Branches | GitHubTab::Ci => 0,
        }
    }

    /// 作者欄寬度：整份已載入清單裡最寬的 `@login`（上限 `AUTHOR_MAX_WIDTH`）。
    /// 跟 label 欄一樣以整份清單為準，捲動或篩選時欄位才不會左右晃。
    fn author_pad_width_for_tab(&self) -> usize {
        let width = |login: &str| console::measure_text_width(&author_text(login));
        match self.active_tab {
            GitHubTab::Issues => self
                .issues
                .iter()
                .map(|i| width(&i.author.login))
                .max()
                .unwrap_or(0),
            GitHubTab::PullRequests => self
                .pull_requests
                .iter()
                .map(|p| width(&p.author.login))
                .max()
                .unwrap_or(0),
            GitHubTab::Branches | GitHubTab::Ci => 0,
        }
    }

    fn current_viewport_rows(
        &self,
        visible_height: usize,
        inner_width: u16,
        marquee_frame: u64,
    ) -> Vec<RowData> {
        let narrow = self.is_narrow();
        // 窄版 label 一律用色塊，名稱會把標題擠掉
        let label_mode = if narrow {
            LabelMode::Swatches
        } else {
            self.label_mode
        };
        let layout = RowLayout {
            label_mode,
            labels_pad: self.labels_pad_width_for_tab(label_mode),
            // 窄版作者不補寬，空間留給標題
            author_pad: if narrow {
                0
            } else {
                self.author_pad_width_for_tab()
            },
            // Paragraph 內部有 Padding::horizontal(1) → 內容可用寬度要 -2。
            content_width: inner_width.saturating_sub(2) as usize,
            narrow,
        };
        let mut rows = Vec::with_capacity(visible_height);
        let mut overflow = false;

        let make_issue = |issue: &GhIssue, vis_i: usize| -> (RowData, bool) {
            let is_selected = vis_i == self.selected_index;
            let frame = is_selected.then_some(marquee_frame);
            let (line, did_scroll) = render_issue_line(issue, layout, is_selected, frame);
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
            let (line, did_scroll) = render_pr_line(pr, layout, is_selected, frame);
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
                // Branches、CI 走卡片版面，不經過列表列
                GitHubTab::Branches | GitHubTab::Ci => {}
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
                GitHubTab::Branches | GitHubTab::Ci => {}
            }
        }
        self.selected_row_overflows.set(overflow);
        rows
    }

    /// 卡片分頁共用的排版：依寬度排成多欄固定高度的卡片，回傳每張可見卡片的
    /// (可視索引, 區域)。
    fn card_slots(&self, cards_area: Rect) -> Vec<(usize, Rect)> {
        let cols = self.branch_columns();
        let card_width = cards_area.width / cols as u16;
        let visible_rows = (cards_area.height / branches::CARD_HEIGHT) as usize;
        // `offset` 對齊列首（見 `adjust_branch_scroll`）；縮放後也在這裡再對齊一次
        let first = self.offset / cols * cols;
        (first..self.current_list_len())
            .take(visible_rows * cols)
            .enumerate()
            .map(|(slot, vis_i)| {
                let (row, col) = ((slot / cols) as u16, (slot % cols) as u16);
                // 除不盡的寬度由最後一欄吃掉，整列才會貼滿
                let width = if col as usize == cols - 1 {
                    cards_area.width - col * card_width
                } else {
                    card_width
                };
                let area = Rect {
                    x: cards_area.x + col * card_width,
                    y: cards_area.y + row * branches::CARD_HEIGHT,
                    width,
                    height: branches::CARD_HEIGHT,
                };
                (vis_i, area)
            })
            .collect()
    }

    /// CI 分頁：沿用 Branches 的卡片版面，沒有 preview。
    fn render_ci(&self, f: &mut Frame, area: Rect) {
        f.render_widget(Clear, area);
        for (vis_i, card_area) in self.card_slots(area) {
            let Some(entry) = self.ci.get(self.actual_index(vis_i)) else {
                break;
            };
            ci::render_card(f, card_area, entry, vis_i == self.selected_index);
        }
    }

    /// Branches 分頁：內容區依寬度排成多欄固定高度的卡片，沒有 preview。
    fn render_branches(&self, f: &mut Frame, area: Rect) {
        f.render_widget(Clear, area);

        // 候選超過上限時，底部留一列說明只顯示了最近的一部分
        let footer = self.branches_truncated && area.height > branches::CARD_HEIGHT;
        let cards_area = Rect {
            height: area.height - u16::from(footer),
            ..area
        };

        for (vis_i, card_area) in self.card_slots(cards_area) {
            let Some(branch) = self.branches.get(self.actual_index(vis_i)) else {
                break;
            };
            branches::render_card(
                f,
                card_area,
                branch,
                vis_i == self.selected_index,
                self.diff_stat_thresholds,
            );
        }

        if footer {
            let footer_area = Rect {
                y: area.bottom() - 1,
                height: 1,
                ..area
            };
            f.render_widget(
                Paragraph::new(truncate_line(
                    Line::styled(
                        t!(
                            "github.render.branches_truncated",
                            n = crate::github::BRANCH_LIMIT
                        ),
                        Style::default().fg(Color::Gray),
                    ),
                    area.width as usize,
                ))
                .alignment(ratatui::layout::Alignment::Center),
                footer_area,
            );
        }
    }

    fn render_preview(&mut self, f: &mut Frame, area: Rect) {
        if self.focus == GitHubFocus::CheckboxEdit {
            self.render_checkbox_preview(f, area);
            return;
        }

        let block = Block::default().padding(Padding::horizontal(1));
        let inner = block.inner(area);
        f.render_widget(block, area);

        // render 是 preview 可用高度的真相來源；捲動處理函式直接讀回這個值，
        // 不會從 `height` 重新推算。
        self.preview_height = inner.height as usize;

        // `preview_input(&self)` 借走整個 self，borrow checker 看不出它沒碰
        // preview_cache，所以無法同時 &mut 它。先剝離成 local 讓兩個借用
        // 不重疊，順序不能反過來。還原要留在下面幾個 early return 之前——
        // 挪到函式尾端的話，return 會把 cache 丟掉，preview 靜默變空白。
        let mut cache = std::mem::take(&mut self.preview_cache);
        let visual_len = cache.get_or_build(&self.preview_input(inner.width));
        self.preview_cache = cache;

        self.last_preview_len = visual_len;
        // 限制 preview_offset，避免捲過內容底端。兩邊算的都是視覺（折行後）
        // 行數——`Paragraph::scroll` 跳過的是折行後的行，不是原始行。`u16`
        // 的邊界也該放在這裡，讓狀態跟畫面不會對「底端在哪」意見不合。
        let max_offset = visual_len
            .saturating_sub(inner.height as usize)
            .min(u16::MAX as usize);
        self.preview_offset = self.preview_offset.min(max_offset);
        let scroll = self.preview_offset as u16;

        let paragraph = Paragraph::new(borrow_lines(self.preview_cache.lines()))
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0));
        f.render_widget(paragraph, inner);

        // 用 OSC 8 超連結疊加 `#N` cell。必須在 Paragraph render 之後執行，
        // 才能覆蓋掉先前畫上去的純文字 `#N` 字元。
        //
        // 跳過 tmux 的判準同上面 `render_list` 那份註解：能安全 early-return
        // 是因為底下已經有同寬度的純文字 `#N` 可以退回。
        if crate::external::is_tmux() {
            return;
        }
        // header 的位置只有在還沒捲動時才可信：捲動偏移量算的是折行後的行，
        // 不是原始行，所以一旦 `scroll != 0`，header 本身就已經捲出畫面。
        // 要在那之後還原超連結，需要把連結附著在 span 上，而不是存一個座標。
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
        let label_width = console::measure_text_width(&ov.label) as u16;
        let remaining = inner.right() - x;
        set_osc8_link(
            f.buffer_mut(),
            x,
            y,
            &ov.url,
            &ov.label,
            label_width.min(remaining),
        );
    }

    fn render_checkbox_preview(&self, f: &mut Frame, area: Rect) {
        let Some(ref panel) = self.task_panel else {
            return;
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow))
            .title(t!("github.render.tasks_title"))
            .padding(Padding::horizontal(1));
        let inner = block.inner(area);
        f.render_widget(block, area);

        // 可用高度扣掉 footer 那一列
        let content_height = inner.height.saturating_sub(1) as usize;

        // 給過長 task 清單用的捲動偏移量
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
                let checkbox_color = if item.checked { Color::Green } else { MUTED_FG };
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

        // Footer 列
        lines.push(truncate_line(
            Line::from(Span::styled(
                t!("github.render.tasks_footer"),
                Style::default().fg(MUTED_FG),
            )),
            inner.width as usize,
        ));

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

fn label_color(label: &crate::github::GhLabel) -> Color {
    label
        .color
        .as_deref()
        .map(hex_to_color)
        .unwrap_or(Color::Yellow)
}

/// GitHub 狀態值（OPEN／CLOSED／MERGED）的顯示文字。未知的值原樣轉小寫。
pub(super) fn state_label(state: &str) -> Cow<'static, str> {
    match state {
        "OPEN" => t!("github.state.open"),
        "CLOSED" => t!("github.state.closed"),
        "MERGED" => t!("github.state.merged"),
        other => Cow::Owned(other.to_lowercase()),
    }
}

/// 列表列的狀態欄固定 8 格寬（最寬的標籤是「已 merge」）。中文字佔 2 格，
/// 所以要用顯示寬度補空白，不能用 `{:<8}`（那是照字元數補）。
const STATE_COL_WIDTH: usize = 8;

fn pad_state_cell(label: &str) -> String {
    console::pad_str(label, STATE_COL_WIDTH, console::Alignment::Left, None).into_owned()
}

pub(super) fn state_color(state: &str) -> Color {
    match state {
        "OPEN" => Color::Green,
        "CLOSED" => Color::Red,
        "MERGED" => Color::Magenta,
        _ => Color::Gray,
    }
}

/// 前面帶一個空白，給 preview 標題接在別的 span 後面用；列表列用
/// `label_name_spans`（欄位間距由列組裝處統一加）。
pub(super) fn label_spans(labels: &[crate::github::GhLabel]) -> Vec<Span<'static>> {
    if labels.is_empty() {
        return vec![];
    }
    let mut spans = vec![Span::raw(" ")];
    spans.extend(label_name_spans(labels));
    spans
}

/// `[bug, enhancement]`，每個名字用該 label 的顏色。
fn label_name_spans(labels: &[crate::github::GhLabel]) -> Vec<Span<'static>> {
    if labels.is_empty() {
        return vec![];
    }
    let mut spans = vec![Span::raw("[")];
    for (i, label) in labels.iter().enumerate() {
        spans.push(Span::styled(
            label.name.clone(),
            Style::default().fg(label_color(label)),
        ));
        if i < labels.len() - 1 {
            spans.push(Span::raw(", "));
        }
    }
    spans.push(Span::raw("]"));
    spans
}

/// 色塊字元，label 精簡模式與 CI check 收合摘要共用。用 `▮`
/// （U+25AE，East Asian Width = Neutral）而不是看起來更方正的 `■`
/// （U+25A0，Ambiguous）——CJK 終端機把 ambiguous 字元設成寬字元時，`■`
/// 會佔 2 格但 `Span::width` 只算 1 格，色塊一多 pad 就整排錯位。
pub(super) const SWATCH: &str = "▮";

/// 精簡模式：每個 label 一個該顏色的方塊，緊密排列，不顯示名稱。
fn label_swatch_spans(labels: &[crate::github::GhLabel]) -> Vec<Span<'static>> {
    labels
        .iter()
        .map(|label| Span::styled(SWATCH, Style::default().fg(label_color(label))))
        .collect()
}

/// 依 `mode` 選擇 label 的呈現方式，是 render 這行跟量寬度唯一的共用來源
/// ——`labels_display_width` 直接加總這裡吐出的 span 寬度，不會另外手算
/// 一次公式，兩邊就不會漂移。
fn row_label_spans(labels: &[crate::github::GhLabel], mode: LabelMode) -> Vec<Span<'static>> {
    match mode {
        LabelMode::Names => label_name_spans(labels),
        LabelMode::Swatches => label_swatch_spans(labels),
    }
}

/// 渲染列尾的標題：沒有溢出時保持截斷／原樣，
/// 有溢出且被選取、又有 frame 時則透過跑馬燈捲動。
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
        // 未選取的溢出列：用刪節號截斷
        let truncated = console::truncate_str(tail, available, "…").to_string();
        return (vec![Span::styled(truncated, style_title)], false);
    }
    (vec![Span::styled(tail.to_string(), style_title)], false)
}

/// `row_label_spans(labels, mode)` 之後補 pad 空格，讓整欄寬度一致。
fn label_cell(
    labels: &[crate::github::GhLabel],
    mode: LabelMode,
    pad: usize,
) -> Vec<Span<'static>> {
    let mut spans = row_label_spans(labels, mode);
    let used = spans.iter().map(Span::width).sum::<usize>();
    if pad > used {
        spans.push(Span::raw(" ".repeat(pad - used)));
    }
    spans
}

/// 狀態格：色塊模式是 `[▮]`（只有 `▮` 帶狀態色），文字模式固定 8 格寬。
fn state_cell(text: &str, color: Color, mode: LabelMode) -> Vec<Span<'static>> {
    match mode {
        LabelMode::Swatches => vec![
            Span::styled("[", Style::default().fg(MUTED_FG)),
            Span::styled(SWATCH, Style::default().fg(color)),
            Span::styled("]", Style::default().fg(MUTED_FG)),
        ],
        LabelMode::Names => vec![Span::styled(
            pad_state_cell(text),
            Style::default().fg(color),
        )],
    }
}

/// 作者欄最寬顯示格數，超過的 login 用 `…` 截斷，避免一個長 bot 名稱吃掉標題。
const AUTHOR_MAX_WIDTH: usize = 16;

/// `@login`，超過 `AUTHOR_MAX_WIDTH` 截斷。
fn author_text(login: &str) -> String {
    console::truncate_str(&format!("@{login}"), AUTHOR_MAX_WIDTH, "…").into_owned()
}

/// 列表一列的排版參數，Issues／PRs 共用，整份列表算一次。
#[derive(Clone, Copy)]
struct RowLayout {
    label_mode: LabelMode,
    labels_pad: usize,
    author_pad: usize,
    content_width: usize,
    narrow: bool,
}

/// 一列要顯示的內容，Issues／PRs 共用。
struct RowFields<'a> {
    number: u64,
    state_text: &'a str,
    state_color: Color,
    labels: &'a [crate::github::GhLabel],
    author: &'a str,
    title: &'a str,
}

/// 組出一列：`▸ #N  狀態  標籤  @作者  標題`。回傳 `(line, scrolled)`，
/// `scrolled=true` 代表標題因溢出而套用了跑馬燈——呼叫端要讓跑馬燈計時器繼續跳動。
///
/// 欄位之間固定留 `gap`；沒有標籤時整欄省略。窄版沒有狀態格，改用 `#N`
/// 的顏色表示狀態，`#N`、作者都不補寬，標題才有地方放。
fn render_row(
    row: &RowFields<'_>,
    layout: RowLayout,
    selected: bool,
    marquee_frame: Option<u64>,
) -> (Line<'static>, bool) {
    let indicator = if selected { "▸ " } else { "  " };
    let style = if selected {
        Style::default().fg(Color::Cyan).bold()
    } else {
        Style::default()
    };
    let gap = if layout.narrow { " " } else { "  " };

    let mut cells: Vec<Vec<Span<'static>>> = Vec::with_capacity(4);
    if layout.narrow {
        cells.push(vec![Span::styled(
            format!("#{}", row.number),
            Style::default().fg(row.state_color),
        )]);
    } else {
        cells.push(vec![Span::styled(
            format!("#{:<5}", row.number),
            Style::default().fg(MUTED_FG),
        )]);
        cells.push(state_cell(
            row.state_text,
            row.state_color,
            layout.label_mode,
        ));
    }
    if layout.labels_pad > 0 {
        cells.push(label_cell(row.labels, layout.label_mode, layout.labels_pad));
    }
    cells.push(vec![Span::raw(
        console::pad_str(
            &author_text(row.author),
            layout.author_pad,
            console::Alignment::Left,
            None,
        )
        .into_owned(),
    )]);

    let mut spans = vec![Span::styled(indicator.to_string(), style)];
    for cell in cells {
        spans.extend(cell);
        spans.push(Span::raw(gap));
    }

    // 寬度只有一個來源：直接加總已組好的 span，不另外手算公式。
    let prefix_width: usize = spans.iter().map(Span::width).sum();
    let (tail, scrolled) = tail_spans(
        row.title,
        layout.content_width.saturating_sub(prefix_width),
        marquee_frame,
        style,
    );
    spans.extend(tail);
    (Line::from(spans), scrolled)
}

fn render_issue_line(
    issue: &GhIssue,
    layout: RowLayout,
    selected: bool,
    marquee_frame: Option<u64>,
) -> (Line<'static>, bool) {
    let state_text = state_label(&issue.state);
    render_row(
        &RowFields {
            number: issue.number,
            state_text: &state_text,
            state_color: state_color(&issue.state),
            labels: &issue.labels,
            author: &issue.author.login,
            title: &issue.title,
        },
        layout,
        selected,
        marquee_frame,
    )
}

fn render_pr_line(
    pr: &GhPullRequest,
    layout: RowLayout,
    selected: bool,
    marquee_frame: Option<u64>,
) -> (Line<'static>, bool) {
    let (color, state_text) = if pr.is_draft {
        (Color::Gray, t!("github.state.draft"))
    } else {
        (state_color(&pr.state), state_label(&pr.state))
    };
    render_row(
        &RowFields {
            number: pr.number,
            state_text: &state_text,
            state_color: color,
            labels: &pr.labels,
            author: &pr.author.login,
            title: &pr.title,
        },
        layout,
        selected,
        marquee_frame,
    )
}

/// `row_label_spans(labels, mode)` 佔用的可視格數總和。直接加總 span 寬度
/// 而不是另外手算一次公式，是唯一的寬度來源——不然色塊格式（例如 `▮`
/// 之間要不要留空白）改了，這裡忘記同步就會悄悄跟畫面上的實際寬度脫鉤。
fn labels_display_width(labels: &[crate::github::GhLabel], mode: LabelMode) -> usize {
    row_label_spans(labels, mode).iter().map(Span::width).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 舊畫面整列都是字，新畫面只有 `#1` 超連結：label 之後的格子必須
    /// 出現在 diff 裡被清掉，不能因為 escape 字串很長就被跳過。
    #[test]
    fn osc8_link_does_not_hide_trailing_cells_from_diff() {
        let area = Rect::new(0, 0, 80, 1);
        let mut prev = Buffer::empty(area);
        prev.set_string(0, 0, "x".repeat(80), Style::default());
        let mut next = Buffer::empty(area);
        set_osc8_link(&mut next, 0, 0, "https://github.com/o/r/issues/1", "#1", 2);

        let updated: Vec<u16> = prev.diff(&next).iter().map(|(x, _, _)| *x).collect();

        assert!(updated.contains(&0), "{updated:?}");
        assert!(!updated.contains(&1), "{updated:?}");
        assert!((2..80).all(|x| updated.contains(&x)), "{updated:?}");
    }
}

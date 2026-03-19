use std::rc::Rc;

use ratatui::{
    crossterm::event::KeyEvent,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Padding, Paragraph},
    Frame,
};

use crate::{
    app::AppContext,
    event::{AppEvent, Sender, UserEvent, UserEventWithCount},
    github::{self, CheckboxItem, GhIssue, GhItemKind, GhPullRequest},
    view::View,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StateFilter {
    Open,
    Closed,
    All,
}

impl StateFilter {
    fn next(self) -> Self {
        match self {
            StateFilter::Open => StateFilter::Closed,
            StateFilter::Closed => StateFilter::All,
            StateFilter::All => StateFilter::Open,
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            StateFilter::Open => "open",
            StateFilter::Closed => "closed",
            StateFilter::All => "all",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitHubTab {
    Issues,
    PullRequests,
}

#[derive(Debug)]
struct TaskListPanel {
    number: u64,
    kind: GhItemKind,
    items: Vec<CheckboxItem>,
    selected: usize,
}

#[derive(Debug, Default)]
enum LoadState {
    #[default]
    Idle,
    Loading,
    Error(String),
}

#[derive(Debug)]
pub struct GitHubView<'a> {
    before: View<'a>,

    active_tab: GitHubTab,
    issues: Vec<GhIssue>,
    pull_requests: Vec<GhPullRequest>,
    selected_index: usize,
    offset: usize,
    height: usize,

    detail: Option<Vec<Line<'static>>>,
    detail_number: Option<(u64, GhItemKind)>,
    detail_offset: usize,

    state_filter: StateFilter,

    task_panel: Option<TaskListPanel>,

    load_state: LoadState,

    flash_message: Option<(String, bool)>,

    ctx: Rc<AppContext>,
    tx: Sender,
    clear: bool,
}

impl<'a> GitHubView<'a> {
    pub fn new(
        before: View<'a>,
        issues: Vec<GhIssue>,
        pull_requests: Vec<GhPullRequest>,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> GitHubView<'a> {
        let load_state = if issues.is_empty() && pull_requests.is_empty() {
            LoadState::Loading
        } else {
            LoadState::Idle
        };
        GitHubView {
            before,
            active_tab: GitHubTab::Issues,
            issues,
            pull_requests,
            selected_index: 0,
            offset: 0,
            height: 0,
            detail: None,
            detail_number: None,
            detail_offset: 0,
            state_filter: StateFilter::Open,
            task_panel: None,
            load_state,
            flash_message: None,
            ctx,
            tx,
            clear: false,
        }
    }

    pub fn take_before_view(&mut self) -> View<'a> {
        std::mem::take(&mut self.before)
    }

    pub fn clear(&mut self) {
        self.clear = true;
    }

    pub fn set_flash(&mut self, msg: String, is_error: bool) {
        self.flash_message = Some((msg, is_error));
    }

    pub fn set_error(&mut self, msg: String) {
        if matches!(self.load_state, LoadState::Loading) {
            self.load_state = LoadState::Error(msg);
        }
    }

    pub fn update_data(&mut self, issues: Vec<GhIssue>, pull_requests: Vec<GhPullRequest>) {
        self.load_state = LoadState::Idle;
        self.issues = issues;
        self.pull_requests = pull_requests;
        // 修正選取索引避免越界
        let max = self.current_list_len().saturating_sub(1);
        if self.selected_index > max {
            self.selected_index = max;
        }
        // 如果在看詳情，清除掉（內容可能已過時）
        self.detail = None;
        self.detail_number = None;
        self.detail_offset = 0;
    }

    pub fn set_detail(&mut self, number: u64, kind: GhItemKind, lines: Vec<Line<'static>>) {
        self.detail = Some(lines);
        self.detail_number = Some((number, kind));
        self.detail_offset = 0;
    }

    pub fn invalidate_detail_cache(&mut self, number: u64, kind: GhItemKind) {
        if self.detail_number == Some((number, kind)) {
            self.detail = None;
            self.detail_number = None;
            self.detail_offset = 0;
        }
    }

    pub fn update_task_panel(&mut self, number: u64, kind: GhItemKind, new_body: &str) {
        if let Some(ref mut panel) = self.task_panel {
            if panel.number == number && panel.kind == kind {
                let items = github::parse_checkboxes(new_body);
                let selected = panel.selected.min(items.len().saturating_sub(1));
                panel.items = items;
                panel.selected = selected;
            }
        }
    }

    pub fn set_task_panel(&mut self, number: u64, kind: GhItemKind, items: Vec<CheckboxItem>) {
        if items.is_empty() {
            self.set_flash("No tasks found".to_string(), false);
            return;
        }
        self.task_panel = Some(TaskListPanel {
            number,
            kind,
            items,
            selected: 0,
        });
    }

    pub fn status_hints(&self) -> Vec<(UserEvent, &'static str)> {
        if self.task_panel.is_some() {
            return vec![(UserEvent::Confirm, "toggle"), (UserEvent::Cancel, "close")];
        }
        if self.detail.is_some() {
            return vec![
                (UserEvent::TaskListToggle, "tasks"),
                (UserEvent::Cancel, "back"),
            ];
        }
        if self.current_list_len() == 0 {
            return match &self.load_state {
                LoadState::Loading => vec![(UserEvent::Cancel, "close")],
                LoadState::Error(_) => {
                    vec![(UserEvent::Refresh, "retry"), (UserEvent::Cancel, "close")]
                }
                LoadState::Idle => vec![
                    (UserEvent::Refresh, "refresh"),
                    (UserEvent::Cancel, "close"),
                ],
            };
        }
        vec![
            (UserEvent::RefList, "switch tab"),
            (UserEvent::Confirm, "detail"),
            (UserEvent::Refresh, "refresh"),
            (UserEvent::Filter, "filter"),
            (UserEvent::GitHubToggle, "close"),
        ]
    }

    pub fn handle_event(&mut self, event_with_count: UserEventWithCount, _: KeyEvent) {
        let event = event_with_count.event;
        let count = event_with_count.count;

        self.flash_message = None;

        // Task list panel 事件（最高優先級）
        if let Some(ref mut panel) = self.task_panel {
            match event {
                UserEvent::Cancel | UserEvent::Close => {
                    self.task_panel = None;
                    return;
                }
                UserEvent::NavigateDown | UserEvent::SelectDown => {
                    let max = panel.items.len().saturating_sub(1);
                    for _ in 0..count {
                        if panel.selected < max {
                            panel.selected += 1;
                        }
                    }
                    return;
                }
                UserEvent::NavigateUp | UserEvent::SelectUp => {
                    for _ in 0..count {
                        panel.selected = panel.selected.saturating_sub(1);
                    }
                    return;
                }
                UserEvent::GoToTop => {
                    panel.selected = 0;
                    return;
                }
                UserEvent::GoToBottom => {
                    panel.selected = panel.items.len().saturating_sub(1);
                    return;
                }
                UserEvent::Confirm => {
                    if let Some(item) = panel.items.get(panel.selected) {
                        self.tx.send(AppEvent::ToggleCheckbox {
                            number: panel.number,
                            kind: panel.kind,
                            checkbox_index: item.index,
                        });
                    }
                    return;
                }
                _ => return,
            }
        }

        // 詳情模式事件
        if self.detail.is_some() {
            match event {
                UserEvent::Cancel | UserEvent::Close => {
                    self.detail = None;
                    self.detail_number = None;
                    self.detail_offset = 0;
                }
                UserEvent::NavigateDown | UserEvent::SelectDown => {
                    for _ in 0..count {
                        self.detail_offset = self.detail_offset.saturating_add(1);
                    }
                }
                UserEvent::NavigateUp | UserEvent::SelectUp => {
                    for _ in 0..count {
                        self.detail_offset = self.detail_offset.saturating_sub(1);
                    }
                }
                UserEvent::PageDown => {
                    let page = self.height.saturating_sub(2).max(1);
                    self.detail_offset = self.detail_offset.saturating_add(page);
                }
                UserEvent::PageUp => {
                    let page = self.height.saturating_sub(2).max(1);
                    self.detail_offset = self.detail_offset.saturating_sub(page);
                }
                UserEvent::HalfPageDown => {
                    let half = self.height.saturating_sub(2).max(1) / 2;
                    self.detail_offset = self.detail_offset.saturating_add(half);
                }
                UserEvent::HalfPageUp => {
                    let half = self.height.saturating_sub(2).max(1) / 2;
                    self.detail_offset = self.detail_offset.saturating_sub(half);
                }
                UserEvent::GoToTop => {
                    self.detail_offset = 0;
                }
                UserEvent::TaskListToggle => {
                    self.request_task_panel();
                }
                _ => {}
            }
            return;
        }

        // 列表模式事件
        match event {
            UserEvent::Quit => {
                self.tx.send(AppEvent::Quit);
            }
            UserEvent::GitHubToggle | UserEvent::Cancel | UserEvent::Close => {
                self.tx.send(AppEvent::ClearGitHub);
                self.tx.send(AppEvent::CloseGitHub);
            }
            UserEvent::RefList => {
                // Tab 鍵 — 切換頁籤
                self.active_tab = match self.active_tab {
                    GitHubTab::Issues => GitHubTab::PullRequests,
                    GitHubTab::PullRequests => GitHubTab::Issues,
                };
                self.selected_index = 0;
                self.offset = 0;
            }
            UserEvent::NavigateDown | UserEvent::SelectDown => {
                let max = self.current_list_len().saturating_sub(1);
                for _ in 0..count {
                    if self.selected_index < max {
                        self.selected_index += 1;
                    }
                }
                self.adjust_scroll();
            }
            UserEvent::NavigateUp | UserEvent::SelectUp => {
                for _ in 0..count {
                    self.selected_index = self.selected_index.saturating_sub(1);
                }
                self.adjust_scroll();
            }
            UserEvent::GoToTop => {
                self.selected_index = 0;
                self.offset = 0;
            }
            UserEvent::GoToBottom => {
                self.selected_index = self.current_list_len().saturating_sub(1);
                self.adjust_scroll();
            }
            UserEvent::Confirm => {
                self.load_detail();
            }
            UserEvent::PageDown => {
                let page = self.height.saturating_sub(3).max(1);
                let max = self.current_list_len().saturating_sub(1);
                self.selected_index = (self.selected_index + page).min(max);
                self.adjust_scroll();
            }
            UserEvent::PageUp => {
                let page = self.height.saturating_sub(3).max(1);
                self.selected_index = self.selected_index.saturating_sub(page);
                self.adjust_scroll();
            }
            UserEvent::HalfPageDown => {
                let half = self.height.saturating_sub(3).max(1) / 2;
                let max = self.current_list_len().saturating_sub(1);
                self.selected_index = (self.selected_index + half).min(max);
                self.adjust_scroll();
            }
            UserEvent::HalfPageUp => {
                let half = self.height.saturating_sub(3).max(1) / 2;
                self.selected_index = self.selected_index.saturating_sub(half);
                self.adjust_scroll();
            }
            UserEvent::Filter => {
                self.state_filter = self.state_filter.next();
                self.selected_index = 0;
                self.offset = 0;
                self.load_state = LoadState::Loading;
                self.tx.send(AppEvent::RefreshGitHub {
                    state: self.state_filter.as_str().to_string(),
                });
            }
            UserEvent::Refresh => {
                self.load_state = LoadState::Loading;
                self.tx.send(AppEvent::RefreshGitHub {
                    state: self.state_filter.as_str().to_string(),
                });
            }
            _ => {}
        }
    }

    fn selected_number_and_kind(&self) -> Option<(u64, GhItemKind)> {
        match self.active_tab {
            GitHubTab::Issues => self
                .issues
                .get(self.selected_index)
                .map(|i| (i.number, GhItemKind::Issue)),
            GitHubTab::PullRequests => self
                .pull_requests
                .get(self.selected_index)
                .map(|p| (p.number, GhItemKind::PullRequest)),
        }
    }

    fn request_task_panel(&self) {
        if let Some((number, kind)) = self.selected_number_and_kind() {
            self.tx.send(AppEvent::LoadTaskPanel { number, kind });
        }
    }

    fn current_list_len(&self) -> usize {
        match self.active_tab {
            GitHubTab::Issues => self.issues.len(),
            GitHubTab::PullRequests => self.pull_requests.len(),
        }
    }

    fn adjust_scroll(&mut self) {
        if self.height == 0 {
            return;
        }
        let visible = self.height.saturating_sub(3);
        if self.selected_index < self.offset {
            self.offset = self.selected_index;
        } else if self.selected_index >= self.offset + visible {
            self.offset = self.selected_index - visible + 1;
        }
    }

    fn load_detail(&self) {
        if let Some((number, kind)) = self.selected_number_and_kind() {
            self.tx.send(AppEvent::LoadDetail { number, kind });
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        if self.clear {
            f.render_widget(Clear, area);
            return;
        }

        self.height = area.height as usize;

        if let Some(ref detail) = self.detail {
            self.render_detail(f, area, detail.clone());
            if self.task_panel.is_some() {
                self.render_task_panel(f, area);
            }
            return;
        }

        // ── 頁籤列 ──
        let filter_label = self.state_filter.as_str();
        let issues_label = format!(" Issues ({}) [{}] ", self.issues.len(), filter_label);
        let prs_label = format!(" PRs ({}) [{}] ", self.pull_requests.len(), filter_label);

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
        ]);

        let [tab_area, list_area] =
            Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(area);

        f.render_widget(
            Paragraph::new(tab_line).block(Block::default().padding(Padding::new(2, 2, 1, 0))),
            tab_area,
        );

        // ── Loading / 錯誤提示 ──
        if self.current_list_len() == 0 {
            let (text, color) = match &self.load_state {
                LoadState::Loading => ("Loading GitHub data...".to_string(), Color::DarkGray),
                LoadState::Error(err) => (err.clone(), Color::Red),
                LoadState::Idle => ("No items".to_string(), Color::DarkGray),
            };
            render_centered_message(f, list_area, text, color);
            self.clear_image_area(area);
            return;
        }

        // ── 列表 ──
        let visible_height = list_area.height as usize;
        let items: Vec<Line> = match self.active_tab {
            GitHubTab::Issues => self
                .issues
                .iter()
                .enumerate()
                .skip(self.offset)
                .take(visible_height)
                .map(|(i, issue)| render_issue_line(issue, i == self.selected_index))
                .collect(),
            GitHubTab::PullRequests => self
                .pull_requests
                .iter()
                .enumerate()
                .skip(self.offset)
                .take(visible_height)
                .map(|(i, pr)| render_pr_line(pr, i == self.selected_index))
                .collect(),
        };

        let list_paragraph =
            Paragraph::new(items).block(Block::default().padding(Padding::horizontal(2)));
        f.render_widget(list_paragraph, list_area);

        // ── Flash message ──
        if let Some((ref msg, is_error)) = self.flash_message {
            let color = if is_error {
                Color::Red
            } else {
                Color::DarkGray
            };
            let flash_area = Rect::new(
                list_area.x,
                list_area.bottom().saturating_sub(1),
                list_area.width,
                1,
            );
            let flash = Paragraph::new(Line::from(Span::styled(
                msg.clone(),
                Style::default().fg(color),
            )))
            .alignment(ratatui::layout::Alignment::Center);
            f.render_widget(flash, flash_area);
        }

        self.clear_image_area(area);
    }

    fn clear_image_area(&self, area: Rect) {
        for y in area.top()..area.bottom() {
            self.ctx.image_protocol.clear_line(y);
        }
    }

    fn render_detail(&self, f: &mut Frame, area: Rect, lines: Vec<Line<'static>>) {
        let visible: Vec<Line> = lines
            .into_iter()
            .skip(self.detail_offset)
            .take(area.height as usize)
            .collect();

        let paragraph =
            Paragraph::new(visible).block(Block::default().padding(Padding::new(3, 3, 1, 0)));
        f.render_widget(paragraph, area);
        self.clear_image_area(area);
    }

    fn render_task_panel(&self, f: &mut Frame, area: Rect) {
        let Some(ref panel) = self.task_panel else {
            return;
        };

        // 計算 overlay 尺寸
        let max_label_width = panel
            .items
            .iter()
            .map(|item| item.label.len() + 5) // "  ☑ " prefix
            .max()
            .unwrap_or(20);
        let dialog_width = (max_label_width as u16 + 4)
            .max(28)
            .min(area.width.saturating_sub(4));
        // items + title(1) + borders(2) + footer(1)
        let dialog_height = (panel.items.len() as u16 + 4).min(area.height.saturating_sub(2));

        let x = area.x + (area.width.saturating_sub(dialog_width)) / 2;
        let y = area.y + (area.height.saturating_sub(dialog_height)) / 2;
        let dialog_area = Rect::new(x, y, dialog_width, dialog_height);

        f.render_widget(Clear, dialog_area);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Tasks ");

        let inner = block.inner(dialog_area);
        f.render_widget(block, dialog_area);

        // 可用高度（扣除 footer 行）
        let content_height = inner.height.saturating_sub(1) as usize;

        // 計算滾動 offset
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
        let footer = Line::from(Span::styled(
            " Enter:toggle  Esc:close",
            Style::default().fg(Color::DarkGray),
        ));
        lines.push(footer);

        let paragraph =
            Paragraph::new(lines).block(Block::default().padding(Padding::horizontal(1)));
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

fn label_spans(labels: &[crate::github::GhLabel]) -> Vec<Span<'static>> {
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

fn render_issue_line(issue: &GhIssue, selected: bool) -> Line<'static> {
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
            format!("{:<8} ", issue.state.to_lowercase()),
            Style::default().fg(state_color),
        ),
        Span::styled(issue.title.clone(), style),
    ];
    spans.extend(label_spans(&issue.labels));
    spans.push(Span::styled(
        format!("  @{}", issue.author.login),
        Style::default().fg(Color::DarkGray),
    ));

    Line::from(spans)
}

fn render_pr_line(pr: &GhPullRequest, selected: bool) -> Line<'static> {
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
            format!("{:<8} ", state_label),
            Style::default().fg(state_color),
        ),
        Span::styled(pr.title.clone(), style),
    ];
    spans.extend(label_spans(&pr.labels));
    spans.push(Span::styled(
        format!("  ← {}", pr.head_ref_name),
        Style::default().fg(Color::Blue),
    ));
    spans.push(Span::styled(
        format!("  @{}", pr.author.login),
        Style::default().fg(Color::DarkGray),
    ));

    Line::from(spans)
}

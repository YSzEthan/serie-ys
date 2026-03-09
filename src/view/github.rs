use std::path::PathBuf;
use std::rc::Rc;

use ansi_to_tui::IntoText as _;
use ratatui::{
    crossterm::event::KeyEvent,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Padding, Paragraph},
    Frame,
};
use rustc_hash::FxHashMap;

use crate::{
    app::AppContext,
    event::{AppEvent, Sender, UserEvent, UserEventWithCount},
    github::{self, GhIssue, GhPullRequest},
    view::View,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitHubTab {
    Issues,
    PullRequests,
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
    detail_offset: usize,

    issue_detail_cache: FxHashMap<u64, Vec<Line<'static>>>,
    pr_detail_cache: FxHashMap<u64, Vec<Line<'static>>>,

    ctx: Rc<AppContext>,
    tx: Sender,
    clear: bool,
    repo_path: PathBuf,
}

impl<'a> GitHubView<'a> {
    pub fn new(
        before: View<'a>,
        issues: Vec<GhIssue>,
        pull_requests: Vec<GhPullRequest>,
        issue_detail_cache: FxHashMap<u64, Vec<Line<'static>>>,
        pr_detail_cache: FxHashMap<u64, Vec<Line<'static>>>,
        ctx: Rc<AppContext>,
        tx: Sender,
        repo_path: PathBuf,
    ) -> GitHubView<'a> {
        GitHubView {
            before,
            active_tab: GitHubTab::Issues,
            issues,
            pull_requests,
            selected_index: 0,
            offset: 0,
            height: 0,
            detail: None,
            detail_offset: 0,
            issue_detail_cache,
            pr_detail_cache,
            ctx,
            tx,
            clear: false,
            repo_path,
        }
    }

    pub fn take_before_view(&mut self) -> View<'a> {
        std::mem::take(&mut self.before)
    }

    pub fn clear(&mut self) {
        self.clear = true;
    }

    pub fn update_data(&mut self, issues: Vec<GhIssue>, pull_requests: Vec<GhPullRequest>) {
        self.issues = issues;
        self.pull_requests = pull_requests;
        // 修正選取索引避免越界
        let max = self.current_list_len().saturating_sub(1);
        if self.selected_index > max {
            self.selected_index = max;
        }
        // 如果在看詳情，清除掉（內容可能已過時）
        self.detail = None;
        self.detail_offset = 0;
    }

    pub fn update_detail_cache(
        &mut self,
        issue_details: FxHashMap<u64, Vec<Line<'static>>>,
        pr_details: FxHashMap<u64, Vec<Line<'static>>>,
    ) {
        self.issue_detail_cache.extend(issue_details);
        self.pr_detail_cache.extend(pr_details);
    }

    pub fn handle_event(&mut self, event_with_count: UserEventWithCount, _: KeyEvent) {
        let event = event_with_count.event;
        let count = event_with_count.count;

        // 詳情模式事件
        if self.detail.is_some() {
            match event {
                UserEvent::Cancel | UserEvent::Close => {
                    self.detail = None;
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
            UserEvent::Refresh => {
                self.tx.send(AppEvent::RefreshGitHub);
            }
            _ => {}
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

    fn load_detail(&mut self) {
        let number = match self.active_tab {
            GitHubTab::Issues => self.issues.get(self.selected_index).map(|i| i.number),
            GitHubTab::PullRequests => self
                .pull_requests
                .get(self.selected_index)
                .map(|p| p.number),
        };
        let Some(number) = number else { return };

        // 先查 cache
        let cached = match self.active_tab {
            GitHubTab::Issues => self.issue_detail_cache.get(&number).cloned(),
            GitHubTab::PullRequests => self.pr_detail_cache.get(&number).cloned(),
        };

        if let Some(lines) = cached {
            self.detail = Some(lines);
            self.detail_offset = 0;
            return;
        }

        // Fallback：即時抓取
        let result = match self.active_tab {
            GitHubTab::Issues => github::view_issue_rendered(&self.repo_path, number),
            GitHubTab::PullRequests => github::view_pr_rendered(&self.repo_path, number),
        };

        match result {
            Ok(rendered) => {
                let lines: Vec<Line<'static>> = rendered
                    .into_text()
                    .map(|t| t.into_iter().collect())
                    .unwrap_or_else(|_| vec![Line::raw("Failed to parse ANSI output")]);
                // 存入 cache
                match self.active_tab {
                    GitHubTab::Issues => {
                        self.issue_detail_cache.insert(number, lines.clone());
                    }
                    GitHubTab::PullRequests => {
                        self.pr_detail_cache.insert(number, lines.clone());
                    }
                }
                self.detail = Some(lines);
                self.detail_offset = 0;
            }
            Err(e) => {
                self.tx
                    .send(AppEvent::NotifyError(format!("Failed to load detail: {e}")));
            }
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
            return;
        }

        // ── 頁籤列 ──
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
        ]);

        let [tab_area, list_area] =
            Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(area);

        f.render_widget(
            Paragraph::new(tab_line).block(Block::default().padding(Padding::new(2, 2, 1, 0))),
            tab_area,
        );

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

        // 清除圖片協定行（避免殘影）
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

        // 清除圖片協定行
        for y in area.top()..area.bottom() {
            self.ctx.image_protocol.clear_line(y);
        }
    }
}

// ── 渲染輔助函數 ──

fn render_issue_line(issue: &GhIssue, selected: bool) -> Line<'static> {
    let indicator = if selected { "▸ " } else { "  " };
    let state_color = match issue.state.as_str() {
        "OPEN" => Color::Green,
        "CLOSED" => Color::Red,
        _ => Color::Gray,
    };
    let labels: String = issue
        .labels
        .iter()
        .map(|l| l.name.clone())
        .collect::<Vec<_>>()
        .join(", ");
    let style = if selected {
        Style::default().fg(Color::Cyan).bold()
    } else {
        Style::default()
    };

    Line::from(vec![
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
        if !labels.is_empty() {
            Span::styled(format!(" [{labels}]"), Style::default().fg(Color::Yellow))
        } else {
            Span::raw("")
        },
        Span::styled(
            format!("  @{}", issue.author.login),
            Style::default().fg(Color::DarkGray),
        ),
    ])
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

    Line::from(vec![
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
        Span::styled(
            format!("  ← {}", pr.head_ref_name),
            Style::default().fg(Color::Blue),
        ),
        Span::styled(
            format!("  @{}", pr.author.login),
            Style::default().fg(Color::DarkGray),
        ),
    ])
}

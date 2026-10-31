use std::time::Instant;

use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use rust_i18n::t;
use tui_input::backend::crossterm::EventHandler;

use crate::{
    event::{AppEvent, RelatedGroup, RelatedItem, UserEvent, UserEventWithCount},
    fuzzy::SearchMatcher,
    github::{self, PrDraftAction, StateAction},
};

use super::{render::state_label, GitHubFocus, GitHubTab, GitHubView, LoadState, TaskListPanel};

impl<'a> GitHubView<'a> {
    pub fn handle_event(&mut self, event_with_count: UserEventWithCount, key: KeyEvent) {
        let count = event_with_count.count;
        // 在偏 modal 的 focus（List/Preview）中，右/左鍵兼作 Confirm/Cancel。
        // Prompt 接收原始按鍵輸入；CheckboxEdit 用左/右鍵切換。
        let event = match self.focus {
            // Branches、CI 是多欄卡片，左右鍵用來在卡片間橫向移動，不當 Confirm/Cancel
            GitHubFocus::List if self.active_tab.is_card_tab() => event_with_count.event,
            GitHubFocus::List | GitHubFocus::Preview => modal_yesno_aliases(event_with_count.event),
            GitHubFocus::Prompt | GitHubFocus::CheckboxEdit => event_with_count.event,
        };

        self.flash_message = None;

        let before = self.selected_number_and_kind();
        match self.focus {
            GitHubFocus::CheckboxEdit => self.handle_checkbox_edit_event(event, count),
            GitHubFocus::Preview => self.handle_preview_event(event, count),
            GitHubFocus::Prompt => self.handle_prompt_event(event, count, key),
            GitHubFocus::List => match digit_tab(event, key) {
                Some(tab) => self.select_tab(tab),
                None => self.handle_list_event(event, count),
            },
        }
        if self.selected_number_and_kind() != before {
            self.schedule_timeline_for_selected(Instant::now());
        }
    }

    fn handle_checkbox_edit_event(&mut self, event: UserEvent, count: usize) {
        let Some(ref mut panel) = self.task_panel else {
            self.focus = GitHubFocus::Preview;
            return;
        };
        match event {
            UserEvent::Cancel => {
                self.task_panel = None;
                self.focus = GitHubFocus::Preview;
            }
            UserEvent::NavigateDown | UserEvent::SelectDown => {
                let max = panel.items.len().saturating_sub(1);
                for _ in 0..count {
                    if panel.selected < max {
                        panel.selected += 1;
                    }
                }
            }
            UserEvent::NavigateUp | UserEvent::SelectUp => {
                for _ in 0..count {
                    panel.selected = panel.selected.saturating_sub(1);
                }
            }
            UserEvent::NavigateLeft | UserEvent::NavigateRight => {
                if let Some(item) = panel.items.get_mut(panel.selected) {
                    item.checked = !item.checked;
                }
            }
            UserEvent::Confirm => {
                let changed: Vec<usize> = panel
                    .items
                    .iter()
                    .enumerate()
                    .filter(|(i, item)| item.checked != panel.original_checked[*i])
                    .map(|(_, item)| item.index)
                    .collect();
                if !changed.is_empty() {
                    self.tx.send(AppEvent::BatchToggleCheckboxes {
                        number: panel.number,
                        kind: panel.kind,
                        checkbox_indices: changed,
                    });
                }
                self.task_panel = None;
                self.focus = GitHubFocus::Preview;
            }
            _ => {}
        }
    }

    pub(super) fn handle_preview_event(&mut self, event: UserEvent, count: usize) {
        match event {
            UserEvent::Cancel | UserEvent::Close => {
                self.focus = GitHubFocus::List;
            }
            UserEvent::NavigateDown | UserEvent::SelectDown => {
                for _ in 0..count {
                    self.preview_offset = self.preview_offset.saturating_add(1);
                }
                self.maybe_load_more_timeline();
            }
            UserEvent::NavigateUp | UserEvent::SelectUp => {
                for _ in 0..count {
                    self.preview_offset = self.preview_offset.saturating_sub(1);
                }
            }
            UserEvent::PageDown => {
                let page = self.preview_height.max(1);
                self.preview_offset = self.preview_offset.saturating_add(page);
                self.maybe_load_more_timeline();
            }
            UserEvent::PageUp => {
                let page = self.preview_height.max(1);
                self.preview_offset = self.preview_offset.saturating_sub(page);
            }
            UserEvent::HalfPageDown => {
                let half = self.preview_height.max(1) / 2;
                self.preview_offset = self.preview_offset.saturating_add(half);
                self.maybe_load_more_timeline();
            }
            UserEvent::HalfPageUp => {
                let half = self.preview_height.max(1) / 2;
                self.preview_offset = self.preview_offset.saturating_sub(half);
            }
            UserEvent::GoToTop => {
                self.preview_offset = 0;
            }
            UserEvent::Confirm => {
                self.try_enter_checkbox_edit();
            }
            UserEvent::DetailPaneToggle => {
                self.open_related_picker();
            }
            UserEvent::ToggleIssueState => {
                self.trigger_toggle_state();
            }
            UserEvent::MergePr if matches!(self.active_tab, GitHubTab::PullRequests) => {
                self.try_merge_selected_pr();
            }
            UserEvent::TogglePrDraft if matches!(self.active_tab, GitHubTab::PullRequests) => {
                self.try_toggle_pr_draft();
            }
            UserEvent::ToggleCommitLog if matches!(self.active_tab, GitHubTab::PullRequests) => {
                self.toggle_commit_log();
            }
            UserEvent::ToggleLabelMode => {
                self.toggle_label_mode();
            }
            UserEvent::Refresh => {
                self.dispatch_refresh();
            }
            UserEvent::Fetch => {
                self.tx.send(AppEvent::FetchAll);
            }
            _ => {}
        }
    }

    /// 跳到指定分頁；已經在該分頁就不動，免得把游標重設到第 0 列。
    fn select_tab(&mut self, tab: GitHubTab) {
        if tab == self.active_tab {
            return;
        }
        self.switch_tab(tab);
        self.bump_generation();
    }

    fn switch_tab(&mut self, tab: GitHubTab) {
        self.active_tab = tab;
        self.selected_index = 0;
        self.offset = 0;
        self.preview_offset = 0;
        self.on_enter_tab();
    }

    pub(super) fn handle_list_event(&mut self, event: UserEvent, count: usize) {
        let handled = match self.active_tab {
            GitHubTab::Branches => self.handle_branches_event(event, count),
            GitHubTab::Ci => self.handle_ci_event(event, count),
            GitHubTab::Issues | GitHubTab::PullRequests => false,
        };
        if handled {
            return;
        }
        match event {
            UserEvent::GitHubToggle | UserEvent::Cancel | UserEvent::Close => {
                self.tx.send(AppEvent::CloseGitHub);
            }
            UserEvent::RefList => self.select_tab(self.active_tab.next()),
            UserEvent::RefListPrev => self.select_tab(self.active_tab.prev()),
            UserEvent::NavigateDown | UserEvent::SelectDown => {
                let max = self.current_list_len().saturating_sub(1);
                let step = self.nav_step();
                for _ in 0..count {
                    self.selected_index = (self.selected_index + step).min(max);
                }
                self.preview_offset = 0;
                self.adjust_scroll();
                self.maybe_load_more();
            }
            UserEvent::NavigateUp | UserEvent::SelectUp => {
                let step = self.nav_step();
                for _ in 0..count {
                    self.selected_index = self.selected_index.saturating_sub(step);
                }
                self.preview_offset = 0;
                self.adjust_scroll();
            }
            UserEvent::GoToTop => {
                self.selected_index = 0;
                self.offset = 0;
                self.preview_offset = 0;
            }
            UserEvent::GoToBottom => {
                self.selected_index = self.current_list_len().saturating_sub(1);
                self.preview_offset = 0;
                self.adjust_scroll();
                self.maybe_load_more();
            }
            UserEvent::Confirm if self.current_list_len() > 0 => {
                self.focus = GitHubFocus::Preview;
            }
            UserEvent::PageDown => {
                let page = self.list_rows().max(1);
                let max = self.current_list_len().saturating_sub(1);
                self.selected_index = (self.selected_index + page).min(max);
                self.preview_offset = 0;
                self.adjust_scroll();
                self.maybe_load_more();
            }
            UserEvent::PageUp => {
                let page = self.list_rows().max(1);
                self.selected_index = self.selected_index.saturating_sub(page);
                self.preview_offset = 0;
                self.adjust_scroll();
            }
            UserEvent::HalfPageDown => {
                let half = self.list_rows().max(1) / 2;
                let max = self.current_list_len().saturating_sub(1);
                self.selected_index = (self.selected_index + half).min(max);
                self.preview_offset = 0;
                self.adjust_scroll();
                self.maybe_load_more();
            }
            UserEvent::HalfPageUp => {
                let half = self.list_rows().max(1) / 2;
                self.selected_index = self.selected_index.saturating_sub(half);
                self.preview_offset = 0;
                self.adjust_scroll();
            }
            UserEvent::Search => {
                self.focus = GitHubFocus::Prompt;
            }
            UserEvent::Filter => {
                self.state_filter = self.state_filter.next();
                self.selected_index = 0;
                self.offset = 0;
                self.preview_offset = 0;
                self.bump_generation();
                self.dispatch_refresh();
            }
            UserEvent::Refresh => {
                self.dispatch_refresh();
            }
            UserEvent::Fetch => {
                self.tx.send(AppEvent::FetchAll);
            }
            UserEvent::ShortCopy => {
                let name = match self.active_tab.kind() {
                    Some(github::GhItemKind::Issue) => t!("github.copy.url_issue"),
                    Some(github::GhItemKind::PullRequest) => t!("github.copy.url_pr"),
                    None if self.active_tab == GitHubTab::Ci => t!("github.copy.url_pr"),
                    None => t!("github.copy.url_branch"),
                };
                self.with_selected_url(|url| AppEvent::CopyToClipboard {
                    name: name.into_owned(),
                    value: url,
                });
            }
            UserEvent::FullCopy => {
                self.with_selected_url(AppEvent::OpenUrl);
            }
            UserEvent::TagCopy => {
                if let Some((number, kind)) = self.selected_number_and_kind() {
                    let name = match kind {
                        github::GhItemKind::Issue => t!("github.copy.number_issue"),
                        github::GhItemKind::PullRequest => t!("github.copy.number_pr"),
                    };
                    self.tx.send(AppEvent::CopyToClipboard {
                        name: name.into_owned(),
                        value: format!("#{number}"),
                    });
                }
            }
            UserEvent::DetailPaneToggle => {
                self.open_related_picker();
            }
            UserEvent::MergePr if matches!(self.active_tab, GitHubTab::PullRequests) => {
                self.try_merge_selected_pr();
            }
            UserEvent::ToggleIssueState => {
                self.trigger_toggle_state();
            }
            UserEvent::TogglePrDraft if matches!(self.active_tab, GitHubTab::PullRequests) => {
                self.try_toggle_pr_draft();
            }
            UserEvent::ToggleCommitLog if matches!(self.active_tab, GitHubTab::PullRequests) => {
                self.toggle_commit_log();
            }
            UserEvent::ToggleLabelMode => {
                self.toggle_label_mode();
            }
            _ => {}
        }
    }

    /// 重抓整份 GitHub 資料；資料沒變時 `update_data` 的 early-return
    /// 分支會順帶重抓選取項目的 timeline。
    fn dispatch_refresh(&mut self) {
        self.load_state = LoadState::Loading;
        self.tx.send(AppEvent::RefreshGitHub {
            state: self.state_filter,
        });
    }

    /// 卡片分頁左右鍵在卡片間橫向移動。
    fn move_card_horizontally(&mut self, event: UserEvent, count: usize) {
        let max = self.current_list_len().saturating_sub(1);
        self.selected_index = if event == UserEvent::NavigateLeft {
            self.selected_index.saturating_sub(count)
        } else {
            (self.selected_index + count).min(max)
        };
        self.adjust_scroll();
    }

    /// CI 分頁會誤用 issue／PR 行為的事件在這裡攔下，回傳值語意同 `handle_branches_event`。
    /// 開瀏覽器（`C`）走共用流程的 `selected_url`（PR 頁）；Enter 開的是 `<PR>/checks`。
    fn handle_ci_event(&mut self, event: UserEvent, count: usize) -> bool {
        match event {
            UserEvent::Refresh => self.refresh_ci(),
            UserEvent::CiRerun => self.try_ci_rerun(),
            UserEvent::NavigateLeft | UserEvent::NavigateRight => {
                self.move_card_horizontally(event, count);
            }
            UserEvent::Confirm => {
                if let Some(entry) = self.selected_ci() {
                    self.tx
                        .send(AppEvent::OpenUrl(format!("{}/checks", entry.url)));
                }
            }
            UserEvent::TagCopy => {
                if let Some(entry) = self.selected_ci() {
                    self.tx.send(AppEvent::CopyToClipboard {
                        name: t!("github.copy.number_pr").into_owned(),
                        value: format!("#{}", entry.number),
                    });
                }
            }
            // 沒有 state filter、preview、related picker
            UserEvent::Filter | UserEvent::DetailPaneToggle => {}
            _ => return false,
        }
        true
    }

    /// Branches 分頁會誤用 issue／PR 行為的事件在這裡攔下。回傳 true 表示已處理；
    /// 移動、搜尋、關閉、複製／開啟 URL（走 `selected_url`）等共用行為回 false，
    /// 交給 `handle_list_event` 原本的流程。
    fn handle_branches_event(&mut self, event: UserEvent, count: usize) -> bool {
        match event {
            UserEvent::Refresh => self.reload_branches(),
            UserEvent::Filter => self.cycle_branch_author(),
            UserEvent::NavigateLeft | UserEvent::NavigateRight => {
                self.move_card_horizontally(event, count);
            }
            UserEvent::TagCopy => {
                let idx = self.actual_index(self.selected_index);
                if let Some(branch) = self.branches.get(idx) {
                    self.tx.send(AppEvent::CopyToClipboard {
                        name: t!("github.copy.branch_name").into_owned(),
                        value: branch.name.clone(),
                    });
                }
            }
            // 沒有 preview、related picker
            UserEvent::Confirm | UserEvent::DetailPaneToggle => {}
            _ => return false,
        }
        true
    }

    fn try_merge_selected_pr(&mut self) {
        let idx = self.actual_index(self.selected_index);
        let Some(pr) = self.pull_requests.get(idx) else {
            return;
        };
        if pr.is_draft {
            self.set_flash(
                t!("github.flash.pr_is_draft", number = pr.number).into_owned(),
                true,
            );
            return;
        }
        if pr.state != "OPEN" {
            self.set_flash(
                t!(
                    "github.flash.pr_state",
                    number = pr.number,
                    state = state_label(&pr.state)
                )
                .into_owned(),
                true,
            );
            return;
        }
        self.tx.send(AppEvent::OpenMergePrMethodPicker {
            number: pr.number,
            head_ref: pr.head_ref_name.clone(),
            head_ref_oid: pr.head_ref_oid.clone(),
            state: self.state_filter,
            remote_deletable: pr.head_branch_deletable,
        });
    }

    fn try_toggle_pr_draft(&mut self) {
        let idx = self.actual_index(self.selected_index);
        let Some(pr) = self.pull_requests.get(idx) else {
            return;
        };
        if pr.state != "OPEN" {
            self.set_flash(
                t!(
                    "github.flash.pr_state",
                    number = pr.number,
                    state = state_label(&pr.state)
                )
                .into_owned(),
                true,
            );
            return;
        }
        let action = PrDraftAction::for_pr(pr.is_draft);
        self.tx.send(AppEvent::OpenTogglePrDraftPrompt {
            number: pr.number,
            action,
            filter_state: self.state_filter,
        });
    }

    fn trigger_toggle_state(&mut self) {
        let Some((number, kind, state)) = self.selected_state_target() else {
            return;
        };
        let Some(action) = StateAction::for_state(state) else {
            // merged 的 PR 不能 reopen。要有回饋，否則按下去毫無反應 ——
            // 與同分頁的 merge / draft 切換遇到不可操作狀態時的行為一致。
            let state = state_label(state);
            let msg = match kind {
                github::GhItemKind::Issue => {
                    t!("github.flash.issue_state", number = number, state = state)
                }
                github::GhItemKind::PullRequest => {
                    t!("github.flash.pr_state", number = number, state = state)
                }
            };
            self.set_flash(msg.into_owned(), true);
            return;
        };
        self.tx.send(AppEvent::OpenToggleStatePrompt {
            number,
            kind,
            action,
            filter_state: self.state_filter,
        });
    }

    fn open_related_picker(&self) {
        let items = self.selected_related_items();
        self.tx.send(AppEvent::OpenRelatedPicker { items });
    }

    fn selected_related_items(&self) -> Vec<RelatedItem> {
        let idx = self.actual_index(self.selected_index);
        let mut items: Vec<RelatedItem> = Vec::new();
        match self.active_tab {
            GitHubTab::Issues => {
                let Some(issue) = self.issues.get(idx) else {
                    return items;
                };
                if let Some(p) = &issue.parent {
                    items.push(RelatedItem {
                        number: p.number,
                        state: p.state.clone(),
                        group: RelatedGroup::Parent,
                    });
                }
                for s in &issue.sub_issues {
                    items.push(RelatedItem {
                        number: s.number,
                        state: s.state.clone(),
                        group: RelatedGroup::Sub,
                    });
                    if items.len() >= 9 {
                        break;
                    }
                }
            }
            GitHubTab::PullRequests => {
                let Some(pr) = self.pull_requests.get(idx) else {
                    return items;
                };
                for l in &pr.linked_issues {
                    items.push(RelatedItem {
                        number: l.number,
                        state: l.state.clone(),
                        group: RelatedGroup::Linked,
                    });
                    if items.len() >= 9 {
                        break;
                    }
                }
            }
            GitHubTab::Branches | GitHubTab::Ci => {}
        }
        items
    }

    pub(super) fn handle_prompt_event(&mut self, event: UserEvent, count: usize, key: KeyEvent) {
        match event {
            UserEvent::Cancel | UserEvent::Close => {
                if self.search_input.value().is_empty() {
                    // 查詢字串是空的 → 關閉 view
                    self.tx.send(AppEvent::CloseGitHub);
                } else {
                    // 清空查詢字串 → 回到未篩選的列表
                    self.clear_search();
                    self.selected_index = 0;
                    self.offset = 0;
                    self.preview_offset = 0;
                    self.focus = GitHubFocus::List;
                }
            }
            UserEvent::Confirm => {
                let trimmed = self.search_input.value().trim();
                // branch、CI 卡片沒有編號可跳：純數字當一般查詢字串，不做 jump
                let jump = (!self.active_tab.is_card_tab())
                    .then(|| trimmed.parse::<u64>().ok())
                    .flatten();
                if let Some(number) = jump {
                    self.clear_search();
                    self.focus = GitHubFocus::List;
                    self.start_jump(number);
                } else {
                    self.focus = GitHubFocus::List;
                }
            }
            UserEvent::NavigateDown | UserEvent::SelectDown => {
                // 移動列表選取，但不離開 prompt
                let max = self.current_list_len().saturating_sub(1);
                let step = self.nav_step();
                for _ in 0..count {
                    self.selected_index = (self.selected_index + step).min(max);
                }
                self.preview_offset = 0;
                self.adjust_scroll();
            }
            UserEvent::NavigateUp | UserEvent::SelectUp => {
                let step = self.nav_step();
                for _ in 0..count {
                    self.selected_index = self.selected_index.saturating_sub(step);
                }
                self.preview_offset = 0;
                self.adjust_scroll();
            }
            // 切分頁時保留查詢字串
            UserEvent::RefList => self.switch_tab(self.active_tab.next()),
            UserEvent::RefListPrev => self.switch_tab(self.active_tab.prev()),
            _ => {
                // 把按鍵轉發給 tui-input；只有值真的變了才重建
                let before = self.search_input.value().to_string();
                self.search_input.handle_event(&Event::Key(key));
                if self.search_input.value() != before {
                    self.rebuild_filtered_indices();
                }
            }
        }
    }

    fn try_enter_checkbox_edit(&mut self) {
        let body = self.selected_body();
        if body.is_empty() {
            return;
        }
        let items = github::parse_checkboxes(&body);
        if items.is_empty() {
            self.set_flash(t!("github.flash.no_tasks").into_owned(), false);
            return;
        }
        if let Some((number, kind)) = self.selected_number_and_kind() {
            let original_checked = items.iter().map(|i| i.checked).collect();
            self.task_panel = Some(TaskListPanel {
                number,
                kind,
                items,
                original_checked,
                selected: 0,
            });
            self.focus = GitHubFocus::CheckboxEdit;
        }
    }

    fn selected_body(&self) -> String {
        let idx = self.actual_index(self.selected_index);
        match self.active_tab {
            GitHubTab::Issues => self
                .issues
                .get(idx)
                .map(|i| i.body.clone())
                .unwrap_or_default(),
            GitHubTab::PullRequests => self
                .pull_requests
                .get(idx)
                .map(|p| p.body.clone())
                .unwrap_or_default(),
            GitHubTab::Branches | GitHubTab::Ci => String::new(),
        }
    }

    pub(super) fn selected_url(&self) -> Option<String> {
        let idx = self.actual_index(self.selected_index);
        match self.active_tab {
            GitHubTab::Issues => self.issues.get(idx).map(|i| i.url.clone()),
            GitHubTab::PullRequests => self.pull_requests.get(idx).map(|p| p.url.clone()),
            GitHubTab::Branches => self.branches.get(idx).map(|b| b.url.clone()),
            GitHubTab::Ci => self.ci.get(idx).map(|e| e.url.clone()),
        }
    }

    fn with_selected_url(&self, on_url: impl FnOnce(String) -> AppEvent) {
        match self.selected_url() {
            Some(url) if !url.is_empty() => self.tx.send(on_url(url)),
            Some(_) => self
                .tx
                .send(AppEvent::NotifyWarn(t!("github.flash.no_url").into_owned())),
            None => {}
        }
    }

    /// 清空搜尋字串；Branches 的作者篩選獨立於搜尋字串，所以重建而非清空索引
    pub(super) fn clear_search(&mut self) {
        self.search_input.reset();
        self.rebuild_filtered_indices();
    }

    pub(super) fn rebuild_filtered_indices(&mut self) {
        let query = self.search_input.value().to_string();
        let matcher = (!query.is_empty()).then(|| SearchMatcher::new(&query, true, true));

        // Branches 另有作者篩選，沒有搜尋字串時也要算
        self.filtered_branch_indices = if matcher.is_some() || self.branches_author.is_some() {
            self.branches
                .iter()
                .enumerate()
                .filter(|(_, b)| {
                    self.branches_author.as_ref().is_none_or(|a| &b.author == a)
                        && matcher
                            .as_ref()
                            .is_none_or(|m| m.matches(&format!("{} @{}", b.name, b.author)))
                })
                .map(|(idx, _)| idx)
                .collect()
        } else {
            Vec::new()
        };

        self.filtered_ci_indices = match &matcher {
            Some(m) => self
                .ci
                .iter()
                .enumerate()
                .filter(|(_, e)| m.matches(&format!("#{} {} {}", e.number, e.title, e.branch)))
                .map(|(idx, _)| idx)
                .collect(),
            None => Vec::new(),
        };

        let Some(matcher) = matcher else {
            self.filtered_issue_indices.clear();
            self.filtered_pr_indices.clear();
            return;
        };

        self.filtered_issue_indices = self
            .issues
            .iter()
            .enumerate()
            .filter(|(_, i)| {
                let target = format!(
                    "#{} {} @{} {}",
                    i.number,
                    i.title,
                    i.author.login,
                    i.labels
                        .iter()
                        .map(|l| l.name.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                );
                matcher.matches(&target)
            })
            .map(|(idx, _)| idx)
            .collect();

        self.filtered_pr_indices = self
            .pull_requests
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                let target = format!(
                    "#{} {} @{} {}",
                    p.number,
                    p.title,
                    p.author.login,
                    p.labels
                        .iter()
                        .map(|l| l.name.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                );
                matcher.matches(&target)
            })
            .map(|(idx, _)| idx)
            .collect();

        // 限制 selected_index 的範圍
        let max = self.current_list_len().saturating_sub(1);
        if self.selected_index > max {
            self.selected_index = max;
        }
        self.offset = 0;
        self.preview_offset = 0;
    }

    fn start_jump(&mut self, number: u64) {
        self.pending_jump = Some(number);
        self.try_resolve_jump();
    }

    fn maybe_load_more(&mut self) {
        if self.loading_more || self.has_active_filter() {
            return;
        }
        if !self.current_has_next_cursor() {
            return;
        }
        let threshold = self
            .current_list_len()
            .saturating_sub(super::PREFETCH_THRESHOLD);
        if self.selected_index < threshold {
            return;
        }
        self.dispatch_load_more();
    }
}

/// 沒綁定、不帶修飾鍵的數字鍵 1~4 → 對應分頁。綁定過的數字鍵會 resolve
/// 成別的事件，不會走到這裡。
fn digit_tab(event: UserEvent, key: KeyEvent) -> Option<GitHubTab> {
    match (event, key.code) {
        (UserEvent::Unknown, KeyCode::Char(c)) if key.modifiers == KeyModifiers::NONE => {
            GitHubTab::from_digit(c)
        }
        _ => None,
    }
}

fn modal_yesno_aliases(event: UserEvent) -> UserEvent {
    match event {
        UserEvent::NavigateRight => UserEvent::Confirm,
        UserEvent::NavigateLeft => UserEvent::Cancel,
        _ => event,
    }
}

use ratatui::crossterm::event::{Event, KeyEvent};
use tui_input::backend::crossterm::EventHandler;

use crate::{
    event::{AppEvent, RelatedGroup, RelatedItem, UserEvent, UserEventWithCount},
    fuzzy::SearchMatcher,
    github::{self, PrDraftAction, StateAction},
};

use super::{GitHubFocus, GitHubTab, GitHubView, LoadState, TaskListPanel};

impl<'a> GitHubView<'a> {
    pub fn handle_event(&mut self, event_with_count: UserEventWithCount, key: KeyEvent) {
        let count = event_with_count.count;
        // 在偏 modal 的 focus（List/Preview）中，右/左鍵兼作 Confirm/Cancel。
        // Prompt 接收原始按鍵輸入；CheckboxEdit 用左/右鍵切換。
        let event = match self.focus {
            GitHubFocus::List | GitHubFocus::Preview => modal_yesno_aliases(event_with_count.event),
            GitHubFocus::Prompt | GitHubFocus::CheckboxEdit => event_with_count.event,
        };

        self.flash_message = None;

        let before = (self.active_tab, self.selected_index);
        match self.focus {
            GitHubFocus::CheckboxEdit => self.handle_checkbox_edit_event(event, count),
            GitHubFocus::Preview => self.handle_preview_event(event, count),
            GitHubFocus::Prompt => self.handle_prompt_event(event, count, key),
            GitHubFocus::List => self.handle_list_event(event, count),
        }
        if (self.active_tab, self.selected_index) != before {
            self.request_timeline_for_selected();
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
            UserEvent::Refresh => {
                self.dispatch_refresh();
            }
            _ => {}
        }
    }

    pub(super) fn handle_list_event(&mut self, event: UserEvent, count: usize) {
        match event {
            UserEvent::GitHubToggle | UserEvent::Cancel | UserEvent::Close => {
                self.tx.send(AppEvent::CloseGitHub);
            }
            UserEvent::RefList => {
                self.active_tab = match self.active_tab {
                    GitHubTab::Issues => GitHubTab::PullRequests,
                    GitHubTab::PullRequests => GitHubTab::Issues,
                };
                self.selected_index = 0;
                self.offset = 0;
                self.preview_offset = 0;
                self.bump_generation();
            }
            UserEvent::NavigateDown | UserEvent::SelectDown => {
                let max = self.current_list_len().saturating_sub(1);
                for _ in 0..count {
                    if self.selected_index < max {
                        self.selected_index += 1;
                    }
                }
                self.preview_offset = 0;
                self.adjust_scroll();
                self.maybe_load_more();
            }
            UserEvent::NavigateUp | UserEvent::SelectUp => {
                for _ in 0..count {
                    self.selected_index = self.selected_index.saturating_sub(1);
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
                let page = self.height.saturating_sub(3).max(1);
                let max = self.current_list_len().saturating_sub(1);
                self.selected_index = (self.selected_index + page).min(max);
                self.preview_offset = 0;
                self.adjust_scroll();
                self.maybe_load_more();
            }
            UserEvent::PageUp => {
                let page = self.height.saturating_sub(3).max(1);
                self.selected_index = self.selected_index.saturating_sub(page);
                self.preview_offset = 0;
                self.adjust_scroll();
            }
            UserEvent::HalfPageDown => {
                let half = self.height.saturating_sub(3).max(1) / 2;
                let max = self.current_list_len().saturating_sub(1);
                self.selected_index = (self.selected_index + half).min(max);
                self.preview_offset = 0;
                self.adjust_scroll();
                self.maybe_load_more();
            }
            UserEvent::HalfPageUp => {
                let half = self.height.saturating_sub(3).max(1) / 2;
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
            UserEvent::ShortCopy => {
                let kind = self.active_tab.kind();
                self.with_selected_url(|url| AppEvent::CopyToClipboard {
                    name: format!("{} URL", kind.display_name()),
                    value: url,
                });
            }
            UserEvent::FullCopy => {
                self.with_selected_url(AppEvent::OpenUrl);
            }
            UserEvent::TagCopy => {
                if let Some((number, kind)) = self.selected_number_and_kind() {
                    self.tx.send(AppEvent::CopyToClipboard {
                        name: format!("{} Number", kind.display_name()),
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

    fn try_merge_selected_pr(&mut self) {
        let idx = self.actual_index(self.selected_index);
        let Some(pr) = self.pull_requests.get(idx) else {
            return;
        };
        if pr.is_draft {
            self.set_flash(format!("PR #{} is draft", pr.number), true);
            return;
        }
        if pr.state != "OPEN" {
            self.set_flash(
                format!("PR #{} is {}", pr.number, pr.state.to_lowercase()),
                true,
            );
            return;
        }
        self.tx.send(AppEvent::OpenMergePrMethodPicker {
            number: pr.number,
            head_ref: pr.head_ref_name.clone(),
            state: self.state_filter,
            deletable: pr.head_branch_deletable,
        });
    }

    fn try_toggle_pr_draft(&mut self) {
        let idx = self.actual_index(self.selected_index);
        let Some(pr) = self.pull_requests.get(idx) else {
            return;
        };
        if pr.state != "OPEN" {
            self.set_flash(
                format!("PR #{} is {}", pr.number, pr.state.to_lowercase()),
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
            let msg = format!("{} #{number} is {}", kind.noun(), state.to_lowercase());
            self.set_flash(msg, true);
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
        }
        items
    }

    fn handle_prompt_event(&mut self, event: UserEvent, count: usize, key: KeyEvent) {
        match event {
            UserEvent::Cancel | UserEvent::Close => {
                if self.search_input.value().is_empty() {
                    // 查詢字串是空的 → 關閉 view
                    self.tx.send(AppEvent::CloseGitHub);
                } else {
                    // 清空查詢字串 → 回到未篩選的列表
                    self.search_input.reset();
                    self.filtered_issue_indices.clear();
                    self.filtered_pr_indices.clear();
                    self.selected_index = 0;
                    self.offset = 0;
                    self.preview_offset = 0;
                    self.focus = GitHubFocus::List;
                }
            }
            UserEvent::Confirm => {
                let trimmed = self.search_input.value().trim();
                if let Ok(number) = trimmed.parse::<u64>() {
                    self.search_input.reset();
                    self.filtered_issue_indices.clear();
                    self.filtered_pr_indices.clear();
                    self.focus = GitHubFocus::List;
                    self.start_jump(number);
                } else {
                    self.focus = GitHubFocus::List;
                }
            }
            UserEvent::NavigateDown | UserEvent::SelectDown => {
                // 移動列表選取，但不離開 prompt
                let max = self.current_list_len().saturating_sub(1);
                for _ in 0..count {
                    if self.selected_index < max {
                        self.selected_index += 1;
                    }
                }
                self.preview_offset = 0;
                self.adjust_scroll();
            }
            UserEvent::NavigateUp | UserEvent::SelectUp => {
                for _ in 0..count {
                    self.selected_index = self.selected_index.saturating_sub(1);
                }
                self.preview_offset = 0;
                self.adjust_scroll();
            }
            UserEvent::RefList => {
                // Tab 鍵：切換 Issues ⇄ PRs（保留查詢字串）
                self.active_tab = match self.active_tab {
                    GitHubTab::Issues => GitHubTab::PullRequests,
                    GitHubTab::PullRequests => GitHubTab::Issues,
                };
                self.selected_index = 0;
                self.offset = 0;
                self.preview_offset = 0;
            }
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
            self.set_flash("No tasks found".to_string(), false);
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
        }
    }

    fn selected_url(&self) -> Option<String> {
        let idx = self.actual_index(self.selected_index);
        match self.active_tab {
            GitHubTab::Issues => self.issues.get(idx).map(|i| i.url.clone()),
            GitHubTab::PullRequests => self.pull_requests.get(idx).map(|p| p.url.clone()),
        }
    }

    fn with_selected_url(&self, on_url: impl FnOnce(String) -> AppEvent) {
        match self.selected_url() {
            Some(url) if !url.is_empty() => self.tx.send(on_url(url)),
            Some(_) => self
                .tx
                .send(AppEvent::NotifyWarn("No URL for this item".into())),
            None => {}
        }
    }

    fn rebuild_filtered_indices(&mut self) {
        let query = self.search_input.value().to_string();
        if query.is_empty() {
            self.filtered_issue_indices.clear();
            self.filtered_pr_indices.clear();
            return;
        }
        let matcher = SearchMatcher::new(&query, true, true);

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

fn modal_yesno_aliases(event: UserEvent) -> UserEvent {
    match event {
        UserEvent::NavigateRight => UserEvent::Confirm,
        UserEvent::NavigateLeft => UserEvent::Cancel,
        _ => event,
    }
}

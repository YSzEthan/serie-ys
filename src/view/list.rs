use std::rc::Rc;

use ratatui::{
    crossterm::event::{KeyCode, KeyEvent},
    layout::Rect,
    Frame,
};

use crate::{
    app::AppContext,
    event::{AppEvent, Sender, UserEvent, UserEventWithCount},
    git::CommitHash,
    view::{
        dispatch_branch_copy, dispatch_checkout, dispatch_tag_copy, partition_branches,
        partition_tags, ListRefreshViewContext, RefreshViewContext,
    },
    widget::commit_list::{ChildJump, CommitList, CommitListState, FilterState, SearchState},
};

#[derive(Debug)]
pub struct ListView<'a> {
    commit_list_state: Option<CommitListState<'a>>,

    ctx: Rc<AppContext>,
    tx: Sender,
}

impl<'a> ListView<'a> {
    pub fn new(
        commit_list_state: CommitListState<'a>,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> ListView<'a> {
        ListView {
            commit_list_state: Some(commit_list_state),
            ctx,
            tx,
        }
    }

    pub fn handle_event(&mut self, event_with_count: UserEventWithCount, key: KeyEvent) {
        if self.commit_list_state.is_none() {
            return;
        }

        let event = event_with_count.event;
        let count = event_with_count.count;

        // 處理 filter 模式輸入
        if let FilterState::Filtering { .. } = self.as_list_state().filter_state() {
            match resolve_input_action(event, key) {
                InputAction::Confirm => {
                    self.as_mut_list_state().apply_filter();
                    self.clear_filter_query();
                }
                InputAction::Cancel => {
                    self.as_mut_list_state().cancel_filter();
                    self.clear_filter_query();
                }
                InputAction::IgnoreCaseToggle => {
                    self.as_mut_list_state().toggle_filter_ignore_case();
                    self.update_filter_query();
                }
                InputAction::FuzzyToggle => {
                    self.as_mut_list_state().toggle_filter_fuzzy();
                    self.update_filter_query();
                }
                InputAction::TextInput => {
                    self.as_mut_list_state().handle_filter_input(key);
                    self.update_filter_query();
                }
            }
            return;
        }

        // 處理 search 模式輸入
        if let SearchState::Searching { .. } = self.as_list_state().search_state() {
            match resolve_input_action(event, key) {
                InputAction::Confirm => {
                    self.as_mut_list_state().apply_search();
                    self.update_matched_message();
                }
                InputAction::Cancel => {
                    self.as_mut_list_state().cancel_search();
                    self.clear_search_query();
                }
                InputAction::IgnoreCaseToggle => {
                    self.as_mut_list_state().toggle_ignore_case();
                    self.update_search_query();
                }
                InputAction::FuzzyToggle => {
                    self.as_mut_list_state().toggle_fuzzy();
                    self.update_search_query();
                }
                InputAction::TextInput => {
                    self.as_mut_list_state().handle_search_input(key);
                    self.update_search_query();
                }
            }
            return;
        }

        // 正常模式
        match event {
            UserEvent::NavigateDown => {
                for _ in 0..count {
                    self.as_mut_list_state().select_next();
                }
            }
            UserEvent::NavigateUp => {
                for _ in 0..count {
                    self.as_mut_list_state().select_prev();
                }
            }
            UserEvent::GoToTop => {
                self.as_mut_list_state().select_first();
            }
            UserEvent::GoToBottom => {
                self.as_mut_list_state().select_last();
            }
            UserEvent::GoToHead => {
                self.as_mut_list_state().select_head();
            }
            // shift-j / shift-k 在 list view 是「捲動圖表」而非「移動游標」。
            // SelectDown / SelectUp 不可數，故不套 count 迴圈。
            UserEvent::SelectDown => {
                self.as_mut_list_state().scroll_down();
            }
            UserEvent::SelectUp => {
                self.as_mut_list_state().scroll_up();
            }
            UserEvent::GoToParent => {
                for _ in 0..count {
                    self.as_mut_list_state().select_parent();
                }
            }
            UserEvent::GoToChild => {
                for _ in 0..count {
                    match self.as_mut_list_state().select_child() {
                        ChildJump::Jumped => continue,
                        ChildJump::None => break,
                        ChildJump::Ambiguous(options) => {
                            self.tx.send(AppEvent::OpenChildPicker { options });
                            break;
                        }
                    }
                }
            }
            UserEvent::UserCommand(n) => {
                self.tx.send(AppEvent::OpenUserCommand(n));
            }
            UserEvent::ShortCopy => {
                self.copy_commit_short_hash();
            }
            UserEvent::FullCopy => {
                self.copy_commit_subject();
            }
            UserEvent::BranchCopy => {
                self.handle_branch_copy(false);
            }
            UserEvent::FullBranchCopy => {
                self.handle_branch_copy(true);
            }
            UserEvent::TagCopy => {
                self.handle_tag_copy();
            }
            UserEvent::Search => {
                self.as_mut_list_state().start_search();
                self.update_search_query();
            }
            UserEvent::Filter => {
                self.as_mut_list_state().start_filter();
                self.update_filter_query();
            }
            UserEvent::Cancel => {
                self.as_mut_list_state().cancel_search();
                self.as_mut_list_state().cancel_filter();
                self.clear_search_query();
            }
            UserEvent::Confirm | UserEvent::NavigateRight => {
                self.tx.send(AppEvent::OpenDetail);
            }
            UserEvent::RefList => {
                self.tx.send(AppEvent::OpenRefs);
            }
            UserEvent::ShellToggle => {
                self.tx.send(AppEvent::OpenShell);
            }
            UserEvent::CreateTag => {
                self.tx.send(AppEvent::OpenCreateTag);
            }
            UserEvent::DeleteTag => {
                self.tx.send(AppEvent::OpenDeleteTag);
            }
            UserEvent::RemoteRefsToggle => {
                let show = self.as_mut_list_state().toggle_remote_refs();
                if show {
                    self.tx
                        .send(AppEvent::NotifyInfo("Remote refs: shown".into()));
                } else {
                    self.tx
                        .send(AppEvent::NotifyInfo("Remote refs: hidden".into()));
                }
                self.tx
                    .send_after(AppEvent::ClearStatusLine, std::time::Duration::from_secs(3));
            }
            UserEvent::Fetch => {
                self.tx.send(AppEvent::FetchAll);
            }
            UserEvent::DeleteRef if !self.as_list_state().is_virtual_row_selected() => {
                let refs = self.as_list_state().selected_commit_refs();
                let (local, _remote) = partition_branches(refs.iter().copied());
                let names: Vec<String> = local.into_iter().map(str::to_owned).collect();
                self.tx.send(AppEvent::OpenDeleteBranch { names });
            }
            UserEvent::Checkout if !self.as_list_state().is_virtual_row_selected() => {
                let refs = self.as_list_state().selected_commit_refs();
                let hash = self
                    .as_list_state()
                    .selected_commit_hash()
                    .as_str()
                    .to_string();
                dispatch_checkout(&self.tx, refs, &hash);
            }
            UserEvent::Refresh => {
                self.refresh();
            }
            _ => {}
        }

        if let SearchState::Applied { .. } = self.as_list_state().search_state() {
            match event {
                UserEvent::GoToNext => {
                    self.as_mut_list_state().select_next_match();
                    self.update_matched_message();
                }
                UserEvent::GoToPrevious => {
                    self.as_mut_list_state().select_prev_match();
                    self.update_matched_message();
                }
                _ => {}
            }
            // 這裡不 return
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect, marquee_frame: u64) {
        let commit_list = CommitList::new(self.ctx.clone(), marquee_frame);
        f.render_stateful_widget(commit_list, area, self.as_mut_list_state());
    }
}

impl<'a> ListView<'a> {
    pub fn take_list_state(&mut self) -> Option<CommitListState<'a>> {
        self.commit_list_state.take()
    }

    pub fn take_graph_clear(&mut self) -> bool {
        self.commit_list_state
            .as_mut()
            .is_some_and(|s| s.take_graph_clear())
    }

    pub fn request_graph_clear(&mut self) {
        if let Some(s) = self.commit_list_state.as_mut() {
            s.request_graph_clear();
        }
    }

    /// child picker 選定候選後跳過去。用 `step_to_commit_hash`（保留 scroll
    /// margin）而不是 `select_commit_hash`（會把目標釘到畫面最上緣）——跟
    /// 剛好一個 child 時 `select_child()` 直接跳的手感要一致。
    pub fn select_commit_by_hash(&mut self, hash: &CommitHash) {
        self.as_mut_list_state().step_to_commit_hash(hash);
    }

    fn as_mut_list_state(&mut self) -> &mut CommitListState<'a> {
        self.commit_list_state
            .as_mut()
            .expect("commit_list_state already taken")
    }

    pub fn as_list_state(&self) -> &CommitListState<'a> {
        self.commit_list_state
            .as_ref()
            .expect("commit_list_state already taken")
    }

    fn update_search_query(&self) {
        let Some(list_state) = self.commit_list_state.as_ref() else {
            return;
        };
        if let SearchState::Searching { .. } = list_state.search_state() {
            if let Some(query) = list_state.search_query_string() {
                let cursor_pos = list_state.search_query_cursor_position();
                let transient_msg = list_state.transient_message_string();
                self.tx.send(AppEvent::UpdateStatusInput(
                    query,
                    Some(cursor_pos),
                    transient_msg,
                ));
            }
        }
    }

    fn clear_search_query(&self) {
        self.tx.send(AppEvent::ClearStatusLine);
    }

    fn update_filter_query(&self) {
        if let FilterState::Filtering { .. } = self.as_list_state().filter_state() {
            let list_state = self.as_list_state();
            if let Some(query) = list_state.filter_query_string() {
                let cursor_pos = list_state.filter_query_cursor_position();
                let transient_msg = list_state.filter_transient_message_string();
                self.tx.send(AppEvent::UpdateStatusInput(
                    query,
                    Some(cursor_pos),
                    transient_msg,
                ));
            }
        }
    }

    fn clear_filter_query(&self) {
        self.tx.send(AppEvent::ClearStatusLine);
    }

    fn update_matched_message(&self) {
        if let Some((msg, matched)) = self.as_list_state().matched_query_string() {
            if matched {
                self.tx.send(AppEvent::NotifyInfo(msg));
            } else {
                self.tx.send(AppEvent::NotifyWarn(msg));
            }
        } else {
            self.tx.send(AppEvent::ClearStatusLine);
        }
    }

    fn copy_commit_short_hash(&self) {
        if self.as_list_state().is_virtual_row_selected() {
            return;
        }
        let selected = self.as_list_state().selected_commit_hash();
        self.copy_to_clipboard("Commit SHA (short)".into(), selected.as_short_hash().into());
    }

    fn copy_commit_subject(&self) {
        if self.as_list_state().is_virtual_row_selected() {
            return;
        }
        let subject = self.as_list_state().selected_commit_subject();
        self.copy_to_clipboard("Commit Subject".into(), subject.into());
    }

    fn handle_branch_copy(&self, full: bool) {
        if self.as_list_state().is_virtual_row_selected() {
            return;
        }
        let refs = self.as_list_state().selected_commit_refs();
        let (local, remote) = partition_branches(refs.iter().copied());
        dispatch_branch_copy(&self.tx, &local, &remote, full);
    }

    fn handle_tag_copy(&self) {
        if self.as_list_state().is_virtual_row_selected() {
            return;
        }
        let refs = self.as_list_state().selected_commit_refs();
        let tags = partition_tags(refs.iter().copied());
        dispatch_tag_copy(&self.tx, &tags);
    }

    fn copy_to_clipboard(&self, name: String, value: String) {
        self.tx.send(AppEvent::CopyToClipboard { name, value });
    }

    pub fn refresh(&self) {
        let list_state = self.as_list_state();
        let list_context = ListRefreshViewContext::from(list_state);
        self.tx
            .send(AppEvent::Refresh(RefreshViewContext::list(list_context)));
    }

    /// 順序有影響，四步缺一不可：
    /// 1. `reset_height` 先寫——`compute_selection`（selection 唯一入口）在
    ///    `height == 0` 時永遠回 `None`，後面幾步的 `set_visible_selection`
    ///    才會是有意義的動作，不是靠「height 還沒設，反正也是 no-op」撐著。
    /// 2. `set_show_remote_refs` + `restore_filter`——兩者都會重建
    ///    `filtered_indices`（改變 `total`）並把游標壓到 `VisibleIdx(0)`，
    ///    必須在動 selection *之前*。
    /// 3. selection 還原（`select_first` / `select_commit_hash`）——目標被
    ///    還原的 filter 藏起來時 `select_commit_hash` 自然不動，游標留在
    ///    上一步的頂端。
    /// 4. `restore_search`——要讀 `current_selected_raw()`，必須排在
    ///    selection 還原之後。
    pub fn reset_commit_list_with(&mut self, list_context: &ListRefreshViewContext) {
        let ListRefreshViewContext {
            commit_hash,
            selected,
            height,
            scroll_to_top,
            show_remote_refs,
            search,
            filter,
        } = list_context;
        let list_state = self.as_mut_list_state();
        list_state.reset_height(*height);
        list_state.set_show_remote_refs(*show_remote_refs);
        if let Some(filter) = filter {
            list_state.restore_filter(filter);
        }
        if *scroll_to_top {
            list_state.select_first();
        } else {
            list_state.select_commit_hash(commit_hash);
            for _ in 0..*selected {
                list_state.scroll_up();
            }
        }
        if let Some(search) = search {
            list_state.restore_search(search);
        }
    }
}

/// 文字輸入模式（search/filter）解析後的動作。
///
/// 當 y/n 按鍵被綁定到 Confirm/Cancel 時，應該視為文字輸入
/// 而非控制動作。這個 enum 就是在捕捉這個判斷結果。
enum InputAction {
    Confirm,
    Cancel,
    IgnoreCaseToggle,
    FuzzyToggle,
    TextInput,
}

fn resolve_input_action(event: UserEvent, key: KeyEvent) -> InputAction {
    match event {
        UserEvent::Confirm if key.code == KeyCode::Char('y') => InputAction::TextInput,
        UserEvent::Confirm => InputAction::Confirm,
        UserEvent::Cancel if key.code == KeyCode::Char('n') => InputAction::TextInput,
        UserEvent::Cancel => InputAction::Cancel,
        UserEvent::IgnoreCaseToggle => InputAction::IgnoreCaseToggle,
        UserEvent::FuzzyToggle => InputAction::FuzzyToggle,
        _ => InputAction::TextInput,
    }
}

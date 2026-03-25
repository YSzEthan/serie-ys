use std::rc::Rc;

use ratatui::{
    crossterm::event::{KeyCode, KeyEvent},
    layout::Rect,
    Frame,
};

use crate::{
    app::AppContext,
    event::{AppEvent, Sender, UserEvent, UserEventWithCount},
    view::{ListRefreshViewContext, RefreshViewContext},
    widget::commit_list::{CommitList, CommitListState, FilterState, SearchState},
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

        // Handle filter mode input
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

        // Handle search mode input
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

        // Normal mode
        match event {
            UserEvent::NavigateDown | UserEvent::SelectDown => {
                for _ in 0..count {
                    self.as_mut_list_state().select_next();
                }
            }
            UserEvent::NavigateUp | UserEvent::SelectUp => {
                for _ in 0..count {
                    self.as_mut_list_state().select_prev();
                }
            }
            UserEvent::ScrollDown => {
                for _ in 0..count {
                    self.as_mut_list_state().scroll_down();
                }
            }
            UserEvent::GoToParent => {
                for _ in 0..count {
                    self.as_mut_list_state().scroll_up();
                }
            }
            UserEvent::ShortCopy => {
                self.copy_commit_short_hash();
            }
            UserEvent::Search => {
                self.as_mut_list_state().start_search();
                self.update_search_query();
            }
            UserEvent::Filter => {
                self.as_mut_list_state().start_filter();
                self.update_filter_query();
            }
            UserEvent::GitHubToggle => {
                self.tx.send(AppEvent::OpenGitHub);
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
            // Do not return here
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        let commit_list = CommitList::new(self.ctx.clone());
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
        self.copy_to_clipboard("Commit SHA (short)".into(), selected.as_short_hash());
    }

    fn copy_to_clipboard(&self, name: String, value: String) {
        self.tx.send(AppEvent::CopyToClipboard { name, value });
    }

    pub fn refresh(&self) {
        let list_state = self.as_list_state();
        let list_context = ListRefreshViewContext::from(list_state);
        let context = RefreshViewContext::List { list_context };
        self.tx.send(AppEvent::Clear); // hack: reset the rendering of the image area
        self.tx.send(AppEvent::Refresh(context));
    }

    pub fn reset_commit_list_with(&mut self, list_context: &ListRefreshViewContext) {
        let ListRefreshViewContext {
            commit_hash,
            selected,
            height,
            scroll_to_top,
        } = list_context;
        let list_state = self.as_mut_list_state();
        list_state.reset_height(*height);
        if *scroll_to_top {
            list_state.select_first();
        } else {
            list_state.select_commit_hash(commit_hash);
            for _ in 0..*selected {
                list_state.scroll_up();
            }
        }
    }
}

/// Resolved action for text input modes (search/filter).
///
/// When y/n keys are bound to Confirm/Cancel, they should be treated as text
/// input rather than as control actions. This enum captures that decision.
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

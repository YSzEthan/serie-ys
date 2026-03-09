use std::rc::Rc;

use ratatui::{
    crossterm::event::KeyEvent,
    layout::{Constraint, Layout, Rect},
    widgets::Clear,
    Frame,
};

use crate::{
    app::AppContext,
    event::{AppEvent, Sender, UserEvent, UserEventWithCount},
    git::{Commit, FileChange, Ref, Repository, WorkingChanges},
    view::{ListRefreshViewContext, RefreshViewContext},
    widget::{
        commit_detail::{CommitDetail, CommitDetailState, WorkingChangesDetail},
        commit_list::{CommitList, CommitListState},
    },
};

#[derive(Debug)]
enum DetailContent {
    Commit {
        commit: Box<Commit>,
        changes: Vec<FileChange>,
        refs: Vec<Ref>,
    },
    WorkingChanges(WorkingChanges),
}

#[derive(Debug)]
pub struct DetailView<'a> {
    commit_list_state: Option<CommitListState<'a>>,
    commit_detail_state: CommitDetailState,

    content: DetailContent,

    ctx: Rc<AppContext>,
    tx: Sender,
    clear: bool,
}

impl<'a> DetailView<'a> {
    pub fn new(
        commit_list_state: CommitListState<'a>,
        commit: Commit,
        changes: Vec<FileChange>,
        refs: Vec<Ref>,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> DetailView<'a> {
        DetailView {
            commit_list_state: Some(commit_list_state),
            commit_detail_state: CommitDetailState::default(),
            content: DetailContent::Commit {
                commit: Box::new(commit),
                changes,
                refs,
            },
            ctx,
            tx,
            clear: false,
        }
    }

    pub fn new_working_changes(
        commit_list_state: CommitListState<'a>,
        working_changes: WorkingChanges,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> DetailView<'a> {
        DetailView {
            commit_list_state: Some(commit_list_state),
            commit_detail_state: CommitDetailState::default(),
            content: DetailContent::WorkingChanges(working_changes),
            ctx,
            tx,
            clear: false,
        }
    }

    pub fn handle_event(&mut self, event_with_count: UserEventWithCount, _: KeyEvent) {
        let event = event_with_count.event;
        let count = event_with_count.count;

        match event {
            UserEvent::NavigateDown => {
                for _ in 0..count {
                    self.commit_detail_state.scroll_down();
                }
            }
            UserEvent::NavigateUp => {
                for _ in 0..count {
                    self.commit_detail_state.scroll_up();
                }
            }
            UserEvent::PageDown => {
                for _ in 0..count {
                    self.commit_detail_state.scroll_page_down();
                }
            }
            UserEvent::PageUp => {
                for _ in 0..count {
                    self.commit_detail_state.scroll_page_up();
                }
            }
            UserEvent::HalfPageDown => {
                for _ in 0..count {
                    self.commit_detail_state.scroll_half_page_down();
                }
            }
            UserEvent::HalfPageUp => {
                for _ in 0..count {
                    self.commit_detail_state.scroll_half_page_up();
                }
            }
            UserEvent::GoToTop => {
                self.commit_detail_state.select_first();
            }
            UserEvent::GoToBottom => {
                self.commit_detail_state.select_last();
            }
            UserEvent::SelectDown => {
                self.tx.send(AppEvent::SelectOlderCommit);
            }
            UserEvent::SelectUp => {
                self.tx.send(AppEvent::SelectNewerCommit);
            }
            UserEvent::GoToParent => {
                self.tx.send(AppEvent::SelectParentCommit);
            }
            UserEvent::ShortCopy => {
                self.copy_commit_short_hash();
            }
            UserEvent::FullCopy => {
                self.copy_commit_hash();
            }
            UserEvent::UserCommand(n) => {
                self.tx.send(AppEvent::OpenUserCommand(n));
            }
            UserEvent::HelpToggle => {
                self.tx.send(AppEvent::OpenHelp);
            }
            UserEvent::Confirm | UserEvent::Cancel | UserEvent::Close => {
                self.tx.send(AppEvent::ClearDetail); // hack: reset the rendering of the image area
                self.tx.send(AppEvent::CloseDetail);
            }
            UserEvent::Refresh => {
                self.refresh();
            }
            _ => {}
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        let detail_height = (area.height - 1).min(self.ctx.ui_config.detail.height);
        let [list_area, detail_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(detail_height)]).areas(area);

        let commit_list = CommitList::new(self.ctx.clone());
        f.render_stateful_widget(
            commit_list,
            list_area,
            self.commit_list_state
                .as_mut()
                .expect("commit_list_state already taken"),
        );

        if self.clear {
            f.render_widget(Clear, detail_area);
            return;
        }

        match &self.content {
            DetailContent::Commit {
                commit,
                changes,
                refs,
            } => {
                let commit_detail = CommitDetail::new(commit, changes, refs, self.ctx.clone());
                f.render_stateful_widget(commit_detail, detail_area, &mut self.commit_detail_state);
            }
            DetailContent::WorkingChanges(wc) => {
                let wc_detail = WorkingChangesDetail::new(wc, self.ctx.clone());
                f.render_stateful_widget(wc_detail, detail_area, &mut self.commit_detail_state);
            }
        }

        // clear the image area if needed
        for y in detail_area.top()..detail_area.bottom() {
            self.ctx.image_protocol.clear_line(y);
        }
    }
}

impl<'a> DetailView<'a> {
    pub fn take_list_state(&mut self) -> Option<CommitListState<'a>> {
        self.commit_list_state.take()
    }

    pub fn as_list_state(&self) -> &CommitListState<'_> {
        self.commit_list_state.as_ref().unwrap()
    }

    pub fn select_older_commit(&mut self, repository: &Repository) {
        self.update_selected_commit(repository, |state| state.select_next());
    }

    pub fn select_newer_commit(&mut self, repository: &Repository) {
        self.update_selected_commit(repository, |state| state.select_prev());
    }

    pub fn select_parent_commit(&mut self, repository: &Repository) {
        self.update_selected_commit(repository, |state| state.select_parent());
    }

    fn update_selected_commit<F>(&mut self, repository: &Repository, update_commit_list_state: F)
    where
        F: FnOnce(&mut CommitListState<'a>),
    {
        let Some(commit_list_state) = self.commit_list_state.as_mut() else {
            return;
        };
        update_commit_list_state(commit_list_state);

        if commit_list_state.is_virtual_row_selected() {
            if let Some(wc) = commit_list_state.working_changes() {
                self.content = DetailContent::WorkingChanges(wc.clone());
            }
        } else {
            let selected = commit_list_state.selected_commit_hash().clone();
            let (commit, changes) = repository.commit_detail(&selected);
            let refs = repository.refs(&selected).into_iter().cloned().collect();
            self.content = DetailContent::Commit {
                commit: Box::new(commit),
                changes,
                refs,
            };
        }

        self.commit_detail_state.select_first();
    }

    pub fn clear(&mut self) {
        self.clear = true;
    }

    fn copy_commit_short_hash(&self) {
        if let DetailContent::Commit { commit, .. } = &self.content {
            self.copy_to_clipboard(
                "Commit SHA (short)".into(),
                commit.commit_hash.as_short_hash(),
            );
        }
    }

    fn copy_commit_hash(&self) {
        if let DetailContent::Commit { commit, .. } = &self.content {
            self.copy_to_clipboard("Commit SHA".into(), commit.commit_hash.as_str().into());
        }
    }

    fn copy_to_clipboard(&self, name: String, value: String) {
        self.tx.send(AppEvent::CopyToClipboard { name, value });
    }

    pub fn refresh(&self) {
        let list_state = self.as_list_state();
        let list_context = ListRefreshViewContext::from(list_state);
        let context = RefreshViewContext::Detail { list_context };
        self.tx.send(AppEvent::Clear); // hack: reset the rendering of the image area
        self.tx.send(AppEvent::Refresh(context));
    }
}

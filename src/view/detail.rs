use std::rc::Rc;

use ratatui::{crossterm::event::KeyEvent, layout::Rect, widgets::Clear, Frame};

use crate::{
    app::AppContext,
    config::UserListColumnType,
    event::{AppEvent, Sender, UserEvent, UserEventWithCount},
    git::{Commit, FileChange, Ref, Repository, WorkingChanges},
    view::{dispatch_branch_copy, partition_branches, ListRefreshViewContext, RefreshViewContext},
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
        }
    }

    pub fn handle_event(&mut self, event_with_count: UserEventWithCount, _key: KeyEvent) {
        let event = event_with_count.event;
        let count = event_with_count.count;

        match event {
            UserEvent::DetailPaneToggle => {
                self.commit_detail_state.toggle_pane();
            }
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
            UserEvent::NavigateRight => {
                self.tx.send(AppEvent::SelectOlderCommit);
            }
            UserEvent::NavigateLeft => {
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
            UserEvent::BranchCopy => {
                self.handle_branch_copy(false);
            }
            UserEvent::FullBranchCopy => {
                self.handle_branch_copy(true);
            }
            UserEvent::RemoteRefsToggle => {
                if let Some(ref mut cls) = self.commit_list_state {
                    let show = cls.toggle_remote_refs();
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
            }
            UserEvent::Confirm | UserEvent::Cancel | UserEvent::Close => {
                self.tx.send(AppEvent::CloseDetail);
            }
            UserEvent::RefList => {
                self.tx.send(AppEvent::OpenRefs);
            }
            UserEvent::Refresh => {
                self.refresh();
            }
            _ => {}
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        let max_height = (area.height.saturating_sub(2)).min(self.ctx.ui_config.detail.height);
        let content_height = match &self.content {
            DetailContent::Commit {
                commit,
                changes,
                refs,
            } => CommitDetail::new(commit, changes, refs, self.ctx.clone()).content_height(),
            DetailContent::WorkingChanges(wc) => {
                WorkingChangesDetail::new(wc, self.ctx.clone()).content_height()
            }
        };
        let detail_height = max_height.min(content_height);

        let commit_list_state = self
            .commit_list_state
            .as_mut()
            .expect("commit_list_state already taken");

        // Set inline detail height so CommitList renders the gap
        commit_list_state.set_inline_detail_height(detail_height);

        // Render CommitList using the full area — it handles the gap internally
        let commit_list = CommitList::new(self.ctx.clone());
        f.render_stateful_widget(commit_list, area, commit_list_state);

        // Calculate the graph+marker width for inline detail positioning
        let graph_marker_width = calc_graph_marker_width(commit_list_state, &self.ctx);

        // Get the content area (below header)
        let content_area = Rect::new(
            area.left(),
            area.top() + 1, // skip header row
            area.width,
            area.height.saturating_sub(1),
        );

        if let Some(detail_rect) =
            commit_list_state.inline_detail_rect(content_area, graph_marker_width)
        {
            // Clear terminal protocol images in the gap rows
            for y in detail_rect.top()..detail_rect.bottom() {
                self.ctx.image_protocol.clear_line(y);
            }

            // Clear the detail area text content
            f.render_widget(Clear, detail_rect);

            match &self.content {
                DetailContent::Commit {
                    commit,
                    changes,
                    refs,
                } => {
                    let commit_detail = CommitDetail::new(commit, changes, refs, self.ctx.clone());
                    f.render_stateful_widget(
                        commit_detail,
                        detail_rect,
                        &mut self.commit_detail_state,
                    );
                }
                DetailContent::WorkingChanges(wc) => {
                    let wc_detail = WorkingChangesDetail::new(wc, self.ctx.clone());
                    f.render_stateful_widget(wc_detail, detail_rect, &mut self.commit_detail_state);
                }
            }
        }
    }
}

/// Calculate the combined width of Graph + Marker columns.
fn calc_graph_marker_width(state: &CommitListState<'_>, ctx: &AppContext) -> u16 {
    let mut width: u16 = 0;
    for col in &ctx.ui_config.list.columns {
        match col {
            UserListColumnType::Graph => {
                width += state.graph_area_cell_width();
            }
            UserListColumnType::Marker => {
                width += 1;
            }
            _ => {}
        }
    }
    width
}

impl<'a> DetailView<'a> {
    pub fn take_graph_clear(&mut self) -> bool {
        self.commit_list_state
            .as_mut()
            .is_some_and(|s| s.take_graph_clear())
    }

    pub fn take_list_state(&mut self) -> Option<CommitListState<'a>> {
        let mut state = self.commit_list_state.take();
        if let Some(ref mut s) = state {
            s.set_inline_detail_height(0);
        }
        state
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
            let (_, refs) = repository.commit_refs(&selected);
            self.content = DetailContent::Commit {
                commit: Box::new(commit.clone()),
                changes,
                refs,
            };
        }

        self.commit_detail_state.select_first();
    }

    fn copy_commit_short_hash(&self) {
        if let DetailContent::Commit { commit, .. } = &self.content {
            self.copy_to_clipboard(
                "Commit SHA (short)".into(),
                commit.commit_hash.as_short_hash().into(),
            );
        }
    }

    fn copy_commit_hash(&self) {
        if let DetailContent::Commit { commit, .. } = &self.content {
            self.copy_to_clipboard("Commit SHA".into(), commit.commit_hash.as_str().into());
        }
    }

    fn handle_branch_copy(&self, full: bool) {
        let DetailContent::Commit { refs, .. } = &self.content else {
            return;
        };
        let (local, remote) = partition_branches(refs.iter());
        dispatch_branch_copy(&self.tx, &local, &remote, full);
    }

    fn copy_to_clipboard(&self, name: String, value: String) {
        self.tx.send(AppEvent::CopyToClipboard { name, value });
    }

    pub fn refresh(&self) {
        let list_state = self.as_list_state();
        let list_context = ListRefreshViewContext::from(list_state);
        let context = RefreshViewContext::Detail { list_context };
        self.tx.send(AppEvent::Refresh(context));
    }
}

use std::rc::Rc;

use ratatui::{
    crossterm::event::KeyEvent,
    layout::{Constraint, Layout, Rect},
    Frame,
};

use crate::{
    app::AppContext,
    event::{AppEvent, Sender, UserEvent, UserEventWithCount},
    git::{Ref, RefType},
    view::{ListRefreshViewContext, RefreshViewContext, RefsRefreshViewContext},
    widget::{
        commit_list::{CommitList, CommitListState},
        ref_list::{RefList, RefListState},
    },
};

#[derive(Debug)]
pub struct RefsView<'a> {
    commit_list_state: Option<CommitListState<'a>>,
    ref_list_state: RefListState,

    refs: Vec<Ref>,

    ctx: Rc<AppContext>,
    tx: Sender,
}

impl<'a> RefsView<'a> {
    pub fn new(
        commit_list_state: CommitListState<'a>,
        refs: Vec<Ref>,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> RefsView<'a> {
        RefsView {
            commit_list_state: Some(commit_list_state),
            ref_list_state: RefListState::new(),
            refs,
            ctx,
            tx,
        }
    }

    pub fn with_state(
        commit_list_state: CommitListState<'a>,
        ref_list_state: RefListState,
        refs: Vec<Ref>,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> RefsView<'a> {
        RefsView {
            commit_list_state: Some(commit_list_state),
            ref_list_state,
            refs,
            ctx,
            tx,
        }
    }

    pub fn handle_event(&mut self, event_with_count: UserEventWithCount, _: KeyEvent) {
        let event = event_with_count.event;
        let count = event_with_count.count;

        match event {
            UserEvent::Cancel => {
                self.tx.send(AppEvent::CloseRefs);
            }
            UserEvent::NavigateDown | UserEvent::SelectDown => {
                for _ in 0..count {
                    self.ref_list_state.select_next();
                }
                self.update_commit_list_selected();
            }
            UserEvent::NavigateUp | UserEvent::SelectUp => {
                for _ in 0..count {
                    self.ref_list_state.select_prev();
                }
                self.update_commit_list_selected();
            }
            UserEvent::NavigateRight => {
                self.ref_list_state.open_node();
                self.update_commit_list_selected();
            }
            UserEvent::NavigateLeft => {
                if self.ref_list_state.is_at_root_level() {
                    self.tx.send(AppEvent::CloseRefs);
                } else {
                    self.ref_list_state.close_node();
                    self.update_commit_list_selected();
                }
            }
            UserEvent::UserCommand(_) | UserEvent::DeleteTag => {
                self.open_delete_ref();
            }
            UserEvent::Refresh => {
                self.refresh();
            }
            _ => {}
        }
    }

    fn open_delete_ref(&self) {
        if let Some(name) = self.ref_list_state.selected_local_branch() {
            self.tx.send(AppEvent::OpenDeleteRef {
                ref_name: name,
                ref_type: RefType::Branch,
            });
        } else if let Some(name) = self.ref_list_state.selected_remote_branch() {
            self.tx.send(AppEvent::OpenDeleteRef {
                ref_name: name,
                ref_type: RefType::RemoteBranch,
            });
        } else if let Some(name) = self.ref_list_state.selected_tag() {
            self.tx.send(AppEvent::OpenDeleteRef {
                ref_name: name,
                ref_type: RefType::Tag,
            });
        } else {
            self.tx.send(AppEvent::NotifyWarn(
                "Select a branch or tag to delete".into(),
            ));
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        let graph_width = self.as_list_state().graph_area_cell_width() + 1; // graph area + marker
        let refs_width =
            (area.width.saturating_sub(graph_width)).min(self.ctx.ui_config.refs.width);

        let [list_area, refs_area] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(refs_width)]).areas(area);

        let commit_list = CommitList::new(self.ctx.clone());
        f.render_stateful_widget(commit_list, list_area, self.as_mut_list_state());

        let ref_list = RefList::new(&self.refs, self.ctx.clone());
        f.render_stateful_widget(ref_list, refs_area, &mut self.ref_list_state);
    }
}

impl<'a> RefsView<'a> {
    pub fn take_list_state(&mut self) -> Option<CommitListState<'a>> {
        self.commit_list_state.take()
    }

    pub fn take_ref_list_state(&mut self) -> RefListState {
        std::mem::take(&mut self.ref_list_state)
    }

    pub fn take_refs(&mut self) -> Vec<Ref> {
        std::mem::take(&mut self.refs)
    }

    fn as_list_state(&self) -> &CommitListState<'a> {
        self.commit_list_state
            .as_ref()
            .expect("commit_list_state already taken")
    }

    fn as_mut_list_state(&mut self) -> &mut CommitListState<'a> {
        self.commit_list_state
            .as_mut()
            .expect("commit_list_state already taken")
    }

    fn update_commit_list_selected(&mut self) {
        if let Some(selected) = self.ref_list_state.selected_ref_name() {
            if let Some(list_state) = self.commit_list_state.as_mut() {
                list_state.select_ref(&selected);
            }
        }
    }

    pub fn refresh(&self) {
        let list_state = self.as_list_state();
        let list_context = ListRefreshViewContext::from(list_state);
        let (tree_selected, tree_opened) = self.ref_list_state.current_tree_status();
        let refs_context = RefsRefreshViewContext {
            selected: tree_selected,
            opened: tree_opened,
        };
        let context = RefreshViewContext::Refs {
            list_context,
            refs_context,
        };
        self.tx.send(AppEvent::Clear); // hack: reset the rendering of the image area
        self.tx.send(AppEvent::Refresh(context));
    }

    pub fn reset_refs_with(&mut self, refs_context: RefsRefreshViewContext) {
        self.ref_list_state
            .reset_tree_status(refs_context.selected, refs_context.opened);
    }
}

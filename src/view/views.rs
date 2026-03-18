use std::{path::PathBuf, rc::Rc};

use ratatui::{crossterm::event::KeyEvent, layout::Rect, Frame};

use crate::{
    app::AppContext,
    event::{AppEvent, Sender, UserEventWithCount},
    external::ExternalCommandParameters,
    git::{Commit, CommitHash, FileChange, Ref, RefType, WorkingChanges},
    view::{
        create_tag::CreateTagView, delete_ref::DeleteRefView, delete_tag::DeleteTagView,
        detail::DetailView, github::GitHubView, help::HelpView, list::ListView, refs::RefsView,
        user_command::UserCommandView,
    },
    widget::{commit_list::CommitListState, ref_list::RefListState},
};

#[derive(Debug, Default)]
pub enum View<'a> {
    #[default]
    Default, // dummy variant to make #[default] work
    List(Box<ListView<'a>>),
    Detail(Box<DetailView<'a>>),
    UserCommand(Box<UserCommandView<'a>>),
    Refs(Box<RefsView<'a>>),
    CreateTag(Box<CreateTagView<'a>>),
    DeleteTag(Box<DeleteTagView<'a>>),
    DeleteRef(Box<DeleteRefView<'a>>),
    Help(Box<HelpView<'a>>),
    GitHub(Box<GitHubView<'a>>),
}

impl<'a> View<'a> {
    pub fn handle_event(&mut self, event_with_count: UserEventWithCount, key_event: KeyEvent) {
        match self {
            View::Default => {}
            View::List(view) => view.handle_event(event_with_count, key_event),
            View::Detail(view) => view.handle_event(event_with_count, key_event),
            View::UserCommand(view) => view.handle_event(event_with_count, key_event),
            View::Refs(view) => view.handle_event(event_with_count, key_event),
            View::CreateTag(view) => view.handle_event(event_with_count, key_event),
            View::DeleteTag(view) => view.handle_event(event_with_count, key_event),
            View::DeleteRef(view) => view.handle_event(event_with_count, key_event),
            View::Help(view) => view.handle_event(event_with_count, key_event),
            View::GitHub(view) => view.handle_event(event_with_count, key_event),
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        match self {
            View::Default => {}
            View::List(view) => view.render(f, area),
            View::Detail(view) => view.render(f, area),
            View::UserCommand(view) => view.render(f, area),
            View::Refs(view) => view.render(f, area),
            View::CreateTag(view) => view.render(f, area),
            View::DeleteTag(view) => view.render(f, area),
            View::DeleteRef(view) => view.render(f, area),
            View::Help(view) => view.render(f, area),
            View::GitHub(view) => view.render(f, area),
        }
    }

    pub fn take_graph_clear(&mut self) -> bool {
        match self {
            View::List(view) => view.take_graph_clear(),
            View::Detail(view) => view.take_graph_clear(),
            _ => false,
        }
    }

    pub fn of_list(
        commit_list_state: CommitListState<'a>,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> Self {
        View::List(Box::new(ListView::new(commit_list_state, ctx, tx)))
    }

    pub fn of_detail(
        commit_list_state: CommitListState<'a>,
        commit: Commit,
        changes: Vec<FileChange>,
        refs: Vec<Ref>,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> Self {
        View::Detail(Box::new(DetailView::new(
            commit_list_state,
            commit,
            changes,
            refs,
            ctx,
            tx,
        )))
    }

    pub fn of_working_changes_detail(
        commit_list_state: CommitListState<'a>,
        working_changes: WorkingChanges,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> Self {
        View::Detail(Box::new(DetailView::new_working_changes(
            commit_list_state,
            working_changes,
            ctx,
            tx,
        )))
    }

    pub fn of_user_command(
        commit_list_state: CommitListState<'a>,
        params: ExternalCommandParameters,
        user_command_number: usize,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> Self {
        View::UserCommand(Box::new(UserCommandView::new(
            commit_list_state,
            params,
            user_command_number,
            ctx,
            tx,
        )))
    }

    pub fn of_refs(
        commit_list_state: CommitListState<'a>,
        refs: Vec<Ref>,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> Self {
        View::Refs(Box::new(RefsView::new(commit_list_state, refs, ctx, tx)))
    }

    pub fn of_refs_with_state(
        commit_list_state: CommitListState<'a>,
        ref_list_state: RefListState,
        refs: Vec<Ref>,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> Self {
        View::Refs(Box::new(RefsView::with_state(
            commit_list_state,
            ref_list_state,
            refs,
            ctx,
            tx,
        )))
    }

    pub fn of_create_tag(
        commit_list_state: CommitListState<'a>,
        commit_hash: CommitHash,
        repo_path: PathBuf,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> Self {
        View::CreateTag(Box::new(CreateTagView::new(
            commit_list_state,
            commit_hash,
            repo_path,
            ctx,
            tx,
        )))
    }

    pub fn of_delete_tag(
        commit_list_state: CommitListState<'a>,
        commit_hash: CommitHash,
        tags: Vec<Ref>,
        repo_path: PathBuf,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> Self {
        View::DeleteTag(Box::new(DeleteTagView::new(
            commit_list_state,
            commit_hash,
            tags,
            repo_path,
            ctx,
            tx,
        )))
    }

    pub fn of_delete_ref(
        commit_list_state: CommitListState<'a>,
        ref_list_state: RefListState,
        refs: Vec<Ref>,
        repo_path: PathBuf,
        ref_name: String,
        ref_type: RefType,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> Self {
        View::DeleteRef(Box::new(DeleteRefView::new(
            commit_list_state,
            ref_list_state,
            refs,
            repo_path,
            ref_name,
            ref_type,
            ctx,
            tx,
        )))
    }

    pub fn of_help(before: View<'a>, ctx: Rc<AppContext>, tx: Sender) -> Self {
        View::Help(Box::new(HelpView::new(before, ctx, tx)))
    }

    pub fn of_github(
        before: View<'a>,
        issues: Vec<crate::github::GhIssue>,
        pull_requests: Vec<crate::github::GhPullRequest>,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> Self {
        View::GitHub(Box::new(GitHubView::new(
            before,
            issues,
            pull_requests,
            ctx,
            tx,
        )))
    }

    pub fn refresh(&mut self) {
        match self {
            View::Default => {}
            View::List(view) => view.refresh(),
            View::Detail(view) => view.refresh(),
            View::UserCommand(view) => view.refresh(),
            View::Refs(view) => view.refresh(),
            View::CreateTag(view) => view.refresh(),
            View::DeleteTag(view) => view.refresh(),
            View::DeleteRef(view) => view.refresh(),
            View::Help(_) => {}
            View::GitHub(_) => {}
        }
    }
}

#[derive(Debug, Clone)]
pub enum RefreshViewContext {
    List {
        list_context: ListRefreshViewContext,
    },
    Detail {
        list_context: ListRefreshViewContext,
    },
    UserCommand {
        list_context: ListRefreshViewContext,
        user_command_context: UserCommandRefreshViewContext,
    },
    Refs {
        list_context: ListRefreshViewContext,
        refs_context: RefsRefreshViewContext,
    },
}

impl RefreshViewContext {
    pub fn list_context(&self) -> &ListRefreshViewContext {
        match self {
            RefreshViewContext::List { list_context }
            | RefreshViewContext::Detail { list_context }
            | RefreshViewContext::UserCommand { list_context, .. }
            | RefreshViewContext::Refs { list_context, .. } => list_context,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ListRefreshViewContext {
    pub commit_hash: CommitHash,
    pub selected: usize,
    pub height: usize,
    pub scroll_to_top: bool,
}

impl From<&CommitListState<'_>> for ListRefreshViewContext {
    fn from(list_state: &CommitListState<'_>) -> Self {
        let commit_hash = list_state.selected_commit_hash().clone();
        let (selected, offset, height) = list_state.current_list_status();
        // If the selected commit is the top one and there is no offset, it means the list is already scrolled to the top.
        // In this case, we set scroll_to_top to true to indicate that the view should be scrolled to the top after refresh.
        let scroll_to_top = selected == 0 && offset == 0;
        ListRefreshViewContext {
            commit_hash,
            selected,
            height,
            scroll_to_top,
        }
    }
}

pub fn send_refresh(list_state: Option<&CommitListState<'_>>, tx: &Sender) {
    if let Some(list_state) = list_state {
        let list_context = ListRefreshViewContext::from(list_state);
        let context = RefreshViewContext::List { list_context };
        tx.send(AppEvent::Clear);
        tx.send(AppEvent::Refresh(context));
    }
}

#[derive(Debug, Clone)]
pub struct UserCommandRefreshViewContext {
    pub n: usize,
}

#[derive(Debug, Clone)]
pub struct RefsRefreshViewContext {
    pub selected: Vec<String>,
    pub opened: Vec<Vec<String>>,
}

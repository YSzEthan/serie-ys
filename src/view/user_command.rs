use std::rc::Rc;

use ratatui::{
    crossterm::event::KeyEvent,
    layout::{Constraint, Layout, Rect},
    text::Line,
    Frame,
};

use crate::{
    app::AppContext,
    event::{AppEvent, Sender, UserEvent, UserEventWithCount},
    git::{Commit, CommitHash, Ref, Repository},
    view::{ansi_output_to_lines, ListRefreshViewContext, RefreshViewContext, ViewContext},
    widget::{
        commit_list::{ChildJump, CommitList, CommitListState},
        h,
        output_pane::{OutputPane, OutputPaneState},
        HintSpec,
    },
};

/// 狀態列提示。優先序＝截斷時從尾端開始丟。
pub fn status_hints() -> Vec<HintSpec> {
    vec![
        h(&[UserEvent::NavigateDown, UserEvent::NavigateUp], "scroll"),
        h(&[UserEvent::HalfPageDown], "half"),
        h(&[UserEvent::PageDown], "page"),
        h(&[UserEvent::GoToTop], "top"),
        h(&[UserEvent::GoToBottom], "bottom"),
        h(
            &[UserEvent::GoToParent, UserEvent::GoToChild],
            "parent/child",
        ),
        h(&[UserEvent::Confirm], "detail"),
        h(&[UserEvent::Refresh], "refresh"),
        h(&[UserEvent::HelpToggle], "help"),
        h(&[UserEvent::Cancel], "close"),
    ]
}

type ExecCommandFn = fn(&Commit, &[Ref], usize, Rect, &AppContext) -> Result<String, String>;

#[derive(Debug)]
pub struct UserCommandView<'a> {
    commit_list_state: Option<CommitListState<'a>>,
    output_pane_state: OutputPaneState,

    user_command_number: usize,
    user_command_output_lines: Vec<Line<'static>>,

    ctx: Rc<AppContext>,
    tx: Sender,
}

impl<'a> UserCommandView<'a> {
    pub fn new(
        commit_list_state: CommitListState<'a>,
        command_output: String,
        user_command_number: usize,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> UserCommandView<'a> {
        let user_command_output_lines =
            build_user_command_output_lines(command_output, ctx.clone()).unwrap_or_else(|err| {
                tx.send(AppEvent::NotifyError(err));
                vec![]
            });

        UserCommandView {
            commit_list_state: Some(commit_list_state),
            output_pane_state: OutputPaneState::default(),
            user_command_number,
            user_command_output_lines,
            ctx,
            tx,
        }
    }

    pub fn handle_event(&mut self, event_with_count: UserEventWithCount, _: KeyEvent) {
        let event = event_with_count.event;
        let count = event_with_count.count;

        match event {
            UserEvent::NavigateDown | UserEvent::SelectDown => {
                for _ in 0..count {
                    self.output_pane_state.scroll_down();
                }
            }
            UserEvent::NavigateUp | UserEvent::SelectUp => {
                for _ in 0..count {
                    self.output_pane_state.scroll_up();
                }
            }
            UserEvent::PageDown => {
                for _ in 0..count {
                    self.output_pane_state.scroll_page_down();
                }
            }
            UserEvent::PageUp => {
                for _ in 0..count {
                    self.output_pane_state.scroll_page_up();
                }
            }
            UserEvent::HalfPageDown => {
                for _ in 0..count {
                    self.output_pane_state.scroll_half_page_down();
                }
            }
            UserEvent::HalfPageUp => {
                for _ in 0..count {
                    self.output_pane_state.scroll_half_page_up();
                }
            }
            UserEvent::GoToTop => {
                self.output_pane_state.select_first();
            }
            UserEvent::GoToBottom => {
                self.output_pane_state.select_last();
            }
            UserEvent::GoToParent => {
                self.tx.send(AppEvent::SelectParentCommit);
            }
            UserEvent::GoToChild => {
                self.tx.send(AppEvent::SelectChildCommit);
            }
            UserEvent::HelpToggle => {
                self.tx.send(AppEvent::OpenHelp);
            }
            UserEvent::UserCommand(n) => {
                if n == self.user_command_number {
                    self.tx.send(AppEvent::CloseUserCommand);
                } else {
                    // 切換到另一個 user command
                    self.tx.send(AppEvent::OpenUserCommand(n));
                }
            }
            UserEvent::Confirm => {
                self.tx.send(AppEvent::OpenDetail);
            }
            UserEvent::Cancel | UserEvent::Close => {
                self.tx.send(AppEvent::CloseUserCommand);
            }
            UserEvent::Refresh => {
                self.refresh();
            }
            _ => {}
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        let user_command_height =
            (area.height - 1).min(self.ctx.ui_config.pane_height.user_command);
        let [list_area, user_command_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(user_command_height)])
                .areas(area);

        let commit_list = CommitList::new(self.ctx.clone(), 0);
        f.render_stateful_widget(commit_list, list_area, self.as_mut_list_state());

        let output_pane = OutputPane::new(&self.user_command_output_lines, self.ctx.clone());
        f.render_stateful_widget(output_pane, user_command_area, &mut self.output_pane_state);
    }
}

impl<'a> UserCommandView<'a> {
    pub fn take_list_state(&mut self) -> Option<CommitListState<'a>> {
        self.commit_list_state.take()
    }

    pub fn as_list_state(&self) -> &CommitListState<'a> {
        self.commit_list_state.as_ref().unwrap()
    }

    pub fn as_mut_list_state(&mut self) -> &mut CommitListState<'a> {
        self.commit_list_state
            .as_mut()
            .expect("commit_list_state already taken")
    }

    pub fn select_older_commit(
        &mut self,
        repository: &Repository,
        view_area: Rect,
        exec_command: ExecCommandFn,
    ) {
        self.update_selected_commit(repository, view_area, exec_command, |state| {
            state.select_next()
        });
    }

    pub fn select_newer_commit(
        &mut self,
        repository: &Repository,
        view_area: Rect,
        exec_command: ExecCommandFn,
    ) {
        self.update_selected_commit(repository, view_area, exec_command, |state| {
            state.select_prev()
        });
    }

    pub fn select_parent_commit(
        &mut self,
        repository: &Repository,
        view_area: Rect,
        exec_command: ExecCommandFn,
    ) {
        self.update_selected_commit(repository, view_area, exec_command, |state| {
            state.select_parent()
        });
    }

    pub fn select_child_commit(
        &mut self,
        repository: &Repository,
        view_area: Rect,
        exec_command: ExecCommandFn,
    ) {
        let Some(state) = self.commit_list_state.as_mut() else {
            return;
        };
        match state.select_child() {
            ChildJump::Ambiguous(options) => {
                self.tx.send(AppEvent::OpenChildPicker { options });
            }
            // Ambiguous 時故意不呼叫 update_selected_commit：那條路徑會重跑
            // exec_command（spawn 外部行程）並 output_pane_state.select_first()，
            // selection 根本沒動卻把使用者在 output pane 的捲動位置洗掉。
            ChildJump::Jumped | ChildJump::None => {
                self.update_selected_commit(repository, view_area, exec_command, |_| {});
            }
        }
    }

    /// child picker 選定候選後跳過去並重刷 output pane。
    pub fn select_commit_by_hash(
        &mut self,
        repository: &Repository,
        view_area: Rect,
        exec_command: ExecCommandFn,
        hash: &CommitHash,
    ) {
        self.update_selected_commit(repository, view_area, exec_command, |state| {
            state.step_to_commit_hash(hash)
        });
    }

    fn update_selected_commit<F>(
        &mut self,
        repository: &Repository,
        view_area: Rect,
        exec_command: ExecCommandFn,
        update_commit_list_state: F,
    ) where
        F: FnOnce(&mut CommitListState<'a>),
    {
        let Some(commit_list_state) = self.commit_list_state.as_mut() else {
            return;
        };
        update_commit_list_state(commit_list_state);

        let selected = commit_list_state.selected_commit_hash().clone();
        let (commit, _) = repository.commit_detail(&selected);
        let refs: Vec<Ref> = repository.refs(&selected).into_iter().cloned().collect();
        self.user_command_output_lines = exec_command(
            commit,
            &refs,
            self.user_command_number,
            view_area,
            &self.ctx,
        )
        .and_then(|output| build_user_command_output_lines(output, self.ctx.clone()))
        .unwrap_or_else(|err| {
            self.tx.send(AppEvent::NotifyError(err));
            vec![]
        });

        self.output_pane_state.select_first();
    }

    pub fn refresh(&self) {
        let list_context = ListRefreshViewContext::from(self.as_list_state());
        let context = RefreshViewContext::new(
            list_context,
            ViewContext::UserCommand {
                n: self.user_command_number,
            },
        );
        self.tx.send(AppEvent::Refresh(context));
    }
}

fn build_user_command_output_lines(
    command_output: String,
    ctx: Rc<AppContext>,
) -> Result<Vec<Line<'static>>, String> {
    ansi_output_to_lines(command_output, ctx.core_config.user_command.tab_width)
}

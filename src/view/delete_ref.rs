use std::{path::PathBuf, rc::Rc, thread};

use ratatui::{
    crossterm::event::KeyEvent,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Padding, Paragraph},
    Frame,
};

use crate::{
    app::AppContext,
    event::{AppEvent, Sender, UserEvent, UserEventWithCount},
    git::{
        delete_branch, delete_branch_force, delete_remote_branch, delete_remote_tag, delete_tag,
        CommitHash, Ref, RefType,
    },
    view::{ListRefreshViewContext, RefreshViewContext, RefsOrigin},
    widget::{
        commit_list::{CommitList, CommitListState},
        ref_list::RefListState,
    },
};

#[derive(Debug)]
pub struct DeleteRefView<'a> {
    commit_list_state: Option<CommitListState<'a>>,
    ref_list_state: RefListState,
    refs: Vec<Ref>,
    repo_path: PathBuf,

    ref_name: String,
    ref_type: RefType,
    delete_from_remote: bool,
    force_delete: bool,

    // 純 passthrough：DeleteRef 本身不關心 Refs 從哪來，
    // 只是為了 close_delete_ref 時把原 origin 還回 RefsView。
    // TODO: DeleteRef 長遠應 demotion 成 RefsView 的 modal dialog state，
    // 就不需要這個欄位。
    refs_origin: RefsOrigin,

    ctx: Rc<AppContext>,
    tx: Sender,
}

impl<'a> DeleteRefView<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        commit_list_state: CommitListState<'a>,
        ref_list_state: RefListState,
        refs: Vec<Ref>,
        repo_path: PathBuf,
        ref_name: String,
        ref_type: RefType,
        refs_origin: RefsOrigin,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> DeleteRefView<'a> {
        DeleteRefView {
            commit_list_state: Some(commit_list_state),
            ref_list_state,
            refs,
            repo_path,
            ref_name,
            ref_type,
            delete_from_remote: ref_type == RefType::RemoteBranch,
            force_delete: false,
            refs_origin,
            ctx,
            tx,
        }
    }

    pub fn refs_origin(&self) -> RefsOrigin {
        self.refs_origin
    }

    pub fn handle_event(&mut self, event_with_count: UserEventWithCount, _key: KeyEvent) {
        let event = event_with_count.event;

        match event {
            UserEvent::Cancel => {
                self.tx.send(AppEvent::CloseDeleteRef);
            }
            UserEvent::Confirm => {
                self.delete_ref();
            }
            UserEvent::NavigateRight | UserEvent::NavigateLeft | UserEvent::NavigateDown => {
                match self.ref_type {
                    RefType::Tag => {
                        self.delete_from_remote = !self.delete_from_remote;
                    }
                    RefType::Branch => {
                        self.force_delete = !self.force_delete;
                    }
                    RefType::RemoteBranch => {}
                }
            }
            _ => {}
        }
    }

    fn delete_ref(&mut self) {
        let ref_name = self.ref_name.clone();
        let ref_type = self.ref_type;
        let repo_path = self.repo_path.clone();
        let delete_from_remote = self.delete_from_remote;
        let force_delete = self.force_delete;
        let tx = self.tx.clone();

        // Build refresh context before closing
        let list_context = self
            .commit_list_state
            .as_ref()
            .map(ListRefreshViewContext::from)
            .unwrap_or(ListRefreshViewContext {
                commit_hash: CommitHash::default(),
                selected: 0,
                height: 0,
                scroll_to_top: true,
                show_remote_refs: true,
            });

        let pending_msg = match ref_type {
            RefType::Tag => {
                if delete_from_remote {
                    format!("Deleting tag '{ref_name}' from local and remote...")
                } else {
                    format!("Deleting tag '{ref_name}'...")
                }
            }
            RefType::Branch => {
                if force_delete {
                    format!("Force deleting branch '{ref_name}'...")
                } else {
                    format!("Deleting branch '{ref_name}'...")
                }
            }
            RefType::RemoteBranch => {
                format!("Deleting remote branch '{ref_name}'...")
            }
        };

        self.tx.send(AppEvent::ShowPendingOverlay {
            message: pending_msg,
        });
        self.tx.send(AppEvent::CloseDeleteRef);

        thread::spawn(move || {
            // Track if local deletion succeeded (for UI update even if remote fails)
            let mut local_deleted = false;

            let result = match ref_type {
                RefType::Tag => {
                    if let Err(e) = delete_tag(&repo_path, &ref_name) {
                        Err(e)
                    } else {
                        local_deleted = true;
                        if delete_from_remote {
                            delete_remote_tag(&repo_path, &ref_name).map_err(|e| {
                                format!("Local tag deleted, but failed to delete from remote: {e}")
                            })
                        } else {
                            Ok(())
                        }
                    }
                }
                RefType::Branch => {
                    let res = if force_delete {
                        delete_branch_force(&repo_path, &ref_name)
                    } else {
                        delete_branch(&repo_path, &ref_name)
                    };
                    if res.is_ok() {
                        local_deleted = true;
                    }
                    res
                }
                RefType::RemoteBranch => delete_remote_branch(&repo_path, &ref_name),
            };

            match result {
                Ok(()) => {
                    let msg = match ref_type {
                        RefType::Tag => {
                            if delete_from_remote {
                                format!("Tag '{ref_name}' deleted from local and remote")
                            } else {
                                format!("Tag '{ref_name}' deleted locally")
                            }
                        }
                        RefType::Branch => {
                            format!("Branch '{ref_name}' deleted")
                        }
                        RefType::RemoteBranch => {
                            format!("Remote branch '{ref_name}' deleted")
                        }
                    };
                    tx.send(AppEvent::NotifySuccess(msg));
                    tx.send(AppEvent::HidePendingOverlay);
                    tx.send(AppEvent::Refresh(RefreshViewContext::List {
                        list_context: list_context.clone(),
                    }));
                }
                Err(e) => {
                    // If local deletion succeeded, still refresh UI
                    if local_deleted {
                        tx.send(AppEvent::Refresh(RefreshViewContext::List {
                            list_context: list_context.clone(),
                        }));
                    }
                    tx.send(AppEvent::HidePendingOverlay);
                    tx.send(AppEvent::NotifyError(e));
                }
            }
        });
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        let Some(list_state) = self.commit_list_state.as_mut() else {
            return;
        };

        let graph_width = list_state.graph_area_cell_width() + 1;
        let refs_width =
            (area.width.saturating_sub(graph_width)).min(self.ctx.ui_config.refs.width);

        let [list_area, refs_area] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(refs_width)]).areas(area);

        let commit_list = CommitList::new(self.ctx.clone(), 0);
        f.render_stateful_widget(commit_list, list_area, list_state);

        let ref_list = crate::widget::ref_list::RefList::new(&self.refs, self.ctx.clone());
        f.render_stateful_widget(ref_list, refs_area, &mut self.ref_list_state);

        let dialog_width = 50u16.min(area.width.saturating_sub(4));
        let dialog_height = 6u16.min(area.height.saturating_sub(2));

        let dialog_x = (area.width.saturating_sub(dialog_width)) / 2;
        let dialog_y = (area.height.saturating_sub(dialog_height)) / 2;

        let dialog_area = Rect::new(
            area.x + dialog_x,
            area.y + dialog_y,
            dialog_width,
            dialog_height,
        );

        f.render_widget(Clear, dialog_area);

        let title = match self.ref_type {
            RefType::Tag => " Delete Tag ",
            RefType::Branch => " Delete Branch ",
            RefType::RemoteBranch => " Delete Remote Branch ",
        };

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.ctx.color_theme.divider_fg))
            .style(
                Style::default()
                    .bg(self.ctx.color_theme.bg)
                    .fg(self.ctx.color_theme.fg),
            )
            .padding(Padding::horizontal(1));

        let inner_area = block.inner(dialog_area);
        f.render_widget(block, dialog_area);

        let [name_area, checkbox_area, hint_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Min(1),
        ])
        .areas(inner_area);

        let name_line = Line::from(vec![Span::raw(&self.ref_name)
            .fg(self.ctx.color_theme.fg)
            .add_modifier(Modifier::BOLD)]);
        f.render_widget(Paragraph::new(name_line), name_area);

        let checkbox_line = match self.ref_type {
            RefType::Tag => {
                let checkbox = if self.delete_from_remote {
                    "[x]"
                } else {
                    "[ ]"
                };
                Line::from(vec![
                    Span::styled(checkbox, Style::default().fg(self.ctx.color_theme.fg)),
                    Span::raw(" Delete from origin").fg(self.ctx.color_theme.fg),
                ])
            }
            RefType::Branch => {
                let checkbox = if self.force_delete { "[x]" } else { "[ ]" };
                Line::from(vec![
                    Span::styled(checkbox, Style::default().fg(self.ctx.color_theme.fg)),
                    Span::raw(" Force delete (-D)").fg(self.ctx.color_theme.fg),
                ])
            }
            RefType::RemoteBranch => Line::from(vec![Span::raw("").fg(self.ctx.color_theme.fg)]),
        };
        f.render_widget(Paragraph::new(checkbox_line), checkbox_area);

        let hint_line = crate::widget::build_hint_line(
            &self.ctx.color_theme,
            &[("Enter/y", "delete"), ("Esc/n", "cancel"), ("←→", "toggle")],
        );
        f.render_widget(Paragraph::new(hint_line).centered(), hint_area);
    }
}

impl<'a> DeleteRefView<'a> {
    pub fn take_list_state(&mut self) -> Option<CommitListState<'a>> {
        self.commit_list_state.take()
    }

    pub fn take_ref_list_state(&mut self) -> RefListState {
        std::mem::take(&mut self.ref_list_state)
    }

    pub fn take_refs(&mut self) -> Vec<Ref> {
        std::mem::take(&mut self.refs)
    }

    pub fn refresh(&self) {
        super::views::send_refresh(self.commit_list_state.as_ref(), &self.tx);
    }
}

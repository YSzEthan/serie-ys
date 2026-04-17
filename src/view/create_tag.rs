use std::{path::PathBuf, rc::Rc, thread};

use ratatui::{
    crossterm::event::{Event, KeyEvent},
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Padding, Paragraph},
    Frame,
};
use tui_input::{backend::crossterm::EventHandler, Input};

use crate::{
    app::AppContext,
    event::{AppEvent, Sender, UserEvent, UserEventWithCount},
    git::{create_tag, push_tag, CommitHash},
    view::{ListRefreshViewContext, RefreshViewContext},
    widget::commit_list::{CommitList, CommitListState},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusedField {
    TagName,
    Message,
    PushCheckbox,
}

#[derive(Debug)]
pub struct CreateTagView<'a> {
    commit_list_state: Option<CommitListState<'a>>,
    commit_hash: CommitHash,
    repo_path: PathBuf,

    tag_name_input: Input,
    tag_message_input: Input,
    push_to_remote: bool,
    focused_field: FocusedField,

    ctx: Rc<AppContext>,
    tx: Sender,
}

impl<'a> CreateTagView<'a> {
    pub fn new(
        commit_list_state: CommitListState<'a>,
        commit_hash: CommitHash,
        repo_path: PathBuf,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> CreateTagView<'a> {
        CreateTagView {
            commit_list_state: Some(commit_list_state),
            commit_hash,
            repo_path,
            tag_name_input: Input::default(),
            tag_message_input: Input::default(),
            push_to_remote: true,
            focused_field: FocusedField::TagName,
            ctx,
            tx,
        }
    }

    pub fn handle_event(&mut self, event_with_count: UserEventWithCount, key: KeyEvent) {
        use ratatui::crossterm::event::KeyCode;

        // Handle Tab for focus switching (before UserEvent processing)
        if key.code == KeyCode::Tab {
            self.focus_next();
            return;
        }
        if key.code == KeyCode::BackTab {
            self.focus_prev();
            return;
        }

        // Handle Backspace for input (don't close dialog)
        if key.code == KeyCode::Backspace {
            self.handle_input(key);
            return;
        }

        // In text fields, y/n are text input, not confirm/cancel
        if matches!(
            self.focused_field,
            FocusedField::TagName | FocusedField::Message
        ) {
            if let KeyCode::Char('y') | KeyCode::Char('n') = key.code {
                self.handle_input(key);
                return;
            }
        }

        let event = event_with_count.event;

        match event {
            UserEvent::Cancel => {
                self.tx.send(AppEvent::CloseCreateTag);
            }
            UserEvent::Confirm => {
                self.submit();
            }
            UserEvent::NavigateDown => {
                self.focus_next();
            }
            UserEvent::NavigateUp => {
                self.focus_prev();
            }
            UserEvent::NavigateRight | UserEvent::NavigateLeft => {
                if self.focused_field == FocusedField::PushCheckbox {
                    self.push_to_remote = !self.push_to_remote;
                } else {
                    self.handle_input(key);
                }
            }
            _ => {
                self.handle_input(key);
            }
        }
    }

    fn handle_input(&mut self, key: KeyEvent) {
        match self.focused_field {
            FocusedField::TagName => {
                self.tag_name_input.handle_event(&Event::Key(key));
            }
            FocusedField::Message => {
                self.tag_message_input.handle_event(&Event::Key(key));
            }
            FocusedField::PushCheckbox => {
                if key.code == ratatui::crossterm::event::KeyCode::Char(' ') {
                    self.push_to_remote = !self.push_to_remote;
                }
            }
        }
    }

    fn focus_next(&mut self) {
        self.focused_field = match self.focused_field {
            FocusedField::TagName => FocusedField::Message,
            FocusedField::Message => FocusedField::PushCheckbox,
            FocusedField::PushCheckbox => FocusedField::TagName,
        };
    }

    fn focus_prev(&mut self) {
        self.focused_field = match self.focused_field {
            FocusedField::TagName => FocusedField::PushCheckbox,
            FocusedField::Message => FocusedField::TagName,
            FocusedField::PushCheckbox => FocusedField::Message,
        };
    }

    fn submit(&mut self) {
        let tag_name = self.tag_name_input.value().trim();
        if tag_name.is_empty() {
            self.tx
                .send(AppEvent::NotifyError("Tag name cannot be empty".into()));
            return;
        }

        let message = self.tag_message_input.value().trim();
        let message: Option<String> = if message.is_empty() {
            None
        } else {
            Some(message.to_string())
        };

        // Prepare data for background thread
        let repo_path = self.repo_path.clone();
        let tag_name = tag_name.to_string();
        let commit_hash = self.commit_hash.clone();
        let push_to_remote = self.push_to_remote;
        let tx = self.tx.clone();

        // Build refresh context before closing
        let list_context = self
            .commit_list_state
            .as_ref()
            .map(ListRefreshViewContext::from)
            .unwrap_or(ListRefreshViewContext {
                commit_hash: commit_hash.clone(),
                selected: 0,
                height: 0,
                scroll_to_top: false,
                show_remote_refs: true,
            });

        // Show pending overlay and close dialog
        let pending_msg = if push_to_remote {
            format!("Creating and pushing tag '{tag_name}'...")
        } else {
            format!("Creating tag '{tag_name}'...")
        };
        self.tx.send(AppEvent::ShowPendingOverlay {
            message: pending_msg,
        });
        self.tx.send(AppEvent::CloseCreateTag);

        // Run git commands in background
        thread::spawn(move || {
            if let Err(e) = create_tag(&repo_path, &tag_name, &commit_hash, message.as_deref()) {
                tx.send(AppEvent::HidePendingOverlay);
                tx.send(AppEvent::NotifyError(e));
                return;
            }

            if push_to_remote {
                if let Err(e) = push_tag(&repo_path, &tag_name) {
                    tx.send(AppEvent::HidePendingOverlay);
                    tx.send(AppEvent::NotifyError(format!(
                        "Tag created locally, but push failed: {e}"
                    )));
                    // Still refresh to show locally created tag
                    tx.send(AppEvent::Refresh(RefreshViewContext::List { list_context }));
                    return;
                }
            }

            // Success
            let msg = if push_to_remote {
                format!("Tag '{tag_name}' created and pushed to origin")
            } else {
                format!("Tag '{tag_name}' created")
            };
            tx.send(AppEvent::NotifySuccess(msg));
            tx.send(AppEvent::HidePendingOverlay);
            tx.send(AppEvent::Refresh(RefreshViewContext::List { list_context }));
        });
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        let Some(list_state) = self.commit_list_state.as_mut() else {
            return;
        };

        // Render commit list in background
        let commit_list = CommitList::new(self.ctx.clone(), 0);
        f.render_stateful_widget(commit_list, area, list_state);

        // Dialog dimensions
        let dialog_width = 50u16.min(area.width.saturating_sub(4));
        let dialog_height = 10u16.min(area.height.saturating_sub(2));

        let dialog_x = (area.width.saturating_sub(dialog_width)) / 2;
        let dialog_y = (area.height.saturating_sub(dialog_height)) / 2;

        let dialog_area = Rect::new(
            area.x + dialog_x,
            area.y + dialog_y,
            dialog_width,
            dialog_height,
        );

        f.render_widget(Clear, dialog_area);

        let block = Block::default()
            .title(" Create Tag ")
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

        let [commit_area, tag_name_area, message_area, push_area, hint_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .areas(inner_area);

        // Commit hash
        let commit_line = Line::from(vec![
            Span::raw("Commit: ").fg(self.ctx.color_theme.fg),
            Span::raw(self.commit_hash.as_short_hash()).fg(self.ctx.color_theme.list_hash_fg),
        ]);
        f.render_widget(Paragraph::new(commit_line), commit_area);

        // Tag name input
        let tag_input_area = self.render_input_field(
            f,
            tag_name_area,
            "Tag name:",
            self.tag_name_input.value(),
            FocusedField::TagName,
        );

        // Message input
        let msg_input_area = self.render_input_field(
            f,
            message_area,
            "Message:",
            self.tag_message_input.value(),
            FocusedField::Message,
        );

        // Push checkbox
        let checkbox = if self.push_to_remote { "[x]" } else { "[ ]" };
        let checkbox_style = if self.focused_field == FocusedField::PushCheckbox {
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(self.ctx.color_theme.status_success_fg)
        } else {
            Style::default().fg(self.ctx.color_theme.fg)
        };
        let push_line = Line::from(vec![
            Span::styled(checkbox, checkbox_style),
            Span::raw(" Push to origin").fg(self.ctx.color_theme.fg),
        ]);
        f.render_widget(Paragraph::new(push_line), push_area);

        // Hints
        let hint_line = Line::from(vec![
            Span::raw("Enter/y").fg(self.ctx.color_theme.help_key_fg),
            Span::raw(" submit  ").fg(self.ctx.color_theme.fg),
            Span::raw("Esc/n").fg(self.ctx.color_theme.help_key_fg),
            Span::raw(" cancel  ").fg(self.ctx.color_theme.fg),
            Span::raw("Tab/↑↓").fg(self.ctx.color_theme.help_key_fg),
            Span::raw(" nav").fg(self.ctx.color_theme.fg),
        ]);
        f.render_widget(Paragraph::new(hint_line).centered(), hint_area);

        // Cursor positioning
        if self.focused_field == FocusedField::TagName {
            let cursor_x = tag_input_area.x + 1 + self.tag_name_input.visual_cursor() as u16;
            f.set_cursor_position((
                cursor_x.min(tag_input_area.right().saturating_sub(1)),
                tag_input_area.y,
            ));
        } else if self.focused_field == FocusedField::Message {
            let cursor_x = msg_input_area.x + 1 + self.tag_message_input.visual_cursor() as u16;
            f.set_cursor_position((
                cursor_x.min(msg_input_area.right().saturating_sub(1)),
                msg_input_area.y,
            ));
        }
    }

    fn render_input_field(
        &self,
        f: &mut Frame,
        area: Rect,
        label: &str,
        value: &str,
        field: FocusedField,
    ) -> Rect {
        let is_focused = self.focused_field == field;
        let label_style = if is_focused {
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(self.ctx.color_theme.status_success_fg)
        } else {
            Style::default().fg(self.ctx.color_theme.fg)
        };

        let [label_area, input_area] =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);

        f.render_widget(
            Paragraph::new(Line::from(Span::styled(label, label_style))),
            label_area,
        );

        let input_style = if is_focused {
            Style::default().bg(self.ctx.color_theme.list_selected_bg)
        } else {
            Style::default()
        };

        let max_width = input_area.width.saturating_sub(2) as usize;
        let char_count = value.chars().count();
        let display_value: String = if char_count > max_width {
            value.chars().skip(char_count - max_width).collect()
        } else {
            value.to_string()
        };

        f.render_widget(
            Paragraph::new(Line::from(Span::raw(format!(" {display_value}")))).style(input_style),
            input_area,
        );

        input_area
    }
}

impl<'a> CreateTagView<'a> {
    pub fn take_list_state(&mut self) -> Option<CommitListState<'a>> {
        self.commit_list_state.take()
    }

    pub fn refresh(&self) {
        super::views::send_refresh(self.commit_list_state.as_ref(), &self.tx);
    }
}

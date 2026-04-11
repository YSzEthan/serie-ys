use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ansi_to_tui::IntoText as _;
use ratatui::{
    crossterm::event::{KeyCode, KeyEvent},
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Padding, Paragraph},
    DefaultTerminal, Frame,
};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::{
    color::{ColorTheme, GraphColorSet},
    config::{CoreConfig, CursorType, UiConfig, UserCommand, UserCommandType},
    event::{AppEvent, EventController, Sender, UserEvent, UserEventWithCount},
    external::{
        copy_to_clipboard, exec_user_command, exec_user_command_suspend, ExternalCommandParameters,
    },
    git::{Commit, CommitHash, FileChange, Head, Ref, RefType, Repository},
    github::GhItemKind,
    graph::{CellWidthType, Graph, GraphImageManager},
    keybind::KeyBind,
    protocol::ImageProtocol,
    view::{RefreshViewContext, View},
    widget::{
        commit_list::{CommitInfo, CommitListState},
        pending_overlay::PendingOverlay,
    },
    FilteredGraphData,
};

/// Clear terminal image overlays and force a full ratatui redraw.
///
/// - `protocol.clear_line` removes leftover Kitty graphics overlays
///   (no-op on iTerm2, whose images live inside cells).
/// - `terminal.clear()` drops ratatui's backing buffer so the next
///   draw repaints every cell instead of diffing against stale state.
pub(crate) fn clear_image_area(
    protocol: ImageProtocol,
    terminal: &mut DefaultTerminal,
    y_range: std::ops::Range<u16>,
) -> std::io::Result<()> {
    for y in y_range {
        protocol.clear_line(y);
    }
    terminal.clear()
}

#[derive(Debug, Default)]
enum StatusLine {
    #[default]
    None,
    Input(String, Option<u16>, Option<String>),
    NotificationInfo(String),
    NotificationSuccess(String),
    NotificationWarn(String),
    NotificationError(String),
}

#[derive(Clone, Copy)]
pub enum InitialSelection {
    Latest,
    Head,
}

pub enum Ret {
    Quit,
    Refresh(RefreshRequest),
}

pub struct RefreshRequest {
    pub context: RefreshViewContext,
}

#[derive(Debug)]
pub struct AppContext {
    pub keybind: KeyBind,
    pub core_config: CoreConfig,
    pub ui_config: UiConfig,
    pub color_theme: ColorTheme,
    pub image_protocol: ImageProtocol,
}

#[derive(Debug, Default)]
struct AppStatus {
    status_line: StatusLine,
    numeric_prefix: String,
    view_area: Rect,
    last_quit_press: Option<Instant>,
}

#[derive(Debug)]
pub struct App<'a> {
    repository: &'a Repository,
    view: View<'a>,
    app_status: AppStatus,
    pending_message: Option<String>,
    github_cache: Option<GitHubCache>,
    github_timer_cancel: Option<Arc<AtomicBool>>,
    ctx: Rc<AppContext>,
    ec: &'a EventController,
}

#[derive(Debug)]
struct GitHubCache {
    issues: Vec<crate::github::GhIssue>,
    pull_requests: Vec<crate::github::GhPullRequest>,
    issue_details: FxHashMap<u64, Vec<Line<'static>>>,
    pr_details: FxHashMap<u64, Vec<Line<'static>>>,
    state_filter: String,
}

impl<'a> App<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        repository: &'a Repository,
        graph_image_manager: GraphImageManager,
        graph: &Rc<Graph>,
        filtered_graph: Option<FilteredGraphData>,
        remote_only_commits: FxHashSet<CommitHash>,
        graph_color_set: &'a GraphColorSet,
        cell_width_type: CellWidthType,
        initial_selection: InitialSelection,
        ctx: Rc<AppContext>,
        ec: &'a EventController,
        refresh_view_context: Option<RefreshViewContext>,
    ) -> Self {
        let mut ref_name_to_commit_index_map = FxHashMap::default();
        let commits = graph
            .commit_hashes
            .iter()
            .enumerate()
            .map(|(i, commit_hash)| {
                let commit = repository
                    .commit(commit_hash)
                    .expect("commit hash from graph must exist in repository");
                let refs = repository.refs(commit_hash);
                for r in &refs {
                    ref_name_to_commit_index_map.insert(r.name().to_string(), i);
                }
                let (pos_x, _) = graph.commit_pos_map[commit_hash];
                let graph_color = graph_color_set.get(pos_x).to_ratatui_color();
                CommitInfo::new(commit, refs, graph_color)
            })
            .collect();
        let graph_cell_width = match cell_width_type {
            CellWidthType::Double => (graph.max_pos_x + 1) as u16 * 2,
            CellWidthType::Single => (graph.max_pos_x + 1) as u16,
        };

        let (filtered_image_manager, filtered_cell_width, filtered_colors) =
            if let Some(fg) = filtered_graph {
                let colors: FxHashMap<CommitHash, ratatui::style::Color> = fg
                    .graph
                    .commit_hashes
                    .iter()
                    .map(|commit_hash| {
                        let (pos_x, _) = fg.graph.commit_pos_map[commit_hash];
                        let color = graph_color_set.get(pos_x).to_ratatui_color();
                        (commit_hash.clone(), color)
                    })
                    .collect();
                (Some(fg.image_manager), fg.cell_width, Some(colors))
            } else {
                (None, 0, None)
            };

        let head = repository.head().clone();
        let working_changes = repository.working_changes().clone();
        let working_changes_opt = if working_changes.is_empty() {
            None
        } else {
            Some(working_changes)
        };
        let mut commit_list_state = CommitListState::new(
            commits,
            graph_image_manager,
            graph_cell_width,
            head,
            ref_name_to_commit_index_map,
            ctx.core_config.search.ignore_case,
            ctx.core_config.search.fuzzy,
            filtered_image_manager,
            filtered_cell_width,
            filtered_colors,
            remote_only_commits,
            working_changes_opt,
        );
        if let InitialSelection::Head = initial_selection {
            match repository.head() {
                Head::Branch { name } => commit_list_state.select_ref(name),
                Head::Detached { target } => commit_list_state.select_commit_hash(target),
                Head::None => {}
            }
        }
        let view = View::of_list(commit_list_state, ctx.clone(), ec.sender());

        let mut app = Self {
            repository,
            view,
            app_status: AppStatus::default(),
            pending_message: None,
            github_cache: None,
            github_timer_cancel: None,
            ctx,
            ec,
        };

        if let Some(context) = refresh_view_context {
            app.init_with_context(context);
        }

        // 啟動時背景預載 GitHub 資料
        app.prefetch_github();

        app
    }
}

impl App<'_> {
    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<Ret, std::io::Error> {
        loop {
            if self.view.take_graph_clear() {
                clear_image_area(
                    self.ctx.image_protocol,
                    terminal,
                    self.app_status.view_area.top()..self.app_status.view_area.bottom(),
                )?;
            }
            terminal.draw(|f| self.render(f))?;
            match self.ec.recv() {
                AppEvent::Key(key) => {
                    // Handle pending overlay - Esc hides it
                    if self.pending_message.is_some() {
                        if let Some(UserEvent::Cancel) = self.ctx.keybind.get(&key) {
                            self.pending_message = None;
                            self.ec.send(AppEvent::NotifyInfo(
                                "Operation continues in background".into(),
                            ));
                            continue;
                        }
                        // Block other keys while pending
                        continue;
                    }

                    match self.app_status.status_line {
                        StatusLine::None | StatusLine::Input(_, _, _) => {
                            // do nothing
                        }
                        StatusLine::NotificationInfo(_)
                        | StatusLine::NotificationSuccess(_)
                        | StatusLine::NotificationWarn(_) => {
                            // Clear message and pass key input as is
                            self.clear_status_line();
                        }
                        StatusLine::NotificationError(_) => {
                            // Clear message and cancel key input
                            self.clear_status_line();
                            continue;
                        }
                    }

                    let user_event = self.ctx.keybind.get(&key);

                    if let Some(UserEvent::Cancel) = user_event {
                        if !self.app_status.numeric_prefix.is_empty() {
                            // Clear numeric prefix and cancel the event
                            self.app_status.numeric_prefix.clear();
                            continue;
                        }
                    }

                    match user_event {
                        Some(UserEvent::ForceQuit) => {
                            self.ec.send(AppEvent::Quit);
                        }
                        Some(UserEvent::Quit) => {
                            if self.is_input_mode() {
                                self.view.handle_event(
                                    UserEventWithCount::from_event(UserEvent::Unknown),
                                    key,
                                );
                            } else if self
                                .app_status
                                .last_quit_press
                                .is_some_and(|t| t.elapsed() < Duration::from_millis(500))
                            {
                                self.ec.send(AppEvent::Quit);
                                self.app_status.last_quit_press = None;
                                continue;
                            } else {
                                self.app_status.last_quit_press = Some(Instant::now());
                                self.info_notification("Press q again to quit".into());
                                self.ec.sender().send_after(
                                    AppEvent::ClearStatusLine,
                                    Duration::from_millis(600),
                                );
                            }
                            self.app_status.numeric_prefix.clear();
                        }
                        Some(ue) => {
                            self.app_status.last_quit_press = None;
                            let event_with_count =
                                process_numeric_prefix(&self.app_status.numeric_prefix, *ue, key);
                            self.view.handle_event(event_with_count, key);
                            self.app_status.numeric_prefix.clear();
                        }
                        None => {
                            self.app_status.last_quit_press = None;
                            if self.is_input_mode() || matches!(self.view, View::Detail(_)) {
                                self.app_status.numeric_prefix.clear();
                                self.view.handle_event(
                                    UserEventWithCount::from_event(UserEvent::Unknown),
                                    key,
                                );
                            } else if let KeyCode::Char(c) = key.code {
                                if c.is_ascii_digit()
                                    && (c != '0' || !self.app_status.numeric_prefix.is_empty())
                                {
                                    self.app_status.numeric_prefix.push(c);
                                }
                            }
                        }
                    }
                }
                AppEvent::Resize(w, h) => {
                    let _ = (w, h);
                }
                AppEvent::Quit => {
                    return Ok(Ret::Quit);
                }
                AppEvent::OpenDetail => {
                    self.open_detail();
                }
                AppEvent::CloseDetail => {
                    self.close_detail();
                }
                AppEvent::ClearDetail => {
                    self.clear_detail();
                }
                AppEvent::OpenUserCommand(n) => {
                    self.open_user_command(n, Some(terminal));
                }
                AppEvent::CloseUserCommand => {
                    self.close_user_command();
                }
                AppEvent::ClearUserCommand => {
                    self.clear_user_command();
                }
                AppEvent::OpenRefs => {
                    self.open_refs();
                }
                AppEvent::CloseRefs => {
                    self.close_refs();
                }
                AppEvent::OpenCreateTag => {
                    self.open_create_tag();
                }
                AppEvent::CloseCreateTag => {
                    self.close_create_tag();
                }
                AppEvent::OpenDeleteTag => {
                    self.open_delete_tag();
                }
                AppEvent::CloseDeleteTag => {
                    self.close_delete_tag();
                }
                AppEvent::OpenDeleteRef { ref_name, ref_type } => {
                    self.open_delete_ref(ref_name, ref_type);
                }
                AppEvent::CloseDeleteRef => {
                    self.close_delete_ref();
                }
                AppEvent::OpenHelp => {
                    self.open_help();
                }
                AppEvent::CloseHelp => {
                    self.close_help();
                }
                AppEvent::ClearHelp => {
                    self.clear_help();
                }
                AppEvent::OpenGitHub => {
                    self.open_github();
                }
                AppEvent::CloseGitHub => {
                    self.close_github();
                }
                AppEvent::ClearGitHub => {
                    self.clear_github();
                }
                AppEvent::RefreshGitHub { state } => {
                    self.refresh_github(&state);
                }
                AppEvent::GitHubDataLoaded {
                    issues,
                    pull_requests,
                    warnings,
                } => {
                    self.on_github_data_loaded(issues, pull_requests, warnings);
                }
                AppEvent::GitHubFlash { message, is_error } => {
                    if let View::GitHub(ref mut view) = self.view {
                        view.set_flash(message, is_error);
                    }
                }
                AppEvent::GitHubLoadFailed { error } => {
                    if let View::GitHub(ref mut view) = self.view {
                        view.set_error(error);
                    }
                }
                AppEvent::GitHubDetailsLoaded {
                    issue_details,
                    pr_details,
                } => {
                    self.on_github_details_loaded(issue_details, pr_details);
                }
                AppEvent::BatchToggleCheckboxes {
                    number,
                    kind,
                    checkbox_indices,
                } => {
                    self.batch_toggle_checkboxes(number, kind, checkbox_indices);
                }
                AppEvent::CheckboxToggled {
                    number,
                    kind,
                    new_body,
                } => {
                    self.on_checkbox_toggled(number, kind, &new_body);
                }
                AppEvent::LoadDetail { number, kind } => {
                    self.on_load_detail(number, kind);
                }
                AppEvent::DetailFetched {
                    number,
                    kind,
                    rendered,
                } => {
                    self.on_detail_fetched(number, kind, &rendered);
                }
                AppEvent::LoadTaskPanel { number, kind } => {
                    self.on_load_task_panel(number, kind);
                }
                AppEvent::TaskPanelLoaded {
                    number,
                    kind,
                    items,
                } => {
                    if let View::GitHub(ref mut view) = self.view {
                        view.set_task_panel(number, kind, items);
                    }
                }
                AppEvent::SelectOlderCommit => {
                    self.select_older_commit();
                }
                AppEvent::SelectNewerCommit => {
                    self.select_newer_commit();
                }
                AppEvent::SelectParentCommit => {
                    self.select_parent_commit();
                }
                AppEvent::CopyToClipboard { name, value } => {
                    self.copy_to_clipboard(name, value);
                }
                AppEvent::Refresh(context) => {
                    let request = RefreshRequest { context };
                    return Ok(Ret::Refresh(request));
                }
                AppEvent::ClearStatusLine => {
                    self.clear_status_line();
                }
                AppEvent::UpdateStatusInput(msg, cursor_pos, msg_r) => {
                    self.update_status_input(msg, cursor_pos, msg_r);
                }
                AppEvent::NotifyInfo(msg) => {
                    self.info_notification(msg);
                }
                AppEvent::NotifySuccess(msg) => {
                    self.success_notification(msg);
                }
                AppEvent::NotifyWarn(msg) => {
                    self.warn_notification(msg);
                }
                AppEvent::NotifyError(msg) => {
                    self.error_notification(msg);
                }
                AppEvent::ShowPendingOverlay { message } => {
                    self.pending_message = Some(message);
                }
                AppEvent::HidePendingOverlay => {
                    self.pending_message = None;
                }
                AppEvent::FetchAll => {
                    self.fetch_all();
                }
                AppEvent::CheckoutCommit { target } => {
                    self.checkout_commit(target);
                }
                AppEvent::AutoRefresh => {
                    self.ec.clear_pending_refresh();
                    self.view.refresh();
                }
            }
        }
    }

    fn render(&mut self, f: &mut Frame) {
        let base = Block::default()
            .fg(self.ctx.color_theme.fg)
            .bg(self.ctx.color_theme.bg);
        f.render_widget(base, f.area());

        let [view_area, status_line_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(2)]).areas(f.area());

        self.update_state(view_area);

        self.view.render(f, view_area);
        self.render_status_line(f, status_line_area);

        if let Some(message) = &self.pending_message {
            let overlay = PendingOverlay::new(message, &self.ctx.color_theme);
            f.render_widget(overlay, f.area());
        }
    }
}

impl App<'_> {
    fn render_status_line(&self, f: &mut Frame, area: Rect) {
        let text: Line = match &self.app_status.status_line {
            StatusLine::None => {
                if self.app_status.numeric_prefix.is_empty() {
                    self.build_hotkey_hints()
                } else {
                    Line::raw(self.app_status.numeric_prefix.as_str())
                        .fg(self.ctx.color_theme.status_input_transient_fg)
                }
            }
            StatusLine::Input(msg, _, transient_msg) => {
                let msg_w = console::measure_text_width(msg.as_str());
                if let Some(t_msg) = transient_msg {
                    let t_msg_w = console::measure_text_width(t_msg.as_str());
                    let pad_w = area.width as usize - msg_w - t_msg_w - 2 /* pad */;
                    Line::from(vec![
                        msg.as_str().fg(self.ctx.color_theme.status_input_fg),
                        " ".repeat(pad_w).into(),
                        t_msg
                            .as_str()
                            .fg(self.ctx.color_theme.status_input_transient_fg),
                    ])
                } else {
                    Line::raw(msg).fg(self.ctx.color_theme.status_input_fg)
                }
            }
            StatusLine::NotificationInfo(msg) => {
                Line::raw(msg).fg(self.ctx.color_theme.status_info_fg)
            }
            StatusLine::NotificationSuccess(msg) => Line::raw(msg)
                .add_modifier(Modifier::BOLD)
                .fg(self.ctx.color_theme.status_success_fg),
            StatusLine::NotificationWarn(msg) => Line::raw(msg)
                .add_modifier(Modifier::BOLD)
                .fg(self.ctx.color_theme.status_warn_fg),
            StatusLine::NotificationError(msg) => Line::raw(format!("ERROR: {msg}"))
                .add_modifier(Modifier::BOLD)
                .fg(self.ctx.color_theme.status_error_fg),
        };
        let paragraph = Paragraph::new(text).block(
            Block::default()
                .borders(Borders::TOP)
                .style(Style::default().fg(self.ctx.color_theme.divider_fg))
                .padding(Padding::horizontal(1)),
        );
        f.render_widget(paragraph, area);

        if let StatusLine::Input(_, Some(cursor_pos), _) = &self.app_status.status_line {
            let (x, y) = (area.x + cursor_pos + 1, area.y + 1);
            match &self.ctx.ui_config.common.cursor_type {
                CursorType::Native => {
                    f.set_cursor_position((x, y));
                }
                CursorType::Virtual(cursor) => {
                    let style = Style::default().fg(self.ctx.color_theme.virtual_cursor_fg);
                    f.buffer_mut().set_string(x, y, cursor, style);
                }
            }
        }
    }

    fn build_hotkey_hints(&self) -> Line<'static> {
        let hints: Vec<(UserEvent, &str)> = match &self.view {
            View::List(_) => vec![
                (UserEvent::Search, "search"),
                (UserEvent::Filter, "filter"),
                (UserEvent::IgnoreCaseToggle, "case"),
                (UserEvent::CreateTag, "tag"),
                (UserEvent::RefList, "refs"),
                (UserEvent::RemoteRefsToggle, "remote"),
                (UserEvent::GitHubToggle, "github"),
                (UserEvent::Refresh, "refresh"),
                (UserEvent::HelpToggle, "help"),
            ],
            View::Detail(_) => vec![
                (UserEvent::ShortCopy, "copy"),
                (UserEvent::Close, "close"),
                (UserEvent::HelpToggle, "help"),
            ],
            View::Refs(_) => vec![
                (UserEvent::ShortCopy, "copy"),
                (UserEvent::UserCommand(1), "delete"),
                (UserEvent::Close, "close"),
                (UserEvent::HelpToggle, "help"),
            ],
            View::CreateTag(_) | View::DeleteTag(_) | View::DeleteRef(_) => vec![
                (UserEvent::Confirm, "confirm"),
                (UserEvent::Cancel, "cancel"),
            ],
            View::Help(_) => vec![(UserEvent::Close, "close")],
            View::GitHub(ref view) => view.status_hints(),
            _ => vec![],
        };

        let key_fg = self.ctx.color_theme.help_key_fg;
        let desc_fg = self.ctx.color_theme.status_input_transient_fg;

        let mut spans: Vec<Span<'static>> = Vec::new();
        for (i, (event, desc)) in hints.iter().enumerate() {
            if let Some(key) = self.ctx.keybind.keys_for_event(*event).first() {
                if i > 0 {
                    spans.push(Span::raw("  "));
                }
                spans.push(Span::styled(key.clone(), Style::default().fg(key_fg)));
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    (*desc).to_string(),
                    Style::default().fg(desc_fg),
                ));
            }
        }
        Line::from(spans)
    }
}

impl App<'_> {
    fn update_state(&mut self, view_area: Rect) {
        self.app_status.view_area = view_area;
    }

    fn is_input_mode(&self) -> bool {
        matches!(self.app_status.status_line, StatusLine::Input(_, _, _))
            || matches!(self.view, View::CreateTag(_))
    }

    fn open_detail(&mut self) {
        let commit_list_state = match self.view {
            View::List(ref mut view) => view.take_list_state(),
            View::UserCommand(ref mut view) => view.take_list_state(),
            _ => return,
        };
        let Some(commit_list_state) = commit_list_state else {
            return;
        };

        if commit_list_state.is_virtual_row_selected() {
            if let Some(wc) = commit_list_state.working_changes().cloned() {
                self.view = View::of_working_changes_detail(
                    commit_list_state,
                    wc,
                    self.ctx.clone(),
                    self.ec.sender(),
                );
            } else {
                self.view = View::of_list(commit_list_state, self.ctx.clone(), self.ec.sender());
            }
            return;
        }

        let (commit, changes, refs) = selected_commit_details(self.repository, &commit_list_state);
        self.view = View::of_detail(
            commit_list_state,
            commit,
            changes,
            refs,
            self.ctx.clone(),
            self.ec.sender(),
        );
    }

    fn close_detail(&mut self) {
        if let View::Detail(ref mut view) = self.view {
            let Some(commit_list_state) = view.take_list_state() else {
                return;
            };
            self.view = View::of_list(commit_list_state, self.ctx.clone(), self.ec.sender());
        }
    }

    fn clear_detail(&mut self) {
        if let View::Detail(ref mut view) = self.view {
            view.clear();
        }
    }

    fn open_user_command(
        &mut self,
        user_command_number: usize,
        terminal: Option<&mut DefaultTerminal>,
    ) {
        match extract_user_command_by_number(user_command_number, &self.ctx).map(|c| &c.r#type) {
            Ok(UserCommandType::Inline) => {
                self.open_user_command_inline(user_command_number);
            }
            Ok(UserCommandType::Silent) => {
                self.open_user_command_silent(user_command_number);
            }
            Ok(UserCommandType::Suspend) => {
                self.open_user_command_suspend(user_command_number);
            }
            Err(err) => {
                self.ec.send(AppEvent::NotifyError(err));
            }
        }
        if let Some(t) = terminal {
            if let Err(err) = t.clear() {
                let msg = format!("Failed to clear terminal: {err:?}");
                self.ec.send(AppEvent::NotifyError(msg));
            }
        }
    }

    fn open_user_command_inline(&mut self, user_command_number: usize) {
        // Guard: skip virtual row
        let is_virtual = match &self.view {
            View::List(view) => view.as_list_state().is_virtual_row_selected(),
            View::Detail(view) => view.as_list_state().is_virtual_row_selected(),
            View::UserCommand(view) => view.as_list_state().is_virtual_row_selected(),
            _ => false,
        };
        if is_virtual {
            return;
        }
        let commit_list_state = match self.view {
            View::List(ref mut view) => view.take_list_state(),
            View::Detail(ref mut view) => view.take_list_state(),
            View::UserCommand(ref mut view) => view.take_list_state(),
            _ => return,
        };
        let Some(commit_list_state) = commit_list_state else {
            return;
        };
        let (commit, refs) = selected_commit_refs(self.repository, &commit_list_state);
        match build_external_command_parameters(
            &commit,
            &refs,
            user_command_number,
            self.app_status.view_area,
            &self.ctx,
        ) {
            Ok(params) => {
                self.view = View::of_user_command(
                    commit_list_state,
                    params,
                    user_command_number,
                    self.ctx.clone(),
                    self.ec.sender(),
                );
            }
            Err(err) => {
                self.ec.send(AppEvent::NotifyError(err));
            }
        };
    }

    fn open_user_command_silent(&mut self, user_command_number: usize) {
        let commit_list_state = match self.view {
            View::List(ref mut view) => view.as_list_state(),
            View::Detail(ref mut view) => view.as_list_state(),
            View::UserCommand(ref mut view) => view.as_list_state(),
            _ => return,
        };
        if commit_list_state.is_virtual_row_selected() {
            return;
        }
        let (commit, refs) = selected_commit_refs(self.repository, commit_list_state);
        let result = build_external_command_parameters(
            &commit,
            &refs,
            user_command_number,
            self.app_status.view_area,
            &self.ctx,
        )
        .and_then(exec_user_command);
        match result {
            Ok(_) => {
                if extract_user_command_refresh_by_number(user_command_number, &self.ctx) {
                    self.view.refresh();
                }
            }
            Err(err) => {
                self.ec.send(AppEvent::NotifyError(err));
            }
        }
    }

    fn open_user_command_suspend(&mut self, user_command_number: usize) {
        let commit_list_state = match self.view {
            View::List(ref mut view) => view.as_list_state(),
            View::Detail(ref mut view) => view.as_list_state(),
            View::UserCommand(ref mut view) => view.as_list_state(),
            _ => return,
        };
        if commit_list_state.is_virtual_row_selected() {
            return;
        }
        let (commit, refs) = selected_commit_refs(self.repository, commit_list_state);
        match build_external_command_parameters(
            &commit,
            &refs,
            user_command_number,
            self.app_status.view_area,
            &self.ctx,
        ) {
            Ok(params) => {
                self.ec.suspend();
                if let Err(err) = exec_user_command_suspend(params) {
                    self.ec.send(AppEvent::NotifyError(err));
                }
                self.ec.resume();

                if extract_user_command_refresh_by_number(user_command_number, &self.ctx) {
                    self.view.refresh();
                }
            }
            Err(err) => {
                self.ec.send(AppEvent::NotifyError(err));
            }
        }
    }

    fn close_user_command(&mut self) {
        if let View::UserCommand(ref mut view) = self.view {
            let commit_list_state = view.take_list_state();
            if let Some(commit_list_state) = commit_list_state {
                self.view = View::of_list(commit_list_state, self.ctx.clone(), self.ec.sender());
            }
        }
    }

    fn clear_user_command(&mut self) {
        if let View::UserCommand(ref mut view) = self.view {
            view.clear();
        }
    }

    fn open_refs(&mut self) {
        if let View::List(ref mut view) = self.view {
            let Some(commit_list_state) = view.take_list_state() else {
                return;
            };
            let refs: Vec<Ref> = self.repository.all_refs().into_iter().cloned().collect();
            self.view = View::of_refs(commit_list_state, refs, self.ctx.clone(), self.ec.sender());
        }
    }

    fn close_refs(&mut self) {
        if let View::Refs(ref mut view) = self.view {
            let Some(commit_list_state) = view.take_list_state() else {
                return;
            };
            self.view = View::of_list(commit_list_state, self.ctx.clone(), self.ec.sender());
        }
    }

    fn open_create_tag(&mut self) {
        if let View::List(ref mut view) = self.view {
            if view.as_list_state().is_virtual_row_selected() {
                return;
            }
            let Some(commit_list_state) = view.take_list_state() else {
                return;
            };
            let commit_hash = commit_list_state.selected_commit_hash().clone();
            self.view = View::of_create_tag(
                commit_list_state,
                commit_hash,
                self.repository.path().to_path_buf(),
                self.ctx.clone(),
                self.ec.sender(),
            );
        }
    }

    fn close_create_tag(&mut self) {
        if let View::CreateTag(ref mut view) = self.view {
            let Some(commit_list_state) = view.take_list_state() else {
                return;
            };
            self.view = View::of_list(commit_list_state, self.ctx.clone(), self.ec.sender());
        }
    }

    fn open_delete_tag(&mut self) {
        if let View::List(ref mut view) = self.view {
            if view.as_list_state().is_virtual_row_selected() {
                return;
            }
            let Some(commit_list_state) = view.take_list_state() else {
                return;
            };
            let commit_hash = commit_list_state.selected_commit_hash().clone();
            let tags: Vec<Ref> = commit_list_state
                .selected_commit_refs()
                .iter()
                .map(|r| (*r).clone())
                .collect();
            let has_tags = tags.iter().any(|r| matches!(r, Ref::Tag { .. }));
            if !has_tags {
                self.view = View::of_list(commit_list_state, self.ctx.clone(), self.ec.sender());
                self.ec
                    .send(AppEvent::NotifyWarn("No tags on this commit".into()));
                return;
            }
            self.view = View::of_delete_tag(
                commit_list_state,
                commit_hash,
                tags,
                self.repository.path().to_path_buf(),
                self.ctx.clone(),
                self.ec.sender(),
            );
        }
    }

    fn close_delete_tag(&mut self) {
        if let View::DeleteTag(ref mut view) = self.view {
            let Some(commit_list_state) = view.take_list_state() else {
                return;
            };
            self.view = View::of_list(commit_list_state, self.ctx.clone(), self.ec.sender());
        }
    }

    fn open_delete_ref(&mut self, ref_name: String, ref_type: RefType) {
        if let View::Refs(ref mut view) = self.view {
            let Some(commit_list_state) = view.take_list_state() else {
                return;
            };
            let ref_list_state = view.take_ref_list_state();
            let refs = view.take_refs();
            self.view = View::of_delete_ref(
                commit_list_state,
                ref_list_state,
                refs,
                self.repository.path().to_path_buf(),
                ref_name,
                ref_type,
                self.ctx.clone(),
                self.ec.sender(),
            );
        }
    }

    fn close_delete_ref(&mut self) {
        if let View::DeleteRef(ref mut view) = self.view {
            let Some(commit_list_state) = view.take_list_state() else {
                return;
            };
            let ref_list_state = view.take_ref_list_state();
            let refs = view.take_refs();
            self.view = View::of_refs_with_state(
                commit_list_state,
                ref_list_state,
                refs,
                self.ctx.clone(),
                self.ec.sender(),
            );
        }
    }

    fn open_help(&mut self) {
        let before_view = std::mem::take(&mut self.view);
        self.view = View::of_help(before_view, self.ctx.clone(), self.ec.sender());
    }

    fn close_help(&mut self) {
        if let View::Help(ref mut view) = self.view {
            self.view = view.take_before_view();
        }
    }

    fn clear_help(&mut self) {
        if let View::Help(ref mut view) = self.view {
            view.clear();
        }
    }

    fn prefetch_github(&self) {
        let repo_path = self.repository.path().to_path_buf();
        let tx = self.ec.sender();

        std::thread::spawn(move || {
            let mut first_run = true;
            loop {
                let issues_result = crate::github::list_issues(&repo_path, "open");
                let prs_result = crate::github::list_pull_requests(&repo_path, "open");

                let mut any_ok = false;
                let mut warnings = Vec::new();

                let issues = match issues_result {
                    Ok(v) => {
                        any_ok = true;
                        v
                    }
                    Err(e) => {
                        warnings.push(format!("GitHub issues unavailable: {e}"));
                        Vec::new()
                    }
                };
                let pull_requests = match prs_result {
                    Ok(v) => {
                        any_ok = true;
                        v
                    }
                    Err(e) => {
                        warnings.push(format!("GitHub PRs unavailable: {e}"));
                        Vec::new()
                    }
                };

                if any_ok {
                    if first_run {
                        Self::fetch_all_details(&repo_path, &issues, &pull_requests, &tx);
                    }
                    tx.send(AppEvent::GitHubDataLoaded {
                        issues,
                        pull_requests,
                        warnings,
                    });
                } else if first_run {
                    let err = warnings.remove(0);
                    tx.send(AppEvent::GitHubLoadFailed { error: err });
                }

                if first_run {
                    first_run = false;
                }

                std::thread::sleep(Duration::from_secs(30));
            }
        });
    }

    fn fetch_all_details(
        repo_path: &std::path::Path,
        issues: &[crate::github::GhIssue],
        pull_requests: &[crate::github::GhPullRequest],
        tx: &Sender,
    ) {
        let issue_details: Vec<(u64, String)> = issues
            .iter()
            .filter_map(|i| {
                crate::github::view_item_rendered(repo_path, i.number, GhItemKind::Issue)
                    .ok()
                    .map(|s| (i.number, s))
            })
            .collect();

        let pr_details: Vec<(u64, String)> = pull_requests
            .iter()
            .filter_map(|p| {
                crate::github::view_item_rendered(repo_path, p.number, GhItemKind::PullRequest)
                    .ok()
                    .map(|s| (p.number, s))
            })
            .collect();

        tx.send(AppEvent::GitHubDetailsLoaded {
            issue_details,
            pr_details,
        });
    }

    fn open_github(&mut self) {
        let (issues, prs) = if let Some(ref cache) = self.github_cache {
            (cache.issues.clone(), cache.pull_requests.clone())
        } else {
            // 取消前一個 stale 計時器
            if let Some(ref prev) = self.github_timer_cancel {
                prev.store(true, Ordering::Relaxed);
            }

            // 3 秒計時器：若資料遲遲未到達，發送失敗事件作為保底
            let cancel = Arc::new(AtomicBool::new(false));
            self.github_timer_cancel = Some(cancel.clone());
            let tx = self.ec.sender();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(3));
                if !cancel.load(Ordering::Relaxed) {
                    tx.send(AppEvent::GitHubLoadFailed {
                        error: "GitHub data not available".into(),
                    });
                }
            });

            (Vec::new(), Vec::new())
        };

        let before_view = std::mem::take(&mut self.view);
        self.view = View::of_github(before_view, issues, prs, self.ctx.clone(), self.ec.sender());
    }

    fn on_github_data_loaded(
        &mut self,
        issues: Vec<crate::github::GhIssue>,
        pull_requests: Vec<crate::github::GhPullRequest>,
        warnings: Vec<String>,
    ) {
        // 資料到達，取消 stale 計時器
        if let Some(ref cancel) = self.github_timer_cancel {
            cancel.store(true, Ordering::Relaxed);
        }

        // 檢查是否與快取相同
        let changed = match &self.github_cache {
            Some(cache) => cache.issues != issues || cache.pull_requests != pull_requests,
            None => true,
        };

        // 偵測新增的 issue/PR number（用於差量抓取詳情）
        let new_issue_numbers: Vec<u64> = if let Some(ref cache) = self.github_cache {
            let existing: FxHashSet<u64> = cache.issues.iter().map(|i| i.number).collect();
            issues
                .iter()
                .filter(|i| !existing.contains(&i.number))
                .map(|i| i.number)
                .collect()
        } else {
            Vec::new()
        };
        let new_pr_numbers: Vec<u64> = if let Some(ref cache) = self.github_cache {
            let existing: FxHashSet<u64> = cache.pull_requests.iter().map(|p| p.number).collect();
            pull_requests
                .iter()
                .filter(|p| !existing.contains(&p.number))
                .map(|p| p.number)
                .collect()
        } else {
            Vec::new()
        };

        // 更新快取（保留既有 detail cache）
        if let Some(ref mut cache) = self.github_cache {
            cache.issues = issues.clone();
            cache.pull_requests = pull_requests.clone();
        } else {
            self.github_cache = Some(GitHubCache {
                issues: issues.clone(),
                pull_requests: pull_requests.clone(),
                issue_details: FxHashMap::default(),
                pr_details: FxHashMap::default(),
                state_filter: "open".to_string(),
            });
        }

        // 背景補抓新增項目的詳情
        if !new_issue_numbers.is_empty() || !new_pr_numbers.is_empty() {
            let repo_path = self.repository.path().to_path_buf();
            let tx = self.ec.sender();
            std::thread::spawn(move || {
                let issue_details: Vec<(u64, String)> = new_issue_numbers
                    .iter()
                    .filter_map(|&n| {
                        crate::github::view_item_rendered(&repo_path, n, GhItemKind::Issue)
                            .ok()
                            .map(|s| (n, s))
                    })
                    .collect();
                let pr_details: Vec<(u64, String)> = new_pr_numbers
                    .iter()
                    .filter_map(|&n| {
                        crate::github::view_item_rendered(&repo_path, n, GhItemKind::PullRequest)
                            .ok()
                            .map(|s| (n, s))
                    })
                    .collect();
                if !issue_details.is_empty() || !pr_details.is_empty() {
                    tx.send(AppEvent::GitHubDetailsLoaded {
                        issue_details,
                        pr_details,
                    });
                }
            });
        }

        if let View::GitHub(ref mut view) = self.view {
            // 已在 GitHub 視圖：有變更才就地更新
            if changed {
                view.update_data(issues, pull_requests);
            }
            if !warnings.is_empty() {
                view.set_flash(warnings.join("; "), false);
            }
        }
    }

    fn on_github_details_loaded(
        &mut self,
        issue_details: Vec<(u64, String)>,
        pr_details: Vec<(u64, String)>,
    ) {
        let convert = |s: &str| -> Vec<Line<'static>> {
            s.into_text()
                .map(|t| t.into_iter().collect())
                .unwrap_or_else(|_| vec![Line::raw(s.to_string())])
        };

        if let Some(ref mut cache) = self.github_cache {
            for (n, s) in &issue_details {
                cache.issue_details.insert(*n, convert(s));
            }
            for (n, s) in &pr_details {
                cache.pr_details.insert(*n, convert(s));
            }
        }
    }

    fn refresh_github(&mut self, state: &str) {
        if let Some(ref mut cache) = self.github_cache {
            cache.state_filter = state.to_string();
        }

        let repo_path = self.repository.path().to_path_buf();
        let tx = self.ec.sender();
        let state = state.to_string();

        std::thread::spawn(move || {
            let issues_result = crate::github::list_issues(&repo_path, &state);
            let prs_result = crate::github::list_pull_requests(&repo_path, &state);

            match (issues_result, prs_result) {
                (Err(e), _) | (_, Err(e)) => {
                    tx.send(AppEvent::GitHubLoadFailed {
                        error: e.to_string(),
                    });
                }
                (Ok(issues), Ok(pull_requests)) => {
                    tx.send(AppEvent::GitHubDataLoaded {
                        issues: issues.clone(),
                        pull_requests: pull_requests.clone(),
                        warnings: Vec::new(),
                    });
                    // R 刷新：全量重抓詳情
                    Self::fetch_all_details(&repo_path, &issues, &pull_requests, &tx);
                }
            }
        });
    }

    fn close_github(&mut self) {
        if let View::GitHub(ref mut view) = self.view {
            self.view = view.take_before_view();
        }
    }

    fn clear_github(&mut self) {
        if let View::GitHub(ref mut view) = self.view {
            view.clear();
        }
    }

    fn batch_toggle_checkboxes(
        &mut self,
        number: u64,
        kind: GhItemKind,
        checkbox_indices: Vec<usize>,
    ) {
        self.pending_message = Some("Updating checkboxes...".to_string());

        let repo_path = self.repository.path().to_path_buf();
        let tx = self.ec.sender();
        let count = checkbox_indices.len();

        std::thread::spawn(move || {
            let result = (|| -> Result<String, String> {
                let body = crate::github::get_body(&repo_path, number, kind)?;
                let new_body = crate::github::toggle_checkboxes(&body, &checkbox_indices);
                crate::github::update_body(&repo_path, number, kind, &new_body)?;
                Ok(new_body)
            })();

            tx.send(AppEvent::HidePendingOverlay);

            match result {
                Ok(new_body) => {
                    tx.send(AppEvent::GitHubFlash {
                        message: format!("{count} checkbox(es) updated"),
                        is_error: false,
                    });
                    tx.send(AppEvent::CheckboxToggled {
                        number,
                        kind,
                        new_body,
                    });
                }
                Err(e) => {
                    tx.send(AppEvent::GitHubFlash {
                        message: format!("Batch toggle failed: {e}"),
                        is_error: true,
                    });
                }
            }
        });
    }

    fn on_checkbox_toggled(&mut self, number: u64, kind: GhItemKind, new_body: &str) {
        // 清除該項目的 detail cache
        if let Some(ref mut cache) = self.github_cache {
            match kind {
                GhItemKind::Issue => cache.issue_details.remove(&number),
                GhItemKind::PullRequest => cache.pr_details.remove(&number),
            };
        }

        // 更新 GitHubView 中的 detail 和 task panel
        if let View::GitHub(ref mut view) = self.view {
            view.invalidate_detail_cache(number, kind);
            view.update_task_panel(number, kind, new_body);
        }

        // 背景重新抓取該項目的渲染詳情
        let repo_path = self.repository.path().to_path_buf();
        let tx = self.ec.sender();
        std::thread::spawn(move || {
            if let Ok(rendered) = crate::github::view_item_rendered(&repo_path, number, kind) {
                tx.send(AppEvent::DetailFetched {
                    number,
                    kind,
                    rendered,
                });
            }
        });
    }

    fn on_load_detail(&mut self, number: u64, kind: GhItemKind) {
        // 先查 cache
        if let Some(ref cache) = self.github_cache {
            let detail_cache = match kind {
                GhItemKind::Issue => &cache.issue_details,
                GhItemKind::PullRequest => &cache.pr_details,
            };
            if let Some(lines) = detail_cache.get(&number) {
                if let View::GitHub(ref mut view) = self.view {
                    view.set_detail(number, kind, lines.clone());
                }
                return;
            }
        }

        // Cache miss：背景抓取
        let repo_path = self.repository.path().to_path_buf();
        let tx = self.ec.sender();
        std::thread::spawn(move || {
            match crate::github::view_item_rendered(&repo_path, number, kind) {
                Ok(rendered) => {
                    tx.send(AppEvent::DetailFetched {
                        number,
                        kind,
                        rendered,
                    });
                }
                Err(e) => {
                    tx.send(AppEvent::GitHubFlash {
                        message: format!("Failed to load detail: {e}"),
                        is_error: true,
                    });
                }
            }
        });
    }

    fn on_detail_fetched(&mut self, number: u64, kind: GhItemKind, rendered: &str) {
        let lines: Vec<Line<'static>> = rendered
            .into_text()
            .map(|t| t.into_iter().collect())
            .unwrap_or_else(|_| vec![Line::raw(rendered.to_string())]);

        // 存入 cache
        if let Some(ref mut cache) = self.github_cache {
            match kind {
                GhItemKind::Issue => cache.issue_details.insert(number, lines.clone()),
                GhItemKind::PullRequest => cache.pr_details.insert(number, lines.clone()),
            };
        }

        // 設定 detail（若仍在 GitHub 視圖）
        if let View::GitHub(ref mut view) = self.view {
            view.set_detail(number, kind, lines);
        }
    }

    fn on_load_task_panel(&mut self, number: u64, kind: GhItemKind) {
        let repo_path = self.repository.path().to_path_buf();
        let tx = self.ec.sender();
        std::thread::spawn(
            move || match crate::github::get_body(&repo_path, number, kind) {
                Ok(body) => {
                    let items = crate::github::parse_checkboxes(&body);
                    tx.send(AppEvent::TaskPanelLoaded {
                        number,
                        kind,
                        items,
                    });
                }
                Err(e) => {
                    tx.send(AppEvent::GitHubFlash {
                        message: format!("Failed to load body: {e}"),
                        is_error: true,
                    });
                }
            },
        );
    }

    fn select_older_commit(&mut self) {
        if let View::Detail(ref mut view) = self.view {
            view.select_older_commit(self.repository);
        } else if let View::UserCommand(ref mut view) = self.view {
            view.select_older_commit(
                self.repository,
                self.app_status.view_area,
                build_external_command_parameters,
            );
        }
    }

    fn select_newer_commit(&mut self) {
        if let View::Detail(ref mut view) = self.view {
            view.select_newer_commit(self.repository);
        } else if let View::UserCommand(ref mut view) = self.view {
            view.select_newer_commit(
                self.repository,
                self.app_status.view_area,
                build_external_command_parameters,
            );
        }
    }

    fn select_parent_commit(&mut self) {
        if let View::Detail(ref mut view) = self.view {
            view.select_parent_commit(self.repository);
        } else if let View::UserCommand(ref mut view) = self.view {
            view.select_parent_commit(
                self.repository,
                self.app_status.view_area,
                build_external_command_parameters,
            );
        }
    }

    fn init_with_context(&mut self, context: RefreshViewContext) {
        if let View::List(ref mut view) = self.view {
            view.reset_commit_list_with(context.list_context());
        }
        match context {
            RefreshViewContext::List { .. } => {}
            RefreshViewContext::Detail { .. } => {
                self.open_detail();
            }
            RefreshViewContext::UserCommand {
                user_command_context,
                ..
            } => {
                self.open_user_command(user_command_context.n, None);
            }
            RefreshViewContext::Refs { refs_context, .. } => {
                self.open_refs();
                if let View::Refs(ref mut view) = self.view {
                    view.reset_refs_with(refs_context);
                }
            }
        }
    }

    fn clear_status_line(&mut self) {
        self.app_status.status_line = StatusLine::None;
    }

    fn update_status_input(
        &mut self,
        msg: String,
        cursor_pos: Option<u16>,
        transient_msg: Option<String>,
    ) {
        self.app_status.status_line = StatusLine::Input(msg, cursor_pos, transient_msg);
    }

    fn info_notification(&mut self, msg: String) {
        self.app_status.status_line = StatusLine::NotificationInfo(msg);
    }

    fn success_notification(&mut self, msg: String) {
        self.app_status.status_line = StatusLine::NotificationSuccess(msg);
    }

    fn warn_notification(&mut self, msg: String) {
        self.app_status.status_line = StatusLine::NotificationWarn(msg);
    }

    fn error_notification(&mut self, msg: String) {
        self.app_status.status_line = StatusLine::NotificationError(msg);
    }

    fn copy_to_clipboard(&self, name: String, value: String) {
        match copy_to_clipboard(value, &self.ctx.core_config.external.clipboard) {
            Ok(_) => {
                let msg = format!("Copied {name} to clipboard successfully");
                self.ec.send(AppEvent::NotifySuccess(msg));
            }
            Err(msg) => {
                self.ec.send(AppEvent::NotifyError(msg));
            }
        }
    }

    fn fetch_all(&self) {
        self.spawn_git_task(
            &["fetch", "--all"],
            "Fetching...".into(),
            "Fetch completed".into(),
            "Fetch failed",
        );
    }

    fn checkout_commit(&self, target: String) {
        self.spawn_git_task(
            &["checkout", &target],
            format!("Checking out '{target}'..."),
            format!("Checked out '{target}'"),
            "Checkout failed",
        );
    }

    fn spawn_git_task(
        &self,
        args: &[&str],
        pending_msg: String,
        success_msg: String,
        error_prefix: &str,
    ) {
        let repo_path = self.repository.path().to_path_buf();
        let tx = self.ec.sender();
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let error_prefix = error_prefix.to_string();

        tx.send(AppEvent::ShowPendingOverlay {
            message: pending_msg,
        });

        std::thread::spawn(move || {
            let output = std::process::Command::new("git")
                .args(&args)
                .current_dir(&repo_path)
                .output();

            tx.send(AppEvent::HidePendingOverlay);
            match output {
                Ok(o) if o.status.success() => {
                    let detail = String::from_utf8_lossy(&o.stderr).trim().to_string();
                    let msg = if detail.is_empty() {
                        success_msg
                    } else {
                        detail
                    };
                    tx.send(AppEvent::NotifySuccess(msg));
                }
                Ok(o) => {
                    let stderr = String::from_utf8_lossy(&o.stderr).trim().to_string();
                    tx.send(AppEvent::NotifyError(format!("{error_prefix}: {stderr}")));
                }
                Err(e) => {
                    tx.send(AppEvent::NotifyError(format!("{error_prefix}: {e}")));
                }
            }
        });
    }
}

fn selected_commit_details(
    repository: &Repository,
    commit_list_state: &CommitListState,
) -> (Commit, Vec<FileChange>, Vec<Ref>) {
    let selected = commit_list_state.selected_commit_hash().clone();
    let (commit, changes) = repository.commit_detail(&selected);
    let refs: Vec<Ref> = repository.refs(&selected).into_iter().cloned().collect();
    (commit.clone(), changes, refs)
}

fn selected_commit_refs(
    repository: &Repository,
    commit_list_state: &CommitListState,
) -> (Commit, Vec<Ref>) {
    let selected = commit_list_state.selected_commit_hash().clone();
    let (commit, refs) = repository.commit_refs(&selected);
    (commit.clone(), refs)
}

fn process_numeric_prefix(
    numeric_prefix: &str,
    user_event: UserEvent,
    _key_event: KeyEvent,
) -> UserEventWithCount {
    if user_event.is_countable() {
        let count = if numeric_prefix.is_empty() {
            1
        } else {
            numeric_prefix.parse::<usize>().unwrap_or(1)
        };
        UserEventWithCount::new(user_event, count)
    } else {
        UserEventWithCount::from_event(user_event)
    }
}

fn extract_user_command_by_number(
    user_command_number: usize,
    ctx: &AppContext,
) -> Result<&UserCommand, String> {
    ctx.core_config
        .user_command
        .commands
        .get(&user_command_number.to_string())
        .ok_or_else(|| format!("No user command configured for number {user_command_number}",))
}

fn extract_user_command_refresh_by_number(user_command_number: usize, ctx: &AppContext) -> bool {
    extract_user_command_by_number(user_command_number, ctx)
        .map(|c| c.refresh)
        .unwrap_or_default()
}

fn build_external_command_parameters(
    commit: &Commit,
    refs: &[Ref],
    user_command_number: usize,
    view_area: Rect,
    ctx: &AppContext,
) -> Result<ExternalCommandParameters, String> {
    let command = extract_user_command_by_number(user_command_number, ctx)?
        .commands
        .iter()
        .map(String::to_string)
        .collect();
    let target_hash = commit.commit_hash.as_str().to_string();
    let parent_hashes: Vec<String> = commit
        .parent_commit_hashes
        .iter()
        .map(|c| c.as_str().to_string())
        .collect();

    let mut all_refs = vec![];
    let mut branches = vec![];
    let mut remote_branches = vec![];
    let mut tags = vec![];
    for r in refs {
        match r {
            Ref::Tag { .. } => tags.push(r.name().to_string()),
            Ref::Branch { .. } => branches.push(r.name().to_string()),
            Ref::RemoteBranch { .. } => remote_branches.push(r.name().to_string()),
            Ref::Stash { .. } => continue, // skip stashes
        }
        all_refs.push(r.name().to_string());
    }

    let area_width = view_area.width.saturating_sub(4); // minus the left and right padding
    let area_height = (view_area.height.saturating_sub(1))
        .min(ctx.ui_config.user_command.height)
        .saturating_sub(1); // minus the top border
    Ok(ExternalCommandParameters {
        command,
        target_hash,
        parent_hashes,
        all_refs,
        branches,
        remote_branches,
        tags,
        area_width,
        area_height,
    })
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rustfmt::skip]
    #[rstest]
    #[case("",    UserEvent::NavigateDown, UserEventWithCount::new(UserEvent::NavigateDown, 1))] // no prefix
    #[case("5",   UserEvent::NavigateUp,   UserEventWithCount::new(UserEvent::NavigateUp, 5))] // with prefix
    #[case("0",   UserEvent::PageDown,     UserEventWithCount::new(UserEvent::PageDown, 1))] // zero should be converted to 1
    #[case("42",  UserEvent::ScrollDown,   UserEventWithCount::new(UserEvent::ScrollDown, 42))] // multi-digit number
    #[case("999", UserEvent::PageDown,     UserEventWithCount::new(UserEvent::PageDown, 999))] // large number
    #[case("abc", UserEvent::ScrollUp,     UserEventWithCount::new(UserEvent::ScrollUp, 1))] // should fallback to 1
    #[case("5",   UserEvent::Quit,         UserEventWithCount::new(UserEvent::Quit, 1))] // non-countable event with prefix
    #[case("",    UserEvent::Confirm,      UserEventWithCount::new(UserEvent::Confirm, 1))] // non-countable event without prefix
    fn test_process_numeric_prefix(
        #[case] numeric_prefix: &str,
        #[case] user_event: UserEvent,
        #[case] expected: UserEventWithCount,
    ) {
        let dummy_key_event = KeyEvent::from(KeyCode::Enter); // KeyEvent is not used in the logic
        let actual = process_numeric_prefix(numeric_prefix, user_event, dummy_key_event);
        assert_eq!(actual, expected);
    }
}

use std::rc::Rc;
use std::time::{Duration, Instant};

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
    event::{
        AppEvent, CheckoutPickKind, EventController, RefCopyKind, UserEvent, UserEventWithCount,
    },
    external::{
        copy_to_clipboard, exec_user_command, exec_user_command_suspend, ExternalCommandParameters,
    },
    git::{Commit, CommitHash, FileChange, Head, Ref, RefType, Repository},
    github::GhItemKind,
    graph::{CellWidthType, Graph, GraphImageManager},
    keybind::KeyBind,
    protocol::ImageProtocol,
    view::{RefreshViewContext, RefsOrigin, View},
    widget::{
        commit_list::{CommitInfo, CommitListState, RawCommitIdx},
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
fn picker_digit_index(key: KeyEvent) -> Option<usize> {
    let KeyCode::Char(c) = key.code else {
        return None;
    };
    let digit = c.to_digit(10)?;
    (digit as usize).checked_sub(1)
}

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
    RefPicker {
        options: Vec<String>,
        kind: RefCopyKind,
    },
    CheckoutPicker {
        options: Vec<String>,
        kind: CheckoutPickKind,
    },
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
    github_loading: bool,
    ctx: Rc<AppContext>,
    ec: &'a EventController,
    marquee_frame: u64,
    marquee_needed: bool,
    last_marquee_hash: Option<CommitHash>,
}

#[derive(Debug)]
struct GitHubCache {
    issues: Vec<crate::github::GhIssue>,
    pull_requests: Vec<crate::github::GhPullRequest>,
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
                    ref_name_to_commit_index_map.insert(r.name().to_string(), RawCommitIdx(i));
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

        let filtered_colors: Option<FxHashMap<CommitHash, ratatui::style::Color>> =
            filtered_graph.as_ref().map(|fg| {
                fg.graph
                    .commit_hashes
                    .iter()
                    .map(|commit_hash| {
                        let (pos_x, _) = fg.graph.commit_pos_map[commit_hash];
                        (
                            commit_hash.clone(),
                            graph_color_set.get(pos_x).to_ratatui_color(),
                        )
                    })
                    .collect()
            });

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
            filtered_graph,
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
            github_loading: false,
            ctx,
            ec,
            marquee_frame: 0,
            marquee_needed: false,
            last_marquee_hash: None,
        };

        if let Some(context) = refresh_view_context {
            app.init_with_context(context);
        }

        app
    }

    pub fn into_parts(
        self,
    ) -> (
        GraphImageManager,
        Option<FilteredGraphData>,
        FxHashSet<CommitHash>,
    ) {
        self.view.into_commit_list_state().into_graph_parts()
    }
}

impl App<'_> {
    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<Ret, std::io::Error> {
        let mut skip_draw = false;
        loop {
            if !skip_draw {
                let current_hash = match &self.view {
                    View::List(lv) => Some(lv.as_list_state().selected_commit_hash().clone()),
                    _ => None,
                };
                if self.last_marquee_hash != current_hash {
                    self.marquee_frame = 0;
                    self.last_marquee_hash = current_hash;
                }

                if self.view.take_graph_clear() {
                    clear_image_area(
                        self.ctx.image_protocol,
                        terminal,
                        self.app_status.view_area.top()..self.app_status.view_area.bottom(),
                    )?;
                }
                terminal.draw(|f| self.render(f))?;

                self.marquee_needed = match &self.view {
                    View::List(lv) => lv.as_list_state().selected_row_overflows.get(),
                    _ => false,
                };
            }
            skip_draw = false;

            match self.ec.recv() {
                AppEvent::Tick => {
                    if self.marquee_needed {
                        self.marquee_frame = self.marquee_frame.wrapping_add(1);
                    } else {
                        skip_draw = true;
                    }
                    continue;
                }
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

                    // Picker intercepts input; ForceQuit (Ctrl-C) falls through so
                    // 使用者在 picker 中仍能離開程式。
                    if !matches!(self.ctx.keybind.get(&key), Some(UserEvent::ForceQuit)) {
                        match self.app_status.status_line {
                            StatusLine::RefPicker { .. } => {
                                self.handle_ref_picker_key(key);
                                continue;
                            }
                            StatusLine::CheckoutPicker { .. } => {
                                self.handle_checkout_picker_key(key);
                                continue;
                            }
                            _ => {}
                        }
                    }

                    match self.app_status.status_line {
                        StatusLine::None
                        | StatusLine::Input(_, _, _)
                        | StatusLine::RefPicker { .. }
                        | StatusLine::CheckoutPicker { .. } => {
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
                            // 只在 browsing view 攔截，確保 modal/input view（CreateTag、
                            // DeleteTag、DeleteRef、UserCommand）保有自己的 keymap。
                            if self.view.is_browsing_view() {
                                let global_event = match event_with_count.event {
                                    UserEvent::GitHubToggle => Some(AppEvent::OpenGitHub),
                                    UserEvent::HelpToggle => Some(AppEvent::OpenHelp),
                                    _ => None,
                                };
                                if let Some(app_event) = global_event {
                                    self.app_status.numeric_prefix.clear();
                                    self.ec.send(app_event);
                                    continue;
                                }
                            }
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
                    self.clear_image(Some(terminal))?;
                    self.open_detail();
                }
                AppEvent::CloseDetail => {
                    terminal.clear()?;
                    self.close_detail();
                }
                AppEvent::OpenUserCommand(n) => {
                    self.clear_image(Some(terminal))?;
                    self.open_user_command(n, Some(terminal));
                }
                AppEvent::CloseUserCommand => {
                    terminal.clear()?;
                    self.close_user_command();
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
                    self.clear_image(None)?;
                    self.open_help();
                }
                AppEvent::CloseHelp => {
                    terminal.clear()?;
                    self.close_help();
                }
                AppEvent::OpenGitHub => {
                    self.open_github();
                }
                AppEvent::CloseGitHub => {
                    self.close_github();
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
                    self.github_loading = false;
                    if let View::GitHub(ref mut view) = self.view {
                        view.set_error(error);
                    }
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
                AppEvent::OpenUrl(url) => {
                    self.open_url(url);
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
                AppEvent::OpenRefPicker { options, kind } => {
                    self.app_status.status_line = StatusLine::RefPicker { options, kind };
                }
                AppEvent::OpenCheckoutPicker { options, kind } => {
                    self.app_status.status_line = StatusLine::CheckoutPicker { options, kind };
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

        let marquee_frame = self.marquee_frame;
        self.view.render(f, view_area, marquee_frame);
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
            StatusLine::RefPicker { options, kind } => {
                self.render_picker_line(kind.picker_prompt(), options)
            }
            StatusLine::CheckoutPicker { options, kind } => {
                self.render_picker_line(kind.picker_prompt(), options)
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

    fn render_picker_line<'s>(&self, prompt: &'s str, options: &'s [String]) -> Line<'s> {
        let mut spans: Vec<Span<'s>> = vec![prompt.into()];
        for (i, name) in options.iter().enumerate() {
            spans.push(format!("[{}]", i + 1).fg(self.ctx.color_theme.status_input_transient_fg));
            spans.push(name.as_str().into());
            spans.push("  ".into());
        }
        spans.push("(Esc to cancel)".fg(self.ctx.color_theme.status_input_transient_fg));
        Line::from(spans)
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

impl<'a> App<'a> {
    fn enter_detail(&mut self, cls: CommitListState<'a>) {
        if cls.is_virtual_row_selected() {
            unreachable!("virtual row must be handled before reaching Detail");
        }
        let (commit, changes, refs) = selected_commit_details(self.repository, &cls);
        self.view = View::of_detail(
            cls,
            commit,
            changes,
            refs,
            self.ctx.clone(),
            self.ec.sender(),
        );
    }
}

impl App<'_> {
    fn update_state(&mut self, view_area: Rect) {
        self.app_status.view_area = view_area;
    }

    fn is_input_mode(&self) -> bool {
        matches!(
            self.app_status.status_line,
            StatusLine::Input(_, _, _)
                | StatusLine::RefPicker { .. }
                | StatusLine::CheckoutPicker { .. }
        ) || matches!(self.view, View::CreateTag(_))
    }

    fn handle_ref_picker_key(&mut self, key: KeyEvent) {
        if let Some(UserEvent::Cancel) = self.ctx.keybind.get(&key) {
            self.app_status.status_line = StatusLine::None;
            return;
        }
        let StatusLine::RefPicker { options, kind } = &self.app_status.status_line else {
            return;
        };
        let Some(idx) = picker_digit_index(key) else {
            return;
        };
        let Some(name) = options.get(idx) else { return };
        let label = kind.copy_label();
        let value = name.clone();
        self.app_status.status_line = StatusLine::None;
        self.copy_to_clipboard(label.into(), value);
    }

    fn handle_checkout_picker_key(&mut self, key: KeyEvent) {
        if let Some(UserEvent::Cancel) = self.ctx.keybind.get(&key) {
            self.app_status.status_line = StatusLine::None;
            return;
        }
        let StatusLine::CheckoutPicker { options, .. } = &self.app_status.status_line else {
            return;
        };
        let Some(idx) = picker_digit_index(key) else {
            return;
        };
        let Some(name) = options.get(idx) else { return };
        let target = name.clone();
        self.app_status.status_line = StatusLine::None;
        self.checkout_commit(target);
    }

    fn clear_image(&self, terminal: Option<&mut DefaultTerminal>) -> Result<(), std::io::Error> {
        // Sometimes the first image fails to render after a full screen clear
        // As a workaround, the first area is preserved when a full clear is not required
        if let Some(t) = terminal {
            for y in 1..t.size()?.height {
                self.ctx.image_protocol.clear_line(y);
            }
        } else {
            self.ctx.image_protocol.clear();
        }
        Ok(())
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

        self.enter_detail(commit_list_state);
    }

    fn close_detail(&mut self) {
        if let View::Detail(ref mut view) = self.view {
            let Some(commit_list_state) = view.take_list_state() else {
                return;
            };
            self.view = View::of_list(commit_list_state, self.ctx.clone(), self.ec.sender());
        }
    }

    fn open_user_command(
        &mut self,
        user_command_number: usize,
        terminal: Option<&mut DefaultTerminal>,
    ) {
        let clear = match extract_user_command_by_number(user_command_number, &self.ctx)
            .map(|c| &c.r#type)
        {
            Ok(UserCommandType::Inline) => {
                self.open_user_command_inline(user_command_number);
                false
            }
            Ok(UserCommandType::Silent) => {
                self.open_user_command_silent(user_command_number);
                true
            }
            Ok(UserCommandType::Suspend) => {
                self.open_user_command_suspend(user_command_number);
                true
            }
            Err(err) => {
                self.ec.send(AppEvent::NotifyError(err));
                false
            }
        };
        if clear {
            if let Some(t) = terminal {
                if let Err(err) = t.clear() {
                    let msg = format!("Failed to clear terminal: {err:?}");
                    self.ec.send(AppEvent::NotifyError(msg));
                }
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
            View::List(ref mut view) => view.as_list_state(),
            View::Detail(ref mut view) => view.as_list_state(),
            View::UserCommand(ref mut view) => view.as_list_state(),
            _ => return,
        };
        let (commit, _, refs) = selected_commit_details(self.repository, commit_list_state);
        let result = build_external_command_parameters_and_exec_command(
            &commit,
            &refs,
            user_command_number,
            self.app_status.view_area,
            &self.ctx,
        );
        match result {
            Ok(output) => {
                // take list state only when the command execution is successful, to avoid losing the state when the command fails
                let commit_list_state = match self.view {
                    View::List(ref mut view) => view.take_list_state(),
                    View::Detail(ref mut view) => view.take_list_state(),
                    View::UserCommand(ref mut view) => view.take_list_state(),
                    _ => return,
                };
                let Some(commit_list_state) = commit_list_state else {
                    return;
                };
                self.view = View::of_user_command(
                    commit_list_state,
                    output,
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
        let (commit, _, refs) = selected_commit_details(self.repository, commit_list_state);
        let result = build_external_command_parameters_and_exec_command(
            &commit,
            &refs,
            user_command_number,
            self.app_status.view_area,
            &self.ctx,
        );
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
        let (commit, _, refs) = selected_commit_details(self.repository, commit_list_state);
        match build_external_command_parameters(
            &commit,
            &refs,
            user_command_number,
            self.app_status.view_area,
            &self.ctx,
        ) {
            Ok(params) => {
                self.ec.suspend();
                let exec_result = exec_user_command_suspend(params);
                self.ec.resume();
                self.marquee_frame = 0;

                if extract_user_command_refresh_by_number(user_command_number, &self.ctx) {
                    self.view.refresh();
                }

                // notify after resuming and refreshing
                if let Err(err) = exec_result {
                    self.ec.send(AppEvent::NotifyError(err));
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
                self.view.request_graph_clear();
            }
        }
    }

    fn open_refs(&mut self) {
        let origin = match self.view {
            View::List(_) => RefsOrigin::List,
            View::Detail(_) => RefsOrigin::Detail,
            _ => return,
        };
        self.open_refs_with_origin(origin);
    }

    fn open_refs_with_origin(&mut self, origin: RefsOrigin) {
        let commit_list_state = match self.view {
            View::List(ref mut view) => view.take_list_state(),
            View::Detail(ref mut view) => view.take_list_state(),
            _ => return,
        };
        let Some(commit_list_state) = commit_list_state else {
            return;
        };
        let refs: Vec<Ref> = self.repository.all_refs().into_iter().cloned().collect();
        self.view = View::of_refs(
            commit_list_state,
            refs,
            origin,
            self.ctx.clone(),
            self.ec.sender(),
        );
    }

    fn close_refs(&mut self) {
        if let View::Refs(ref mut view) = self.view {
            let origin = view.origin();
            let Some(commit_list_state) = view.take_list_state() else {
                return;
            };
            match origin {
                RefsOrigin::List => {
                    self.view =
                        View::of_list(commit_list_state, self.ctx.clone(), self.ec.sender());
                }
                RefsOrigin::Detail => {
                    self.enter_detail(commit_list_state);
                }
            }
            self.view.request_graph_clear();
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
            self.view.request_graph_clear();
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
            self.view.request_graph_clear();
        }
    }

    fn open_delete_ref(&mut self, ref_name: String, ref_type: RefType) {
        if let View::Refs(ref mut view) = self.view {
            let refs_origin = view.origin();
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
                refs_origin,
                self.ctx.clone(),
                self.ec.sender(),
            );
        }
    }

    fn close_delete_ref(&mut self) {
        if let View::DeleteRef(ref mut view) = self.view {
            let refs_origin = view.refs_origin();
            let Some(commit_list_state) = view.take_list_state() else {
                return;
            };
            let ref_list_state = view.take_ref_list_state();
            let refs = view.take_refs();
            self.view = View::of_refs_with_state(
                commit_list_state,
                ref_list_state,
                refs,
                refs_origin,
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
            self.view.request_graph_clear();
        }
    }

    fn open_github(&mut self) {
        let (issues, prs) = if let Some(ref cache) = self.github_cache {
            (cache.issues.clone(), cache.pull_requests.clone())
        } else {
            self.refresh_github("open");
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
        self.github_loading = false;
        // 檢查是否與快取相同
        let changed = match &self.github_cache {
            Some(cache) => cache.issues != issues || cache.pull_requests != pull_requests,
            None => true,
        };

        // 更新快取（body 已含在 list data 中，不需要 detail cache）
        if let Some(ref mut cache) = self.github_cache {
            cache.issues = issues.clone();
            cache.pull_requests = pull_requests.clone();
        } else {
            self.github_cache = Some(GitHubCache {
                issues: issues.clone(),
                pull_requests: pull_requests.clone(),
                state_filter: "open".to_string(),
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

    fn refresh_github(&mut self, state: &str) {
        if self.github_loading {
            return;
        }
        self.github_loading = true;

        if let Some(ref mut cache) = self.github_cache {
            cache.state_filter = state.to_string();
        }

        let repo_path = self.repository.path().to_path_buf();
        let tx = self.ec.sender();
        let state = state.to_string();

        std::thread::spawn(move || {
            let issues_result = crate::github::list_issues(&repo_path, &state);
            let prs_result = crate::github::list_pull_requests(&repo_path, &state);

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
                tx.send(AppEvent::GitHubDataLoaded {
                    issues,
                    pull_requests,
                    warnings,
                });
            } else {
                tx.send(AppEvent::GitHubLoadFailed {
                    error: warnings.join("; "),
                });
            }
        });
    }

    fn close_github(&mut self) {
        if let View::GitHub(ref mut view) = self.view {
            self.view = view.take_before_view();
            self.view.request_graph_clear();
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
        // 更新 list item 的 body 欄位，preview 直接從 body 渲染（零 API）
        if let View::GitHub(ref mut view) = self.view {
            view.update_body_for_item(number, kind, new_body.to_string());
        }
        // 同步更新 cache 的 list data
        if let Some(ref mut cache) = self.github_cache {
            match kind {
                GhItemKind::Issue => {
                    if let Some(issue) = cache.issues.iter_mut().find(|i| i.number == number) {
                        issue.body = new_body.to_string();
                    }
                }
                GhItemKind::PullRequest => {
                    if let Some(pr) = cache.pull_requests.iter_mut().find(|p| p.number == number) {
                        pr.body = new_body.to_string();
                    }
                }
            }
        }
    }

    fn select_older_commit(&mut self) {
        if let View::Detail(ref mut view) = self.view {
            view.select_older_commit(self.repository);
        } else if let View::UserCommand(ref mut view) = self.view {
            view.select_older_commit(
                self.repository,
                self.app_status.view_area,
                build_external_command_parameters_and_exec_command,
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
                build_external_command_parameters_and_exec_command,
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
                build_external_command_parameters_and_exec_command,
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
            RefreshViewContext::Refs {
                refs_context,
                origin,
                ..
            } => {
                // origin 只是 close Refs 時「要回哪」的記號，不需要先把 view 切成 Detail
                // 再立刻被 open_refs_with_origin take 走 list_state——那會白跑一次 git diff。
                self.open_refs_with_origin(origin);
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

    fn open_url(&self, url: String) {
        match crate::external::open_url(&url) {
            Ok(()) => {
                self.ec.send(AppEvent::NotifyInfo(format!("Opening {url}")));
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
        // 預先 set pending flag，讓 git watcher 在 debounce 視窗內偵測到的 fs 事件
        // 被吞掉；主動 refresh 走完後，watcher 不會重複觸發 slow-path。
        self.ec.mark_pending_refresh();

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
                    tx.send(AppEvent::AutoRefresh);
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

fn build_external_command_parameters_and_exec_command(
    commit: &Commit,
    refs: &[Ref],
    user_command_number: usize,
    view_area: Rect,
    ctx: &AppContext,
) -> Result<String, String> {
    build_external_command_parameters(commit, refs, user_command_number, view_area, ctx)
        .and_then(exec_user_command)
}

fn build_external_command_parameters<'a>(
    commit: &'a Commit,
    refs: &'a [Ref],
    user_command_number: usize,
    view_area: Rect,
    ctx: &'a AppContext,
) -> Result<ExternalCommandParameters<'a>, String> {
    let command = &extract_user_command_by_number(user_command_number, ctx)?.commands;
    let target_hash = commit.commit_hash.as_str();
    let parent_hashes = commit
        .parent_commit_hashes
        .iter()
        .map(|c| c.as_str())
        .collect();

    let mut all_refs = vec![];
    let mut branches = vec![];
    let mut remote_branches = vec![];
    let mut tags = vec![];
    for r in refs {
        match r {
            Ref::Tag { .. } => tags.push(r.name()),
            Ref::Branch { .. } => branches.push(r.name()),
            Ref::RemoteBranch { .. } => remote_branches.push(r.name()),
            Ref::Stash { .. } => continue, // skip stashes
        }
        all_refs.push(r.name());
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

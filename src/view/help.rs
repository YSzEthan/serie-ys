use std::{borrow::Cow, rc::Rc};

use rust_i18n::t;

use ratatui::{
    crossterm::event::KeyEvent,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Stylize},
    text::{Line, Span},
    widgets::{Block, Padding, Paragraph},
    Frame,
};

use crate::{
    app::AppContext,
    color::ColorTheme,
    config::CoreConfig,
    event::{AppEvent, Sender, UserEvent, UserEventWithCount},
    keybind::KeyBind,
    view::View,
};

#[derive(Debug, Default)]
struct HelpRow {
    keys: Line<'static>,
    desc: Line<'static>,
}

#[derive(Clone)]
struct BindingSpec {
    events: Vec<UserEvent>,
    desc: Cow<'static, str>,
}

fn b(events: Vec<UserEvent>, desc: Cow<'static, str>) -> BindingSpec {
    BindingSpec { events, desc }
}

/// 說明頁的分區。用 enum 而非字串當 key，是為了讓「新增一個分區」在
/// `title()` 與 `source_files()` 兩個窮盡 match 同時不編譯 —— 兩份可能
/// 不同步的清單被壓成一份不可能不同步的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HelpBlock {
    Common,
    Help,
    List,
    Detail,
    Refs,
    GitHub,
    CreateTag,
    DeleteTag,
    DeleteRef,
    UserCommand,
    Shell,
    ReleaseNotes,
}

impl HelpBlock {
    fn title(self) -> Cow<'static, str> {
        match self {
            HelpBlock::Common => t!("help.block.common"),
            HelpBlock::Help => t!("help.block.help"),
            HelpBlock::List => t!("help.block.list"),
            HelpBlock::Detail => t!("help.block.detail"),
            HelpBlock::Refs => t!("help.block.refs"),
            HelpBlock::GitHub => t!("help.block.github"),
            HelpBlock::CreateTag => t!("help.block.create_tag"),
            HelpBlock::DeleteTag => t!("help.block.delete_tag"),
            HelpBlock::DeleteRef => t!("help.block.delete_ref"),
            HelpBlock::UserCommand => t!("help.block.user_command"),
            HelpBlock::Shell => t!("help.block.shell"),
            HelpBlock::ReleaseNotes => t!("help.block.release_notes"),
        }
    }

    /// 實作這個分區 keymap 的原始碼。一致性測試據此比對宣稱與實作。
    #[cfg(test)]
    fn source_files(self) -> &'static [&'static str] {
        match self {
            // 共通鍵由事件迴圈直接處理，不屬於任何 view
            HelpBlock::Common => &["src/app.rs"],
            HelpBlock::Help => &["src/view/help.rs"],
            HelpBlock::List => &["src/view/list.rs"],
            HelpBlock::Detail => &["src/view/detail.rs"],
            HelpBlock::Refs => &["src/view/refs.rs"],
            HelpBlock::GitHub => &[
                "src/view/github/mod.rs",
                "src/view/github/event.rs",
                "src/view/github/render.rs",
                "src/view/github/preview.rs",
                "src/view/github/timeline.rs",
            ],
            HelpBlock::CreateTag => &["src/view/create_tag.rs"],
            HelpBlock::DeleteTag => &["src/view/delete_tag.rs"],
            HelpBlock::DeleteRef => &["src/view/delete_ref.rs"],
            HelpBlock::UserCommand => &["src/view/user_command.rs"],
            HelpBlock::Shell => &["src/view/shell.rs"],
            HelpBlock::ReleaseNotes => &["src/view/release_notes.rs"],
        }
    }
}

#[derive(Debug)]
pub struct HelpView<'a> {
    before: View<'a>,

    rows: Vec<HelpRow>,
    key_col_width: u16,

    offset: usize,
    height: usize,

    tx: Sender,
}

impl HelpView<'_> {
    pub fn new<'a>(before: View<'a>, ctx: Rc<AppContext>, tx: Sender) -> HelpView<'a> {
        let rows = build_rows(&ctx.color_theme, &ctx.keybind, &ctx.core_config);
        let key_col_width = rows
            .iter()
            .map(|r| r.keys.width())
            .max()
            .unwrap_or_default() as u16;
        HelpView {
            before,
            rows,
            key_col_width,
            offset: 0,
            height: 0,
            tx,
        }
    }

    pub fn handle_event(&mut self, event_with_count: UserEventWithCount, _: KeyEvent) {
        let event = event_with_count.event;
        let count = event_with_count.count;

        match event {
            UserEvent::Quit => {
                self.tx.send(AppEvent::Quit);
            }
            UserEvent::HelpToggle
            | UserEvent::Cancel
            | UserEvent::Close
            | UserEvent::NavigateLeft => {
                self.tx.send(AppEvent::CloseHelp);
            }
            UserEvent::NavigateDown | UserEvent::SelectDown => {
                for _ in 0..count {
                    self.scroll_down();
                }
            }
            UserEvent::NavigateUp | UserEvent::SelectUp => {
                for _ in 0..count {
                    self.scroll_up();
                }
            }
            _ => {}
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        self.update_state(area);

        let key_col = self.key_col_width + 4;
        let [keys_area, desc_area] =
            Layout::horizontal([Constraint::Length(key_col), Constraint::Min(10)]).areas(area);

        let visible = self
            .rows
            .iter()
            .skip(self.offset)
            .take(area.height as usize);
        let n = visible.clone().count();
        let mut keys_lines = Vec::with_capacity(n);
        let mut desc_lines = Vec::with_capacity(n);
        for r in visible {
            keys_lines.push(r.keys.clone());
            desc_lines.push(r.desc.clone());
        }

        let keys_paragraph = Paragraph::new(keys_lines)
            .block(Block::default().padding(Padding::new(3, 1, 0, 0)))
            .centered();
        let desc_paragraph = Paragraph::new(desc_lines)
            .block(Block::default().padding(Padding::new(1, 3, 0, 0)))
            .left_aligned();

        f.render_widget(keys_paragraph, keys_area);
        f.render_widget(desc_paragraph, desc_area);
    }
}

impl<'a> HelpView<'a> {
    pub fn take_before_view(&mut self) -> View<'a> {
        std::mem::take(&mut self.before)
    }

    pub(super) fn before_view_mut(&mut self) -> &mut View<'a> {
        &mut self.before
    }

    fn scroll_down(&mut self) {
        let max_offset = self.rows.len().saturating_sub(self.height);
        self.offset = self.offset.saturating_add(1).min(max_offset);
    }

    fn scroll_up(&mut self) {
        self.offset = self.offset.saturating_sub(1);
    }

    fn update_state(&mut self, area: Rect) {
        self.height = area.height as usize;
        let max_offset = self.rows.len().saturating_sub(self.height);
        self.offset = self.offset.min(max_offset);
    }
}

fn build_rows(
    color_theme: &ColorTheme,
    keybind: &KeyBind,
    core_config: &CoreConfig,
) -> Vec<HelpRow> {
    let blocks = help_blocks(keybind, core_config);
    let mut rows: Vec<HelpRow> = Vec::new();
    let n = blocks.len();
    for (i, (block, specs)) in blocks.into_iter().enumerate() {
        push_block(&mut rows, &block.title(), specs, color_theme, keybind);
        if i + 1 < n {
            rows.push(HelpRow::default());
        }
    }
    rows
}

/// 說明頁的全部內容 —— 純資料，不涉及渲染。一致性測試直接吃這份。
#[rustfmt::skip]
fn help_blocks(
    keybind: &KeyBind,
    core_config: &CoreConfig,
) -> Vec<(HelpBlock, Vec<BindingSpec>)> {
    let user_command_items: Vec<BindingSpec> = keybind
        .user_command_event_numbers()
        .into_iter()
        .flat_map(|n| {
            core_config
                .user_command
                .commands
                .get(&n.to_string())
                .map(|c| BindingSpec {
                    events: vec![UserEvent::UserCommand(n)],
                    desc: t!("help.common.user_command_execute", n = n, name = c.name),
                })
        })
        .collect();

    let common = vec![
        b(vec![UserEvent::ForceQuit], t!("help.common.force_quit")),
        b(vec![UserEvent::Quit], t!("help.common.quit_press_twice")),
        b(vec![UserEvent::HelpToggle], t!("help.common.open_help")),
        b(vec![UserEvent::CheckUpdate], t!("help.common.check_for_update")),
    ];

    let help = vec![
        b(vec![UserEvent::HelpToggle, UserEvent::Cancel, UserEvent::Close, UserEvent::NavigateLeft], t!("help.help.close_help")),
        b(vec![UserEvent::NavigateDown, UserEvent::SelectDown], t!("help.help.scroll_down")),
        b(vec![UserEvent::NavigateUp, UserEvent::SelectUp], t!("help.help.scroll_up")),
    ];

    let mut list = vec![
        b(vec![UserEvent::NavigateDown], t!("help.list.move_down")),
        b(vec![UserEvent::NavigateUp], t!("help.list.move_up")),
        b(vec![UserEvent::GoToTop], t!("help.list.go_to_top")),
        b(vec![UserEvent::GoToBottom], t!("help.list.go_to_bottom")),
        b(vec![UserEvent::GoToHead], t!("help.list.go_to_head")),
        b(vec![UserEvent::SelectDown], t!("help.list.scroll_down")),
        b(vec![UserEvent::SelectUp], t!("help.list.scroll_up")),
        b(vec![UserEvent::GoToParent], t!("help.list.select_parent_commit")),
        b(vec![UserEvent::GoToChild], t!("help.list.select_child_commit")),
        b(vec![UserEvent::Confirm, UserEvent::NavigateRight], t!("help.list.show_commit_details")),
        b(vec![UserEvent::RefList], t!("help.list.open_refs_list")),
        b(vec![UserEvent::Search], t!("help.list.start_search")),
        b(vec![UserEvent::Filter], t!("help.list.start_filter")),
        b(vec![UserEvent::Cancel], t!("help.list.cancel_search_filter")),
        b(vec![UserEvent::GoToNext], t!("help.list.go_to_next_search_match")),
        b(vec![UserEvent::GoToPrevious], t!("help.list.go_to_previous_search_match")),
        b(vec![UserEvent::FuzzyToggle], t!("help.list.toggle_fuzzy_match")),
        b(vec![UserEvent::IgnoreCaseToggle], t!("help.list.toggle_ignore_case")),
        b(vec![UserEvent::TargetToggle], t!("help.list.toggle_match_target_field")),
        b(vec![UserEvent::ShortCopy], t!("help.list.copy_commit_short_hash")),
        b(vec![UserEvent::FullCopy], t!("help.list.copy_commit_subject")),
        b(vec![UserEvent::BranchCopy], t!("help.list.copy_branch_name_prefer_local")),
        b(vec![UserEvent::FullBranchCopy], t!("help.list.copy_remote_branch_name")),
        b(vec![UserEvent::TagCopy], t!("help.list.copy_tag_name")),
        b(vec![UserEvent::CreateTag], t!("help.list.create_tag_on_commit")),
        b(vec![UserEvent::DeleteTag], t!("help.list.delete_tag_from_commit")),
        b(vec![UserEvent::DeleteRef], t!("help.list.delete_local_branch_from_commit")),
        b(vec![UserEvent::RemoteRefsToggle], t!("help.list.toggle_remote_refs")),
        b(vec![UserEvent::GitHubToggle], t!("help.list.open_github_issues_prs")),
        b(vec![UserEvent::Fetch], t!("help.list.fetch_all_remotes")),
        b(vec![UserEvent::Checkout], t!("help.list.checkout_selected_commit_ref")),
        b(vec![UserEvent::Refresh], t!("help.list.refresh")),
        b(vec![UserEvent::ShellToggle], t!("help.list.open_shell")),
    ];

    let detail = vec![
        b(vec![UserEvent::Cancel, UserEvent::Close, UserEvent::Confirm], t!("help.detail.close_commit_details")),
        b(vec![UserEvent::DetailPaneToggle], t!("help.detail.toggle_detail_pane")),
        b(vec![UserEvent::NavigateDown], t!("help.detail.scroll_down_move_file_cursor_in_files_pane")),
        b(vec![UserEvent::NavigateUp], t!("help.detail.scroll_up_move_file_cursor_in_files_pane")),
        b(vec![UserEvent::SelectDown], t!("help.detail.files_pane_scroll_diff_down")),
        b(vec![UserEvent::SelectUp], t!("help.detail.files_pane_scroll_diff_up")),
        b(vec![UserEvent::HalfPageDown], t!("help.detail.files_pane_scroll_diff_down_half_a_page")),
        b(vec![UserEvent::HalfPageUp], t!("help.detail.files_pane_scroll_diff_up_half_a_page")),
        b(vec![UserEvent::GoToNext], t!("help.detail.files_pane_go_to_next_hunk")),
        b(vec![UserEvent::GoToPrevious], t!("help.detail.files_pane_go_to_previous_hunk")),
        b(vec![UserEvent::PageDown], t!("help.detail.files_pane_scroll_diff_down_a_page")),
        b(vec![UserEvent::PageUp], t!("help.detail.files_pane_scroll_diff_up_a_page")),
        b(vec![UserEvent::NavigateRight], t!("help.detail.select_older_commit")),
        b(vec![UserEvent::NavigateLeft], t!("help.detail.select_newer_commit")),
        b(vec![UserEvent::GoToParent], t!("help.detail.select_parent_commit")),
        b(vec![UserEvent::GoToChild], t!("help.detail.select_child_commit")),
        b(vec![UserEvent::ShortCopy], t!("help.detail.copy_commit_short_hash")),
        b(vec![UserEvent::FullCopy], t!("help.detail.copy_commit_subject")),
        b(vec![UserEvent::BranchCopy], t!("help.detail.copy_branch_name_prefer_local")),
        b(vec![UserEvent::FullBranchCopy], t!("help.detail.copy_remote_branch_name")),
        b(vec![UserEvent::TagCopy], t!("help.detail.copy_tag_name")),
        b(vec![UserEvent::RemoteRefsToggle], t!("help.detail.toggle_remote_refs")),
        b(vec![UserEvent::RefList], t!("help.detail.open_refs_list")),
        // 這個一直都能用（`is_browsing_view()` 含 Detail，事件由 `global_app_event`
        // 在 App 層攔下），只是說明頁從來沒列出來。
        b(vec![UserEvent::GitHubToggle], t!("help.detail.open_github_issues_prs")),
        b(vec![UserEvent::HelpToggle], t!("help.detail.open_help")),
        b(vec![UserEvent::Refresh], t!("help.detail.refresh")),
        b(vec![UserEvent::ShellToggle], t!("help.detail.open_shell")),
    ];

    let refs = vec![
        b(vec![UserEvent::Cancel, UserEvent::RefList], t!("help.refs.close_refs_list")),
        b(vec![UserEvent::NavigateDown, UserEvent::SelectDown], t!("help.refs.move_down")),
        b(vec![UserEvent::NavigateUp, UserEvent::SelectUp], t!("help.refs.move_up")),
        b(vec![UserEvent::NavigateRight], t!("help.refs.open_node")),
        b(vec![UserEvent::NavigateLeft], t!("help.refs.close_node_close_refs")),
        b(vec![UserEvent::Checkout], t!("help.refs.checkout_selected_branch")),
        b(vec![UserEvent::DeleteRef, UserEvent::DeleteTag], t!("help.refs.delete_ref")),
        b(vec![UserEvent::Refresh], t!("help.refs.refresh")),
    ];

    let github = vec![
        b(vec![UserEvent::GitHubToggle, UserEvent::Cancel, UserEvent::Close], t!("help.github.close_github_view")),
        b(vec![UserEvent::RefList], t!("help.github.switch_issue_pr_tab")),
        b(vec![UserEvent::NavigateDown, UserEvent::SelectDown], t!("help.github.move_down")),
        b(vec![UserEvent::NavigateUp, UserEvent::SelectUp], t!("help.github.move_up")),
        b(vec![UserEvent::PageDown], t!("help.github.page_down")),
        b(vec![UserEvent::PageUp], t!("help.github.page_up")),
        b(vec![UserEvent::HalfPageDown], t!("help.github.half_page_down")),
        b(vec![UserEvent::HalfPageUp], t!("help.github.half_page_up")),
        b(vec![UserEvent::GoToTop], t!("help.github.go_to_top")),
        b(vec![UserEvent::GoToBottom], t!("help.github.go_to_bottom")),
        b(vec![UserEvent::Confirm, UserEvent::NavigateRight], t!("help.github.preview_toggle_checkbox")),
        b(vec![UserEvent::NavigateLeft], t!("help.github.back_cancel")),
        b(vec![UserEvent::Search], t!("help.github.search_type_number_to_jump_to_n")),
        b(vec![UserEvent::Filter], t!("help.github.filter")),
        b(vec![UserEvent::ShortCopy], t!("help.github.copy_issue_pr_url")),
        b(vec![UserEvent::FullCopy], t!("help.github.open_issue_pr_in_browser")),
        b(vec![UserEvent::TagCopy], t!("help.github.copy_issue_pr_number_n")),
        b(vec![UserEvent::DetailPaneToggle], t!("help.github.open_related_issue_pr_picker")),
        b(vec![UserEvent::Refresh], t!("help.github.refresh")),
        b(vec![UserEvent::MergePr], t!("help.github.3_stage_merge_pr_pick_method_delete_branch_confirm")),
        b(vec![UserEvent::ToggleIssueState], t!("help.github.close_reopen_issue_or_pr")),
        b(vec![UserEvent::TogglePrDraft], t!("help.github.mark_pr_ready_back_to_draft")),
        b(vec![UserEvent::ToggleCommitLog], t!("help.github.expand_collapse_commit_log")),
        b(vec![UserEvent::CreateTag], t!("help.github.toggle_label_names_color_swatches")),
    ];

    let create_tag = vec![
        b(vec![UserEvent::Confirm], t!("help.create_tag.confirm_create")),
        b(vec![UserEvent::Cancel], t!("help.create_tag.cancel_and_close")),
        b(vec![UserEvent::NavigateDown, UserEvent::NavigateUp], t!("help.create_tag.switch_input_field")),
        b(vec![UserEvent::NavigateRight, UserEvent::NavigateLeft], t!("help.create_tag.toggle_push_option")),
    ];

    let delete_tag = vec![
        b(vec![UserEvent::Confirm], t!("help.delete_tag.confirm_delete")),
        b(vec![UserEvent::Cancel], t!("help.delete_tag.cancel_and_close")),
        b(vec![UserEvent::NavigateDown, UserEvent::SelectDown], t!("help.delete_tag.select_next_tag")),
        b(vec![UserEvent::NavigateUp, UserEvent::SelectUp], t!("help.delete_tag.select_previous_tag")),
        b(vec![UserEvent::NavigateRight, UserEvent::NavigateLeft], t!("help.delete_tag.toggle_delete_from_remote")),
    ];

    let delete_ref = vec![
        b(vec![UserEvent::Confirm], t!("help.delete_ref.confirm_delete_ref")),
        b(vec![UserEvent::Cancel], t!("help.delete_ref.cancel")),
        b(vec![UserEvent::NavigateRight, UserEvent::NavigateLeft, UserEvent::NavigateDown], t!("help.delete_ref.toggle_yes_no")),
    ];

    let mut user_command = vec![
        b(vec![UserEvent::Cancel, UserEvent::Close], t!("help.user_command.close_user_command")),
        b(vec![UserEvent::NavigateDown, UserEvent::SelectDown], t!("help.user_command.scroll_down")),
        b(vec![UserEvent::NavigateUp, UserEvent::SelectUp], t!("help.user_command.scroll_up")),
        b(vec![UserEvent::PageDown], t!("help.user_command.scroll_page_down")),
        b(vec![UserEvent::PageUp], t!("help.user_command.scroll_page_up")),
        b(vec![UserEvent::HalfPageDown], t!("help.user_command.scroll_half_page_down")),
        b(vec![UserEvent::HalfPageUp], t!("help.user_command.scroll_half_page_up")),
        b(vec![UserEvent::GoToTop], t!("help.user_command.go_to_top")),
        b(vec![UserEvent::GoToBottom], t!("help.user_command.go_to_bottom")),
        b(vec![UserEvent::GoToParent], t!("help.user_command.select_parent_commit")),
        b(vec![UserEvent::GoToChild], t!("help.user_command.select_child_commit")),
        b(vec![UserEvent::Refresh], t!("help.user_command.refresh")),
        b(vec![UserEvent::Confirm], t!("help.user_command.show_commit_details")),
        b(vec![UserEvent::HelpToggle], t!("help.user_command.open_help")),
    ];
    list.extend(user_command_items.iter().cloned());
    user_command.extend(user_command_items);

    let shell = vec![
        b(vec![UserEvent::Confirm], t!("help.shell.run_command")),
        b(vec![UserEvent::Cancel], t!("help.shell.close_shell")),
    ];

    let release_notes = vec![
        b(vec![UserEvent::Quit], t!("help.release_notes.quit_press_twice")),
        b(vec![UserEvent::Cancel, UserEvent::Close, UserEvent::NavigateLeft], t!("help.release_notes.close_release_notes")),
        b(vec![UserEvent::NavigateDown, UserEvent::SelectDown], t!("help.release_notes.scroll_down")),
        b(vec![UserEvent::NavigateUp, UserEvent::SelectUp], t!("help.release_notes.scroll_up")),
        b(vec![UserEvent::PageDown], t!("help.release_notes.scroll_page_down")),
        b(vec![UserEvent::PageUp], t!("help.release_notes.scroll_page_up")),
        b(vec![UserEvent::HalfPageDown], t!("help.release_notes.scroll_half_page_down")),
        b(vec![UserEvent::HalfPageUp], t!("help.release_notes.scroll_half_page_up")),
    ];

    vec![
        (HelpBlock::Common,       common),
        (HelpBlock::Help,         help),
        (HelpBlock::List,         list),
        (HelpBlock::Detail,       detail),
        (HelpBlock::Refs,         refs),
        (HelpBlock::GitHub,       github),
        (HelpBlock::CreateTag,    create_tag),
        (HelpBlock::DeleteTag,    delete_tag),
        (HelpBlock::DeleteRef,    delete_ref),
        (HelpBlock::UserCommand,  user_command),
        (HelpBlock::Shell,        shell),
        (HelpBlock::ReleaseNotes, release_notes),
    ]
}

fn push_block(
    rows: &mut Vec<HelpRow>,
    title: &str,
    specs: Vec<BindingSpec>,
    color_theme: &ColorTheme,
    keybind: &KeyBind,
) {
    rows.push(HelpRow {
        keys: Line::from(format!("── {title} ──"))
            .fg(color_theme.help_block_title_fg)
            .add_modifier(Modifier::BOLD),
        desc: Line::default(),
    });
    for spec in specs {
        let keys = join_span_groups_with_space(
            spec.events
                .iter()
                .flat_map(|event| keybind.keys_for_event(*event))
                .map(|key| vec!["<".into(), key.fg(color_theme.help_key_fg), ">".into()])
                .collect(),
        );
        rows.push(HelpRow {
            keys,
            desc: Line::raw(spec.desc),
        });
    }
}

fn join_span_groups_with_space(span_groups: Vec<Vec<Span<'static>>>) -> Line<'static> {
    let mut spans: Vec<Span> = Vec::new();
    let n = span_groups.len();
    for (i, ss) in span_groups.into_iter().enumerate() {
        spans.extend(ss);
        if i < n - 1 {
            spans.push(Span::raw(" "));
        }
    }
    Line::from(spans)
}

/// 說明頁宣稱的鍵位與實作的一致性檢查。
///
/// 這是**靜態近似**：比對的是「原始碼裡有沒有出現這個 event 名稱」，不是
/// 「這個 event 真的被 handle_event 處理」。名稱出現在 `status_hints()` 或
/// 註解裡也會算數，所以只會漏抓、不會誤殺。
///
/// 真正的解是讓 `handle_event` 回報自己消化了什麼（keymap 契約測試），但那
/// 需要為 9 個 view 建 fixture，且 `handle_event` 目前回傳 `()`、`AppEvent`
/// 沒有 `PartialEq`，「未處理」與「處理了但無外顯副作用」無法區分。等到有
/// 足夠理由付那個成本再說。
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// `UserEvent` 的變體名稱。`UserCommand(1)` → `"UserCommand"`，
    /// 好對上原始碼裡的 `UserEvent::UserCommand(_)`。
    fn event_name(event: UserEvent) -> String {
        let debug = format!("{event:?}");
        match debug.split_once('(') {
            Some((name, _)) => name.to_string(),
            None => debug,
        }
    }

    /// 掃出原始碼中所有 `UserEvent::Xxx` 的變體名稱。
    fn events_in_source(src: &str) -> BTreeSet<String> {
        const PREFIX: &str = "UserEvent::";
        src.match_indices(PREFIX)
            .map(|(i, _)| {
                src[i + PREFIX.len()..]
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect::<String>()
            })
            .filter(|s| !s.is_empty())
            .collect()
    }

    fn read_source(rel_path: &str) -> String {
        let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), rel_path);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("讀不到 {path}: {e}"))
    }

    fn blocks() -> Vec<(HelpBlock, Vec<BindingSpec>)> {
        help_blocks(&KeyBind::new(None), &CoreConfig::default())
    }

    /// 說明頁列出的每個動作都必須真的綁著按鍵。
    ///
    /// 沒綁鍵的條目在畫面上是「說明文字 + 空白按鍵欄」，使用者看得到功能卻按不出來。
    #[test]
    fn every_claimed_event_has_a_key() {
        let keybind = KeyBind::new(None);
        let mut unbound = Vec::new();
        for (block, specs) in blocks() {
            for spec in &specs {
                for event in &spec.events {
                    if keybind.keys_for_event(*event).is_empty() {
                        unbound.push(format!(
                            "「{}」的『{}』列出 {:?}，但沒有任何按鍵綁定",
                            block.title(),
                            spec.desc,
                            event
                        ));
                    }
                }
            }
        }
        assert!(unbound.is_empty(), "\n{}", unbound.join("\n"));
    }

    /// 狀態列提示是**第二份**手寫清單，`source_events_are_all_claimed` 涵蓋不到它的
    /// 漂移：那個測試的粒度是「檔案」，分不出 detail 的 Info 與 Files 兩組提示，
    /// 而且它的比對方向反過來還會被提示表餵成假性通過（提示表裡出現的名字會讓
    /// `claimed_events_exist_in_source` 以為說明頁的宣稱有原始碼佐證）。
    ///
    /// 所以這裡直接把提示表本身當受測對象：每個提示的每個 event 都必須
    /// (a) 真的綁著鍵，(b) 也在對應的說明頁分區列出。
    #[test]
    fn status_hints_are_bound_and_documented() {
        use crate::view::detail::status_hints_for;
        use crate::widget::commit_detail::DetailPane;

        let keybind = KeyBind::new(None);
        let claimed_in = |block: HelpBlock| -> BTreeSet<String> {
            blocks()
                .into_iter()
                .filter(|(b, _)| *b == block)
                .flat_map(|(_, specs)| specs)
                .flat_map(|s| s.events.into_iter().map(event_name))
                .collect()
        };

        let mut problems = Vec::new();
        let mut check = |block: HelpBlock, hints: Vec<crate::widget::HintSpec>| {
            let claimed = claimed_in(block);
            for (events, desc) in hints {
                for event in events {
                    if keybind.display_key(*event).is_none() {
                        problems.push(format!(
                            "「{}」的狀態列提示『{desc}』用了 {event:?}，但它沒有綁定按鍵",
                            block.title()
                        ));
                    }
                    if !claimed.contains(&event_name(*event)) {
                        problems.push(format!(
                            "「{}」的狀態列提示『{desc}』用了 {event:?}，但說明頁沒有列出",
                            block.title()
                        ));
                    }
                }
            }
        };

        check(HelpBlock::Detail, status_hints_for(DetailPane::Info));
        check(HelpBlock::Detail, status_hints_for(DetailPane::Files));
        check(
            HelpBlock::UserCommand,
            crate::view::user_command::status_hints(),
        );
        check(
            HelpBlock::GitHub,
            crate::view::github::GitHubView::every_status_hint(),
        );

        assert!(problems.is_empty(), "\n{}", problems.join("\n"));
    }

    /// 說明頁宣稱的每個動作，都必須出現在該 view 的原始碼裡（不可亂宣稱）。
    #[test]
    fn claimed_events_exist_in_source() {
        let mut missing = Vec::new();
        for (block, specs) in blocks() {
            // 說明頁分區的資料就住在本檔，自我比對是恆真式，沒有驗證價值
            if block == HelpBlock::Help {
                continue;
            }
            let sources: BTreeSet<String> = block
                .source_files()
                .iter()
                .flat_map(|f| events_in_source(&read_source(f)))
                .collect();
            for spec in &specs {
                for event in &spec.events {
                    let name = event_name(*event);
                    if !sources.contains(&name) && !app_level_ok(block, *event) {
                        missing.push(format!(
                            "「{}」宣稱 {:?}，但 {:?} 裡找不到",
                            block.title(),
                            event,
                            block.source_files()
                        ));
                    }
                }
            }
        }
        assert!(missing.is_empty(), "\n{}", missing.join("\n"));
    }

    /// view 實際處理的每個動作，說明頁都必須列出（不可漏宣稱）。
    ///
    /// 漏宣稱比亂宣稱嚴重：亂宣稱使用者按了沒反應會發現，漏宣稱使用者永遠
    /// 不知道有這個功能。
    #[test]
    fn source_events_are_all_claimed() {
        let mut unclaimed = Vec::new();
        for (block, specs) in blocks() {
            if block == HelpBlock::Help || block == HelpBlock::Common {
                continue;
            }
            let claimed: BTreeSet<String> = specs
                .iter()
                .flat_map(|s| s.events.iter().map(|e| event_name(*e)))
                .collect();
            for file in block.source_files() {
                for name in events_in_source(&read_source(file)) {
                    if !claimed.contains(&name)
                        && !NOT_USER_FACING.contains(&name.as_str())
                        && !dynamically_claimed(block, &name)
                    {
                        unclaimed.push(format!(
                            "{file} 處理 UserEvent::{name}，但「{}」沒有列出",
                            block.title()
                        ));
                    }
                }
            }
        }
        assert!(unclaimed.is_empty(), "\n{}", unclaimed.join("\n"));
    }

    /// 這些 event 由 `App` 的事件迴圈直接處理，不會進 view —— 但只在
    /// `is_browsing_view()` 的三個 view 生效（見 `view::views::is_browsing_view`）。
    fn app_level_ok(block: HelpBlock, event: UserEvent) -> bool {
        matches!(block, HelpBlock::List | HelpBlock::Detail | HelpBlock::Refs)
            && matches!(event, UserEvent::HelpToggle | UserEvent::GitHubToggle)
    }

    /// mdBook 上的鍵位頁。內容由本測試產生 —— 那是全 repo 唯一有讀者的鍵位文件
    /// （`docs/src/SUMMARY.md` 收錄、README 指過去）。
    const DOC_PATH: &str = "docs/src/keybindings/index.md";

    /// 無法透過 config 變更的按鍵。它們散落在 `app.rs` 的 status-line modal
    /// 處理常式裡，不經過 `KeyBind`，所以進不了 `help_blocks`。
    /// 不揭露就是產出一份「看起來完整但不完整」的文件。
    const HARDCODED_KEYS_SECTION: &str = "\
## 寫死的按鍵

以下按鍵無法透過設定檔變更，因為它們屬於一次性的提示互動，不歸任何 view 的 keymap 管。

| 按鍵 | 出現位置 | 動作 |
| --- | ----- | ------ |
| <kbd>1</kbd>–<kbd>9</kbd> | Ref／checkout／相關／branch 選擇器 | 選第 n 項 |
| <kbd>m</kbd> <kbd>s</kbd> <kbd>r</kbd> | Merge PR 提示（第 1 步） | merge/squash/rebase |
| <kbd>y</kbd> <kbd>n</kbd> | Merge PR 提示（第 2 步） | merge 後是否刪除該 branch |
| <kbd>f</kbd> | 刪除 branch 確認 | 強制刪除 |
| <kbd>Tab</kbd> <kbd>Shift-Tab</kbd> | 建立 Tag 對話框 | 在欄位間移動 |
| <kbd>Space</kbd> | 建立 Tag 對話框（checkbox） | 切換勾選狀態 |
| <kbd>↑</kbd> <kbd>↓</kbd> | Shell | 瀏覽指令歷史 |
| <kbd>PageUp</kbd> <kbd>PageDown</kbd> | Shell | 捲動輸出 pane |
";

    fn render_doc(keybind: &KeyBind, core_config: &CoreConfig) -> String {
        let mut out = String::new();
        out.push_str(
            "# 快捷鍵\n\n\
             <!-- 由 `cargo test` 從 `src/view/help.rs` 產生，請勿手動編輯。 -->\n\
             <!-- 重新產生：UPDATE_KEYBINDINGS_DOC=1 cargo test -->\n\n\
             在應用程式中按 <kbd>?</kbd> 可隨時查看這份清單，且已套用你自己的覆寫設定。\n\n\
             以下是預設值，修改方式請參閱[自訂快捷鍵](./custom-keybindings.md)。\n\n\
             ## 預設快捷鍵\n",
        );

        for (block, specs) in help_blocks(keybind, core_config) {
            out.push_str(&format!("\n### {}\n\n", block.title()));
            out.push_str("| 按鍵 | 說明 | 設定鍵名 |\n| --- | --- | --- |\n");
            for spec in specs {
                let keys: Vec<String> = spec
                    .events
                    .iter()
                    .flat_map(|e| keybind.keys_for_event(*e))
                    .map(|k| format!("<kbd>{k}</kbd>"))
                    .collect();
                let names: Vec<String> = spec
                    .events
                    .iter()
                    .filter_map(|e| e.config_name())
                    .map(|n| format!("`{n}`"))
                    .collect();
                out.push_str(&format!(
                    "| {} | {} | {} |\n",
                    keys.join(" "),
                    spec.desc,
                    names.join(" ")
                ));
            }
        }

        out.push('\n');
        out.push_str(HARDCODED_KEYS_SECTION);
        out
    }

    /// mdBook 的鍵位頁必須與 in-app help 一致。
    ///
    /// 這份文件過去是手寫的，漂移到幾乎每一項都錯（`/` vs `:`、`g` vs `i`、
    /// `Ctrl-e/y` vs `,`/`.`）。現在由 `help_blocks` 產生，不可能再各自演化。
    #[test]
    fn generated_doc_matches_committed_file() {
        let generated = render_doc(&KeyBind::new(None), &CoreConfig::default());
        let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), DOC_PATH);

        if std::env::var_os("UPDATE_KEYBINDINGS_DOC").is_some() {
            std::fs::write(&path, &generated).unwrap_or_else(|e| panic!("寫不進 {path}: {e}"));
            return;
        }

        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == generated,
            "{DOC_PATH} 與 in-app help 不一致。\n\
             執行 `UPDATE_KEYBINDINGS_DOC=1 cargo test` 重新產生。"
        );
    }

    /// 「User Command」與「Commit List」分區的 `UserCommand` 條目是依使用者設定的
    /// `user_command_N` 動態產生的（見 `help_blocks` 開頭的 `user_command_items`；
    /// list.rs 也處理 `UserCommand(n)` 以開啟該畫面）。若某份 keybind 沒有任何
    /// user command 綁定，這兩個分區就列不出 `UserCommand`，故在此豁免。
    fn dynamically_claimed(block: HelpBlock, name: &str) -> bool {
        matches!(block, HelpBlock::UserCommand | HelpBlock::List) && name == "UserCommand"
    }

    /// 出現在原始碼但不該出現在說明頁的 event。
    const NOT_USER_FACING: &[&str] = &[
        // 沒有對應按鍵的內部訊號：App 把「無綁定的按鍵」轉成這個丟給輸入模式的 view
        "Unknown",
    ];
}

use std::rc::Rc;

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
    cn: Line<'static>,
    keys: Line<'static>,
    en: Line<'static>,
}

struct BindingSpec {
    events: Vec<UserEvent>,
    cn: String,
    en: String,
}

fn b(events: Vec<UserEvent>, cn: &str, en: &str) -> BindingSpec {
    BindingSpec {
        events,
        cn: cn.to_string(),
        en: en.to_string(),
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

        let key_col = self.key_col_width + 2;
        let [cn_area, keys_area, en_area] = Layout::horizontal([
            Constraint::Min(10),
            Constraint::Length(key_col),
            Constraint::Min(10),
        ])
        .areas(area);

        let visible = self
            .rows
            .iter()
            .skip(self.offset)
            .take(area.height as usize);
        let n = visible.clone().count();
        let mut cn_lines = Vec::with_capacity(n);
        let mut keys_lines = Vec::with_capacity(n);
        let mut en_lines = Vec::with_capacity(n);
        for r in visible {
            cn_lines.push(r.cn.clone());
            keys_lines.push(r.keys.clone());
            en_lines.push(r.en.clone());
        }

        let cn_paragraph = Paragraph::new(cn_lines)
            .block(Block::default().padding(Padding::new(3, 1, 0, 0)))
            .right_aligned();
        let keys_paragraph = Paragraph::new(keys_lines)
            .block(Block::default().padding(Padding::new(1, 1, 0, 0)))
            .centered();
        let en_paragraph = Paragraph::new(en_lines)
            .block(Block::default().padding(Padding::new(1, 3, 0, 0)))
            .left_aligned();

        f.render_widget(cn_paragraph, cn_area);
        f.render_widget(keys_paragraph, keys_area);
        f.render_widget(en_paragraph, en_area);
    }
}

impl<'a> HelpView<'a> {
    pub fn take_before_view(&mut self) -> View<'a> {
        std::mem::take(&mut self.before)
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

#[rustfmt::skip]
fn build_rows(
    color_theme: &ColorTheme,
    keybind: &KeyBind,
    core_config: &CoreConfig,
) -> Vec<HelpRow> {
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
                    cn: format!("執行 user command {} - {}", n, c.name),
                    en: format!("Execute user command {} - {}", n, c.name),
                })
        })
        .collect();

    let common = vec![
        b(vec![UserEvent::ForceQuit],   "強制離開",      "Force quit"),
        b(vec![UserEvent::Quit],        "離開（按兩下）", "Quit (press twice)"),
        b(vec![UserEvent::HelpToggle],  "開啟說明",      "Open help"),
    ];

    let help = vec![
        b(vec![UserEvent::HelpToggle, UserEvent::Cancel, UserEvent::Close, UserEvent::NavigateLeft],
            "關閉說明", "Close help"),
        b(vec![UserEvent::NavigateDown, UserEvent::SelectDown], "向下捲動", "Scroll down"),
        b(vec![UserEvent::NavigateUp,   UserEvent::SelectUp],   "向上捲動", "Scroll up"),
    ];

    let list = vec![
        b(vec![UserEvent::NavigateDown],                          "向下移動",            "Move down"),
        b(vec![UserEvent::NavigateUp],                            "向上移動",            "Move up"),
        b(vec![UserEvent::ScrollDown],                            "graph 向下捲動",      "Scroll down"),
        b(vec![UserEvent::GoToParent],                            "graph 向上捲動",      "Scroll up"),
        b(vec![UserEvent::Confirm, UserEvent::NavigateRight],     "顯示 commit 詳情",    "Show commit details"),
        b(vec![UserEvent::RefList],                               "開啟 refs 清單",      "Open refs list"),
        b(vec![UserEvent::Search],                                "開始搜尋",            "Start search"),
        b(vec![UserEvent::Filter],                                "開始過濾",            "Start filter"),
        b(vec![UserEvent::Cancel],                                "取消搜尋／過濾",      "Cancel search/filter"),
        b(vec![UserEvent::GoToNext],                              "下一個符合項",        "Go to next search match"),
        b(vec![UserEvent::GoToPrevious],                          "上一個符合項",        "Go to previous search match"),
        b(vec![UserEvent::FuzzyToggle],                           "切換模糊比對",        "Toggle fuzzy match"),
        b(vec![UserEvent::ShortCopy],                             "複製 commit short hash", "Copy commit short hash"),
        b(vec![UserEvent::FullCopy],                              "複製 commit hash",    "Copy commit hash"),
        b(vec![UserEvent::BranchCopy],                            "複製 branch 名稱（優先 local）", "Copy branch name (prefer local)"),
        b(vec![UserEvent::FullBranchCopy],                        "複製 remote branch 名稱", "Copy remote branch name"),
        b(vec![UserEvent::TagCopy],                               "複製 tag 名稱",       "Copy tag name"),
        b(vec![UserEvent::CreateTag],                             "在 commit 上建立 tag", "Create tag on commit"),
        b(vec![UserEvent::DeleteTag],                             "刪除 commit 上的 tag", "Delete tag from commit"),
        b(vec![UserEvent::DeleteRef],                             "刪除 commit 上的 local branch", "Delete local branch from commit"),
        b(vec![UserEvent::RemoteRefsToggle],                      "切換 remote refs",    "Toggle remote refs"),
        b(vec![UserEvent::GitHubToggle],                          "開啟 GitHub issues/PRs", "Open GitHub issues/PRs"),
        b(vec![UserEvent::TaskListToggle],                        "切換工作清單",        "Toggle task list"),
        b(vec![UserEvent::Fetch],                                 "fetch 所有 remote",   "Fetch all remotes"),
        b(vec![UserEvent::Checkout],                              "checkout 選取的 commit/ref", "Checkout selected commit/ref"),
        b(vec![UserEvent::Refresh],                               "重新整理",            "Refresh"),
    ];

    let detail = vec![
        b(vec![UserEvent::Cancel, UserEvent::Close, UserEvent::Confirm], "關閉 commit 詳情", "Close commit details"),
        b(vec![UserEvent::DetailPaneToggle],                             "切換詳情區塊",     "Toggle detail pane"),
        b(vec![UserEvent::NavigateDown],                                 "向下捲動",         "Scroll down"),
        b(vec![UserEvent::NavigateUp],                                   "向上捲動",         "Scroll up"),
        b(vec![UserEvent::NavigateRight],                                "選擇較舊 commit",  "Select older commit"),
        b(vec![UserEvent::NavigateLeft],                                 "選擇較新 commit",  "Select newer commit"),
        b(vec![UserEvent::GoToParent],                                   "選擇 parent commit", "Select parent commit"),
        b(vec![UserEvent::ShortCopy],                                    "複製 commit short hash", "Copy commit short hash"),
        b(vec![UserEvent::FullCopy],                                     "複製 commit hash",  "Copy commit hash"),
        b(vec![UserEvent::BranchCopy],                                   "複製 branch 名稱（優先 local）", "Copy branch name (prefer local)"),
        b(vec![UserEvent::FullBranchCopy],                               "複製 remote branch 名稱", "Copy remote branch name"),
        b(vec![UserEvent::TagCopy],                                      "複製 tag 名稱",     "Copy tag name"),
        b(vec![UserEvent::RemoteRefsToggle],                             "切換 remote refs",  "Toggle remote refs"),
        b(vec![UserEvent::HelpToggle],                                   "開啟說明",          "Open help"),
        b(vec![UserEvent::Refresh],                                      "重新整理",          "Refresh"),
    ];

    let refs = vec![
        b(vec![UserEvent::Cancel],                    "關閉 refs 清單",         "Close refs list"),
        b(vec![UserEvent::NavigateDown],              "向下移動",               "Move down"),
        b(vec![UserEvent::NavigateUp],                "向上移動",               "Move up"),
        b(vec![UserEvent::NavigateRight],             "展開節點",               "Open node"),
        b(vec![UserEvent::NavigateLeft],              "收合節點／關閉",         "Close node / Close refs"),
        b(vec![UserEvent::UserCommand(1)],            "刪除 ref",               "Delete ref"),
        b(vec![UserEvent::Refresh],                   "重新整理",               "Refresh"),
    ];

    let github = vec![
        b(vec![UserEvent::GitHubToggle, UserEvent::Cancel, UserEvent::Close], "關閉 GitHub view", "Close GitHub view"),
        b(vec![UserEvent::RefList],                  "切換 Issue／PR 分頁",     "Switch issue/PR tab"),
        b(vec![UserEvent::NavigateDown, UserEvent::SelectDown], "向下移動",     "Move down"),
        b(vec![UserEvent::NavigateUp,   UserEvent::SelectUp],   "向上移動",     "Move up"),
        b(vec![UserEvent::PageDown],                  "向下一頁",               "Page down"),
        b(vec![UserEvent::PageUp],                    "向上一頁",               "Page up"),
        b(vec![UserEvent::HalfPageDown],              "向下半頁",               "Half page down"),
        b(vec![UserEvent::HalfPageUp],                "向上半頁",               "Half page up"),
        b(vec![UserEvent::GoToTop],                   "跳到頂端",               "Go to top"),
        b(vec![UserEvent::GoToBottom],                "跳到底端",               "Go to bottom"),
        b(vec![UserEvent::Confirm],                   "預覽內容／切換 checkbox", "Preview / toggle checkbox"),
        b(vec![UserEvent::Search],                    "搜尋",                   "Search"),
        b(vec![UserEvent::Filter],                    "過濾",                   "Filter"),
        b(vec![UserEvent::ShortCopy],                 "複製 issue/PR URL",      "Copy issue/PR URL"),
        b(vec![UserEvent::FullCopy],                  "在瀏覽器開啟 issue/PR",  "Open issue/PR in browser"),
        b(vec![UserEvent::BranchCopy],                "輸入 #num 開啟對應 issue/PR", "Open #num issue/PR"),
        b(vec![UserEvent::Refresh],                   "重新整理",               "Refresh"),
    ];

    let tag_edit = vec![
        b(vec![UserEvent::Confirm],                   "確定建立／刪除",          "Confirm create/delete"),
        b(vec![UserEvent::Cancel],                    "取消並關閉",              "Cancel and close"),
        b(vec![UserEvent::NavigateDown, UserEvent::NavigateUp], "切換輸入欄位",  "Switch input field"),
        b(vec![UserEvent::NavigateRight, UserEvent::NavigateLeft], "切換選項",  "Toggle option"),
    ];

    let delete_ref = vec![
        b(vec![UserEvent::Confirm],                                    "確定刪除 ref",     "Confirm delete ref"),
        b(vec![UserEvent::Cancel],                                     "取消",             "Cancel"),
        b(vec![UserEvent::NavigateRight, UserEvent::NavigateLeft, UserEvent::NavigateDown],
                                                                       "切換 yes／no",      "Toggle yes/no"),
    ];

    let mut user_command = vec![
        b(vec![UserEvent::Cancel, UserEvent::Close], "關閉 user command",  "Close user command"),
        b(vec![UserEvent::NavigateDown],              "向下捲動",           "Scroll down"),
        b(vec![UserEvent::NavigateUp],                "向上捲動",           "Scroll up"),
        b(vec![UserEvent::PageDown],                  "向下一頁",           "Scroll page down"),
        b(vec![UserEvent::PageUp],                    "向上一頁",           "Scroll page up"),
        b(vec![UserEvent::HalfPageDown],              "向下半頁",           "Scroll half page down"),
        b(vec![UserEvent::HalfPageUp],                "向上半頁",           "Scroll half page up"),
        b(vec![UserEvent::GoToTop],                   "跳到頂端",           "Go to top"),
        b(vec![UserEvent::GoToBottom],                "跳到底端",           "Go to bottom"),
        b(vec![UserEvent::SelectDown],                "選擇較舊 commit",    "Select older commit"),
        b(vec![UserEvent::SelectUp],                  "選擇較新 commit",    "Select newer commit"),
        b(vec![UserEvent::GoToParent],                "選擇 parent commit", "Select parent commit"),
        b(vec![UserEvent::Refresh],                   "重新整理",           "Refresh"),
        b(vec![UserEvent::Confirm],                   "顯示 commit 詳情",   "Show commit details"),
    ];
    user_command.extend(user_command_items);

    let blocks: Vec<(&str, Vec<BindingSpec>)> = vec![
        ("共通",        common),
        ("說明頁",      help),
        ("Commit 清單", list),
        ("Commit 詳情", detail),
        ("Refs 清單",   refs),
        ("GitHub View", github),
        ("Create/Delete Tag", tag_edit),
        ("Delete Ref",  delete_ref),
        ("User Command", user_command),
    ];

    let mut rows: Vec<HelpRow> = Vec::new();
    let n = blocks.len();
    for (i, (title, specs)) in blocks.into_iter().enumerate() {
        push_block(&mut rows, title, specs, color_theme, keybind);
        if i + 1 < n {
            rows.push(HelpRow::default());
        }
    }
    rows
}

fn push_block(
    rows: &mut Vec<HelpRow>,
    title: &str,
    specs: Vec<BindingSpec>,
    color_theme: &ColorTheme,
    keybind: &KeyBind,
) {
    rows.push(HelpRow {
        cn: Line::default(),
        keys: Line::from(format!("── {title} ──"))
            .fg(color_theme.help_block_title_fg)
            .add_modifier(Modifier::BOLD),
        en: Line::default(),
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
            cn: Line::raw(spec.cn),
            keys,
            en: Line::raw(spec.en),
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

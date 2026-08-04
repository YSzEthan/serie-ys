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

#[derive(Clone)]
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
}

impl HelpBlock {
    fn title(self) -> &'static str {
        match self {
            HelpBlock::Common => "共通",
            HelpBlock::Help => "說明頁",
            HelpBlock::List => "Commit 清單",
            HelpBlock::Detail => "Commit 詳情",
            HelpBlock::Refs => "Refs 清單",
            HelpBlock::GitHub => "GitHub View",
            HelpBlock::CreateTag => "Create Tag",
            HelpBlock::DeleteTag => "Delete Tag",
            HelpBlock::DeleteRef => "Delete Ref",
            HelpBlock::UserCommand => "User Command",
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
            HelpBlock::GitHub => &["src/view/github.rs"],
            HelpBlock::CreateTag => &["src/view/create_tag.rs"],
            HelpBlock::DeleteTag => &["src/view/delete_tag.rs"],
            HelpBlock::DeleteRef => &["src/view/delete_ref.rs"],
            HelpBlock::UserCommand => &["src/view/user_command.rs"],
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

fn build_rows(
    color_theme: &ColorTheme,
    keybind: &KeyBind,
    core_config: &CoreConfig,
) -> Vec<HelpRow> {
    let blocks = help_blocks(keybind, core_config);
    let mut rows: Vec<HelpRow> = Vec::new();
    let n = blocks.len();
    for (i, (block, specs)) in blocks.into_iter().enumerate() {
        push_block(&mut rows, block.title(), specs, color_theme, keybind);
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

    let mut list = vec![
        b(vec![UserEvent::NavigateDown],                          "向下移動",            "Move down"),
        b(vec![UserEvent::NavigateUp],                            "向上移動",            "Move up"),
        b(vec![UserEvent::GoToTop],                               "跳到頂端",            "Go to top"),
        b(vec![UserEvent::GoToBottom],                             "跳到底端",            "Go to bottom"),
        b(vec![UserEvent::GoToHead],                              "回到 HEAD",           "Go to HEAD"),
        b(vec![UserEvent::SelectDown],                            "graph 向下捲動",      "Scroll down"),
        b(vec![UserEvent::SelectUp],                              "graph 向上捲動",      "Scroll up"),
        b(vec![UserEvent::GoToParent],                            "選擇 parent commit",  "Select parent commit"),
        b(vec![UserEvent::Confirm, UserEvent::NavigateRight],     "顯示 commit 詳情",    "Show commit details"),
        b(vec![UserEvent::RefList],                               "開啟 refs 清單",      "Open refs list"),
        b(vec![UserEvent::Search],                                "開始搜尋",            "Start search"),
        b(vec![UserEvent::Filter],                                "開始過濾",            "Start filter"),
        b(vec![UserEvent::Cancel],                                "取消搜尋／過濾",      "Cancel search/filter"),
        b(vec![UserEvent::GoToNext],                              "下一個符合項",        "Go to next search match"),
        b(vec![UserEvent::GoToPrevious],                          "上一個符合項",        "Go to previous search match"),
        b(vec![UserEvent::FuzzyToggle],                           "切換模糊比對",        "Toggle fuzzy match"),
        b(vec![UserEvent::IgnoreCaseToggle],                      "切換大小寫忽略",      "Toggle ignore case"),
        b(vec![UserEvent::ShortCopy],                             "複製 commit short hash", "Copy commit short hash"),
        b(vec![UserEvent::FullCopy],                              "複製 commit subject", "Copy commit subject"),
        b(vec![UserEvent::BranchCopy],                            "複製 branch 名稱（優先 local）", "Copy branch name (prefer local)"),
        b(vec![UserEvent::FullBranchCopy],                        "複製 remote branch 名稱", "Copy remote branch name"),
        b(vec![UserEvent::TagCopy],                               "複製 tag 名稱",       "Copy tag name"),
        b(vec![UserEvent::CreateTag],                             "在 commit 上建立 tag", "Create tag on commit"),
        b(vec![UserEvent::DeleteTag],                             "刪除 commit 上的 tag", "Delete tag from commit"),
        b(vec![UserEvent::DeleteRef],                             "刪除 commit 上的 local branch", "Delete local branch from commit"),
        b(vec![UserEvent::RemoteRefsToggle],                      "切換 remote refs",    "Toggle remote refs"),
        b(vec![UserEvent::GitHubToggle],                          "開啟 GitHub issues/PRs", "Open GitHub issues/PRs"),
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
        b(vec![UserEvent::FullCopy],                                     "複製 commit subject", "Copy commit subject"),
        b(vec![UserEvent::BranchCopy],                                   "複製 branch 名稱（優先 local）", "Copy branch name (prefer local)"),
        b(vec![UserEvent::FullBranchCopy],                               "複製 remote branch 名稱", "Copy remote branch name"),
        b(vec![UserEvent::TagCopy],                                      "複製 tag 名稱",     "Copy tag name"),
        b(vec![UserEvent::RemoteRefsToggle],                             "切換 remote refs",  "Toggle remote refs"),
        b(vec![UserEvent::RefList],                                      "開啟 refs 清單",    "Open refs list"),
        b(vec![UserEvent::HelpToggle],                                   "開啟說明",          "Open help"),
        b(vec![UserEvent::Refresh],                                      "重新整理",          "Refresh"),
    ];

    let refs = vec![
        b(vec![UserEvent::Cancel],                    "關閉 refs 清單",         "Close refs list"),
        b(vec![UserEvent::NavigateDown, UserEvent::SelectDown], "向下移動",     "Move down"),
        b(vec![UserEvent::NavigateUp,   UserEvent::SelectUp],   "向上移動",     "Move up"),
        b(vec![UserEvent::NavigateRight],             "展開節點",               "Open node"),
        b(vec![UserEvent::NavigateLeft],              "收合節點／關閉",         "Close node / Close refs"),
        b(vec![UserEvent::Checkout],                  "checkout 選取的 branch", "Checkout selected branch"),
        b(vec![UserEvent::DeleteRef, UserEvent::DeleteTag], "刪除 ref",         "Delete ref"),
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
        b(vec![UserEvent::Confirm, UserEvent::NavigateRight], "預覽內容／切換 checkbox", "Preview / toggle checkbox"),
        b(vec![UserEvent::NavigateLeft],              "返回／取消",             "Back / cancel"),
        b(vec![UserEvent::Search],                    "搜尋／輸入純數字跳到 #N", "Search / type number to jump to #N"),
        b(vec![UserEvent::Filter],                    "過濾",                   "Filter"),
        b(vec![UserEvent::ShortCopy],                 "複製 issue/PR URL",      "Copy issue/PR URL"),
        b(vec![UserEvent::FullCopy],                  "在瀏覽器開啟 issue/PR",  "Open issue/PR in browser"),
        b(vec![UserEvent::TagCopy],                   "複製 issue/PR 編號 (#N)", "Copy issue/PR number (#N)"),
        b(vec![UserEvent::DetailPaneToggle],          "開啟相關 issue/PR 選單",  "Open related issue/PR picker"),
        b(vec![UserEvent::Refresh],                   "重新整理",               "Refresh"),
        b(vec![UserEvent::MergePr],                   "三階段 merge PR：選 method、刪 branch、確認", "3-stage merge PR: pick method, delete branch, confirm"),
        b(vec![UserEvent::ToggleIssueState],          "關閉／重開 issue 或 PR",  "Close/reopen issue or PR"),
        b(vec![UserEvent::TogglePrDraft],             "PR 定案／打回草稿",       "Mark PR ready / back to draft"),
        b(vec![UserEvent::ToggleCommitLog],           "展開／摺疊 commit 記錄",  "Expand/collapse commit log"),
    ];

    let create_tag = vec![
        b(vec![UserEvent::Confirm],                   "確定建立",                "Confirm create"),
        b(vec![UserEvent::Cancel],                    "取消並關閉",              "Cancel and close"),
        b(vec![UserEvent::NavigateDown, UserEvent::NavigateUp], "切換輸入欄位",  "Switch input field"),
        b(vec![UserEvent::NavigateRight, UserEvent::NavigateLeft], "切換 push 選項", "Toggle push option"),
    ];

    let delete_tag = vec![
        b(vec![UserEvent::Confirm],                   "確定刪除",                "Confirm delete"),
        b(vec![UserEvent::Cancel],                    "取消並關閉",              "Cancel and close"),
        b(vec![UserEvent::NavigateDown, UserEvent::SelectDown], "選擇下一個 tag", "Select next tag"),
        b(vec![UserEvent::NavigateUp,   UserEvent::SelectUp],   "選擇上一個 tag", "Select previous tag"),
        b(vec![UserEvent::NavigateRight, UserEvent::NavigateLeft], "切換「從 remote 刪除」", "Toggle delete from remote"),
    ];

    let delete_ref = vec![
        b(vec![UserEvent::Confirm],                                    "確定刪除 ref",     "Confirm delete ref"),
        b(vec![UserEvent::Cancel],                                     "取消",             "Cancel"),
        b(vec![UserEvent::NavigateRight, UserEvent::NavigateLeft, UserEvent::NavigateDown],
                                                                       "切換 yes／no",      "Toggle yes/no"),
    ];

    let mut user_command = vec![
        b(vec![UserEvent::Cancel, UserEvent::Close], "關閉 user command",  "Close user command"),
        b(vec![UserEvent::NavigateDown, UserEvent::SelectDown],   "向下捲動",            "Scroll down"),
        b(vec![UserEvent::NavigateUp,   UserEvent::SelectUp],     "向上捲動",            "Scroll up"),
        b(vec![UserEvent::PageDown],                  "向下一頁",           "Scroll page down"),
        b(vec![UserEvent::PageUp],                    "向上一頁",           "Scroll page up"),
        b(vec![UserEvent::HalfPageDown],              "向下半頁",           "Scroll half page down"),
        b(vec![UserEvent::HalfPageUp],                "向上半頁",           "Scroll half page up"),
        b(vec![UserEvent::GoToTop],                   "跳到頂端",           "Go to top"),
        b(vec![UserEvent::GoToBottom],                "跳到底端",           "Go to bottom"),
        b(vec![UserEvent::GoToParent],                "選擇 parent commit", "Select parent commit"),
        b(vec![UserEvent::Refresh],                   "重新整理",           "Refresh"),
        b(vec![UserEvent::Confirm],                   "顯示 commit 詳情",   "Show commit details"),
        b(vec![UserEvent::HelpToggle],                "開啟說明",           "Open help"),
    ];
    list.extend(user_command_items.iter().cloned());
    user_command.extend(user_command_items);

    vec![
        (HelpBlock::Common,      common),
        (HelpBlock::Help,        help),
        (HelpBlock::List,        list),
        (HelpBlock::Detail,      detail),
        (HelpBlock::Refs,        refs),
        (HelpBlock::GitHub,      github),
        (HelpBlock::CreateTag,   create_tag),
        (HelpBlock::DeleteTag,   delete_tag),
        (HelpBlock::DeleteRef,   delete_ref),
        (HelpBlock::UserCommand, user_command),
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
                            spec.cn,
                            event
                        ));
                    }
                }
            }
        }
        assert!(unbound.is_empty(), "\n{}", unbound.join("\n"));
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
| <kbd>1</kbd>–<kbd>9</kbd> | Ref／checkout／關聯／branch 選擇器 | 選第 n 項 |
| <kbd>m</kbd> <kbd>s</kbd> <kbd>r</kbd> | Merge PR 提示（第 1 步） | merge／squash／rebase |
| <kbd>y</kbd> <kbd>n</kbd> | Merge PR 提示（第 2 步） | merge 後是否刪除該 branch |
| <kbd>f</kbd> | 刪除 branch 確認 | 強制刪除 |
| <kbd>Tab</kbd> <kbd>Shift-Tab</kbd> | Create tag 對話框 | 在欄位間移動 |
| <kbd>Space</kbd> | Create tag 對話框（核取方塊） | 切換核取狀態 |
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
                    spec.cn,
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

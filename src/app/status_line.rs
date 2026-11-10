use std::{borrow::Cow, path::PathBuf, rc::Rc};

use rust_i18n::t;

use ratatui::{
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Padding, Paragraph},
    Frame,
};

use crate::{
    color::MUTED_FG,
    config::CursorType,
    event::{
        AppEvent, AutoFetchClock, CheckoutPickKind, RefCopyKind, RelatedItem, Sender, UserEvent,
    },
    github::{GhItemKind, MergeMethod, PrDraftAction, RerunScope, StateAction, StateFilter},
    keybind::KeyBind,
    reload::FullStatus,
    view::View,
    widget::{
        commit_list::ChildPickOption, fit_key_groups, h, hint_line, hint_pairs, truncate_line,
        wrap_key_groups, HintSpec,
    },
};

use super::AppContext;

/// 輸入框要顯示的整串，以及游標前那段內容。
///
/// Virtual 游標字元接在切點上而不是事後覆寫，游標下的字元才不會被蓋掉；
/// 接成單一字串（不拆 span）是為了讓 `truncate_line` 的截斷行為跟沒有游標
/// 時一樣，不會整段丟掉游標後的內容。`cursor` 不在 char boundary 時退回
/// 尾端——每一幀都會跑，不能 panic。
fn input_display<'a>(
    msg: &'a str,
    cursor: usize,
    cursor_type: &CursorType,
) -> (&'a str, Cow<'a, str>) {
    let (before, after) = msg.split_at_checked(cursor).unwrap_or((msg, ""));
    match cursor_type {
        CursorType::Native => (before, Cow::Borrowed(msg)),
        CursorType::Virtual(c) => (before, Cow::Owned(format!("{before}{c}{after}"))),
    }
}

/// `RestartPrompt` 取消後留下的提醒；`maybe_open_restart_prompt`（app.rs）
/// 守衛沒過時也是同一句，兩處共用同一份字面值不會漂移。
pub(super) fn update_installed_hint() -> Cow<'static, str> {
    t!("app.status.update_installed")
}

/// `AppEvent::AutoFetchCompleted` 顯示用的固定文案，唯一生產者是
/// `app.rs` 的處理端；`auto_fetch` 模組不帶 payload，見該事件的文件。
pub(super) fn auto_fetch_success_msg() -> Cow<'static, str> {
    t!("app.status.auto_fetched")
}

/// 使用者沒有按下重啟：更新其實已經做完了，留一句話提醒手動重啟。
fn restart_skipped_notice() -> StatusLine {
    StatusLine::NotificationSuccess(update_installed_hint().into_owned())
}

fn picker_digit_index(key: KeyEvent) -> Option<usize> {
    let KeyCode::Char(c) = key.code else {
        return None;
    };
    let digit = c.to_digit(10)?;
    (digit as usize).checked_sub(1)
}

/// 彈窗選單的游標與勾選位置。放在各個選單變體裡而不是 `StatusLineState`，
/// 狀態才會跟著變體一起產生、一起消失（例如 `DeleteBranchPicker` 轉成
/// `DeleteBranchConfirm` 時不會殘留上一個畫面的游標）。
#[derive(Debug, Clone, Copy, Default)]
struct Menu {
    cursor: usize,
    marked: Option<usize>,
}

/// 選單的一列。`quick` 是直接執行的字母快選鍵（數字鍵另外處理）。
struct MenuItem {
    quick: Option<char>,
    label: String,
    /// 已結束（closed／merged）的項目用暗色顯示。
    dim: bool,
    /// 這一列前面要插入的分組標題（不可選）。
    group: Option<String>,
}

impl MenuItem {
    fn new(label: impl Into<String>) -> Self {
        Self {
            quick: None,
            label: label.into(),
            dim: false,
            group: None,
        }
    }

    fn quick(mut self, key: char) -> Self {
        self.quick = Some(key);
        self
    }
}

/// 一個選單彈窗要畫的全部內容。`handle_menu_key` 與 `render_menu_popup`
/// 都從這裡取，列數與快選表只在 [`StatusLine::menu_spec`] 算一次。
struct MenuSpec {
    title: String,
    items: Vec<MenuItem>,
    /// 不可選的尾註，例如「還有 N 項」。
    footer: Option<String>,
}

/// 標題原本是狀態列上的提示句（結尾帶冒號或空白），放進框線標題前先清掉。
fn clean_title(s: &str) -> String {
    s.trim_end_matches([' ', ':', '：']).to_string()
}

/// `y`／`n` 二選一的選單：第一列確認，第二列取消。
fn yes_no_items(confirm: Cow<'static, str>) -> Vec<MenuItem> {
    vec![
        MenuItem::new(confirm).quick('y'),
        MenuItem::new(t!("common.hint.cancel")).quick('n'),
    ]
}

/// CI 重跑可選的範圍，順序就是選單的列順序；空的那一項不出現。
fn ci_rerun_scopes(failed_runs: &[u64], all_runs: &[u64]) -> Vec<RerunScope> {
    [
        (!failed_runs.is_empty()).then_some(RerunScope::FailedOnly),
        (!all_runs.is_empty()).then_some(RerunScope::All),
    ]
    .into_iter()
    .flatten()
    .collect()
}

enum MenuKey {
    Cancel,
    Pick(usize),
    Consumed,
}

/// 選單按鍵的共同規則。順序有意義：
///
/// 1. 沒有任何選項時只認取消，避免後面對 `len` 取餘數 panic。
/// 2. 字母快選與數字鍵先於 `Cancel`／`Confirm`——預設 keybind 的
///    `y`／`n` 同時屬於這兩個事件，不先比對就會被吃掉。快選比對原始字元，
///    所以 `f` 被綁成別的功能也不受影響；帶 Ctrl／Alt 時不算快選。
/// 3. `Enter` 只送出已勾選的列，沒勾就什麼都不做（double check）。
fn menu_key(menu: &mut Menu, quick: &[Option<char>], key: KeyEvent, keybind: &KeyBind) -> MenuKey {
    let len = quick.len();
    let cancel = keybind.has(&key, UserEvent::Cancel) || keybind.has(&key, UserEvent::NavigateLeft);
    if len == 0 {
        return if cancel {
            MenuKey::Cancel
        } else {
            MenuKey::Consumed
        };
    }
    let plain = !key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
    if plain {
        if let KeyCode::Char(c) = key.code {
            if let Some(idx) = quick
                .iter()
                .position(|q| q.is_some_and(|q| q.eq_ignore_ascii_case(&c)))
            {
                return MenuKey::Pick(idx);
            }
        }
        if let Some(idx) = picker_digit_index(key).filter(|&i| i < len) {
            return MenuKey::Pick(idx);
        }
    }
    if cancel {
        return MenuKey::Cancel;
    }
    if keybind.has(&key, UserEvent::NavigateDown) {
        menu.cursor = (menu.cursor + 1) % len;
    } else if keybind.has(&key, UserEvent::NavigateUp) {
        menu.cursor = (menu.cursor + len - 1) % len;
    } else if keybind.has(&key, UserEvent::NavigateRight) {
        menu.marked = (menu.marked != Some(menu.cursor)).then_some(menu.cursor);
    } else if keybind.has(&key, UserEvent::Confirm) || key.code == KeyCode::Enter {
        if let Some(idx) = menu.marked {
            return MenuKey::Pick(idx);
        }
    }
    MenuKey::Consumed
}

fn build_hotkey_hints(view: &View, ctx: &AppContext, max_width: usize) -> Line<'static> {
    let hints: Vec<HintSpec> = match view {
        View::List(_) => vec![
            h(&[UserEvent::Search], t!("common.hint.search")),
            h(&[UserEvent::Filter], t!("common.hint.filter")),
            h(&[UserEvent::IgnoreCaseToggle], t!("common.hint.case")),
            h(&[UserEvent::FuzzyToggle], t!("common.hint.fuzzy")),
            h(&[UserEvent::TargetToggle], t!("common.hint.target")),
            h(&[UserEvent::CreateTag], t!("common.hint.tag")),
            h(&[UserEvent::RefList], t!("common.hint.refs")),
            h(&[UserEvent::RemoteRefsToggle], t!("common.hint.remote")),
            h(&[UserEvent::CondensedToggle], t!("common.hint.condensed")),
            h(&[UserEvent::GitHubToggle], t!("common.hint.github")),
            h(&[UserEvent::Refresh], t!("common.hint.refresh")),
            h(&[UserEvent::HelpToggle], t!("common.hint.help")),
        ],
        View::Detail(ref view) => view.status_hints(),
        View::Refs(_) => vec![
            h(
                &[UserEvent::NavigateDown, UserEvent::NavigateUp],
                t!("common.hint.move"),
            ),
            h(&[UserEvent::Checkout], t!("common.hint.checkout")),
            h(&[UserEvent::DeleteRef], t!("common.hint.delete")),
            h(&[UserEvent::Refresh], t!("common.hint.refresh")),
            h(&[UserEvent::HelpToggle], t!("common.hint.help")),
            h(
                &[UserEvent::Cancel, UserEvent::RefList],
                t!("common.hint.close"),
            ),
        ],
        View::CreateTag(_) | View::DeleteTag(_) | View::DeleteRef(_) => vec![
            h(&[UserEvent::Confirm], t!("common.hint.confirm")),
            h(&[UserEvent::Cancel], t!("common.hint.cancel")),
        ],
        View::UserCommand(_) => crate::view::user_command::status_hints(),
        // Up/Down／PageUp/PageDown 不經過 `KeyBind`（見
        // `view::shell::resolve` 的文件註解），沒有 `UserEvent` 可以掛，早
        // return 手動補上這兩組固定提示——其餘 view 都走尾端共用的
        // `hint_pairs` → `fit_key_groups` → `hint_line` 流程，不必為了這一個特例把它拆開。
        View::Shell(_) => {
            let specs = [
                h(&[UserEvent::Confirm], t!("common.hint.run")),
                h(&[UserEvent::Cancel], t!("common.hint.close")),
            ];
            let history = t!("common.hint.history");
            let scroll = t!("common.hint.scroll");
            let mut pairs = hint_pairs(&ctx.keybind, &specs);
            pairs.extend([
                ("↑↓".to_string(), history.as_ref()),
                ("PgUp/PgDn".to_string(), scroll.as_ref()),
            ]);
            let pairs = fit_key_groups(&pairs, max_width);
            return hint_line(&ctx.color_theme, pairs, ctx.color_theme.help_key_fg);
        }
        View::Help(_) => vec![
            h(
                &[UserEvent::NavigateDown, UserEvent::NavigateUp],
                t!("common.hint.scroll"),
            ),
            h(&[UserEvent::Close], t!("common.hint.close")),
        ],
        View::GitHub(ref view) => view.status_hints(),
        View::ReleaseNotes(_) => vec![
            h(
                &[UserEvent::NavigateDown, UserEvent::NavigateUp],
                t!("common.hint.scroll"),
            ),
            h(&[UserEvent::HalfPageDown], t!("common.hint.half")),
            h(&[UserEvent::PageDown], t!("common.hint.page")),
            h(&[UserEvent::Close], t!("common.hint.close")),
        ],
        // 窮舉而非 `_`：新增一個 view 時要在這裡被編譯器叫住，
        // 而不是靜默得到一條空提示列。
        View::Default => vec![],
    };

    let pairs = hint_pairs(&ctx.keybind, &hints);
    hint_line(
        &ctx.color_theme,
        fit_key_groups(&pairs, max_width),
        ctx.color_theme.help_key_fg,
    )
}

/// merge PR 表單裡可勾選的 head branch 處置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MergeDelete {
    /// 本機同名分支
    Local,
    /// 遠端（GitHub）上的分支
    Remote,
    /// 本機追蹤 ref（`origin/<head_ref>`）
    Tracking,
}

/// merge PR 的兩個彈窗。`rows` 只含「可以問」的項目——不可刪的（fork／default
/// branch／head==base、本機找不到或正是 HEAD、沒有追蹤 ref）直接不出現，順序固定
/// Local → Remote → Tracking。
#[derive(Debug, Clone)]
enum MergePrStage {
    /// 表單：游標 0 是「合併方式」列，其後依序是 `rows`。
    Form {
        method: MergeMethod,
        rows: Vec<(MergeDelete, bool)>,
        cursor: usize,
    },
    /// 第二個彈窗：最後確認。
    Confirm {
        method: MergeMethod,
        rows: Vec<(MergeDelete, bool)>,
    },
}

/// merge 方式在表單裡 ←／→ 循環：merge → squash → rebase → merge。
fn cycle_method(method: MergeMethod, forward: bool) -> MergeMethod {
    use MergeMethod::*;
    match (method, forward) {
        (Merge, true) | (Rebase, false) => Squash,
        (Squash, true) | (Merge, false) => Rebase,
        (Rebase, true) | (Squash, false) => Merge,
    }
}

/// 開啟 merge PR 表單。預設：合併方式 squash、本機分支 ✔、遠端分支 ✔、
/// 本機追蹤 ref ✘。
fn merge_pr_prompt_line(
    number: u64,
    head_ref: String,
    state: StateFilter,
    remote_deletable: bool,
    local_delete: Option<bool>,
    tracking_ref: Option<String>,
) -> StatusLine {
    let mut rows = Vec::with_capacity(3);
    if local_delete.is_some() {
        rows.push((MergeDelete::Local, true));
    }
    if remote_deletable {
        rows.push((MergeDelete::Remote, true));
    }
    if tracking_ref.is_some() {
        rows.push((MergeDelete::Tracking, false));
    }
    StatusLine::MergePrPrompt {
        number,
        head_ref,
        state,
        local_delete,
        tracking_ref,
        stage: MergePrStage::Form {
            method: MergeMethod::Squash,
            rows,
            cursor: 0,
        },
    }
}

#[derive(Debug, Default)]
#[cfg_attr(test, derive(Clone))]
enum StatusLine {
    #[default]
    None,
    /// 訊息、游標在訊息內的 byte 位置、右側的 transient 訊息。
    Input(String, usize, Option<String>),
    // 以下帶 `menu` 的變體都是彈窗選單，共用 `handle_menu_key`／`render_menu_popup`。
    RefPicker {
        options: Vec<String>,
        kind: RefCopyKind,
        menu: Menu,
    },
    CheckoutPicker {
        options: Vec<String>,
        kind: CheckoutPickKind,
        menu: Menu,
    },
    /// `options` 已經截斷到 9 個（數字鍵上限），`total` 是截斷前的實際候選數，
    /// 供 render 顯示「還有幾個沒列出」。截斷在 `open_child_picker` 做，
    /// 不是在送 `AppEvent::OpenChildPicker` 之前——呼叫端有 List/Detail/
    /// UserCommand 三處，收在這個唯一消費端才不會抄三份 `.take(9)`。
    ChildPicker {
        options: Vec<ChildPickOption>,
        total: usize,
        menu: Menu,
    },
    DeleteBranchPicker {
        options: Vec<String>,
        total: usize,
        menu: Menu,
    },
    DeleteBranchConfirm {
        name: String,
        menu: Menu,
    },
    MergePrPrompt {
        number: u64,
        head_ref: String,
        state: StateFilter,
        /// `None` = 本地找不到同名分支，或該分支正是目前 HEAD——表單不出現 Local 列。
        /// `Some(force_safe)` = 可以問，`force_safe` 由
        /// `app::local_branch_delete_check` 依 `headRefOid` 算好帶進來，
        /// 不是使用者選項，決定送出 `MergePrRequested` 時要不要強刪。
        local_delete: Option<bool>,
        /// `Some("origin/<head_ref>")` = 本機有這條追蹤分支且可以問；`None` 表單不出現
        /// Tracking 列，見 `app::tracking_ref_to_delete`。
        tracking_ref: Option<String>,
        stage: MergePrStage,
    },
    ToggleStatePrompt {
        number: u64,
        kind: GhItemKind,
        action: StateAction,
        filter_state: StateFilter,
        menu: Menu,
    },
    TogglePrDraftPrompt {
        number: u64,
        action: PrDraftAction,
        filter_state: StateFilter,
        menu: Menu,
    },
    /// CI 監測 tab 的重跑選擇：`f` 只重跑失敗、`a` 全部重跑。兩份 run id 都由
    /// view 依卡片的 check 算好，空的那個選項不出現也不接受。
    CiRerunPrompt {
        number: u64,
        failed_runs: Vec<u64>,
        all_runs: Vec<u64>,
        menu: Menu,
    },
    /// 有新版 GitHub Release 可更新。跟其他 y/n prompt 一樣是「確認／取消」
    /// 兩列選單，唯一差異是確認後送的事件不同。
    UpdatePrompt {
        tag: String,
        menu: Menu,
    },
    /// 下載＋替換執行檔完成，問是否要離開並以新版重啟。
    RestartPrompt {
        tag: String,
        exe: PathBuf,
        menu: Menu,
    },
    RelatedPicker {
        items: Vec<RelatedItem>,
        menu: Menu,
    },
    NotificationInfo(String),
    NotificationSuccess(String),
    NotificationWarn(String),
    NotificationError(String),
}

impl StatusLine {
    fn menu(&self) -> Option<&Menu> {
        match self {
            StatusLine::RefPicker { menu, .. }
            | StatusLine::CheckoutPicker { menu, .. }
            | StatusLine::ChildPicker { menu, .. }
            | StatusLine::DeleteBranchPicker { menu, .. }
            | StatusLine::DeleteBranchConfirm { menu, .. }
            | StatusLine::CiRerunPrompt { menu, .. }
            | StatusLine::ToggleStatePrompt { menu, .. }
            | StatusLine::TogglePrDraftPrompt { menu, .. }
            | StatusLine::UpdatePrompt { menu, .. }
            | StatusLine::RestartPrompt { menu, .. }
            | StatusLine::RelatedPicker { menu, .. } => Some(menu),
            _ => None,
        }
    }

    /// 彈窗選單變體的游標狀態；其餘變體回 `None`。
    fn menu_mut(&mut self) -> Option<&mut Menu> {
        match self {
            StatusLine::RefPicker { menu, .. }
            | StatusLine::CheckoutPicker { menu, .. }
            | StatusLine::ChildPicker { menu, .. }
            | StatusLine::DeleteBranchPicker { menu, .. }
            | StatusLine::DeleteBranchConfirm { menu, .. }
            | StatusLine::CiRerunPrompt { menu, .. }
            | StatusLine::ToggleStatePrompt { menu, .. }
            | StatusLine::TogglePrDraftPrompt { menu, .. }
            | StatusLine::UpdatePrompt { menu, .. }
            | StatusLine::RestartPrompt { menu, .. }
            | StatusLine::RelatedPicker { menu, .. } => Some(menu),
            _ => None,
        }
    }

    /// 彈窗選單的內容；不是選單變體回 `None`。列數、快選鍵與畫面標籤都從
    /// 這裡取，`handle_menu_key` 與 `render_menu_popup` 不各算一次。
    fn menu_spec(&self) -> Option<MenuSpec> {
        let plain = |title: String, labels: Vec<&str>| MenuSpec {
            title,
            items: labels.into_iter().map(MenuItem::new).collect(),
            footer: None,
        };
        Some(match self {
            StatusLine::RefPicker { options, kind, .. } => plain(
                clean_title(&kind.picker_prompt()),
                options.iter().map(String::as_str).collect(),
            ),
            StatusLine::CheckoutPicker { options, kind, .. } => plain(
                clean_title(&kind.picker_prompt()),
                options.iter().map(String::as_str).collect(),
            ),
            StatusLine::ChildPicker { options, total, .. } => MenuSpec {
                footer: (*total > options.len())
                    .then(|| t!("app.status.more", n = total - options.len()).into_owned()),
                ..plain(
                    clean_title(&t!("app.status.go_to_child")),
                    options.iter().map(|o| o.label.as_str()).collect(),
                )
            },
            StatusLine::DeleteBranchPicker { options, total, .. } => MenuSpec {
                footer: (*total > options.len()).then(|| {
                    t!("app.status.more_tab_view", n = total - options.len()).into_owned()
                }),
                ..plain(
                    clean_title(&t!("app.status.delete_branch")),
                    options.iter().map(String::as_str).collect(),
                )
            },
            StatusLine::DeleteBranchConfirm { name, .. } => MenuSpec {
                title: clean_title(&t!("app.status.delete_branch_confirm", name = name)),
                items: vec![
                    MenuItem::new(t!("common.yes")).quick('y'),
                    MenuItem::new(t!("common.force")).quick('f'),
                    MenuItem::new(t!("common.no")).quick('n'),
                ],
                footer: None,
            },
            StatusLine::CiRerunPrompt {
                number,
                failed_runs,
                all_runs,
                ..
            } => MenuSpec {
                title: clean_title(&t!("app.status.ci_rerun", number = number)),
                items: ci_rerun_scopes(failed_runs, all_runs)
                    .into_iter()
                    .map(|scope| match scope {
                        RerunScope::FailedOnly => {
                            MenuItem::new(t!("app.status.ci_rerun_failed")).quick('f')
                        }
                        RerunScope::All => MenuItem::new(t!("app.status.ci_rerun_all")).quick('a'),
                    })
                    .collect(),
                footer: None,
            },
            StatusLine::ToggleStatePrompt {
                number,
                kind,
                action,
                ..
            } => MenuSpec {
                title: clean_title(&action.prompt(*kind, *number)),
                items: yes_no_items(t!("common.hint.confirm")),
                footer: None,
            },
            StatusLine::TogglePrDraftPrompt { number, action, .. } => MenuSpec {
                title: clean_title(&action.prompt(*number)),
                items: yes_no_items(t!("common.hint.confirm")),
                footer: None,
            },
            StatusLine::UpdatePrompt { tag, .. } => MenuSpec {
                title: format!("v{} → {tag}", env!("CARGO_PKG_VERSION")),
                items: yes_no_items(t!("common.hint.update")),
                footer: None,
            },
            StatusLine::RestartPrompt { tag, .. } => MenuSpec {
                title: clean_title(&t!("app.status.updated_to", tag = tag)),
                items: yes_no_items(t!("common.hint.restart")),
                footer: None,
            },
            StatusLine::RelatedPicker { items, .. } => {
                let mut last_group = None;
                MenuSpec {
                    title: t!("common.hint.related").into_owned(),
                    items: items
                        .iter()
                        .map(|item| {
                            let mut entry = MenuItem::new(format!("#{}", item.number));
                            entry.dim = item.state.eq_ignore_ascii_case("CLOSED")
                                || item.state.eq_ignore_ascii_case("MERGED");
                            if last_group != Some(item.group) {
                                last_group = Some(item.group);
                                entry.group = Some(item.group.label().into_owned());
                            }
                            entry
                        })
                        .collect(),
                    footer: None,
                }
            }
            _ => return None,
        })
    }
}

/// Phase 6 換資料時要保留的通知——不透明包住 `StatusLine`，`app.rs` 拿到手
/// 只能原樣帶著走，不能塞 picker／prompt 進來。`take_notification()`／
/// `restore_notification()` 是唯一的生產與消費端；`blocks_swap()` 已經先
/// 擋掉 picker／prompt／輸入框，走到這裡的 `StatusLine` 只可能是四種
/// `Notification*` variant 之一。
#[derive(Debug)]
pub(super) struct Notification(StatusLine);

#[derive(Debug)]
pub(super) struct StatusLineState {
    line: StatusLine,
    ctx: Rc<AppContext>,
    tx: Sender,
    /// 下一輪 auto-fetch 的 deadline，倒數用。在這裡持有而不是每次 render
    /// 從 `App` 傳進來：`App::render` 對倒數沒有任何話語權，讓它收一個純轉手
    /// 參數等於逼呼叫端知道一件與它無關的事。
    auto_fetch_clock: AutoFetchClock,
}

impl StatusLineState {
    pub(super) fn new(ctx: Rc<AppContext>, tx: Sender, auto_fetch_clock: AutoFetchClock) -> Self {
        Self {
            line: StatusLine::default(),
            ctx,
            tx,
            auto_fetch_clock,
        }
    }

    /// 這一幀該顯示的倒數秒數。`None` = 沒開 auto-fetch、還沒排第一輪，或
    /// 狀態列正被別的東西佔用。
    ///
    /// **重畫判斷（`App::run` 的 `Tick`）與實際繪製共用這一個函式**——兩邊
    /// 各寫一次條件的話，顯示條件一改重畫條件就會默默不同步，變成開著
    /// picker／輸入框時每秒白畫一次全螢幕（倒數那時根本沒顯示）。
    ///
    /// 取上界而不是截斷：`interval` 是 600 時，`arm` 當下的 remaining 是
    /// 599.999，截斷的話永遠不會顯示 `10:00`，而且 `00:00` 會多停一整秒。
    /// 取上界之後 `00:00` 精確等於「已過期 = worker 正在抓」。
    pub(super) fn countdown_secs(&self) -> Option<u64> {
        if !self.is_idle() {
            return None;
        }
        self.auto_fetch_clock
            .remaining()
            // 先除再 cast：結果必定落在 u64 內，讀者不必回頭確認
            // `as_millis()` 的 u128 上界。
            .map(|d| d.as_millis().div_ceil(1000) as u64)
    }

    pub(super) fn open_ref_picker(&mut self, options: Vec<String>, kind: RefCopyKind) {
        self.line = StatusLine::RefPicker {
            options,
            kind,
            menu: Menu::default(),
        };
    }

    pub(super) fn open_checkout_picker(&mut self, options: Vec<String>, kind: CheckoutPickKind) {
        self.line = StatusLine::CheckoutPicker {
            options,
            kind,
            menu: Menu::default(),
        };
    }

    pub(super) fn open_child_picker(&mut self, options: Vec<ChildPickOption>) {
        let total = options.len();
        let options = options.into_iter().take(9).collect();
        self.line = StatusLine::ChildPicker {
            options,
            total,
            menu: Menu::default(),
        };
    }

    /// 空清單走同步賦值，不透過 `tx` 送 `AppEvent::NotifyInfo`——`App::run()`
    /// 是「畫一幀 → recv 一個事件 → 處理 → 再畫一幀」的迴圈，送事件的話這
    /// 一輪迴圈會先用 `line` 還沒變的狀態畫一幀（顯示 hotkey hints），下一
    /// 輪才變成通知，等於平白多一幀「先閃一下 hint 列才出現提示」。
    pub(super) fn open_related_picker(&mut self, items: Vec<RelatedItem>) {
        if items.is_empty() {
            self.set_notification_info(t!("app.status.no_related").into_owned());
        } else {
            self.line = StatusLine::RelatedPicker {
                items,
                menu: Menu::default(),
            };
        }
    }

    pub(super) fn open_delete_branch_picker(&mut self, options: Vec<String>, total: usize) {
        self.line = StatusLine::DeleteBranchPicker {
            options,
            total,
            menu: Menu::default(),
        };
    }

    pub(super) fn open_delete_branch_confirm(&mut self, name: String) {
        self.line = StatusLine::DeleteBranchConfirm {
            name,
            menu: Menu::default(),
        };
    }

    pub(super) fn open_merge_pr_prompt(
        &mut self,
        number: u64,
        head_ref: String,
        state: StateFilter,
        remote_deletable: bool,
        local_delete: Option<bool>,
        tracking_ref: Option<String>,
    ) {
        self.line = merge_pr_prompt_line(
            number,
            head_ref,
            state,
            remote_deletable,
            local_delete,
            tracking_ref,
        );
    }

    pub(super) fn open_toggle_state_prompt(
        &mut self,
        number: u64,
        kind: GhItemKind,
        action: StateAction,
        filter_state: StateFilter,
    ) {
        self.line = StatusLine::ToggleStatePrompt {
            number,
            kind,
            action,
            filter_state,
            menu: Menu::default(),
        };
    }

    pub(super) fn open_toggle_pr_draft_prompt(
        &mut self,
        number: u64,
        action: PrDraftAction,
        filter_state: StateFilter,
    ) {
        self.line = StatusLine::TogglePrDraftPrompt {
            number,
            action,
            filter_state,
            menu: Menu::default(),
        };
    }

    pub(super) fn open_ci_rerun_prompt(
        &mut self,
        number: u64,
        failed_runs: Vec<u64>,
        all_runs: Vec<u64>,
    ) {
        self.line = StatusLine::CiRerunPrompt {
            number,
            failed_runs,
            all_runs,
            menu: Menu::default(),
        };
    }

    pub(super) fn open_update_prompt(&mut self, tag: String) {
        self.line = StatusLine::UpdatePrompt {
            tag,
            menu: Menu::default(),
        };
    }

    pub(super) fn open_restart_prompt(&mut self, tag: String, exe: PathBuf) {
        self.line = StatusLine::RestartPrompt {
            tag,
            exe,
            menu: Menu::default(),
        };
    }

    /// 給 `App::maybe_open_update_prompt` 的守衛用：狀態列現在有沒有被別的
    /// 東西佔著（picker、prompt、甚至一則通知）。更新提示是背景檢查回來時
    /// 才會出現的，不能覆蓋掉使用者當下正在看的任何東西。
    pub(super) fn is_idle(&self) -> bool {
        matches!(self.line, StatusLine::None)
    }

    /// 給 `App::maybe_open_restart_prompt` 用：比 `is_idle` 寬一格，容許狀態列
    /// 正顯示一則通知。重啟提示是使用者自己按 `U`、自己按 `y`、自己等下載完
    /// 的結果，蓋掉一則同樣在講這件事的通知不算冒犯——跟 `is_idle` 服務的
    /// 「背景不請自來」語意不同，不要合併這兩個判斷式。
    ///
    /// **不含 `NotificationError`**：這裡若也蓋得掉錯誤，使用者原本想按任意鍵
    /// 去確認錯誤，卻可能誤觸重啟提示的 y/n（例如習慣性按 `y`），後果比通知
    /// 被覆寫嚴重得多。
    pub(super) fn is_showing_notification(&self) -> bool {
        matches!(
            self.line,
            StatusLine::NotificationInfo(_)
                | StatusLine::NotificationSuccess(_)
                | StatusLine::NotificationWarn(_)
        )
    }

    /// 狀態列現在能不能被「背景不請自來」的通知寫入：閒置，或只是壓著
    /// 另一則通知。picker／prompt 都不算。`is_idle`／`is_showing_notification`
    /// 各自的語意仍然分開（見上），這裡只是把兩者的 `||` 組合具名化，讓
    /// `can_interrupt` 與背景通知的守衛共用同一份真值，不要各自手寫一次。
    pub(super) fn is_idle_or_notification(&self) -> bool {
        self.is_idle() || self.is_showing_notification()
    }

    /// 換資料能不能進行的判斷之一：狀態列現在是不是擋著——picker、prompt、
    /// 或正在輸入的 search／filter 輸入框（`StatusLine::Input`）。跟
    /// `is_idle_or_notification` 不同的地方是這裡把 `NotificationError`
    /// 也算「不擋」：重載失敗的錯誤通知不該卡住下一次成功結果的顯示，
    /// 使用者不需要先按鍵確認錯誤才能看到新資料。`Input` 則反過來一定要擋
    /// ——換資料會重建 `StatusLineState`（回到 `None`），使用者半打的搜尋
    /// 字串會憑空消失。
    pub(super) fn blocks_swap(&self) -> bool {
        !matches!(
            self.line,
            StatusLine::None
                | StatusLine::NotificationInfo(_)
                | StatusLine::NotificationSuccess(_)
                | StatusLine::NotificationWarn(_)
                | StatusLine::NotificationError(_)
        )
    }

    /// 換資料時取出目前的通知（如果有）——只有四種 `Notification*`
    /// variant 會被取到並清空狀態列；其餘情況原樣放回、回 `None`。呼叫端
    /// （`App::try_swap`）在呼叫這個之前已經用 `blocks_swap()` 確認過狀態列
    /// 不是 picker／prompt／輸入框，所以「其餘情況」實務上只有 `None`。
    pub(super) fn take_notification(&mut self) -> Option<Notification> {
        match std::mem::take(&mut self.line) {
            line @ (StatusLine::NotificationInfo(_)
            | StatusLine::NotificationSuccess(_)
            | StatusLine::NotificationWarn(_)
            | StatusLine::NotificationError(_)) => Some(Notification(line)),
            other => {
                self.line = other;
                None
            }
        }
    }

    /// 換資料重建 `App` 之後呼叫，把 `take_notification()` 取走的通知放回
    /// 新的 `StatusLineState` 上。
    pub(super) fn restore_notification(&mut self, notification: Notification) {
        self.line = notification.0;
    }

    /// Full 重載狀態指示——插在 hotkey hints／輸入框那一列最前面，跟倒數
    /// span 共用同一個插入點模式（見 `render` 尾端的插入點註解）。
    /// `Idle` 不畫任何東西；`Loading` 代表 worker 正在跑（含冷卻中排隊）；
    /// `Ready` 只可能在 `render()` 被呼叫到時出現——它代表換資料被目前狀態
    /// 擋住了（`App::try_swap()` 每輪都先試過，能換的話早就換掉、根本不會
    /// 走到這裡渲染）。
    fn full_reload_span(&self, status: FullStatus) -> Option<Span<'static>> {
        let text = match status {
            FullStatus::Idle => return None,
            FullStatus::Loading => t!("app.status.reload_loading"),
            FullStatus::Ready => t!("app.status.reload_pending"),
        };
        Some(Span::styled(
            text,
            Style::default().fg(self.ctx.color_theme.status_input_transient_fg),
        ))
    }

    pub(super) fn clear(&mut self) {
        self.line = StatusLine::None;
    }

    pub(super) fn update_input(
        &mut self,
        msg: String,
        cursor: usize,
        transient_msg: Option<String>,
    ) {
        self.line = StatusLine::Input(msg, cursor, transient_msg);
    }

    pub(super) fn set_notification_info(&mut self, msg: String) {
        self.line = StatusLine::NotificationInfo(msg);
    }

    pub(super) fn set_notification_success(&mut self, msg: String) {
        self.line = StatusLine::NotificationSuccess(msg);
    }

    pub(super) fn set_notification_warn(&mut self, msg: String) {
        self.line = StatusLine::NotificationWarn(msg);
    }

    pub(super) fn set_notification_error(&mut self, msg: String) {
        self.line = StatusLine::NotificationError(msg);
    }

    /// 回傳 true 表示這是 12 個攔截變體之一（11 個彈窗選單變體共用
    /// `handle_menu_key`，`MergePrPrompt` 另有自己的 handler）、鍵已被吃掉。
    ///
    /// 尾巴刻意窮舉非攔截變體而非 `_ => false`：漏接一個新變體只會讓它卡在
    /// 畫面上完全不吃鍵，編譯器不會提醒（比照 app.rs 原本就有的同款警告，
    /// 這裡原封不動繼承）。
    pub(super) fn handle_intercepting_key(&mut self, key: KeyEvent) -> bool {
        match self.line {
            StatusLine::RefPicker { .. }
            | StatusLine::CheckoutPicker { .. }
            | StatusLine::ChildPicker { .. }
            | StatusLine::RelatedPicker { .. }
            | StatusLine::DeleteBranchPicker { .. }
            | StatusLine::DeleteBranchConfirm { .. }
            | StatusLine::CiRerunPrompt { .. }
            | StatusLine::ToggleStatePrompt { .. }
            | StatusLine::TogglePrDraftPrompt { .. }
            | StatusLine::UpdatePrompt { .. }
            | StatusLine::RestartPrompt { .. } => {
                self.handle_menu_key(key);
                true
            }
            StatusLine::MergePrPrompt { .. } => {
                self.handle_merge_pr_prompt_key(key);
                true
            }
            StatusLine::None
            | StatusLine::Input(_, _, _)
            | StatusLine::NotificationInfo(_)
            | StatusLine::NotificationSuccess(_)
            | StatusLine::NotificationWarn(_)
            | StatusLine::NotificationError(_) => false,
        }
    }

    /// 清掉目前的通知（若有）。回傳 true 表示這把鍵已被吞掉——任何通知
    /// （Info／Success／Warn／Error）都吞鍵：這把鍵的唯一作用就是消除訊息，
    /// 不會再觸發其他動作。窮盡 match，理由同
    /// `handle_intercepting_key`。
    pub(super) fn dismiss_notification(&mut self) -> bool {
        match self.line {
            StatusLine::None
            | StatusLine::Input(_, _, _)
            | StatusLine::RefPicker { .. }
            | StatusLine::CheckoutPicker { .. }
            | StatusLine::ChildPicker { .. }
            | StatusLine::RelatedPicker { .. }
            | StatusLine::DeleteBranchPicker { .. }
            | StatusLine::DeleteBranchConfirm { .. }
            | StatusLine::MergePrPrompt { .. }
            | StatusLine::CiRerunPrompt { .. }
            | StatusLine::ToggleStatePrompt { .. }
            | StatusLine::TogglePrDraftPrompt { .. }
            | StatusLine::UpdatePrompt { .. }
            | StatusLine::RestartPrompt { .. } => false,
            StatusLine::NotificationInfo(_)
            | StatusLine::NotificationSuccess(_)
            | StatusLine::NotificationWarn(_)
            | StatusLine::NotificationError(_) => {
                self.line = StatusLine::None;
                true
            }
        }
    }

    /// 對應 `App::is_input_mode()` 判斷式裡跟狀態列相關的那部分。**故意跟
    /// `handle_intercepting_key` 涵蓋的變體不同步**：6 個 prompt
    /// （`MergePrPrompt`/`CiRerunPrompt`/`ToggleStatePrompt`/`TogglePrDraftPrompt`/`UpdatePrompt`/
    /// `RestartPrompt`）不在這份清單裡，因為它們在 `handle_intercepting_key`
    /// 永遠回傳 `true`，唯一能繞過的只有 `ForceQuit`，而 `ForceQuit` 分支根本
    /// 不查這個方法。`MergePrPrompt` 的兩個彈窗一樣靠這個攔截擋鍵。動
    /// `ForceQuit` 分支或動 `handle_intercepting_key` 涵蓋範圍時，回來檢查這裡。
    pub(super) fn is_input_mode_variant(&self) -> bool {
        match self.line {
            StatusLine::Input(_, _, _)
            | StatusLine::RefPicker { .. }
            | StatusLine::CheckoutPicker { .. }
            | StatusLine::ChildPicker { .. }
            | StatusLine::RelatedPicker { .. }
            | StatusLine::DeleteBranchPicker { .. }
            | StatusLine::DeleteBranchConfirm { .. } => true,
            StatusLine::None
            | StatusLine::MergePrPrompt { .. }
            | StatusLine::CiRerunPrompt { .. }
            | StatusLine::ToggleStatePrompt { .. }
            | StatusLine::TogglePrDraftPrompt { .. }
            | StatusLine::UpdatePrompt { .. }
            | StatusLine::RestartPrompt { .. }
            | StatusLine::NotificationInfo(_)
            | StatusLine::NotificationSuccess(_)
            | StatusLine::NotificationWarn(_)
            | StatusLine::NotificationError(_) => false,
        }
    }

    /// 所有彈窗選單（picker 與 y/n 類 prompt）共用的按鍵處理，規則見 [`menu_key`]。
    fn handle_menu_key(&mut self, key: KeyEvent) {
        // related picker 的開啟鍵同時是關閉鍵
        if matches!(self.line, StatusLine::RelatedPicker { .. })
            && self.ctx.keybind.has(&key, UserEvent::DetailPaneToggle)
        {
            self.cancel_menu();
            return;
        }
        let Some(spec) = self.line.menu_spec() else {
            return;
        };
        let quick: Vec<Option<char>> = spec.items.iter().map(|item| item.quick).collect();
        let Some(menu) = self.line.menu_mut() else {
            return;
        };
        match menu_key(menu, &quick, key, &self.ctx.keybind) {
            MenuKey::Cancel => self.cancel_menu(),
            MenuKey::Pick(idx) => self.pick(idx),
            MenuKey::Consumed => {}
        }
    }

    /// 關掉選單。`RestartPrompt` 取消時更新其實已經做完了，跟其餘選單「取消 =
    /// 什麼都沒發生」不同——靜默清空會讓使用者忘記要手動重啟，留一句話。
    fn cancel_menu(&mut self) {
        self.line = if matches!(self.line, StatusLine::RestartPrompt { .. }) {
            restart_skipped_notice()
        } else {
            StatusLine::None
        };
    }

    /// 執行選單的第 `idx` 列。列的順序與 [`StatusLine::menu_spec`] 一致。
    fn pick(&mut self, idx: usize) {
        // take 同時結束對 line 的借用並清掉彈窗；要轉場的變體再自己放回去
        let line = std::mem::take(&mut self.line);
        match line {
            StatusLine::RefPicker { options, kind, .. } => {
                if let Some(value) = options.into_iter().nth(idx) {
                    self.tx.send(AppEvent::CopyToClipboard {
                        name: kind.copy_label().into(),
                        value,
                    });
                }
            }
            StatusLine::CheckoutPicker { options, .. } => {
                if let Some(target) = options.into_iter().nth(idx) {
                    self.tx.send(AppEvent::CheckoutCommit { target });
                }
            }
            StatusLine::ChildPicker { options, .. } => {
                if let Some(option) = options.into_iter().nth(idx) {
                    self.tx.send(AppEvent::SelectChildCommitByHash {
                        hash: option.commit_hash,
                    });
                }
            }
            StatusLine::RelatedPicker { items, .. } => {
                if let Some(item) = items.get(idx) {
                    self.tx.send(AppEvent::GitHubJumpToIssue {
                        number: item.number,
                    });
                }
            }
            StatusLine::DeleteBranchPicker { options, .. } => {
                if let Some(name) = options.into_iter().nth(idx) {
                    self.open_delete_branch_confirm(name);
                }
            }
            // 0 = 刪除、1 = 強制刪除、2 = 取消
            StatusLine::DeleteBranchConfirm { name, .. } => {
                if idx < 2 {
                    self.tx.send(AppEvent::DeleteBranchRequested {
                        name,
                        force: idx == 1,
                    });
                }
            }
            StatusLine::CiRerunPrompt {
                number,
                failed_runs,
                all_runs,
                ..
            } => {
                if let Some(scope) = ci_rerun_scopes(&failed_runs, &all_runs).get(idx).copied() {
                    let run_ids = match scope {
                        RerunScope::FailedOnly => failed_runs,
                        RerunScope::All => all_runs,
                    };
                    self.tx.send(AppEvent::CiRerunRequested {
                        number,
                        run_ids,
                        scope,
                    });
                }
            }
            // y/n 類：0 = 確認、1 = 取消
            StatusLine::ToggleStatePrompt {
                number,
                kind,
                action,
                filter_state,
                ..
            } if idx == 0 => {
                self.tx.send(AppEvent::ToggleItemStateRequested {
                    number,
                    kind,
                    action,
                    filter_state,
                });
            }
            StatusLine::TogglePrDraftPrompt {
                number,
                action,
                filter_state,
                ..
            } if idx == 0 => {
                self.tx.send(AppEvent::TogglePrDraftRequested {
                    number,
                    action,
                    filter_state,
                });
            }
            StatusLine::UpdatePrompt { tag, .. } if idx == 0 => {
                self.tx.send(AppEvent::UpdateRequested { tag });
            }
            StatusLine::RestartPrompt { exe, .. } if idx == 0 => {
                self.tx.send(AppEvent::RestartRequested { exe });
            }
            StatusLine::RestartPrompt { .. } => self.line = restart_skipped_notice(),
            _ => {}
        }
    }

    fn handle_merge_pr_prompt_key(&mut self, key: KeyEvent) {
        let keybind = &self.ctx.keybind;
        let StatusLine::MergePrPrompt {
            number,
            head_ref,
            state,
            local_delete,
            tracking_ref,
            stage,
        } = &mut self.line
        else {
            return;
        };
        let is_cancel = keybind.has(&key, UserEvent::Cancel);
        let is_confirm = keybind.has(&key, UserEvent::Confirm) || key.code == KeyCode::Enter;

        let mut close = false;
        let mut request = None;
        match stage {
            MergePrStage::Form {
                method,
                rows,
                cursor,
            } => {
                // 游標 0 是方式列，其後才是 rows，所以總列數要 +1。
                let len = rows.len() + 1;
                if is_cancel {
                    close = true;
                } else if is_confirm {
                    *stage = MergePrStage::Confirm {
                        method: *method,
                        rows: std::mem::take(rows),
                    };
                } else if keybind.has(&key, UserEvent::NavigateDown) {
                    *cursor = (*cursor + 1) % len;
                } else if keybind.has(&key, UserEvent::NavigateUp) {
                    *cursor = (*cursor + len - 1) % len;
                } else {
                    let forward = keybind.has(&key, UserEvent::NavigateRight);
                    if forward || keybind.has(&key, UserEvent::NavigateLeft) {
                        match *cursor {
                            0 => *method = cycle_method(*method, forward),
                            i => rows[i - 1].1 = !rows[i - 1].1,
                        }
                    }
                }
            }
            MergePrStage::Confirm { method, rows } => {
                if is_cancel {
                    close = true;
                } else if is_confirm {
                    let enabled = |opt| rows.iter().any(|&(o, checked)| o == opt && checked);
                    request = Some(AppEvent::MergePrRequested {
                        number: *number,
                        state: *state,
                        method: *method,
                        delete_local_branch: enabled(MergeDelete::Local)
                            .then(|| (head_ref.clone(), *local_delete == Some(true))),
                        delete_remote_branch: enabled(MergeDelete::Remote)
                            .then(|| head_ref.clone()),
                        delete_tracking_ref: tracking_ref
                            .clone()
                            .filter(|_| enabled(MergeDelete::Tracking)),
                    });
                    close = true;
                }
            }
        }
        if close {
            self.line = StatusLine::None;
        }
        if let Some(event) = request {
            self.tx.send(event);
        }
    }

    /// `nf-cod-git_fetch`（U+EC1D）+ 剩餘時間。
    ///
    /// 不足一分鐘印 `29s`，超過才印 `9:58`——`interval` 最小值是 30 秒，
    /// 用固定 `mm:ss` 的話那一整段時間都在顯示一個恆為 `00:` 的前綴，是純
    /// 噪音。帶 `s` 字尾讓「這是秒」不必靠上下文猜（少了 `:` 之後純數字會
    /// 有歧義）。
    ///
    /// 分鐘數不補零：`interval` 在 CLI 與設定檔兩個入口都夾在 30–3600 秒，
    /// 最寬就是 `60:00`。
    ///
    /// 沒有 Nerd Font 的終端會看到豆腐字——auto-fetch 本來就預設關閉、要明確
    /// 開啟，不為此加一個顯示開關。
    fn countdown_span(&self, secs: u64) -> Span<'static> {
        let remaining = if secs < 60 {
            format!("{secs}s")
        } else {
            format!("{}:{:02}", secs / 60, secs % 60)
        };
        Span::styled(
            format!("\u{EC1D} {remaining}  "),
            Style::default().fg(self.ctx.color_theme.status_input_transient_fg),
        )
    }

    pub(super) fn render(
        &self,
        f: &mut Frame,
        area: Rect,
        view: &View,
        numeric_prefix: &str,
        full_status: FullStatus,
    ) {
        // 提早算好、只算一次：下面 `Input` 分支的 exact-fit padding 與
        // 游標位置都要扣掉／讓開這個前綴的寬度——`full_reload_span` 不像
        // `countdown_span` 有 `is_idle()` 擋著，Input 輸入框開著時一樣可能
        // 同時被插進最前面，兩處若各自重算一次容易對不上。
        let full_reload_span = self.full_reload_span(full_status);
        let full_reload_w = full_reload_span.as_ref().map_or(0, Span::width);
        // 倒數 span 只在 `StatusLine::None`（也就是提示列）出現，一樣要先量好
        let countdown_span = self.countdown_secs().map(|s| self.countdown_span(s));

        let block = Block::default()
            .borders(Borders::TOP)
            .style(Style::default().fg(self.ctx.color_theme.divider_fg))
            .padding(Padding::horizontal(1));
        // 可用寬度從 `block.inner()` 拿，不要手算 padding。提示列／確認列照扣掉
        // 前綴後的寬度整組取捨，之後才不會被 `truncate_line` 再切出殘字。
        let inner = block.inner(area);
        let max_width = inner.width as usize;
        let hint_width = max_width
            .saturating_sub(full_reload_w + countdown_span.as_ref().map_or(0, Span::width));

        // `Input` 分支算出的「游標前內容的顯示寬度」，渲染後定位游標用。
        let mut cursor_col = 0usize;

        let mut text: Line = match &self.line {
            StatusLine::None => {
                if numeric_prefix.is_empty() {
                    build_hotkey_hints(view, &self.ctx, hint_width)
                } else {
                    Line::raw(numeric_prefix).fg(self.ctx.color_theme.status_input_transient_fg)
                }
            }
            StatusLine::Input(msg, cursor, transient_msg) => {
                let (before, display) =
                    input_display(msg, *cursor, &self.ctx.ui_config.cursor_type);
                cursor_col = console::measure_text_width(before);
                let msg_w = console::measure_text_width(&display);
                if let Some(t_msg) = transient_msg {
                    let t_msg_w = console::measure_text_width(t_msg.as_str());
                    // saturating：窄終端 + 長 transient message 會讓這串減法
                    // underflow，接著 `" ".repeat(天文數字)` 直接吃光記憶體。
                    let pad_w = (area.width as usize)
                        .saturating_sub(msg_w)
                        .saturating_sub(t_msg_w)
                        .saturating_sub(2 /* pad */)
                        .saturating_sub(full_reload_w);
                    Line::from(vec![
                        display.fg(self.ctx.color_theme.status_input_fg),
                        " ".repeat(pad_w).into(),
                        t_msg
                            .as_str()
                            .fg(self.ctx.color_theme.status_input_transient_fg),
                    ])
                } else {
                    Line::raw(display).fg(self.ctx.color_theme.status_input_fg)
                }
            }
            // 內容與提示全部在彈窗裡（`render_popup`），狀態列留白。
            StatusLine::RefPicker { .. }
            | StatusLine::CheckoutPicker { .. }
            | StatusLine::ChildPicker { .. }
            | StatusLine::RelatedPicker { .. }
            | StatusLine::DeleteBranchPicker { .. }
            | StatusLine::DeleteBranchConfirm { .. }
            | StatusLine::MergePrPrompt { .. }
            | StatusLine::CiRerunPrompt { .. }
            | StatusLine::ToggleStatePrompt { .. }
            | StatusLine::TogglePrDraftPrompt { .. }
            | StatusLine::UpdatePrompt { .. }
            | StatusLine::RestartPrompt { .. } => Line::raw(""),
            StatusLine::NotificationInfo(msg) => {
                Line::raw(msg).fg(self.ctx.color_theme.status_info_fg)
            }
            StatusLine::NotificationSuccess(msg) => Line::raw(msg)
                .add_modifier(Modifier::BOLD)
                .fg(self.ctx.color_theme.status_success_fg),
            StatusLine::NotificationWarn(msg) => Line::raw(msg)
                .add_modifier(Modifier::BOLD)
                .fg(self.ctx.color_theme.status_warn_fg),
            StatusLine::NotificationError(msg) => Line::raw(t!("app.status.error", msg = msg))
                .add_modifier(Modifier::BOLD)
                .fg(self.ctx.color_theme.status_error_fg),
        };

        // 插入點只有這一個，不在 match 的個別 arm 裡各插一次——要不要擴到
        // 別的 variant（例如通知）是改 `countdown_secs` 那個述詞，不是回來
        // 翻這裡的每一支。
        //
        // 上面幾支 `Line::raw(..).fg(..)` 的樣式是設在 *line* 上的，這個
        // span 帶自己的 `fg`，不會被蓋掉也不會污染別人。
        if let Some(span) = countdown_span {
            text.spans.insert(0, span);
        }
        if let Some(span) = full_reload_span {
            text.spans.insert(0, span);
        }

        // 截斷統一在這裡套一次，不分支 —— 輸入框與通知的長度沒有上限，
        // 提示列反而是人工策劃過的。
        let paragraph = Paragraph::new(truncate_line(text, max_width)).block(block);
        f.render_widget(paragraph, area);

        if let StatusLine::Input(..) = &self.line {
            // `+ full_reload_w`：Full 重載指示插在 `msg` 前面時會把整行往右
            // 推，游標必須跟著讓開，否則會畫在指示文字底下而不是輸入內容
            // 對應的字元位置。
            let offset = u16::try_from(full_reload_w + cursor_col).unwrap_or(u16::MAX);
            let (x, y) = (inner.x.saturating_add(offset), inner.y);
            match &self.ctx.ui_config.cursor_type {
                CursorType::Native => {
                    f.set_cursor_position((x, y));
                }
                CursorType::Virtual(cursor) => {
                    // 游標字元已由 `input_display` 接進字串，這裡只上色、不覆寫。
                    // 被 `truncate_line` 截掉時那格已是 `…`／空白，染色會誤導，
                    // 所以先確認格子上真的是游標字元。
                    let w = u16::try_from(console::measure_text_width(cursor)).unwrap_or(u16::MAX);
                    if x.saturating_add(w) <= inner.right()
                        && f.buffer_mut()
                            .cell((x, y))
                            .is_some_and(|c| c.symbol() == cursor.as_str())
                    {
                        let style = Style::default().fg(self.ctx.color_theme.virtual_cursor_fg);
                        f.buffer_mut().set_style(Rect::new(x, y, w, 1), style);
                    }
                }
            }
        }
    }

    /// 彈窗（merge PR 表單／確認，以及所有選單），畫在整個 view 區域上方。
    /// 其餘狀態不畫。
    pub(super) fn render_popup(&self, f: &mut Frame, area: Rect) {
        if matches!(self.line, StatusLine::MergePrPrompt { .. }) {
            self.render_merge_popup(f, area);
        } else {
            self.render_menu_popup(f, area);
        }
    }

    /// 選單彈窗：每列 `[ ] 1 label`，游標列反白，`l` 勾選後變 `[x]`。
    /// 列太多時捲動，讓游標那一行一直留在畫面內。
    fn render_menu_popup(&self, f: &mut Frame, area: Rect) {
        let (Some(spec), Some(menu)) = (self.line.menu_spec(), self.line.menu()) else {
            return;
        };
        let theme = &self.ctx.color_theme;
        let fg = theme.fg;

        let mut rows: Vec<(String, Style)> = Vec::new();
        let mut focus = 0;
        for (i, item) in spec.items.iter().enumerate() {
            if let Some(group) = &item.group {
                rows.push((group.clone(), Style::default().fg(MUTED_FG)));
            }
            let mut style = Style::default().fg(fg);
            if item.dim {
                style = style.fg(MUTED_FG);
            }
            if i == menu.cursor {
                focus = rows.len();
                style = style.add_modifier(Modifier::REVERSED);
            }
            let mark = if menu.marked == Some(i) { "[x]" } else { "[ ]" };
            // 第 10 列起按不到數字鍵，不印編號
            let number = if i < 9 {
                format!("{} ", i + 1)
            } else {
                "  ".to_string()
            };
            let quick = item.quick.map(|c| format!(" ({c})")).unwrap_or_default();
            rows.push((format!("{mark}{number}{}{quick}", item.label), style));
        }
        if let Some(footer) = spec.footer {
            rows.push((footer, Style::default().fg(MUTED_FG)));
        }

        let content_width = rows
            .iter()
            .map(|(text, _)| console::measure_text_width(text))
            .chain([console::measure_text_width(&spec.title)])
            .max()
            .unwrap_or(0);
        let width = u16::try_from(content_width + 4)
            .unwrap_or(u16::MAX)
            .clamp(40, 72)
            .min(area.width.saturating_sub(4));
        let lines = rows
            .into_iter()
            .map(|(text, style)| Line::from(Span::styled(text, style)))
            .collect();
        let hints = vec![
            h(&[UserEvent::Confirm], t!("common.hint.confirm")),
            h(
                &[UserEvent::NavigateLeft, UserEvent::Cancel],
                t!("common.hint.cancel"),
            ),
            h(&[UserEvent::NavigateRight], t!("common.hint.mark")),
            h(
                &[UserEvent::NavigateUp, UserEvent::NavigateDown],
                t!("common.hint.move"),
            ),
        ];
        self.draw_popup(
            f,
            area,
            Popup {
                title: &spec.title,
                header: None,
                lines,
                hints,
                width,
                focus,
            },
        );
    }

    /// merge PR 的兩個彈窗（表單／確認）。
    fn render_merge_popup(&self, f: &mut Frame, area: Rect) {
        let StatusLine::MergePrPrompt {
            number,
            head_ref,
            tracking_ref,
            stage,
            ..
        } = &self.line
        else {
            return;
        };
        let theme = &self.ctx.color_theme;
        let fg = theme.fg;
        let row_label = |opt: MergeDelete| match opt {
            MergeDelete::Local => t!("view.delete_ref.opt_local", name = head_ref),
            MergeDelete::Remote => t!("view.delete_ref.opt_remote", name = head_ref),
            MergeDelete::Tracking => t!(
                "view.delete_ref.opt_tracking",
                name = tracking_ref.as_deref().unwrap_or_default()
            ),
        };

        let (title, lines, hints) = match stage {
            MergePrStage::Form {
                method,
                rows,
                cursor,
            } => {
                let style_at = |i: usize| {
                    let style = Style::default().fg(fg);
                    if i == *cursor {
                        style.add_modifier(Modifier::REVERSED)
                    } else {
                        style
                    }
                };
                let mut lines = vec![Line::from(Span::styled(
                    format!(
                        "{}: < {} >",
                        t!("app.merge.dialog.method"),
                        method.display()
                    ),
                    style_at(0),
                ))];
                if !rows.is_empty() {
                    lines.push(Line::from(Span::styled(
                        t!("app.merge.dialog.delete_after"),
                        Style::default().fg(MUTED_FG),
                    )));
                }
                lines.extend(rows.iter().enumerate().map(|(i, &(opt, checked))| {
                    let checkbox = if checked { "[x]" } else { "[ ]" };
                    Line::from(Span::styled(
                        format!("{checkbox}{}", row_label(opt)),
                        style_at(i + 1),
                    ))
                }));
                let hints = vec![
                    h(&[UserEvent::Confirm], t!("common.hint.next")),
                    h(&[UserEvent::Cancel], t!("common.hint.cancel")),
                    h(
                        &[UserEvent::NavigateLeft, UserEvent::NavigateRight],
                        t!("common.hint.toggle"),
                    ),
                    h(
                        &[UserEvent::NavigateUp, UserEvent::NavigateDown],
                        t!("common.hint.move"),
                    ),
                ];
                (t!("app.merge.dialog.title", number = number), lines, hints)
            }
            MergePrStage::Confirm { method, rows } => {
                let mut lines = vec![Line::from(
                    t!(
                        "app.merge.dialog.confirm_summary",
                        number = number,
                        method = method.display()
                    )
                    .into_owned(),
                )];
                let mut deleting = rows.iter().filter(|&&(_, checked)| checked).peekable();
                if deleting.peek().is_none() {
                    lines.push(Line::from(
                        t!("app.merge.dialog.keep_branches").into_owned(),
                    ));
                }
                lines.extend(deleting.map(|&(opt, _)| {
                    Line::from(format!(
                        "{}{}",
                        t!("app.merge.dialog.delete_prefix"),
                        row_label(opt)
                    ))
                }));
                let hints = vec![
                    h(&[UserEvent::Confirm], t!("common.hint.merge_pr")),
                    h(&[UserEvent::Cancel], t!("common.hint.cancel")),
                ];
                (
                    t!("app.merge.dialog.confirm_title", number = number),
                    lines,
                    hints,
                )
            }
        };

        self.draw_popup(
            f,
            area,
            Popup {
                title: &title,
                header: Some(head_ref),
                lines,
                hints,
                width: 50u16.min(area.width.saturating_sub(4)),
                focus: 0,
            },
        );
    }

    /// 彈窗的共用外框：框線標題、可選的第一行 `header`、內容、換行的提示列。
    /// 內容比可用高度長時依 `focus`（內容裡要保持可見的那一行）捲動。
    fn draw_popup(&self, f: &mut Frame, area: Rect, popup: Popup) {
        let Popup {
            title,
            header,
            lines,
            hints,
            width,
            focus,
        } = popup;
        let theme = &self.ctx.color_theme;
        let fg = theme.fg;
        // 內層寬 = 彈窗寬 - 框線 2 - 左右留白 2；窄螢幕的提示要換行而不是被切掉
        let inner_width = (width as usize).saturating_sub(4);
        let pairs = hint_pairs(&self.ctx.keybind, &hints);
        let hint_rows = wrap_key_groups(&pairs, inner_width);
        let header_rows = u16::from(header.is_some());
        // 上下框線 2 + header + 提示列 + 內容列
        let height = (lines.len() as u16 + 2 + header_rows + hint_rows.len() as u16)
            .min(area.height.saturating_sub(2));
        let rect = Rect::new(
            area.x + area.width.saturating_sub(width) / 2,
            area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        );
        f.render_widget(Clear, rect);
        let block = Block::default()
            .title(Span::styled(title, Style::default().fg(MUTED_FG)))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.divider_fg))
            .style(Style::default().bg(theme.bg).fg(fg))
            .padding(Padding::horizontal(1));
        let inner = block.inner(rect);
        f.render_widget(block, rect);

        let [header_area, body_area, hint_area] = Layout::vertical([
            Constraint::Length(header_rows),
            Constraint::Min(0),
            Constraint::Length(hint_rows.len() as u16),
        ])
        .areas(inner);
        if let Some(header) = header {
            let name = console::truncate_str(header, inner_width, "…");
            f.render_widget(
                Paragraph::new(Line::from(Span::raw(name.as_ref()).fg(fg).bold())),
                header_area,
            );
        }
        let scroll = (focus + 1).saturating_sub(body_area.height as usize);
        let lines: Vec<Line> = lines
            .into_iter()
            .map(|l| truncate_line(l, inner_width))
            .collect();
        f.render_widget(
            Paragraph::new(lines).scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0)),
            body_area,
        );
        let hint_lines: Vec<Line> = hint_rows
            .into_iter()
            .map(|row| truncate_line(hint_line(theme, row, theme.help_key_fg), inner_width))
            .collect();
        f.render_widget(Paragraph::new(hint_lines).centered(), hint_area);
    }
}

/// [`StatusLineState::draw_popup`] 的內容。
struct Popup<'a> {
    title: &'a str,
    header: Option<&'a str>,
    lines: Vec<Line<'a>>,
    hints: Vec<HintSpec>,
    width: u16,
    /// 內容裡要保持在畫面內的那一行（0 起算）。
    focus: usize,
}

#[cfg(test)]
mod tests {
    use std::{path::Path, sync::mpsc};

    use ratatui::crossterm::event::KeyModifiers;

    use crate::{
        color::ColorTheme,
        config::{CoreConfig, UiConfig},
        git::CommitHash,
        github::{GhItemKind, PrDraftAction, StateAction},
        graph::GraphStyle,
        keybind::KeyBind,
    };

    use super::*;

    /// 建構測試用 `AppContext`。`keybind` 必須用 `KeyBind::new(None)`（讀
    /// `assets/default-keybind.toml`），不能用 `KeyBind::default()`——那是
    /// 一張空 map，會讓 `self.ctx.keybind.has(&key, _)` 對所有鍵都回 `false`，
    /// 測出來的綠燈/紅燈都是巧合，不是邏輯真的對或錯。以下幾條測試要驗的
    /// 正是「靠預設鍵位撐著的順序依賴」（例如 `f` 綁在 `fetch` 而非沒綁到
    /// 任何東西），換一份空 keybind 根本測不出這些依賴存不存在。
    fn test_ctx() -> Rc<AppContext> {
        Rc::new(AppContext {
            keybind: KeyBind::new(None),
            core_config: CoreConfig::default(),
            ui_config: UiConfig::default(),
            color_theme: ColorTheme::default(),
            graph_style: GraphStyle::default(),
            graph_width: None,
            compact: None,
            update: crate::update::UpdateSettings::default(),
            auto_fetch: crate::auto_fetch::AutoFetchSettings::default(),
            fetch_prune: crate::git::FetchPrune::default(),
            shell_command: Vec::new(),
        })
    }

    fn test_state() -> (StatusLineState, mpsc::Receiver<AppEvent>) {
        let (tx, rx) = Sender::channel_for_test();
        (
            StatusLineState::new(test_ctx(), tx, AutoFetchClock::default()),
            rx,
        )
    }

    fn char_key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::empty())
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    const RED: ratatui::style::Color = ratatui::style::Color::Red;

    fn virtual_bar() -> CursorType {
        CursorType::Virtual("|".into())
    }

    /// 輸入框狀態；`virtual_cursor_fg` 固定成紅色，才能用顏色分辨「這格有沒有被當游標染色」。
    fn input_state(
        cursor_type: CursorType,
        msg: &str,
        cursor: usize,
        transient: Option<&str>,
    ) -> StatusLineState {
        let (tx, _rx) = Sender::channel_for_test();
        let mut ctx = test_ctx();
        let c = Rc::get_mut(&mut ctx).unwrap();
        c.ui_config.cursor_type = cursor_type;
        c.color_theme.virtual_cursor_fg = RED;
        let mut state = StatusLineState::new(ctx, tx, AutoFetchClock::default());
        state.update_input(msg.into(), cursor, transient.map(Into::into));
        state
    }

    /// 畫出狀態列（2 列：上邊框＋輸入列），回傳 buffer。
    fn render_input(state: &StatusLineState, width: u16) -> ratatui::buffer::Buffer {
        use ratatui::{backend::TestBackend, Terminal};

        let mut terminal = Terminal::new(TestBackend::new(width, 2)).unwrap();
        terminal
            .draw(|f| state.render(f, f.area(), &View::Default, "", FullStatus::Idle))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn input_row(buf: &ratatui::buffer::Buffer) -> String {
        (0..buf.area.width).map(|x| buf[(x, 1)].symbol()).collect()
    }

    fn red_cells(buf: &ratatui::buffer::Buffer) -> Vec<u16> {
        (0..buf.area.width)
            .filter(|&x| buf[(x, 1)].fg == RED)
            .collect()
    }

    #[test]
    fn input_display_virtual_inserts_cursor_without_dropping_chars() {
        let (before, display) = input_display("/abc", 2, &virtual_bar());
        assert_eq!(before, "/a");
        assert_eq!(display, "/a|bc");

        let (_, display) = input_display("/abc", 4, &virtual_bar());
        assert_eq!(display, "/abc|");

        let (before, display) = input_display("/界a", 4, &virtual_bar());
        assert_eq!(before, "/界");
        assert_eq!(display, "/界|a");
    }

    #[test]
    fn input_display_native_keeps_message_untouched() {
        let (before, display) = input_display("/abc", 2, &CursorType::Native);
        assert_eq!(before, "/a");
        assert_eq!(display, "/abc");
    }

    /// 切點落在多 byte 字元中間時退回尾端，不 panic。
    #[test]
    fn input_display_falls_back_when_cursor_is_not_on_char_boundary() {
        let (before, display) = input_display("/界", 2, &virtual_bar());
        assert_eq!(before, "/界");
        assert_eq!(display, "/界|");

        let (before, _) = input_display("/ab", 99, &virtual_bar());
        assert_eq!(before, "/ab");
    }

    /// 回歸 #198：游標在中間時，它底下的字元不能被蓋掉。
    #[test]
    fn virtual_cursor_renders_between_chars_and_is_colored() {
        let state = input_state(virtual_bar(), "/abc", 2, None);
        let buf = render_input(&state, 20);

        let row = input_row(&buf);
        assert!(row.contains("/a|bc"), "{row}");
        assert_eq!(red_cells(&buf), vec![3]); // 左 padding 1 + "/a" 2 = x 3
    }

    /// 長 query、游標在中間、終端太窄：游標後的內容仍要以截斷形式看得到，
    /// 不能像拆 span 那樣整段消失。
    #[test]
    fn virtual_cursor_in_long_query_keeps_text_after_cursor_visible() {
        let state = input_state(virtual_bar(), "/abcdefghijklmnopqrstuvwxyz", 3, None);
        let buf = render_input(&state, 14);
        let row = input_row(&buf);

        assert!(row.contains("/ab|cdef"), "{row}");
        assert!(row.contains('…'), "{row}");
        assert_eq!(red_cells(&buf).len(), 1);
    }

    /// 游標在被截掉的尾端：該格已是 `…`，不能被染成游標色。
    #[test]
    fn virtual_cursor_truncated_away_colors_nothing() {
        let state = input_state(virtual_bar(), "/abcdefghijklmnopqrstuvwxyz", 27, None);
        let buf = render_input(&state, 14);

        let row = input_row(&buf);
        assert!(row.contains('…'), "{row}");
        assert!(red_cells(&buf).is_empty());
    }

    /// 游標多佔一格後，右側 transient 訊息仍要貼齊右緣、不被擠掉。
    #[test]
    fn virtual_cursor_keeps_transient_message_right_aligned() {
        let state = input_state(virtual_bar(), "/ab", 1, Some("OK"));
        let buf = render_input(&state, 20);
        let row = input_row(&buf);

        assert!(row.contains("/|ab"), "{row}");
        assert!(row.trim_end().ends_with("OK"), "{row}");
        assert_eq!(buf[(17, 1)].symbol(), "O"); // 右 padding 1 → 內容到 x 18
    }

    /// Native 游標只定位、不改字串。
    #[test]
    fn native_cursor_does_not_alter_rendered_text() {
        let state = input_state(CursorType::Native, "/abc", 2, None);
        let buf = render_input(&state, 20);

        let row = input_row(&buf);
        assert!(row.contains("/abc"), "{row}");
        assert!(red_cells(&buf).is_empty());
    }

    #[test]
    fn ref_picker_selects_and_sends_copy_to_clipboard() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::RefPicker {
            options: vec!["main".into(), "dev".into()],
            kind: RefCopyKind::Local,
            menu: Menu::default(),
        };

        state.handle_menu_key(char_key('2'));

        assert!(matches!(state.line, StatusLine::None));
        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::CopyToClipboard { name, value })
                if name == RefCopyKind::Local.copy_label() && value == "dev"
        ));
    }

    #[test]
    fn ref_picker_cancel_clears_without_sending() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::RefPicker {
            options: vec!["main".into()],
            kind: RefCopyKind::Local,
            menu: Menu::default(),
        };

        state.handle_menu_key(key(KeyCode::Esc));

        assert!(matches!(state.line, StatusLine::None));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn ref_picker_out_of_range_digit_is_ignored() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::RefPicker {
            options: vec!["main".into()],
            kind: RefCopyKind::Local,
            menu: Menu::default(),
        };

        state.handle_menu_key(char_key('9'));

        assert!(matches!(state.line, StatusLine::RefPicker { .. }));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn checkout_picker_selects_and_sends_checkout_commit() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::CheckoutPicker {
            options: vec!["v1.0".into(), "v2.0".into()],
            kind: CheckoutPickKind::Tag,
            menu: Menu::default(),
        };

        state.handle_menu_key(char_key('1'));

        assert!(matches!(state.line, StatusLine::None));
        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::CheckoutCommit { target }) if target == "v1.0"
        ));
    }

    #[test]
    fn child_picker_selects_and_sends_select_child_commit_by_hash() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::ChildPicker {
            options: vec![
                ChildPickOption {
                    label: "feature/x: fix a".into(),
                    commit_hash: CommitHash::from("aaa"),
                },
                ChildPickOption {
                    label: "fix b".into(),
                    commit_hash: CommitHash::from("bbb"),
                },
            ],
            total: 2,
            menu: Menu::default(),
        };

        state.handle_menu_key(char_key('2'));

        assert!(matches!(state.line, StatusLine::None));
        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::SelectChildCommitByHash { hash }) if hash.as_str() == "bbb"
        ));
    }

    #[test]
    fn child_picker_cancel_clears_without_sending() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::ChildPicker {
            options: vec![ChildPickOption {
                label: "fix a".into(),
                commit_hash: CommitHash::from("aaa"),
            }],
            total: 1,
            menu: Menu::default(),
        };

        state.handle_menu_key(key(KeyCode::Esc));

        assert!(matches!(state.line, StatusLine::None));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn open_child_picker_truncates_to_nine_but_keeps_total() {
        let (mut state, _rx) = test_state();
        let options: Vec<ChildPickOption> = (0..12)
            .map(|i| ChildPickOption {
                label: format!("child {i}"),
                commit_hash: CommitHash::from(format!("hash{i}").as_str()),
            })
            .collect();

        state.open_child_picker(options);

        let StatusLine::ChildPicker { options, total, .. } = &state.line else {
            panic!("expected ChildPicker, got {:?}", state.line);
        };
        assert_eq!(options.len(), 9);
        assert_eq!(*total, 12);
    }

    #[test]
    fn delete_branch_picker_selection_transitions_to_confirm() {
        let (mut state, _rx) = test_state();
        state.line = StatusLine::DeleteBranchPicker {
            options: vec!["feature/x".into()],
            total: 1,
            menu: Menu::default(),
        };

        state.handle_menu_key(char_key('1'));

        assert!(matches!(
            state.line,
            StatusLine::DeleteBranchConfirm { ref name, .. } if name == "feature/x"
        ));
    }

    #[test]
    fn delete_branch_confirm_y_sends_non_force_request() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::DeleteBranchConfirm {
            name: "feature/x".into(),
            menu: Menu::default(),
        };

        state.handle_menu_key(char_key('y'));

        assert!(matches!(state.line, StatusLine::None));
        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::DeleteBranchRequested { name, force: false }) if name == "feature/x"
        ));
    }

    /// `f` 綁在 `fetch`（assets/default-keybind.toml），不是沒綁到任何東西。
    /// `menu_key` 先比對 `MenuItem::quick` 的原始字元、再查 keybind 的
    /// `UserEvent`，強制刪除才不會被 `fetch` 吃掉，這條順序依賴只有真
    /// keybind 才驗得到（見 test_ctx 的說明）。
    #[test]
    fn delete_branch_confirm_f_sends_force_request_despite_being_bound_to_fetch() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::DeleteBranchConfirm {
            name: "feature/x".into(),
            menu: Menu::default(),
        };

        state.handle_menu_key(char_key('f'));

        assert!(matches!(state.line, StatusLine::None));
        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::DeleteBranchRequested { name, force: true }) if name == "feature/x"
        ));
    }

    #[test]
    fn delete_branch_confirm_n_clears_without_sending() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::DeleteBranchConfirm {
            name: "feature/x".into(),
            menu: Menu::default(),
        };

        state.handle_menu_key(char_key('n'));

        assert!(matches!(state.line, StatusLine::None));
        assert!(rx.try_recv().is_err());
    }

    fn merge_pr_prompt() -> StatusLine {
        merge_pr_prompt_with_deletable(true, Some(false), Some("origin/feature/x"))
    }

    fn merge_pr_prompt_with_deletable(
        remote_deletable: bool,
        local_delete: Option<bool>,
        tracking_ref: Option<&str>,
    ) -> StatusLine {
        merge_pr_prompt_line(
            42,
            "feature/x".into(),
            StateFilter::Open,
            remote_deletable,
            local_delete,
            tracking_ref.map(String::from),
        )
    }

    /// 取出目前表單的 (method, rows, cursor)。
    fn merge_form(state: &StatusLineState) -> (MergeMethod, Vec<(MergeDelete, bool)>, usize) {
        match &state.line {
            StatusLine::MergePrPrompt {
                stage:
                    MergePrStage::Form {
                        method,
                        rows,
                        cursor,
                    },
                ..
            } => (*method, rows.clone(), *cursor),
            other => panic!("不在表單：{other:?}"),
        }
    }

    fn is_merge_confirm(state: &StatusLineState) -> bool {
        matches!(
            state.line,
            StatusLine::MergePrPrompt {
                stage: MergePrStage::Confirm { .. },
                ..
            }
        )
    }

    /// 預設：squash、本機 ✔、遠端 ✔、追蹤 ref ✘，順序 Local → Remote → Tracking。
    #[test]
    fn merge_pr_form_defaults() {
        let (mut state, _rx) = test_state();
        state.line = merge_pr_prompt();

        let (method, rows, cursor) = merge_form(&state);
        assert_eq!(method.display(), "squash");
        assert_eq!(
            rows,
            vec![
                (MergeDelete::Local, true),
                (MergeDelete::Remote, true),
                (MergeDelete::Tracking, false),
            ]
        );
        assert_eq!(cursor, 0);
    }

    /// 不可刪的項目整列不出現：本機找不到、遠端不可刪、沒有追蹤 ref。
    #[test]
    fn merge_pr_form_omits_rows_that_cannot_be_deleted() {
        let (mut state, _rx) = test_state();

        state.line = merge_pr_prompt_with_deletable(false, None, None);
        assert!(merge_form(&state).1.is_empty());

        state.line = merge_pr_prompt_with_deletable(true, None, None);
        assert_eq!(merge_form(&state).1, vec![(MergeDelete::Remote, true)]);

        state.line = merge_pr_prompt_with_deletable(false, Some(false), Some("origin/feature/x"));
        assert_eq!(
            merge_form(&state).1,
            vec![(MergeDelete::Local, true), (MergeDelete::Tracking, false)]
        );
    }

    /// 方式列 ←／→ 循環（含繞回）；游標不在方式列時不動 method。
    #[test]
    fn merge_pr_form_cycles_method_on_first_row() {
        let (mut state, _rx) = test_state();
        state.line = merge_pr_prompt();

        state.handle_merge_pr_prompt_key(key(KeyCode::Right)); // squash → rebase
        assert_eq!(merge_form(&state).0.display(), "rebase");
        state.handle_merge_pr_prompt_key(key(KeyCode::Right)); // rebase → merge
        assert_eq!(merge_form(&state).0.display(), "merge");
        state.handle_merge_pr_prompt_key(key(KeyCode::Left)); // merge → rebase
        assert_eq!(merge_form(&state).0.display(), "rebase");

        state.handle_merge_pr_prompt_key(key(KeyCode::Down));
        state.handle_merge_pr_prompt_key(key(KeyCode::Right));
        assert_eq!(merge_form(&state).0.display(), "rebase");
    }

    /// ↑／↓ 循環移動；勾選列 ←／→ 切換該列。
    #[test]
    fn merge_pr_form_moves_cursor_and_toggles_rows() {
        let (mut state, _rx) = test_state();
        state.line = merge_pr_prompt();

        state.handle_merge_pr_prompt_key(key(KeyCode::Up)); // 0 → 最後一列
        assert_eq!(merge_form(&state).2, 3);
        state.handle_merge_pr_prompt_key(key(KeyCode::Right)); // 切換 Tracking
        assert_eq!(merge_form(&state).1[2], (MergeDelete::Tracking, true));
        state.handle_merge_pr_prompt_key(key(KeyCode::Left));
        assert_eq!(merge_form(&state).1[2], (MergeDelete::Tracking, false));
        state.handle_merge_pr_prompt_key(key(KeyCode::Down)); // 最後一列 → 方式列
        assert_eq!(merge_form(&state).2, 0);
    }

    /// 沒有任何可刪項目時只剩方式列：上下移動留在 0，左右只切方式，不 panic。
    #[test]
    fn merge_pr_form_with_only_method_row_does_not_panic() {
        let (mut state, _rx) = test_state();
        state.line = merge_pr_prompt_with_deletable(false, None, None);

        state.handle_merge_pr_prompt_key(key(KeyCode::Down));
        state.handle_merge_pr_prompt_key(key(KeyCode::Up));
        state.handle_merge_pr_prompt_key(key(KeyCode::Right));

        let (method, rows, cursor) = merge_form(&state);
        assert_eq!(method.display(), "rebase");
        assert!(rows.is_empty());
        assert_eq!(cursor, 0);
    }

    /// 表單 Enter 只進確認彈窗，不送事件；確認彈窗 Enter 才送出，欄位與勾選一致。
    #[test]
    fn merge_pr_form_then_confirm_sends_request_with_checked_rows() {
        let (mut state, rx) = test_state();
        state.line = merge_pr_prompt();

        state.handle_merge_pr_prompt_key(key(KeyCode::Enter));
        assert!(is_merge_confirm(&state));
        assert!(rx.try_recv().is_err());

        state.handle_merge_pr_prompt_key(key(KeyCode::Enter));
        assert!(matches!(state.line, StatusLine::None));
        // 預設：本機（force_safe 原樣帶出 `Some(false)` → false）＋遠端，不含追蹤 ref。
        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::MergePrRequested {
                number: 42,
                method: MergeMethod::Squash,
                delete_local_branch: Some((local, false)),
                delete_remote_branch: Some(remote),
                delete_tracking_ref: None,
                ..
            }) if local == "feature/x" && remote == "feature/x"
        ));
    }

    /// 改選方式與勾選後送出：追蹤 ref 原樣帶出，取消勾選的本機分支為 `None`。
    #[test]
    fn merge_pr_request_reflects_edited_form() {
        let (mut state, rx) = test_state();
        state.line = merge_pr_prompt();

        state.handle_merge_pr_prompt_key(key(KeyCode::Left)); // squash → merge
        state.handle_merge_pr_prompt_key(key(KeyCode::Down)); // Local
        state.handle_merge_pr_prompt_key(key(KeyCode::Right)); // 取消勾選
        state.handle_merge_pr_prompt_key(key(KeyCode::Down)); // Remote
        state.handle_merge_pr_prompt_key(key(KeyCode::Down)); // Tracking
        state.handle_merge_pr_prompt_key(key(KeyCode::Right)); // 勾選
        state.handle_merge_pr_prompt_key(key(KeyCode::Enter));
        state.handle_merge_pr_prompt_key(key(KeyCode::Enter));

        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::MergePrRequested {
                method: MergeMethod::Merge,
                delete_local_branch: None,
                delete_remote_branch: Some(_),
                delete_tracking_ref: Some(t),
                ..
            }) if t == "origin/feature/x"
        ));
    }

    /// 本機分支需強刪（`local_delete: Some(true)`）時，force_safe 要原樣傳出去。
    #[test]
    fn merge_pr_request_passes_force_safe_through() {
        let (mut state, rx) = test_state();
        state.line = merge_pr_prompt_with_deletable(false, Some(true), None);

        state.handle_merge_pr_prompt_key(key(KeyCode::Enter));
        state.handle_merge_pr_prompt_key(key(KeyCode::Enter));

        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::MergePrRequested {
                delete_local_branch: Some((_, true)),
                delete_remote_branch: None,
                ..
            })
        ));
    }

    #[test]
    fn merge_pr_cancel_in_form_clears_without_sending() {
        let (mut state, rx) = test_state();
        state.line = merge_pr_prompt();

        state.handle_merge_pr_prompt_key(key(KeyCode::Esc));

        assert!(matches!(state.line, StatusLine::None));
        assert!(rx.try_recv().is_err());
    }

    /// 確認彈窗取消 = 整個流程取消（不退回表單），`n` 也是取消。
    #[test]
    fn merge_pr_cancel_in_confirm_clears_without_sending() {
        let (mut state, rx) = test_state();
        for cancel in [key(KeyCode::Esc), char_key('n')] {
            state.line = merge_pr_prompt();
            state.handle_merge_pr_prompt_key(key(KeyCode::Enter));
            assert!(is_merge_confirm(&state));

            state.handle_merge_pr_prompt_key(cancel);

            assert!(matches!(state.line, StatusLine::None));
            assert!(rx.try_recv().is_err());
        }
    }

    /// 兩個彈窗各自畫出該有的內容：表單有方式列與勾選框，確認有摘要。
    #[test]
    fn merge_pr_popups_render_expected_text() {
        use ratatui::{backend::TestBackend, Terminal};

        fn screen(state: &StatusLineState) -> String {
            screen_at(state, 70)
        }

        fn screen_at(state: &StatusLineState, width: u16) -> String {
            let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
            terminal.draw(|f| state.render_popup(f, f.area())).unwrap();
            let buf = terminal.backend().buffer().clone();
            (0..buf.area.height)
                .map(|y| {
                    (0..buf.area.width)
                        .map(|x| buf[(x, y)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        }

        let (mut state, _rx) = test_state();
        state.line = merge_pr_prompt();
        let form = screen(&state);
        assert!(form.contains("< squash >"), "{form}");
        assert!(form.contains("[x]"), "{form}");
        assert!(form.contains("[ ]"), "{form}");

        state.handle_merge_pr_prompt_key(key(KeyCode::Enter));
        let confirm = screen(&state);
        assert!(confirm.contains("squash"), "{confirm}");
        assert!(!confirm.contains("[x]"), "{confirm}");

        state.line = StatusLine::None;
        assert!(!screen(&state).contains("squash"));

        // 40 欄（手機）：提示換行，每個按鍵說明都完整；彈窗不超出畫面
        state.line = merge_pr_prompt();
        let narrow = screen_at(&state, 40);
        for hint in ["Enter:", "Esc:", "h/l:", "k/j:"] {
            assert!(narrow.contains(hint), "{hint} missing:\n{narrow}");
        }
        assert!(narrow.contains("< squash >"), "{narrow}");
    }

    #[test]
    fn yes_no_prompt_confirm_sends_toggle_state_request() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::ToggleStatePrompt {
            number: 7,
            kind: GhItemKind::Issue,
            action: StateAction::Close,
            filter_state: StateFilter::Open,
            menu: Menu::default(),
        };

        state.handle_menu_key(char_key('y'));

        assert!(matches!(state.line, StatusLine::None));
        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::ToggleItemStateRequested {
                number: 7,
                kind: GhItemKind::Issue,
                action: StateAction::Close,
                ..
            })
        ));
    }

    #[test]
    fn ci_rerun_prompt_only_accepts_options_that_have_runs() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::CiRerunPrompt {
            number: 3,
            failed_runs: vec![],
            all_runs: vec![10, 11],
            menu: Menu::default(),
        };

        // 沒有可重跑的失敗 run：f 無效，prompt 留著
        state.handle_menu_key(char_key('f'));
        assert!(matches!(state.line, StatusLine::CiRerunPrompt { .. }));
        assert!(rx.try_recv().is_err());

        state.handle_menu_key(char_key('a'));
        assert!(matches!(state.line, StatusLine::None));
        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::CiRerunRequested {
                number: 3,
                scope: RerunScope::All,
                run_ids,
            }) if run_ids == vec![10, 11]
        ));

        state.line = StatusLine::CiRerunPrompt {
            number: 3,
            failed_runs: vec![10],
            all_runs: vec![10, 11],
            menu: Menu::default(),
        };
        state.handle_menu_key(char_key('f'));
        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::CiRerunRequested {
                scope: RerunScope::FailedOnly,
                run_ids,
                ..
            }) if run_ids == vec![10]
        ));
    }

    #[test]
    fn yes_no_prompt_confirm_sends_toggle_pr_draft_request() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::TogglePrDraftPrompt {
            number: 9,
            action: PrDraftAction::MarkReady,
            filter_state: StateFilter::Open,
            menu: Menu::default(),
        };

        // l 勾選第一列，Enter 才送出
        state.handle_menu_key(char_key('l'));
        assert!(rx.try_recv().is_err());
        state.handle_menu_key(key(KeyCode::Enter));

        assert!(matches!(state.line, StatusLine::None));
        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::TogglePrDraftRequested {
                number: 9,
                action: PrDraftAction::MarkReady,
                ..
            })
        ));
    }

    fn related_items(n: u64) -> Vec<RelatedItem> {
        use crate::event::RelatedGroup;
        (1..=n)
            .map(|number| RelatedItem {
                number,
                state: "OPEN".into(),
                group: if number <= 2 {
                    RelatedGroup::Parent
                } else {
                    RelatedGroup::Sub
                },
            })
            .collect()
    }

    /// j 移動、l 勾選、Enter 送出游標那一項。
    #[test]
    fn menu_moves_marks_and_enter_picks_marked_row() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::CheckoutPicker {
            options: vec!["a".into(), "b".into(), "c".into()],
            kind: CheckoutPickKind::Branch,
            menu: Menu::default(),
        };

        state.handle_menu_key(char_key('j'));
        state.handle_menu_key(char_key('l'));
        assert!(rx.try_recv().is_err());
        state.handle_menu_key(key(KeyCode::Enter));

        assert!(matches!(state.line, StatusLine::None));
        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::CheckoutCommit { target }) if target == "b"
        ));
    }

    /// 沒有勾任何列時 Enter 什麼都不做（double check）。
    #[test]
    fn menu_enter_without_mark_does_nothing() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::CheckoutPicker {
            options: vec!["a".into()],
            kind: CheckoutPickKind::Branch,
            menu: Menu::default(),
        };

        state.handle_menu_key(key(KeyCode::Enter));

        assert!(matches!(state.line, StatusLine::CheckoutPicker { .. }));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn menu_h_cancels_without_sending() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::CheckoutPicker {
            options: vec!["a".into()],
            kind: CheckoutPickKind::Branch,
            menu: Menu::default(),
        };

        state.handle_menu_key(char_key('h'));

        assert!(matches!(state.line, StatusLine::None));
        assert!(rx.try_recv().is_err());
    }

    /// 帶 Ctrl 的快選鍵不能觸發強制刪除。
    #[test]
    fn menu_quick_key_ignores_ctrl_modifier() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::DeleteBranchConfirm {
            name: "feature/x".into(),
            menu: Menu::default(),
        };

        state.handle_menu_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));

        assert!(matches!(state.line, StatusLine::DeleteBranchConfirm { .. }));
        assert!(rx.try_recv().is_err());
    }

    /// y/n 類：`2` 與 `n` 都是取消，不送事件。
    #[test]
    fn yes_no_prompt_second_row_and_n_cancel() {
        for cancel in ['2', 'n'] {
            let (mut state, rx) = test_state();
            state.line = StatusLine::UpdatePrompt {
                tag: "v9.9.9".into(),
                menu: Menu::default(),
            };

            state.handle_menu_key(char_key(cancel));

            assert!(matches!(state.line, StatusLine::None));
            assert!(rx.try_recv().is_err());
        }
    }

    /// 只有失敗項時，第 2 列與 `a` 都不存在。
    #[test]
    fn ci_rerun_prompt_ignores_rows_that_do_not_exist() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::CiRerunPrompt {
            number: 5,
            failed_runs: vec![10],
            all_runs: vec![],
            menu: Menu::default(),
        };

        state.handle_menu_key(char_key('2'));
        state.handle_menu_key(char_key('a'));

        assert!(matches!(state.line, StatusLine::CiRerunPrompt { .. }));
        assert!(rx.try_recv().is_err());
    }

    /// group 標題列夾在可選列之間，pick 仍對應到正確的 issue。
    #[test]
    fn related_picker_pick_ignores_group_header_rows() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::RelatedPicker {
            items: related_items(4),
            menu: Menu::default(),
        };

        state.handle_menu_key(char_key('3'));

        assert!(matches!(state.line, StatusLine::None));
        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::GitHubJumpToIssue { number: 3 })
        ));
    }

    fn popup_screen(state: &StatusLineState, width: u16, height: u16) -> String {
        use ratatui::{backend::TestBackend, Terminal};

        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| state.render_popup(f, f.area())).unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn menu_popup_renders_numbered_rows_and_marks() {
        let (mut state, _rx) = test_state();
        state.line = StatusLine::CheckoutPicker {
            options: vec!["main".into(), "dev".into()],
            kind: CheckoutPickKind::Branch,
            menu: Menu::default(),
        };
        state.handle_menu_key(char_key('l'));

        let screen = popup_screen(&state, 70, 20);

        assert!(screen.contains("[x]1 main"), "{screen}");
        assert!(screen.contains("[ ]2 dev"), "{screen}");
    }

    /// 項目比畫面高時，游標移到最後一個仍然看得到。
    #[test]
    fn menu_popup_scrolls_to_keep_cursor_visible() {
        let (mut state, _rx) = test_state();
        state.line = StatusLine::RelatedPicker {
            items: related_items(15),
            menu: Menu::default(),
        };
        for _ in 0..14 {
            state.handle_menu_key(char_key('j'));
        }

        let screen = popup_screen(&state, 70, 12);

        assert!(screen.contains("#15"), "{screen}");
        assert!(!screen.contains("#1 "), "{screen}");
    }

    /// 選項畫在彈窗裡，不再塞進狀態列。
    #[test]
    fn menu_popup_shows_options() {
        let (mut state, _rx) = test_state();
        state.line = StatusLine::RefPicker {
            options: vec!["main".into()],
            kind: RefCopyKind::Local,
            menu: Menu::default(),
        };

        let screen = popup_screen(&state, 70, 20);

        assert!(screen.contains("main"), "{screen}");
    }

    #[test]
    fn yes_no_prompt_n_clears_without_sending() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::ToggleStatePrompt {
            number: 7,
            kind: GhItemKind::Issue,
            action: StateAction::Close,
            filter_state: StateFilter::Open,
            menu: Menu::default(),
        };

        state.handle_menu_key(char_key('n'));

        assert!(matches!(state.line, StatusLine::None));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn restart_prompt_confirm_sends_restart_request() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::RestartPrompt {
            tag: "v2.6.0".to_string(),
            exe: PathBuf::from("/usr/local/bin/ysgit"),
            menu: Menu::default(),
        };

        state.handle_menu_key(char_key('y'));

        assert!(matches!(state.line, StatusLine::None));
        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::RestartRequested { exe }) if exe == Path::new("/usr/local/bin/ysgit")
        ));
    }

    /// 跟其餘三個 y/n prompt 不同：`RestartPrompt` 取消時更新其實已經做完了，
    /// 靜默清空會讓使用者忘記要手動重啟，所以要留一句提醒——這是整批改動裡
    /// 唯一的特判，沒有這個測試擋著，日後 `/simplify` 很容易把它當成多餘的
    /// 分歧併掉。
    #[test]
    fn restart_prompt_cancel_leaves_a_reminder_instead_of_clearing() {
        let (mut state, rx) = test_state();
        state.line = StatusLine::RestartPrompt {
            tag: "v2.6.0".to_string(),
            exe: PathBuf::from("/usr/local/bin/ysgit"),
            menu: Menu::default(),
        };

        state.handle_menu_key(char_key('n'));

        assert!(matches!(
            &state.line,
            StatusLine::NotificationSuccess(msg) if *msg == update_installed_hint()
        ));
        assert!(rx.try_recv().is_err());
    }

    /// `StatusLine` 全部 18 個變體各自對 `handle_intercepting_key`／
    /// `is_input_mode_variant` 的回傳值，把「兩者故意不同步」這件事從一句
    /// 註解變成有東西擋著的不變式（做法比照這個 session 的 `2fa4064` 先
    /// 例）。6 個 prompt 變體在 handle_intercepting_key 永遠攔截，但
    /// is_input_mode_variant 故意不含它們——見兩個方法各自的文件註解。
    #[test]
    fn intercept_and_input_mode_tables_stay_intentionally_out_of_sync() {
        fn make(line: StatusLine) -> StatusLineState {
            let (tx, _rx) = Sender::channel_for_test();
            StatusLineState {
                line,
                ctx: test_ctx(),
                tx,
                auto_fetch_clock: AutoFetchClock::default(),
            }
        }

        let cases: Vec<(&str, StatusLine, bool, bool)> = vec![
            ("None", StatusLine::None, false, false),
            (
                "Input",
                StatusLine::Input(String::new(), 0, None),
                false,
                true,
            ),
            (
                "RefPicker",
                StatusLine::RefPicker {
                    options: vec![],
                    kind: RefCopyKind::Local,
                    menu: Menu::default(),
                },
                true,
                true,
            ),
            (
                "CheckoutPicker",
                StatusLine::CheckoutPicker {
                    options: vec![],
                    kind: CheckoutPickKind::Branch,
                    menu: Menu::default(),
                },
                true,
                true,
            ),
            (
                "ChildPicker",
                StatusLine::ChildPicker {
                    options: vec![],
                    total: 0,
                    menu: Menu::default(),
                },
                true,
                true,
            ),
            (
                "RelatedPicker",
                StatusLine::RelatedPicker {
                    items: vec![],
                    menu: Menu::default(),
                },
                true,
                true,
            ),
            (
                "DeleteBranchPicker",
                StatusLine::DeleteBranchPicker {
                    options: vec![],
                    total: 0,
                    menu: Menu::default(),
                },
                true,
                true,
            ),
            (
                "DeleteBranchConfirm",
                StatusLine::DeleteBranchConfirm {
                    name: String::new(),
                    menu: Menu::default(),
                },
                true,
                true,
            ),
            ("MergePrPrompt", merge_pr_prompt(), true, false),
            (
                "CiRerunPrompt",
                StatusLine::CiRerunPrompt {
                    number: 0,
                    failed_runs: vec![],
                    all_runs: vec![],
                    menu: Menu::default(),
                },
                true,
                false,
            ),
            (
                "ToggleStatePrompt",
                StatusLine::ToggleStatePrompt {
                    number: 0,
                    kind: GhItemKind::Issue,
                    action: StateAction::Close,
                    filter_state: StateFilter::Open,
                    menu: Menu::default(),
                },
                true,
                false,
            ),
            (
                "TogglePrDraftPrompt",
                StatusLine::TogglePrDraftPrompt {
                    number: 0,
                    action: PrDraftAction::MarkReady,
                    filter_state: StateFilter::Open,
                    menu: Menu::default(),
                },
                true,
                false,
            ),
            (
                "UpdatePrompt",
                StatusLine::UpdatePrompt {
                    tag: String::new(),
                    menu: Menu::default(),
                },
                true,
                false,
            ),
            (
                "RestartPrompt",
                StatusLine::RestartPrompt {
                    tag: String::new(),
                    exe: PathBuf::new(),
                    menu: Menu::default(),
                },
                true,
                false,
            ),
            (
                "NotificationInfo",
                StatusLine::NotificationInfo(String::new()),
                false,
                false,
            ),
            (
                "NotificationSuccess",
                StatusLine::NotificationSuccess(String::new()),
                false,
                false,
            ),
            (
                "NotificationWarn",
                StatusLine::NotificationWarn(String::new()),
                false,
                false,
            ),
            (
                "NotificationError",
                StatusLine::NotificationError(String::new()),
                false,
                false,
            ),
        ];

        assert_eq!(
            cases.len(),
            18,
            "StatusLine 有 18 個變體，表格漏列或多列了，先檢查表格本身"
        );

        for (label, line, want_intercept, want_input_mode) in cases {
            // handle_intercepting_key 在 12 個攔截變體上會呼叫對應 handler、
            // 可能改變 self.line 或送事件——這裡只關心它的回傳值，用一個
            // 全新的 state 避免副作用互相汙染。
            let mut for_intercept = make(line.clone());
            let got_intercept = for_intercept.handle_intercepting_key(key(KeyCode::Null));
            assert_eq!(
                got_intercept, want_intercept,
                "{label}: handle_intercepting_key 回傳值不對"
            );

            let for_input_mode = make(line);
            let got_input_mode = for_input_mode.is_input_mode_variant();
            assert_eq!(
                got_input_mode, want_input_mode,
                "{label}: is_input_mode_variant 回傳值不對"
            );
        }
    }

    /// `blocks_swap` 決定 Phase 6 換資料能不能進行。跟
    /// `is_idle_or_notification` 唯一的差異是 `NotificationError` 也算
    /// 「不擋」（重載失敗不該卡住下一次成功結果），`Input`（正在打字的
    /// search／filter）則反過來一定要擋，換資料會重建 `StatusLineState`
    /// 把半打的字串吃掉。
    #[test]
    fn blocks_swap_matches_expected_table() {
        fn make(line: StatusLine) -> StatusLineState {
            let (tx, _rx) = Sender::channel_for_test();
            StatusLineState {
                line,
                ctx: test_ctx(),
                tx,
                auto_fetch_clock: AutoFetchClock::default(),
            }
        }

        let cases: Vec<(&str, StatusLine, bool)> = vec![
            ("None", StatusLine::None, false),
            (
                "NotificationInfo",
                StatusLine::NotificationInfo(String::new()),
                false,
            ),
            (
                "NotificationSuccess",
                StatusLine::NotificationSuccess(String::new()),
                false,
            ),
            (
                "NotificationWarn",
                StatusLine::NotificationWarn(String::new()),
                false,
            ),
            (
                "NotificationError",
                StatusLine::NotificationError(String::new()),
                false,
            ),
            ("Input", StatusLine::Input(String::new(), 0, None), true),
            (
                "RefPicker",
                StatusLine::RefPicker {
                    options: vec![],
                    kind: RefCopyKind::Local,
                    menu: Menu::default(),
                },
                true,
            ),
            ("MergePrPrompt", merge_pr_prompt(), true),
        ];

        for (label, line, want_blocks) in cases {
            let got = make(line).blocks_swap();
            assert_eq!(got, want_blocks, "{label}: blocks_swap 回傳值不對");
        }
    }

    /// 換資料時取出通知、重建之後放回去，內容原樣還原；其餘狀態（picker、
    /// prompt、輸入框）不受影響，`take_notification` 回 `None`。
    #[test]
    fn take_and_restore_notification_round_trips() {
        let (mut state, _rx) = test_state();
        state.set_notification_error("boom".to_string());

        let notification = state.take_notification().expect("應該取到通知");
        assert!(matches!(state.line, StatusLine::None));

        state.restore_notification(notification);
        assert!(matches!(&state.line, StatusLine::NotificationError(m) if m == "boom"));
    }

    /// 任何通知都吞鍵並清掉；非通知狀態不吞鍵、也不動狀態列。
    #[test]
    fn dismiss_notification_swallows_key_for_every_notification() {
        let notifications = [
            StatusLine::NotificationInfo("m".into()),
            StatusLine::NotificationSuccess("m".into()),
            StatusLine::NotificationWarn("m".into()),
            StatusLine::NotificationError("m".into()),
        ];
        for line in notifications {
            let (mut state, _rx) = test_state();
            state.line = line;
            assert!(state.dismiss_notification());
            assert!(matches!(state.line, StatusLine::None));
        }

        let (mut state, _rx) = test_state();
        assert!(!state.dismiss_notification());
    }

    #[test]
    fn take_notification_on_picker_returns_none_and_leaves_it_untouched() {
        let (mut state, _rx) = test_state();
        state.line = StatusLine::RefPicker {
            options: vec!["main".into()],
            kind: RefCopyKind::Local,
            menu: Menu::default(),
        };

        assert!(state.take_notification().is_none());
        assert!(matches!(state.line, StatusLine::RefPicker { .. }));
    }
}

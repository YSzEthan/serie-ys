mod event;
mod preview;
mod render;
mod timeline;

use rust_i18n::t;
use std::{
    borrow::Cow,
    cell::Cell,
    time::{Duration, Instant},
};

use ratatui::{style::Color, text::Line};
use rustc_hash::FxHashMap;
use tui_input::Input;

use crate::{
    event::{AppEvent, Sender, UserEvent},
    github::{
        CheckboxItem, GhIssue, GhItemKind, GhPullRequest, GhTimelinePage, GitHubData,
        PrDraftAction, StateAction, StateFilter,
    },
    view::View,
    widget::{h, HintSpec},
};

use preview::{PreviewCache, PreviewInput, SelectedItem, SelectedItemExtra};
use timeline::{TimelineEntry, TimelineLoad};

const PREFETCH_THRESHOLD: usize = 5;
const TIMELINE_LOAD_MORE_THRESHOLD: usize = 5;
/// 選取停住多久才真的送 timeline 請求；按住 `j` 滑過去的項目不必各打一次 API。
/// 由 100ms 的 Tick 輪詢，實際延遲是 150～250ms。
const TIMELINE_DEBOUNCE: Duration = Duration::from_millis(150);

/// 分隔線關閉的是 timeline 的哪個區段。線條顏色由分隔線*之前*的內容決定，
/// 不是後面的——由上往下讀時，那才是眼睛在捲動時需要的上下文；標籤文字
/// 則寫出*後面*是什麼，用後面那個區段自己的顏色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Body,
    Comment,
    Commit,
    Ci,
    Review,
}

impl Section {
    /// 用 Indexed 而非 Rgb，讓這些顏色在不支援 truecolor 的終端機上也能顯示。
    fn color(self) -> Color {
        match self {
            Section::Body => Color::Indexed(146),    // 粉藍灰 (#afafd7)
            Section::Comment => Color::Indexed(151), // 粉綠灰 (#afd7af)
            Section::Commit => Color::Indexed(186),  // 粉黃灰 (#d7d787)
            Section::Ci => Color::Indexed(152),      // 粉青灰 (#afd7d7)
            Section::Review => Color::Indexed(181),  // 粉紅灰 (#d7afaf)
        }
    }

    fn label(self) -> Cow<'static, str> {
        match self {
            Section::Body => t!("github.section.body"),
            Section::Comment => t!("github.section.comment"),
            Section::Commit => t!("github.section.commit"),
            Section::Ci => t!("github.section.ci"),
            Section::Review => t!("github.section.review"),
        }
    }

    /// `self` 是分隔線上方的區段，`next` 是下方的。
    fn divider(self, next: Section, width: usize) -> Line<'static> {
        super::markdown::labeled_rule(width, &next.label(), self.color(), next.color())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitHubTab {
    Issues,
    PullRequests,
}

impl GitHubTab {
    fn kind(self) -> GhItemKind {
        match self {
            GitHubTab::Issues => GhItemKind::Issue,
            GitHubTab::PullRequests => GhItemKind::PullRequest,
        }
    }
}

#[derive(Debug)]
struct TaskListPanel {
    number: u64,
    kind: GhItemKind,
    items: Vec<CheckboxItem>,
    original_checked: Vec<bool>,
    selected: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitHubFocus {
    List,
    Preview,
    Prompt,
    CheckboxEdit,
}

#[derive(Debug, Default)]
enum LoadState {
    #[default]
    Idle,
    Loading,
    Error(String),
}

/// GitHub 模式列表 labels 的顯示方式，`t` 切換。色塊模式讓標題有更多空間，
/// 代價是看不到 label 名稱——preview 永遠顯示完整名稱，不受這個設定影響。
///
/// 沒有另外分配鍵位：`t` 在 GitHub 模式下沒有既有用途（`create_tag` 只在
/// commit 清單有意義），直接重用 `UserEvent::CreateTag` 觸發，省一個新的
/// config 鍵名，`t` 在 commit 清單仍照舊是建立 tag。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum LabelMode {
    #[default]
    Names,
    Swatches,
}

#[derive(Debug)]
pub struct GitHubView<'a> {
    before: View<'a>,

    focus: GitHubFocus,

    active_tab: GitHubTab,
    issues: Vec<GhIssue>,
    pull_requests: Vec<GhPullRequest>,
    selected_index: usize,
    offset: usize,
    height: usize,

    preview_offset: usize,

    search_input: Input,
    filtered_issue_indices: Vec<usize>,
    filtered_pr_indices: Vec<usize>,

    state_filter: StateFilter,

    task_panel: Option<TaskListPanel>,

    load_state: LoadState,

    flash_message: Option<(String, bool)>,

    /// 渲染時的溢出旗標（選取列的 title+author 寬度超過可用空間）。
    /// App 讀取這個值來決定要不要跳動 `marquee_frame`。
    selected_row_overflows: Cell<bool>,

    issues_next_cursor: Option<String>,
    prs_next_cursor: Option<String>,
    loading_more: bool,
    request_generation: u64,

    pending_jump: Option<u64>,

    timeline: FxHashMap<(GhItemKind, u64), TimelineEntry>,
    /// 選取變動後排定的 timeline 請求時間，由 `on_tick` 到期時送出。
    timeline_due: Option<Instant>,
    last_preview_len: usize,

    /// 在 preview 能顯示的任何就地編輯時遞增——目前是 body 取代與批次重新
    /// 載入。`set_pr_draft_flag` 刻意不遞增，因為 `is_draft` 從未傳到
    /// `build_preview_content`；等哪天 preview 開始顯示 draft 狀態，這點就要改。
    body_rev: u64,
    preview_cache: PreviewCache,
    /// Preview 內容高度，由 `render_preview` 記錄。捲動處理函式直接用這個值，
    /// 不會從 `height` 重新推算。
    preview_height: usize,
    /// commit log 是逐筆顯示還是收合成單一摘要列。全有全無（`z` 切換整個
    /// log），不是逐筆切換。
    expand_commits: bool,
    /// labels 顯示名稱還是色塊，`t` 切換（重用 `CreateTag`，理由見上面
    /// `LabelMode` 的註解）。session 內由 `App::github_label_mode` 保存
    /// （關掉 GitHub 模式再開仍維持），重啟 serie 回預設。
    label_mode: LabelMode,

    tx: Sender,
}

impl<'a> GitHubView<'a> {
    /// 一律從 Loading 起跳：唯一呼叫者 `App::open_github` 開 view 時必定
    /// 送出 refresh，帶快取重開也一樣。
    pub fn new(before: View<'a>, data: GitHubData, tx: Sender) -> GitHubView<'a> {
        GitHubView {
            before,
            focus: GitHubFocus::List,
            active_tab: GitHubTab::Issues,
            issues: data.issues,
            pull_requests: data.pull_requests,
            selected_index: 0,
            offset: 0,
            height: 0,
            preview_offset: 0,
            search_input: Input::default(),
            filtered_issue_indices: Vec::new(),
            filtered_pr_indices: Vec::new(),
            state_filter: data.state_filter,
            task_panel: None,
            load_state: LoadState::Loading,
            flash_message: None,
            selected_row_overflows: Cell::new(false),
            issues_next_cursor: data.issues_next_cursor,
            prs_next_cursor: data.prs_next_cursor,
            loading_more: false,
            request_generation: 0,
            pending_jump: None,
            timeline: FxHashMap::default(),
            // 帶快取資料重開時沒有任何選取變動或 update_data 會觸發請求，
            // 由第一個 Tick 補送初始選取項目的 timeline。
            timeline_due: Some(Instant::now()),
            last_preview_len: 0,
            body_rev: 0,
            preview_cache: PreviewCache::default(),
            preview_height: 0,
            expand_commits: true,
            label_mode: LabelMode::default(),
            tx,
        }
    }

    pub fn marquee_id(&self) -> Option<std::sync::Arc<str>> {
        let tab = match self.active_tab {
            GitHubTab::Issues => "issues",
            GitHubTab::PullRequests => "prs",
        };
        let idx = self.actual_index(self.selected_index);
        let num = match self.active_tab {
            GitHubTab::Issues => self.issues.get(idx).map(|i| i.number)?,
            GitHubTab::PullRequests => self.pull_requests.get(idx).map(|p| p.number)?,
        };
        Some(std::sync::Arc::from(format!("gh:{tab}:{num}")))
    }

    pub fn marquee_needed(&self) -> bool {
        self.selected_row_overflows.get()
    }

    pub fn take_before_view(&mut self) -> View<'a> {
        std::mem::take(&mut self.before)
    }

    pub(super) fn before_view_mut(&mut self) -> &mut View<'a> {
        &mut self.before
    }

    /// 交出目前持有的資料快照，供 `App` 在關閉 view 時暫存。比照
    /// [`Self::take_before_view`]：view 即將被丟棄，直接 take 沒有殘留成本。
    pub fn take_data(&mut self) -> GitHubData {
        GitHubData {
            issues: std::mem::take(&mut self.issues),
            pull_requests: std::mem::take(&mut self.pull_requests),
            state_filter: self.state_filter,
            issues_next_cursor: self.issues_next_cursor.take(),
            prs_next_cursor: self.prs_next_cursor.take(),
        }
    }

    pub fn state_filter(&self) -> StateFilter {
        self.state_filter
    }

    pub fn label_mode(&self) -> LabelMode {
        self.label_mode
    }

    /// 只給 `View::of_github` 在建構時設定初始值（從 `App::github_label_mode`
    /// 帶入 session 記住的選擇）——消費 `self` 讓「只能在建構時用一次」
    /// 直接寫進型別，不用靠註解自律；其餘地方一律用 `toggle_label_mode`。
    pub(crate) fn with_label_mode(mut self, mode: LabelMode) -> Self {
        self.label_mode = mode;
        self
    }

    pub fn next_cursor(&self, kind: GhItemKind) -> Option<String> {
        match kind {
            GhItemKind::Issue => self.issues_next_cursor.clone(),
            GhItemKind::PullRequest => self.prs_next_cursor.clone(),
        }
    }

    pub fn set_flash(&mut self, msg: String, is_error: bool) {
        self.flash_message = Some((msg, is_error));
    }

    /// 列表有內容時置中錯誤訊息不會畫出來（見 render.rs），改用 flash 告知
    /// 這次重抓失敗、畫面上是舊資料。
    pub fn set_error(&mut self, msg: String) {
        if !matches!(self.load_state, LoadState::Loading) {
            return;
        }
        if self.current_list_len() == 0 {
            self.load_state = LoadState::Error(msg);
        } else {
            self.load_state = LoadState::Idle;
            self.set_flash(msg, true);
        }
    }

    /// 資料跟目前持有的一模一樣時只收尾 Loading 指示器，不重置捲動位置、
    /// 選取列或 timeline 快取——背景自動 refresh 拿到同樣的資料時，
    /// 不該把使用者正在看的位置甩掉。游標仍然要換新：這批資料雖然跟畫面上
    /// 的一樣，但可能是從一個游標已經往前推進的請求抓回來的，沿用舊游標
    /// 會讓「載入更多」拿過期游標重複抓同一頁。選取項目的 timeline（含
    /// commit CI 狀態）會就地重新請求，新內容落地前畫面不變——見
    /// `refresh_selected_timeline`。
    pub fn update_data(
        &mut self,
        issues: Vec<GhIssue>,
        pull_requests: Vec<GhPullRequest>,
        issues_next_cursor: Option<String>,
        prs_next_cursor: Option<String>,
    ) {
        if self.issues == issues && self.pull_requests == pull_requests {
            self.issues_next_cursor = issues_next_cursor;
            self.prs_next_cursor = prs_next_cursor;
            self.refresh_selected_timeline();
            self.finish_loading();
            return;
        }
        self.load_state = LoadState::Idle;
        self.issues = issues;
        self.pull_requests = pull_requests;
        self.issues_next_cursor = issues_next_cursor;
        self.prs_next_cursor = prs_next_cursor;
        self.timeline.clear();
        self.body_rev = self.body_rev.wrapping_add(1);
        self.bump_generation();
        // 修正選取索引避免越界
        let max = self.current_list_len().saturating_sub(1);
        if self.selected_index > max {
            self.selected_index = max;
        }
        self.preview_offset = 0;
        self.request_timeline_for_selected();
    }

    fn finish_loading(&mut self) {
        if matches!(self.load_state, LoadState::Loading) {
            self.load_state = LoadState::Idle;
        }
    }

    /// generation 不符（view 關閉重開、或又切換了 filter）就直接丟棄這頁——
    /// 分頁游標是靠 generation 驗證新舊的增量更新，跟 `update_data` 的整批
    /// 快照替換不是同一種東西，見 `App::on_github_data_loaded` 的註解。
    pub fn append_issues(
        &mut self,
        items: Vec<GhIssue>,
        next_cursor: Option<String>,
        generation: u64,
    ) {
        if generation != self.request_generation {
            return;
        }
        self.issues.extend(items);
        self.issues_next_cursor = next_cursor;
        self.loading_more = false;
        if self.pending_jump.is_some() {
            self.try_resolve_jump();
        }
    }

    pub fn append_pull_requests(
        &mut self,
        items: Vec<GhPullRequest>,
        next_cursor: Option<String>,
        generation: u64,
    ) {
        if generation != self.request_generation {
            return;
        }
        self.pull_requests.extend(items);
        self.prs_next_cursor = next_cursor;
        self.loading_more = false;
        if self.pending_jump.is_some() {
            self.try_resolve_jump();
        }
    }

    fn bump_generation(&mut self) {
        self.request_generation = self.request_generation.wrapping_add(1);
        self.loading_more = false;
        self.pending_jump = None;
    }

    fn try_resolve_jump(&mut self) {
        let Some(number) = self.pending_jump else {
            return;
        };

        let found = match self.active_tab {
            GitHubTab::Issues => self.issues.iter().position(|i| i.number == number),
            GitHubTab::PullRequests => self.pull_requests.iter().position(|p| p.number == number),
        };
        if let Some(idx) = found {
            self.selected_index = idx;
            self.preview_offset = 0;
            self.adjust_scroll();
            self.pending_jump = None;
            self.request_timeline_for_selected();
            return;
        }

        if self.current_has_next_cursor() {
            if self.loading_more {
                return;
            }
            self.dispatch_load_more();
            return;
        }

        let filter = self.state_filter.label();
        let msg = match self.active_tab {
            GitHubTab::Issues => {
                t!(
                    "github.jump.not_found_issues",
                    number = number,
                    filter = filter
                )
            }
            GitHubTab::PullRequests => {
                t!(
                    "github.jump.not_found_prs",
                    number = number,
                    filter = filter
                )
            }
        };
        self.set_flash(msg.into_owned(), true);
        self.pending_jump = None;
    }

    /// 選取變動只排程，不立即送請求。
    fn schedule_timeline_for_selected(&mut self, now: Instant) {
        self.timeline_due = Some(now + TIMELINE_DEBOUNCE);
    }

    /// 由 App 的 Tick 呼叫，到期才真的送請求。`now` 由呼叫端傳入，
    /// 測試才不必依賴真實時鐘（比照 `KeyState::register_quit_press`）。
    pub fn on_tick(&mut self, now: Instant) {
        if self.timeline_due.is_some_and(|due| now >= due) {
            self.timeline_due = None;
            self.request_timeline_for_selected();
        }
    }

    fn request_timeline_for_selected(&mut self) {
        // 排程的請求對象永遠是到期當下的選取項目，立即送出就等於把它做完了
        self.timeline_due = None;
        let Some((number, kind)) = self.selected_number_and_kind() else {
            return;
        };
        let entry = self.timeline.entry((kind, number)).or_default();
        if matches!(entry.state, TimelineLoad::NotRequested) {
            entry.state = TimelineLoad::Loading;
            self.tx.send(AppEvent::LoadGitHubTimeline {
                number,
                kind,
                after: None,
            });
        }
    }

    /// 只重抓目前選取項目的 timeline（含 commit CI 狀態），供 `update_data`
    /// 在清單沒變時呼叫。`state` 全程停在 `Loaded`（除非還沒載入過），
    /// `items` 保持原樣直到新資料落地才在 `append_timeline_items` 原地
    /// 替換——沒有 remove/clear，不會有中繼的空畫面，捲動位置也不受影響。
    fn refresh_selected_timeline(&mut self) {
        let Some((number, kind)) = self.selected_number_and_kind() else {
            return;
        };
        let entry = self.timeline.entry((kind, number)).or_default();
        // 已經有載入更多／上一次刷新在飛就不重複發
        if entry.loading_more || entry.refreshing {
            return;
        }
        // 窮舉寫法（比照 preview.rs 的 cache_key）：新增的 TimelineLoad
        // 變體不能悄悄落進某個分支，逼著這裡明確決定它該怎麼處理。
        match entry.state {
            // 已經有首次載入在飛，不重複發
            TimelineLoad::Loading => return,
            // 還沒載入過：畫面本來就沒東西，照首次載入的樣子顯示 loading 提示
            TimelineLoad::NotRequested => entry.state = TimelineLoad::Loading,
            // 已有內容（Loaded 或 Error）：畫面留著舊內容，state 刻意不動
            TimelineLoad::Loaded | TimelineLoad::Error(_) => entry.refreshing = true,
        }
        self.tx.send(AppEvent::LoadGitHubTimeline {
            number,
            kind,
            after: None,
        });
    }

    fn maybe_load_more_timeline(&mut self) {
        let visible = self.preview_height;
        let near_bottom = self
            .preview_offset
            .saturating_add(visible)
            .saturating_add(TIMELINE_LOAD_MORE_THRESHOLD)
            >= self.last_preview_len;
        if !near_bottom {
            return;
        }
        let Some((number, kind)) = self.selected_number_and_kind() else {
            return;
        };
        let Some(entry) = self.timeline.get_mut(&(kind, number)) else {
            return;
        };
        if entry.state != TimelineLoad::Loaded
            || entry.loading_more
            || entry.refreshing
            || entry.next_cursor.is_none()
        {
            return;
        }
        let cursor = entry.next_cursor.clone();
        entry.loading_more = true;
        self.tx.send(AppEvent::LoadGitHubTimeline {
            number,
            kind,
            after: cursor,
        });
    }

    /// `after` 由回應自己帶回請求時的值，用來判斷這是不是第一頁——不是靠
    /// `entry.loading_more` 猜，避免刷新請求跟「載入更多」請求交錯時，
    /// 先落地的那個被誤判成另一種而清錯或漏清內容。
    pub fn append_timeline_items(
        &mut self,
        number: u64,
        kind: GhItemKind,
        after: Option<String>,
        page: GhTimelinePage,
    ) {
        // commit／CI block 畫在 timeline 最前面（見 `timeline::build_timeline`），
        // 它們的行數一變（載入更多插進新 commit、刷新時 CI 狀態改變），
        // 位在下面的內容就整段位移。commit 與 CI 各自的高度可能變多也可能
        // 變少（force-push 後新 head 還沒有 check），所以分開量、分段對映
        // （見 `LeadingHeights::remap`）：頂端在 CI 區塊內、commit 區塊長高
        // 時，CI 的內容也被推下去，同樣要補。頂端在 body／header 內時
        // 上面的內容沒動，不補。超出底端的情況交給 `render_preview` 的
        // clamp。冷快取（還沒 render 過）沒有起點可比，使用者也不可能已經
        // 捲動，直接略過。
        let timeline_start = if self.selected_number_and_kind() == Some((number, kind)) {
            self.preview_cache.timeline_start(self.active_tab, number)
        } else {
            None
        };

        let entry = self.timeline.entry((kind, number)).or_default();
        // 首次載入／重試（state 還不是 Loaded）時畫面上的是 loading／錯誤提示，
        // 沒有 leading 區塊可位移，補償只會把視窗推過剛落地的 commit／CI。
        let before = timeline_start
            .filter(|_| entry.state == TimelineLoad::Loaded)
            .map(|start| (start, timeline::leading_heights(entry, self.expand_commits)));
        // CI 只有首頁會查（見 `github::build_timeline_query`），載入更多的頁面
        // 帶的是空清單，不能拿來覆寫。
        if after.is_none() {
            entry.items.clear();
            entry.ci_checks = page.ci_checks;
        }
        entry.items.extend(page.items);
        entry.next_cursor = page.next_cursor;
        entry.mergeable = page.mergeable;
        entry.state = TimelineLoad::Loaded;
        entry.loading_more = false;
        entry.refreshing = false;
        entry.rev = entry.rev.wrapping_add(1);

        if let Some((start, before)) = before {
            if let Some(rel) = self.preview_offset.checked_sub(start) {
                let now = timeline::leading_heights(entry, self.expand_commits);
                self.preview_offset = start + before.remap(now, rel);
            }
        }
    }

    pub fn set_timeline_error(&mut self, number: u64, kind: GhItemKind, error: String) {
        let entry = self.timeline.entry((kind, number)).or_default();
        // 已經有內容（背景刷新或載入更多失敗）就不要把畫面換成錯誤提示——
        // `build_timeline` 的 Error 分支不看 `items`，整條 timeline 會被
        // 蓋掉，剛好推翻 `refreshing` 存在的理由（重抓中畫面不能塌）。只有
        // 首次載入失敗（`state` 還不是 `Loaded`）才沒有內容可保留，需要
        // 整頁報錯。
        if entry.state != TimelineLoad::Loaded {
            entry.state = TimelineLoad::Error(error);
        }
        // 兩個旗標都要清：漏了 refreshing，這個項目從此按 r 永遠被
        // refresh_selected_timeline 的 guard 擋掉；漏了 loading_more，
        // maybe_load_more_timeline 會誤判成一直有請求在飛，永遠不再觸發
        // 真正的載入更多。
        entry.refreshing = false;
        entry.loading_more = false;
    }

    fn current_has_next_cursor(&self) -> bool {
        match self.active_tab {
            GitHubTab::Issues => self.issues_next_cursor.is_some(),
            GitHubTab::PullRequests => self.prs_next_cursor.is_some(),
        }
    }

    fn dispatch_load_more(&mut self) {
        self.loading_more = true;
        let kind = self.active_tab.kind();
        self.tx.send(AppEvent::LoadMoreGitHub {
            kind,
            generation: self.request_generation,
        });
    }

    pub fn update_body_for_item(&mut self, number: u64, kind: GhItemKind, new_body: String) {
        match kind {
            GhItemKind::Issue => {
                if let Some(issue) = self.issues.iter_mut().find(|i| i.number == number) {
                    issue.body = new_body;
                }
            }
            GhItemKind::PullRequest => {
                if let Some(pr) = self.pull_requests.iter_mut().find(|p| p.number == number) {
                    pr.body = new_body;
                }
            }
        }
        self.body_rev = self.body_rev.wrapping_add(1);
        self.preview_offset = 0;
    }

    pub fn status_hints(&self) -> Vec<HintSpec> {
        match self.focus {
            GitHubFocus::CheckboxEdit => {
                vec![
                    h(&[UserEvent::NavigateLeft], t!("common.hint.toggle")),
                    h(&[UserEvent::Confirm], t!("common.hint.submit")),
                    h(&[UserEvent::Cancel], t!("common.hint.cancel")),
                ]
            }
            GitHubFocus::Prompt => {
                vec![
                    h(&[UserEvent::Confirm], t!("common.hint.done")),
                    h(&[UserEvent::Cancel], t!("common.hint.clear_close")),
                ]
            }
            GitHubFocus::Preview => {
                let mut hints = vec![h(&[UserEvent::Cancel], t!("common.hint.back"))];
                hints.extend(self.action_hints());
                hints.extend(self.commit_log_hint());
                if self.selected_has_related() {
                    hints.push(h(&[UserEvent::DetailPaneToggle], t!("common.hint.related")));
                }
                hints.push(h(&[UserEvent::Refresh], t!("common.hint.refresh")));
                hints
            }
            GitHubFocus::List => {
                if self.current_list_len() == 0 {
                    return match &self.load_state {
                        LoadState::Loading => {
                            vec![h(&[UserEvent::Cancel], t!("common.hint.close"))]
                        }
                        LoadState::Error(_) => {
                            vec![
                                h(&[UserEvent::Refresh], t!("common.hint.retry")),
                                h(&[UserEvent::Cancel], t!("common.hint.close")),
                            ]
                        }
                        LoadState::Idle => vec![
                            h(&[UserEvent::Refresh], t!("common.hint.refresh")),
                            h(&[UserEvent::Cancel], t!("common.hint.close")),
                        ],
                    };
                }
                // contextual action 隨選取項目變動、使用者猜不到，排在靜態提示之前，
                // 讓被終端寬度切掉的是 help 裡查得到的那些。
                let mut hints = vec![h(&[UserEvent::RefList], t!("common.hint.switch_tab"))];
                hints.extend(self.action_hints());
                hints.extend([
                    h(&[UserEvent::Search], t!("common.hint.search")),
                    h(&[UserEvent::Confirm], t!("common.hint.preview")),
                    h(&[UserEvent::Refresh], t!("common.hint.refresh")),
                    h(&[UserEvent::Filter], t!("common.hint.filter")),
                    h(&[UserEvent::ShortCopy], t!("common.hint.copy_url")),
                    h(&[UserEvent::FullCopy], t!("common.hint.open")),
                    h(&[UserEvent::TagCopy], t!("common.hint.num")),
                ]);
                hints.extend(self.commit_log_hint());
                hints.extend(self.label_mode_hint());
                if self.selected_has_related() {
                    hints.push(h(&[UserEvent::DetailPaneToggle], t!("common.hint.related")));
                }
                hints.push(h(&[UserEvent::GitHubToggle], t!("common.hint.close")));
                hints
            }
        }
    }

    /// 這個 view 有可能顯示的每一條狀態列提示，給
    /// `help::status_hints_are_bound_and_documented` 拿去跟說明頁比對。
    ///
    /// 樣本資料與掃描都放在這裡而不是 help 裡：`focus`／`active_tab` 這些欄位
    /// 對別的模組是私有的，而「哪些狀態組合會長出不同提示」也只有這個模組知道。
    #[cfg(test)]
    pub(crate) fn every_status_hint() -> Vec<HintSpec> {
        use crate::github::{DiffStat, GhAuthor, GhRelatedIssue};

        let author = || GhAuthor {
            login: "alice".to_string(),
        };
        let related = || {
            vec![GhRelatedIssue {
                number: 9,
                title: "related".to_string(),
                state: "OPEN".to_string(),
                url: String::new(),
            }]
        };
        let issue = |number, state: &str, sub_issues: Vec<GhRelatedIssue>| GhIssue {
            number,
            title: "t".to_string(),
            state: state.to_string(),
            labels: Vec::new(),
            author: author(),
            created_at: String::new(),
            body: String::new(),
            url: String::new(),
            closed_at: None,
            updated_at: String::new(),
            parent: None,
            sub_issues,
        };
        let pr = |number, state: &str, is_draft, linked_issues| GhPullRequest {
            number,
            title: "t".to_string(),
            state: state.to_string(),
            labels: Vec::new(),
            author: author(),
            head_ref_name: "topic".to_string(),
            head_ref_oid: "deadbeef".to_string(),
            base_ref_name: "main".to_string(),
            is_draft,
            head_branch_deletable: true,
            body: String::new(),
            url: String::new(),
            closed_at: None,
            updated_at: String::new(),
            linked_issues,
            diff_stat: DiffStat::default(),
        };

        let data = GitHubData {
            issues: vec![issue(1, "OPEN", related()), issue(2, "CLOSED", Vec::new())],
            pull_requests: vec![
                pr(3, "OPEN", false, related()),
                pr(4, "OPEN", true, Vec::new()),
                pr(5, "MERGED", false, Vec::new()),
            ],
            ..Default::default()
        };
        let (tx, _rx) = Sender::channel_for_test();
        let mut view = GitHubView::new(View::Default, data, tx);

        let mut all = Vec::new();
        for focus in [
            GitHubFocus::List,
            GitHubFocus::Preview,
            GitHubFocus::Prompt,
            GitHubFocus::CheckboxEdit,
        ] {
            for tab in [GitHubTab::Issues, GitHubTab::PullRequests] {
                for expand in [false, true] {
                    for label_mode in [LabelMode::Names, LabelMode::Swatches] {
                        view.focus = focus;
                        view.active_tab = tab;
                        view.expand_commits = expand;
                        view.label_mode = label_mode;
                        for i in 0..view.current_list_len() {
                            view.selected_index = i;
                            all.extend(view.status_hints());
                        }
                    }
                }
            }
        }

        // 空清單的三條分支走的是另一組提示，清單有東西時掃不到。
        view.issues.clear();
        view.pull_requests.clear();
        view.focus = GitHubFocus::List;
        for state in [
            LoadState::Idle,
            LoadState::Loading,
            LoadState::Error(String::new()),
        ] {
            view.load_state = state;
            all.extend(view.status_hints());
        }

        all
    }

    /// 同時收合／展開 commit 與 CI 區塊。
    ///
    /// 收合／展開不會重置 `preview_offset`——跟另外約 20 個在導覽時會重置
    /// 它的地方不同，這只是內容密度的切換，不是「你現在看的是別的東西了」
    /// 那種時刻。`render_preview` 裡既有的 clamp 會擋住捲過新結尾的情況。
    fn toggle_commit_log(&mut self) {
        self.expand_commits = !self.expand_commits;
    }

    fn toggle_label_mode(&mut self) {
        self.label_mode = match self.label_mode {
            LabelMode::Names => LabelMode::Swatches,
            LabelMode::Swatches => LabelMode::Names,
        };
    }

    /// 就地更新 draft 狀態。`RefreshGitHub` 是非同步的，成功通知到列表刷新之間
    /// 若讀到過期的 `is_draft`，反向操作會挑錯方向，而 `gh pr ready` 對非 draft
    /// PR 是 idempotent 成功 — 使用者不會收到任何錯誤提示。
    pub fn set_pr_draft_flag(&mut self, number: u64, is_draft: bool) {
        if let Some(pr) = self.pull_requests.iter_mut().find(|p| p.number == number) {
            pr.is_draft = is_draft;
        }
    }

    /// 選取項目的 (編號, 型別, 狀態)，兩個分頁共用。
    fn selected_state_target(&self) -> Option<(u64, GhItemKind, &str)> {
        let idx = self.actual_index(self.selected_index);
        match self.active_tab {
            GitHubTab::Issues => self
                .issues
                .get(idx)
                .map(|i| (i.number, GhItemKind::Issue, i.state.as_str())),
            GitHubTab::PullRequests => self
                .pull_requests
                .get(idx)
                .map(|p| (p.number, GhItemKind::PullRequest, p.state.as_str())),
        }
    }

    fn action_hints(&self) -> Vec<HintSpec> {
        let Some((_, kind, state)) = self.selected_state_target() else {
            return Vec::new();
        };
        let mut hints = Vec::new();

        // merge 與 draft 切換只對 open 的 PR 有意義
        if matches!(self.active_tab, GitHubTab::PullRequests) && state == "OPEN" {
            let idx = self.actual_index(self.selected_index);
            if let Some(pr) = self.pull_requests.get(idx) {
                if !pr.is_draft {
                    hints.push(h(&[UserEvent::MergePr], t!("common.hint.merge_pr")));
                }
                hints.push(h(
                    &[UserEvent::TogglePrDraft],
                    PrDraftAction::for_pr(pr.is_draft).hint_label(),
                ));
            }
        }
        if let Some(action) = StateAction::for_state(state) {
            hints.push(h(&[UserEvent::ToggleIssueState], action.hint_label(kind)));
        }
        hints
    }

    /// 跟 `action_hints` 不同，不受 `state == "OPEN"` 限制——已關閉或已
    /// merge 的 PR 一樣有值得收合的 commit log。
    fn commit_log_hint(&self) -> Option<HintSpec> {
        if !matches!(self.active_tab, GitHubTab::PullRequests) {
            return None;
        }
        let label = if self.expand_commits {
            t!("github.hint.collapse_commits")
        } else {
            t!("github.hint.expand_commits")
        };
        Some(h(&[UserEvent::ToggleCommitLog], label))
    }

    /// 只在色塊模式下顯示——名稱模式是預設，不需要提示佔狀態列空間。
    fn label_mode_hint(&self) -> Option<HintSpec> {
        (self.label_mode == LabelMode::Swatches)
            .then(|| h(&[UserEvent::CreateTag], t!("common.hint.show_label_names")))
    }

    pub fn jump_to_issue(&mut self, number: u64) -> bool {
        let Some(raw_idx) = self.issues.iter().position(|i| i.number == number) else {
            return false;
        };
        self.active_tab = GitHubTab::Issues;
        self.focus = GitHubFocus::List;
        self.search_input.reset();
        self.filtered_issue_indices.clear();
        self.filtered_pr_indices.clear();
        self.selected_index = raw_idx;
        self.offset = 0;
        self.preview_offset = 0;
        self.adjust_scroll();
        self.request_timeline_for_selected();
        true
    }

    fn selected_has_related(&self) -> bool {
        let idx = self.actual_index(self.selected_index);
        match self.active_tab {
            GitHubTab::Issues => self
                .issues
                .get(idx)
                .is_some_and(|i| i.parent.is_some() || !i.sub_issues.is_empty()),
            GitHubTab::PullRequests => self
                .pull_requests
                .get(idx)
                .is_some_and(|p| !p.linked_issues.is_empty()),
        }
    }

    fn selected_number_and_kind(&self) -> Option<(u64, GhItemKind)> {
        let idx = self.actual_index(self.selected_index);
        match self.active_tab {
            GitHubTab::Issues => self.issues.get(idx).map(|i| (i.number, GhItemKind::Issue)),
            GitHubTab::PullRequests => self
                .pull_requests
                .get(idx)
                .map(|p| (p.number, GhItemKind::PullRequest)),
        }
    }

    fn current_list_len(&self) -> usize {
        if self.has_active_filter() {
            self.current_filtered_indices().len()
        } else {
            match self.active_tab {
                GitHubTab::Issues => self.issues.len(),
                GitHubTab::PullRequests => self.pull_requests.len(),
            }
        }
    }

    fn current_filtered_indices(&self) -> &[usize] {
        match self.active_tab {
            GitHubTab::Issues => &self.filtered_issue_indices,
            GitHubTab::PullRequests => &self.filtered_pr_indices,
        }
    }

    fn has_active_filter(&self) -> bool {
        !self.search_input.value().is_empty()
    }

    /// 把可視索引對應到實際資料索引（若有篩選則透過篩選對應）
    fn actual_index(&self, visible_idx: usize) -> usize {
        if self.has_active_filter() {
            self.current_filtered_indices()
                .get(visible_idx)
                .copied()
                .unwrap_or(0)
        } else {
            visible_idx
        }
    }

    fn adjust_scroll(&mut self) {
        if self.height == 0 {
            return;
        }
        let visible = self.height.saturating_sub(3);
        if self.selected_index < self.offset {
            self.offset = self.selected_index;
        } else if self.selected_index >= self.offset + visible {
            self.offset = self.selected_index - visible + 1;
        }
    }

    /// 把選取的 issue/PR 及其 timeline entry 一併借出成單一值——這是
    /// `build_preview_content` 與 `PreviewInput::cache_key` 讀取的全部內容，
    /// 兩者不會因為讀到對方不知道的欄位而彼此失準。
    fn preview_input(&self, width: u16) -> PreviewInput<'_> {
        let (number, kind) = self
            .selected_number_and_kind()
            .unwrap_or((0, GhItemKind::Issue));
        let idx = self.actual_index(self.selected_index);
        let item = match self.active_tab {
            GitHubTab::Issues => self.issues.get(idx).map(|issue| SelectedItem {
                title: issue.title.as_str(),
                state: issue.state.as_str(),
                author: issue.author.login.as_str(),
                labels: issue.labels.as_slice(),
                body: issue.body.as_str(),
                url: issue.url.as_str(),
                extra: SelectedItemExtra::Issue {
                    parent: issue.parent.as_ref(),
                    sub_issues: issue.sub_issues.as_slice(),
                },
            }),
            GitHubTab::PullRequests => self.pull_requests.get(idx).map(|pr| SelectedItem {
                title: pr.title.as_str(),
                state: pr.state.as_str(),
                author: pr.author.login.as_str(),
                labels: pr.labels.as_slice(),
                body: pr.body.as_str(),
                url: pr.url.as_str(),
                extra: SelectedItemExtra::PullRequest {
                    base_ref_name: pr.base_ref_name.as_str(),
                    head_ref_name: pr.head_ref_name.as_str(),
                    diff_stat: pr.diff_stat,
                },
            }),
        };
        PreviewInput {
            tab: self.active_tab,
            number,
            width,
            body_rev: self.body_rev,
            entry: self.timeline.get(&(kind, number)),
            expand_commits: self.expand_commits,
            item,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    use crate::event::UserEventWithCount;
    use crate::github::{
        DiffStat, GhAuthor, GhCommit, GhReviewComment, GhReviewCommentConn, GhStatusCheckRollup,
        GhTimelineItem, Mergeable,
    };

    use super::preview::build_preview_content;

    const TERM_W: u16 = 60;
    const TERM_H: u16 = 20;
    const LAST_MARKER: &str = "尾端標記";

    /// `App` 在 view 開／關之間交接資料靠的就是這個 round-trip：
    /// 建構子放進去什麼，`take_data` 就該原封不動交還什麼。
    #[test]
    fn take_data_round_trips_everything_the_view_was_constructed_with() {
        let issue = GhIssue {
            number: 7,
            title: "t".to_string(),
            state: "OPEN".to_string(),
            labels: Vec::new(),
            author: GhAuthor {
                login: "alice".to_string(),
            },
            created_at: String::new(),
            body: "b".to_string(),
            url: String::new(),
            closed_at: None,
            updated_at: String::new(),
            parent: None,
            sub_issues: Vec::new(),
        };
        let data = GitHubData {
            issues: vec![issue],
            pull_requests: Vec::new(),
            state_filter: StateFilter::Closed,
            issues_next_cursor: Some("cursor-a".to_string()),
            prs_next_cursor: None,
        };
        let (tx, _rx) = Sender::channel_for_test();
        let mut view = GitHubView::new(View::Default, data, tx);

        let taken = view.take_data();

        assert_eq!(taken.issues.len(), 1);
        assert_eq!(taken.issues[0].number, 7);
        assert!(taken.pull_requests.is_empty());
        assert_eq!(taken.state_filter, StateFilter::Closed);
        assert_eq!(taken.issues_next_cursor.as_deref(), Some("cursor-a"));
        assert_eq!(taken.prs_next_cursor, None);
    }

    /// 資料跟目前持有的一模一樣時（背景 auto-refresh 的常態），游標仍然要
    /// 換新——沿用舊游標會讓「載入更多」拿過期游標重複抓同一頁。
    #[test]
    fn update_data_refreshes_cursor_even_when_items_are_unchanged() {
        let data = GitHubData {
            issues_next_cursor: Some("cursor-a".to_string()),
            ..Default::default()
        };
        let (tx, _rx) = Sender::channel_for_test();
        let mut view = GitHubView::new(View::Default, data, tx);

        view.update_data(Vec::new(), Vec::new(), Some("cursor-b".to_string()), None);

        assert_eq!(
            view.next_cursor(GhItemKind::Issue).as_deref(),
            Some("cursor-b")
        );
    }

    /// 帶快取重開也在重抓中（#142）：header 要顯示「重新抓取中」。
    #[test]
    fn new_with_cached_data_starts_loading() {
        let view = view_with_body("body".to_string());

        assert!(matches!(view.load_state, LoadState::Loading));
    }

    /// 帶快取重抓失敗：列表保留、錯誤走 flash，不能因置中錯誤訊息只在
    /// 列表空時才畫而被靜默吞掉。
    #[test]
    fn set_error_with_cached_list_keeps_list_and_flashes() {
        let mut view = view_with_body("body".to_string());

        view.set_error("boom".to_string());

        assert!(matches!(view.load_state, LoadState::Idle));
        assert_eq!(view.flash_message, Some(("boom".to_string(), true)));
        assert!(render_to_string(&mut view).contains("boom"));
    }

    #[test]
    fn set_error_with_empty_list_shows_error_state() {
        let (tx, _rx) = Sender::channel_for_test();
        let mut view = GitHubView::new(View::Default, GitHubData::default(), tx);

        view.set_error("boom".to_string());

        assert!(matches!(view.load_state, LoadState::Error(ref m) if m == "boom"));
        assert!(view.flash_message.is_none());
    }

    /// 每一行原始內容在 preview 寬度下都會折行好幾次的長 body——這正是
    /// 舊版「先切片再折行」程式碼會出錯的情況。
    fn long_body() -> String {
        let mut body = String::new();
        for i in 0..10 {
            body.push_str(&format!("第{i}行內容刻意寫得很長好觸發折行折行折行折行\n"));
        }
        body.push_str(LAST_MARKER);
        body
    }

    fn view_with_long_body() -> GitHubView<'static> {
        view_with_body(long_body())
    }

    fn view_with_body(body: String) -> GitHubView<'static> {
        view_with_body_and_rx(body).0
    }

    /// 跟 `view_with_body` 一樣，但保留 `rx`——要斷言「送出了什麼事件」的
    /// 測試才需要，`view_with_body` 內部把它丟掉是因為多數測試只在乎渲染
    /// 結果，用不到。
    fn view_with_body_and_rx(
        body: String,
    ) -> (GitHubView<'static>, std::sync::mpsc::Receiver<AppEvent>) {
        let pr = GhPullRequest {
            number: 1,
            title: "t".to_string(),
            state: "OPEN".to_string(),
            labels: Vec::new(),
            author: GhAuthor {
                login: "alice".to_string(),
            },
            head_ref_name: "topic".to_string(),
            head_ref_oid: "deadbeef".to_string(),
            base_ref_name: "main".to_string(),
            is_draft: false,
            head_branch_deletable: true,
            body,
            url: String::new(),
            closed_at: None,
            updated_at: String::new(),
            linked_issues: Vec::new(),
            diff_stat: DiffStat::default(),
        };

        let (tx, rx) = Sender::channel_for_test();
        let data = GitHubData {
            pull_requests: vec![pr],
            ..Default::default()
        };
        let mut view = GitHubView::new(View::Default, data, tx);
        view.active_tab = GitHubTab::PullRequests;
        (view, rx)
    }

    /// 已載入一頁、CI 狀態是 PENDING 的 PR#1——多數刷新測試的共同起點。
    fn load_pending_commit(view: &mut GitHubView<'_>) {
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            timeline_page(vec![timeline_commit("aaaaaaa", "PENDING")], None),
        );
    }

    /// 模擬按 r 刷新，但清單內容沒變——走 `update_data` 的 early-return
    /// 分支，`refresh_selected_timeline` 才會被觸發。
    fn refresh_with_unchanged_data(view: &mut GitHubView<'_>) {
        let issues = view.issues.clone();
        let pull_requests = view.pull_requests.clone();
        view.update_data(issues, pull_requests, None, None);
    }

    fn render_to_string(view: &mut GitHubView<'_>) -> String {
        render_to_string_at_width(view, TERM_W)
    }

    /// 跟 `render_to_string` 一樣，但欄寬可調——labels 的寬度測試需要比
    /// 標準 60 欄更寬的畫面，才不會連 label 名稱本身都被切斷。
    fn render_to_string_at_width(view: &mut GitHubView<'_>, width: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, TERM_H)).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                view.render(f, area, 0);
            })
            .unwrap();
        // `TestBackend` 的 Display 會跳過雙寬字元後面接的空白填充格，
        // 所以 CJK 文字讀回來會是連續的。
        terminal.backend().to_string()
    }

    #[test]
    fn preview_scrolls_all_the_way_to_the_last_line() {
        let mut view = view_with_long_body();
        // 第一次 render 會填入 `last_preview_len`（視覺行數）。
        render_to_string(&mut view);
        // 要求遠超過實際內容的捲動量；render 會把它限制在底端。
        view.preview_offset = usize::MAX / 2;
        let screen = render_to_string(&mut view);

        assert!(
            screen.contains(LAST_MARKER),
            "bottom of the body must be reachable, got:\n{screen}"
        );
    }

    #[test]
    fn preview_offset_is_clamped_to_visual_lines() {
        let mut view = view_with_long_body();
        render_to_string(&mut view);
        let total = view.last_preview_len;

        view.preview_offset = usize::MAX / 2;
        render_to_string(&mut view);

        let expected = total.saturating_sub(view.preview_height);
        assert_eq!(view.preview_offset, expected);

        // 限制範圍算的必須是折行後的行數，不是原始行數。跟 cache 自己的
        // 原始行數比對，才是這個測試真正有威力的地方：舊版用邏輯行計算，
        // 會讓兩者算出一樣的值。
        let source_lines = view.preview_cache.lines().len();
        assert!(
            total > source_lines,
            "last_preview_len must count wrapped lines ({total}) not source lines ({source_lines})"
        );
    }

    fn timeline_page(items: Vec<GhTimelineItem>, next_cursor: Option<String>) -> GhTimelinePage {
        GhTimelinePage {
            items,
            next_cursor,
            mergeable: None,
            ci_checks: Vec::new(),
        }
    }

    fn ci_check(name: &str, state: crate::github::CheckState) -> crate::github::GhCheck {
        crate::github::GhCheck {
            name: name.to_string(),
            state,
        }
    }

    fn ci_page(
        items: Vec<GhTimelineItem>,
        next_cursor: Option<String>,
        ci_checks: Vec<crate::github::GhCheck>,
    ) -> GhTimelinePage {
        GhTimelinePage {
            ci_checks,
            ..timeline_page(items, next_cursor)
        }
    }

    /// 按 `z` 時 CI 區塊跟 commit 一起收合：展開列出每個 check，收合變成
    /// 一行計數加色塊。
    #[test]
    fn ci_checks_render_and_collapse_with_the_commit_toggle() {
        use crate::github::CheckState::{Failed, Passed};
        let mut view = view_with_body("body".to_string());
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            ci_page(
                vec![timeline_commit("aaaaaaa", "FAILURE")],
                None,
                vec![ci_check("lint", Failed), ci_check("test", Passed)],
            ),
        );
        let screen = render_to_string(&mut view);
        assert!(screen.contains("✗ lint"), "got:\n{screen}");
        assert!(screen.contains("✓ test"), "got:\n{screen}");

        view.toggle_commit_log();
        let screen = render_to_string(&mut view);
        assert!(screen.contains("▸ 2 個 check ▮▮"), "got:\n{screen}");
        assert!(!screen.contains("✗ lint"), "got:\n{screen}");
    }

    /// 先 render 一次讓 preview cache 暖起來，回傳 timeline 起點的視覺行號。
    /// 捲動補償靠這個值判斷使用者在看 body 還是 timeline，冷快取不補償。
    fn warm_timeline_start(view: &mut GitHubView<'_>) -> usize {
        render_to_string(view);
        view.preview_cache
            .timeline_start(view.active_tab, 1)
            .expect("cache must be warm for the selected item")
    }

    /// 視窗頂端在 CI 區塊內、載入更多讓 commit 區塊長高：CI 的內容被往下推，
    /// `preview_offset` 要跟著位移，不然畫面會往上跳。
    #[test]
    fn loading_more_shifts_preview_offset_inside_ci_block_when_commit_block_grows() {
        use crate::github::CheckState::Passed;
        let mut view = view_with_body("body".to_string());
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            ci_page(
                vec![timeline_commit("aaaaaaa", "SUCCESS")],
                Some("cursor".to_string()),
                vec![
                    ci_check("a", Passed),
                    ci_check("b", Passed),
                    ci_check("c", Passed),
                ],
            ),
        );
        let start = warm_timeline_start(&mut view);
        // commit 區塊 = 分隔線 + 1 行；停在 CI 區塊的第二行
        view.preview_offset = start + 2 + 1;

        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            Some("cursor".to_string()),
            timeline_page(
                vec![
                    timeline_commit("bbbbbbb", "SUCCESS"),
                    timeline_commit("ccccccc", "SUCCESS"),
                ],
                None,
            ),
        );

        assert_eq!(view.preview_offset, start + 2 + 1 + 2);
    }

    /// CI 只有首頁會查：載入更多的頁面帶的是空清單，不能覆寫既有的 check，
    /// 也就不會讓 CI 區塊塌掉、把視窗位置帶歪。
    #[test]
    fn loading_more_keeps_the_existing_ci_checks() {
        use crate::github::CheckState::{Failed, Passed};
        let mut view = view_with_body("body".to_string());
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            ci_page(
                vec![timeline_comment("a", "one")],
                Some("cursor".to_string()),
                vec![ci_check("a", Passed), ci_check("b", Passed)],
            ),
        );
        let start = warm_timeline_start(&mut view);
        view.preview_offset = start + 10;

        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            Some("cursor".to_string()),
            ci_page(Vec::new(), None, Vec::new()),
        );
        let entry = &view.timeline[&(GhItemKind::PullRequest, 1)];
        assert_eq!(entry.ci_checks.len(), 2);
        assert_eq!(view.preview_offset, start + 10);

        // 就算載入更多的頁面帶了 check，也不採用
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            Some("cursor".to_string()),
            ci_page(Vec::new(), None, vec![ci_check("z", Failed)]),
        );
        let entry = &view.timeline[&(GhItemKind::PullRequest, 1)];
        assert_eq!(
            entry.ci_checks,
            [ci_check("a", Passed), ci_check("b", Passed)]
        );
    }

    /// 載入更多時使用者還在看 body（長 body、短 timeline 也會走到 near-bottom）：
    /// 視窗頂端在 timeline 之上，新 commit 插進 timeline 不影響它，不能補償。
    #[test]
    fn loading_more_does_not_shift_preview_offset_while_viewing_body() {
        let mut view = view_with_long_body();
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            timeline_page(
                vec![timeline_comment("a", "one")],
                Some("cursor".to_string()),
            ),
        );
        let start = warm_timeline_start(&mut view);
        assert!(start > 2, "long body must push the timeline down");
        view.preview_offset = 2;

        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            Some("cursor".to_string()),
            timeline_page(vec![timeline_commit("aaaaaaa", "SUCCESS")], None),
        );

        assert_eq!(view.preview_offset, 2);
    }

    /// 刷新（`after: None`）時 CI 行數變多：使用者正在看留言區，
    /// 下面的內容被往下推，`preview_offset` 要跟著位移。
    #[test]
    fn refreshing_shifts_preview_offset_when_ci_block_grows() {
        use crate::github::CheckState::Passed;
        let mut view = view_with_body("body".to_string());
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            ci_page(
                vec![timeline_comment("a", "one")],
                None,
                vec![ci_check("a", Passed)],
            ),
        );
        let start = warm_timeline_start(&mut view);
        // CI 區塊（分隔線 + 1）之下
        view.preview_offset = start + 2 + 3;

        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            ci_page(
                vec![timeline_comment("a", "one")],
                None,
                vec![
                    ci_check("a", Passed),
                    ci_check("b", Passed),
                    ci_check("c", Passed),
                ],
            ),
        );

        assert_eq!(view.preview_offset, start + 2 + 3 + 2);
    }

    /// 重跑中的 check 暫時消失，CI 區塊縮小：補償量是負的。
    #[test]
    fn refreshing_shifts_preview_offset_back_when_ci_block_shrinks() {
        use crate::github::CheckState::Passed;
        let mut view = view_with_body("body".to_string());
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            ci_page(
                vec![timeline_comment("a", "one")],
                None,
                vec![ci_check("a", Passed), ci_check("b", Passed)],
            ),
        );
        let start = warm_timeline_start(&mut view);
        view.preview_offset = start + 10;

        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            ci_page(vec![timeline_comment("a", "one")], None, Vec::new()),
        );

        assert_eq!(view.preview_offset, start + 10 - 3);
    }

    /// 視窗頂端停在 CI 區塊最後一行，刷新後 CI 縮到頂端之上：落在第一則
    /// 留言，不會越過它。
    #[test]
    fn refreshing_clamps_preview_offset_when_ci_block_shrinks_above_it() {
        use crate::github::CheckState::Passed;
        let mut view = view_with_body("body".to_string());
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            ci_page(
                vec![
                    timeline_commit("aaaaaaa", "SUCCESS"),
                    timeline_comment("a", "one"),
                ],
                None,
                vec![
                    ci_check("a", Passed),
                    ci_check("b", Passed),
                    ci_check("c", Passed),
                ],
            ),
        );
        let start = warm_timeline_start(&mut view);
        // commit 區塊 2 行、CI 區塊 4 行；停在 CI 最後一行
        view.preview_offset = start + 2 + 3;

        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            ci_page(
                vec![
                    timeline_commit("aaaaaaa", "SUCCESS"),
                    timeline_comment("a", "one"),
                ],
                None,
                vec![ci_check("a", Passed)],
            ),
        );

        // commit 2 行 + CI 2 行，之後就是第一則留言
        assert_eq!(view.preview_offset, start + 2 + 2);
    }

    /// 收合後 CI 區塊從無到有是「分隔線 + 一行摘要」= 2 行，不是 check 筆數。
    #[test]
    fn refreshing_shifts_preview_offset_by_the_collapsed_ci_block_height() {
        use crate::github::CheckState::Passed;
        let mut view = view_with_body("body".to_string());
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            timeline_page(vec![timeline_comment("a", "one")], None),
        );
        view.toggle_commit_log();
        let start = warm_timeline_start(&mut view);
        view.preview_offset = start + 1;

        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            ci_page(
                vec![timeline_comment("a", "one")],
                None,
                vec![
                    ci_check("a", Passed),
                    ci_check("b", Passed),
                    ci_check("c", Passed),
                ],
            ),
        );

        assert_eq!(view.preview_offset, start + 1 + 2);
    }

    /// 視窗頂端落在 commit 區塊內時，頂端內容本身沒位移（CI 區塊在它
    /// 之後），刷新讓 CI 變多不能補償。
    #[test]
    fn refreshing_does_not_shift_preview_offset_inside_leading_blocks() {
        use crate::github::CheckState::Passed;
        let mut view = view_with_body("body".to_string());
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            ci_page(
                vec![timeline_commit("aaaaaaa", "SUCCESS")],
                None,
                vec![ci_check("a", Passed)],
            ),
        );
        let start = warm_timeline_start(&mut view);
        // commit 區塊 = 分隔線 + 1 行；停在它的第二行
        view.preview_offset = start + 1;

        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            ci_page(
                vec![timeline_commit("aaaaaaa", "SUCCESS")],
                None,
                vec![ci_check("a", Passed), ci_check("b", Passed)],
            ),
        );

        assert_eq!(view.preview_offset, start + 1);
    }

    /// 收合後 leading 區塊固定是「分隔線 + 一行摘要」×2，check 筆數變動
    /// 不改變高度，`preview_offset` 不動。
    #[test]
    fn refreshing_does_not_shift_preview_offset_when_collapsed() {
        use crate::github::CheckState::Passed;
        let mut view = view_with_body("body".to_string());
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            ci_page(
                vec![timeline_commit("aaaaaaa", "SUCCESS")],
                None,
                vec![ci_check("a", Passed)],
            ),
        );
        view.toggle_commit_log();
        let start = warm_timeline_start(&mut view);
        view.preview_offset = start + 4 + 3;

        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            ci_page(
                vec![timeline_commit("aaaaaaa", "SUCCESS")],
                None,
                vec![
                    ci_check("a", Passed),
                    ci_check("b", Passed),
                    ci_check("c", Passed),
                ],
            ),
        );

        assert_eq!(view.preview_offset, start + 4 + 3);
    }

    #[test]
    fn preview_cache_invalidates_when_comments_load_empty() {
        // body 短一點，讓留言區段不用捲動就在畫面上。
        let mut view = view_with_body("short".to_string());
        let screen = render_to_string(&mut view);
        assert!(screen.contains("載入留言中"), "got:\n{screen}");

        // 零則留言，但*已載入*——項目數量仍是 0，所以只有 stage 能區分
        // 這跟 pending 狀態的差別。
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            timeline_page(Vec::new(), None),
        );
        let screen = render_to_string(&mut view);

        assert!(
            !screen.contains("載入留言中"),
            "preview must leave the loading state, got:\n{screen}"
        );
        assert!(screen.contains("沒有留言"), "got:\n{screen}");
    }

    fn timeline_comment(login: &str, body: &str) -> GhTimelineItem {
        GhTimelineItem::IssueComment {
            body: body.to_string(),
            created_at: "2026-07-27".to_string(),
            author: Some(GhAuthor {
                login: login.to_string(),
            }),
        }
    }

    fn timeline_commit(oid: &str, state: &str) -> GhTimelineItem {
        GhTimelineItem::PullRequestCommit {
            commit: GhCommit {
                abbreviated_oid: oid.to_string(),
                message_headline: format!("headline for {oid}"),
                status_check_rollup: Some(GhStatusCheckRollup {
                    state: state.to_string(),
                }),
            },
        }
    }

    /// `submitted_at: None` 代表 PENDING（尚未送出的草稿），會被
    /// `TimelineBlock::from_gh` 過濾掉——測試 PENDING 過濾時傳 `None`。
    fn timeline_review(
        login: &str,
        state: &str,
        submitted_at: Option<&str>,
        body: &str,
        comments: Vec<GhReviewComment>,
    ) -> GhTimelineItem {
        GhTimelineItem::PullRequestReview {
            state: state.to_string(),
            body: body.to_string(),
            submitted_at: submitted_at.map(str::to_string),
            author: Some(GhAuthor {
                login: login.to_string(),
            }),
            comments: GhReviewCommentConn {
                total_count: comments.len(),
                nodes: comments,
            },
        }
    }

    fn review_comment(path: &str, line: Option<u32>, body: &str) -> GhReviewComment {
        GhReviewComment {
            id: format!("{path}:{line:?}"),
            path: path.to_string(),
            line,
            original_line: line,
            outdated: false,
            body: body.to_string(),
            resolved: false,
        }
    }

    /// 按 r 刷新（清單沒變，走 `update_data` early-return 分支）要對已載入
    /// 的選取項目送出一個新的 `LoadGitHubTimeline` 首頁請求。
    #[test]
    fn refresh_selected_timeline_requests_a_fresh_first_page() {
        let (mut view, rx) = view_with_body_and_rx("body".to_string());
        load_pending_commit(&mut view);

        refresh_with_unchanged_data(&mut view);

        let events: Vec<AppEvent> = rx.try_iter().collect();
        assert!(
            events.iter().any(|e| matches!(
                e,
                AppEvent::LoadGitHubTimeline {
                    number: 1,
                    kind: GhItemKind::PullRequest,
                    after: None,
                }
            )),
            "got: {events:?}"
        );
    }

    /// 刷新請求送出後、回應還沒回來前，畫面必須維持原本的內容——不能塌成
    /// 「(loading comments…)」，這正是這次改動存在的理由（相對於 remove
    /// entry 的做法會讓 render.rs 的捲動 clamp 把捲動位置永久夾死）。
    #[test]
    fn refresh_selected_timeline_keeps_old_content_visible_while_in_flight() {
        let (mut view, _rx) = view_with_body_and_rx("body".to_string());
        load_pending_commit(&mut view);

        refresh_with_unchanged_data(&mut view);

        let screen = render_to_string(&mut view);
        assert!(screen.contains("aaaaaaa"), "got:\n{screen}");
        assert!(!screen.contains("載入留言中"), "got:\n{screen}");
    }

    #[test]
    fn refresh_selected_timeline_skips_when_load_more_in_flight() {
        let (mut view, rx) = view_with_body_and_rx("body".to_string());
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            timeline_page(
                vec![timeline_commit("aaaaaaa", "PENDING")],
                Some("cursor".to_string()),
            ),
        );
        view.timeline
            .get_mut(&(GhItemKind::PullRequest, 1))
            .unwrap()
            .loading_more = true;

        refresh_with_unchanged_data(&mut view);

        assert!(rx.try_iter().next().is_none());
    }

    #[test]
    fn refresh_selected_timeline_skips_when_refresh_already_in_flight() {
        let (mut view, rx) = view_with_body_and_rx("body".to_string());
        load_pending_commit(&mut view);
        view.timeline
            .get_mut(&(GhItemKind::PullRequest, 1))
            .unwrap()
            .refreshing = true;

        refresh_with_unchanged_data(&mut view);

        assert!(rx.try_iter().next().is_none());
    }

    /// 逼出定稿設計的那個 bug 的迴歸測試：一個「載入更多」請求跟一個刷新
    /// 請求同時在飛，回應交錯抵達時不能遺失或重複 commit——`after` 是不是
    /// `None` 才是「這是不是第一頁」的準則，不能靠 `loading_more` 猜。
    #[test]
    fn interleaved_refresh_and_load_more_responses_do_not_corrupt_items() {
        let (mut view, _rx) = view_with_body_and_rx("body".to_string());
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            timeline_page(
                vec![timeline_commit("aaaaaaa", "PENDING")],
                Some("cursor-a".to_string()),
            ),
        );
        // 模擬「載入更多」已經發出請求
        view.timeline
            .get_mut(&(GhItemKind::PullRequest, 1))
            .unwrap()
            .loading_more = true;

        // 刷新的回應先落地（after: None ⇒ 完整第一頁，整批替換）
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            timeline_page(
                vec![timeline_commit("aaaaaaa", "SUCCESS")],
                Some("cursor-a".to_string()),
            ),
        );
        assert_eq!(
            view.timeline[&(GhItemKind::PullRequest, 1)].items.len(),
            1,
            "刷新回應必須整批替換，不能疊加在舊內容後面"
        );

        // 載入更多的回應後落地（after: Some(cursor-a) ⇒ 續接在剛才那批之後）
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            Some("cursor-a".to_string()),
            timeline_page(vec![timeline_commit("bbbbbbb", "PENDING")], None),
        );
        assert_eq!(
            view.timeline[&(GhItemKind::PullRequest, 1)].items.len(),
            2,
            "load more 回應必須續接在刷新後的內容上，不能遺失也不能重複"
        );
    }

    #[test]
    fn append_timeline_items_replaces_on_first_page_and_extends_on_continuation() {
        let (mut view, _rx) = view_with_body_and_rx("body".to_string());
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            timeline_page(vec![timeline_comment("a", "one")], None),
        );
        assert_eq!(view.timeline[&(GhItemKind::PullRequest, 1)].rev, 1);

        // after: None ⇒ 替換，不是疊加
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            timeline_page(vec![timeline_comment("b", "two")], None),
        );
        let entry = &view.timeline[&(GhItemKind::PullRequest, 1)];
        assert_eq!(entry.items.len(), 1);
        assert_eq!(entry.rev, 2);

        // after: Some(..) ⇒ 續接
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            Some("cursor".to_string()),
            timeline_page(vec![timeline_comment("c", "three")], None),
        );
        let entry = &view.timeline[&(GhItemKind::PullRequest, 1)];
        assert_eq!(entry.items.len(), 2);
        assert_eq!(entry.rev, 3);
    }

    /// commit block 畫在 timeline 最前面。載入更多把新 commit 插進正在
    /// 檢視的項目時，等於在使用者視窗上方塞進新內容——`preview_offset`
    /// 必須跟著位移，畫面才不會因為視窗上方多出內容而往回跳。
    #[test]
    fn loading_more_commits_shifts_preview_offset_to_keep_scroll_position() {
        let mut view = view_with_body("body".to_string());
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            timeline_page(
                vec![timeline_comment("a", "one")],
                Some("cursor".to_string()),
            ),
        );

        let start = warm_timeline_start(&mut view);
        view.preview_offset = start;

        // 續接第二頁，這頁帶了兩個新 commit——commit block 從無到有，
        // 佔掉 1 條分隔線 + 2 行 commit（展開模式）。
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            Some("cursor".to_string()),
            timeline_page(
                vec![
                    timeline_commit("aaaaaaa", "SUCCESS"),
                    timeline_commit("bbbbbbb", "SUCCESS"),
                ],
                None,
            ),
        );

        assert_eq!(view.preview_offset, start + 3);
    }

    /// 首次載入落地時（畫面上只有 loading 提示），視窗即使已捲到 timeline
    /// 起點之下，也不能被推過剛出現的 commit／CI 區塊。
    #[test]
    fn first_load_does_not_shift_preview_offset() {
        use crate::github::CheckState::Passed;
        let mut view = view_with_body("body".to_string());
        let start = warm_timeline_start(&mut view);
        view.preview_offset = start + 1;

        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            ci_page(
                vec![timeline_commit("aaaaaaa", "SUCCESS")],
                None,
                vec![ci_check("a", Passed)],
            ),
        );

        assert_eq!(view.preview_offset, start + 1);
    }

    /// 刷新時視窗頂端還在 body 內（在 timeline 之上），新出現的 commit 區塊
    /// 不影響它，`preview_offset` 不該被補償邏輯動到。
    #[test]
    fn refreshing_first_page_does_not_shift_preview_offset_while_viewing_body() {
        let mut view = view_with_long_body();
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            timeline_page(vec![timeline_comment("a", "one")], None),
        );
        let start = warm_timeline_start(&mut view);
        assert!(start > 5, "long body must push the timeline down");
        view.preview_offset = 5;

        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            timeline_page(
                vec![
                    timeline_commit("aaaaaaa", "SUCCESS"),
                    timeline_comment("a", "one"),
                ],
                None,
            ),
        );

        assert_eq!(view.preview_offset, 5);
    }

    /// `item_count`／`mergeable` 都沒變，只有 CI 狀態換了——沒有 `rev`
    /// 欄位，`PreviewKey` 會判定「跟上次一樣」，直接命中舊 cache，畫面
    /// 紋風不動。這裡刻意走 `render_to_string`（進到
    /// `PreviewCache::get_or_build`），不能直接呼叫 pure function
    /// `build_preview_content`，否則沒有 `rev` 這條測試也會綠。
    #[test]
    fn preview_cache_invalidates_when_ci_status_changes_with_same_item_count() {
        let mut view = view_with_body("body".to_string());
        load_pending_commit(&mut view);
        let screen = render_to_string(&mut view);
        assert!(screen.contains('●'), "got:\n{screen}");

        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            timeline_page(vec![timeline_commit("aaaaaaa", "SUCCESS")], None),
        );
        let screen = render_to_string(&mut view);
        assert!(screen.contains('✓'), "got:\n{screen}");
        assert!(!screen.contains('●'), "got:\n{screen}");
    }

    /// 背景刷新失敗不該把已顯示的內容換成錯誤訊息——`build_timeline` 的
    /// `Error` 分支不看 `items`，這正是 `refreshing` 獨立於 `state` 想避免
    /// 的那種畫面塌陷，只是換成從失敗路徑發生。首次載入失敗（沒有內容可
    /// 保留）不受這條規則影響，另外測。
    #[test]
    fn refresh_failure_does_not_clear_already_loaded_content() {
        let mut view = view_with_body("body".to_string());
        load_pending_commit(&mut view);

        view.set_timeline_error(1, GhItemKind::PullRequest, "boom".to_string());

        let screen = render_to_string(&mut view);
        assert!(screen.contains("aaaaaaa"), "got:\n{screen}");
        assert!(!screen.contains("留言載入失敗"), "got:\n{screen}");
    }

    /// 首次載入（還沒有任何內容）失敗時，沒有東西可保留，必須整頁報錯。
    #[test]
    fn first_load_failure_shows_the_error_notice() {
        let mut view = view_with_body("body".to_string());

        view.set_timeline_error(1, GhItemKind::PullRequest, "boom".to_string());

        let screen = render_to_string(&mut view);
        assert!(screen.contains("留言載入失敗"), "got:\n{screen}");
    }

    /// `set_timeline_error` 這個修正的迴歸測試：漏了重置 `refreshing`，
    /// 一次刷新失敗會讓這個項目從此按 r 永遠被 guard 擋掉。
    #[test]
    fn refresh_after_previous_failure_is_not_blocked() {
        let (mut view, rx) = view_with_body_and_rx("body".to_string());
        load_pending_commit(&mut view);
        view.set_timeline_error(1, GhItemKind::PullRequest, "boom".to_string());

        refresh_with_unchanged_data(&mut view);

        let events: Vec<AppEvent> = rx.try_iter().collect();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AppEvent::LoadGitHubTimeline { .. })),
            "got: {events:?}"
        );
    }

    /// 乙版（原地替換）存在的頭號理由：甲版（remove entry）會被
    /// `render.rs` 的捲動 clamp 永久夾死，這條測試直接釘住不會發生。
    #[test]
    fn update_data_early_return_does_not_reset_scroll_position() {
        let mut view = view_with_long_body();
        render_to_string(&mut view);
        view.preview_offset = view.last_preview_len / 2;
        // 先 render 一次讓 clamp 穩定下來，才抓真正的基準值——不然基準值
        // 本身可能還沒被 clamp 過，第二次 render 才第一次夾，會誤判成
        // 這次改動造成的位移。
        render_to_string(&mut view);
        let offset_before = view.preview_offset;
        assert!(offset_before > 0, "測試前提：捲動位置真的有移動過");

        refresh_with_unchanged_data(&mut view);
        render_to_string(&mut view);

        assert_eq!(view.preview_offset, offset_before);
    }

    #[test]
    fn dividers_are_colour_coded_by_section() {
        let mut view = view_with_body("body".to_string());
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            timeline_page(
                vec![
                    timeline_commit("aaaaaaa", "SUCCESS"),
                    timeline_commit("bbbbbbb", "SUCCESS"),
                    timeline_comment("a", "one"),
                    timeline_comment("b", "two"),
                ],
                None,
            ),
        );

        let (lines, _, _) = build_preview_content(&view.preview_input(40));
        let dividers: Vec<(char, Option<Color>)> = lines
            .iter()
            .filter_map(|l| {
                let text: String = l.spans.iter().map(|s| s.content.to_string()).collect();
                let first = text.chars().next()?;
                if first != '─' {
                    return None;
                }
                Some((first, l.style.fg))
            })
            .collect();

        assert_eq!(
            dividers,
            vec![
                // meta → body：markdown 自己的灰色，維持不變
                ('─', Some(Color::DarkGray)),
                // body → commit block（兩個 commit 集中在一起）
                ('─', Some(Section::Body.color())),
                // commit block → 第一則留言
                ('─', Some(Section::Commit.color())),
                // 第一則留言 → 第二則留言
                ('─', Some(Section::Comment.color())),
            ],
        );
    }

    /// block 分隔線的顏色——markdown 自己那條固定 DarkGray 的不算，
    /// 那條不是 `append_comment_lines` 畫的。
    fn block_divider_colors(lines: &[Line<'static>]) -> Vec<Option<Color>> {
        lines
            .iter()
            .filter(|l| {
                let text: String = l.spans.iter().map(|s| s.content.to_string()).collect();
                text.starts_with('─') && l.style.fg != Some(Color::DarkGray)
            })
            .map(|l| l.style.fg)
            .collect()
    }

    /// 每條分隔線都寫出「下面是什麼」：header 下方是 Body，之後依序是各 block
    /// 自己的 section 名稱。
    #[test]
    fn every_divider_is_labeled_with_the_section_below_it() {
        use crate::github::CheckState::Passed;
        let mut view = view_with_body("body".to_string());
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            ci_page(
                vec![
                    timeline_commit("aaaaaaa", "SUCCESS"),
                    timeline_comment("a", "one"),
                    timeline_review(
                        "reviewer",
                        "APPROVED",
                        Some("2026-08-30T12:00:00Z"),
                        "lgtm",
                        Vec::new(),
                    ),
                ],
                None,
                vec![ci_check("lint", Passed)],
            ),
        );

        let (lines, _, _) = build_preview_content(&view.preview_input(60));
        let labels: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .filter(|t| t.starts_with("── "))
            .map(|t| {
                t.trim_start_matches("── ")
                    .split(' ')
                    .next()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(labels, ["內文", "Commits", "CI", "留言", "Review"]);
    }

    /// 分隔線顏色來自它*之前*那個 block 的 section，所以 `Section::Review`
    /// 的顏色要在 review block *之後*還有其他 block 時才會被畫出來。
    #[test]
    fn review_block_divider_is_colour_coded() {
        let mut view = view_with_body("body".to_string());
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            timeline_page(
                vec![
                    timeline_comment("a", "one"),
                    timeline_review(
                        "reviewer",
                        "APPROVED",
                        Some("2026-08-30T12:00:00Z"),
                        "lgtm",
                        Vec::new(),
                    ),
                    timeline_comment("b", "two"),
                ],
                None,
            ),
        );

        let (lines, _, _) = build_preview_content(&view.preview_input(40));
        assert_eq!(
            block_divider_colors(&lines),
            vec![
                Some(Section::Body.color()),    // body → 第一則留言
                Some(Section::Comment.color()), // 第一則留言 → review block
                Some(Section::Review.color()),  // review block → 第二則留言
            ],
            "got:\n{lines:?}"
        );
    }

    /// 一則 review 連同它的行內留言只算*一個* block：只有一條 Review 色
    /// 分隔線，行內留言之間不會各自被切成獨立 block（那樣會多出好幾條
    /// 分隔線，把「這些留言屬於同一次 review」的關係拆散）。
    #[test]
    fn review_and_its_inline_comments_render_as_one_block() {
        let mut view = view_with_body("body".to_string());
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            timeline_page(
                vec![timeline_review(
                    "reviewer",
                    "APPROVED",
                    Some("2026-08-30T12:00:00Z"),
                    "looks good",
                    vec![
                        review_comment("src/a.rs", Some(10), "nit here"),
                        review_comment("src/b.rs", Some(20), "and here"),
                    ],
                )],
                None,
            ),
        );

        // 分隔線的顏色來自它*之前*那個 block 的 section（見
        // `dividers_are_colour_coded_by_section`），這裡唯一的 block 前面
        // 是初始值 `Section::Body`，所以直接數「不是 markdown 自己那條
        // 固定 DarkGray 分隔線」的數量——一個 block 只會有一條。
        let (lines, _, _) = build_preview_content(&view.preview_input(40));
        let block_dividers = block_divider_colors(&lines).len();
        assert_eq!(
            block_dividers, 1,
            "review + its inline comments must draw exactly one divider, got:\n{lines:?}"
        );
    }

    #[test]
    fn approved_review_renders_marker_author_state_and_body() {
        let mut view = view_with_body("body".to_string());
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            timeline_page(
                vec![timeline_review(
                    "reviewer",
                    "APPROVED",
                    Some("2026-08-30T12:00:00Z"),
                    "looks good",
                    Vec::new(),
                )],
                None,
            ),
        );

        let screen = render_to_string(&mut view);
        assert!(screen.contains('✓'), "got:\n{screen}");
        assert!(screen.contains("@reviewer"), "got:\n{screen}");
        assert!(screen.contains("已核准"), "got:\n{screen}");
        assert!(screen.contains("looks good"), "got:\n{screen}");
    }

    #[test]
    fn inline_comment_renders_path_and_line() {
        let mut view = view_with_body("body".to_string());
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            timeline_page(
                vec![timeline_review(
                    "reviewer",
                    "COMMENTED",
                    Some("2026-08-30T12:00:00Z"),
                    "",
                    vec![review_comment(
                        "src/github.rs",
                        Some(604),
                        "missing a type here",
                    )],
                )],
                None,
            ),
        );

        let screen = render_to_string(&mut view);
        assert!(screen.contains("src/github.rs:604"), "got:\n{screen}");
        assert!(screen.contains("missing a type here"), "got:\n{screen}");
    }

    /// PENDING review（`submitted_at: None`，尚未送出的草稿）要被過濾掉；
    /// 一頁*只有* PENDING review 時仍要 fallback 到 `(no comments)`，跟
    /// 全 `Unknown` 節點是同一種情況。
    #[test]
    fn pending_review_is_filtered_and_falls_back_to_no_comments() {
        let mut view = view_with_body("body".to_string());
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            timeline_page(
                vec![timeline_review(
                    "reviewer",
                    "PENDING",
                    None,
                    "draft, not submitted yet",
                    Vec::new(),
                )],
                None,
            ),
        );

        let screen = render_to_string(&mut view);
        assert!(
            !screen.contains("draft, not submitted yet"),
            "PENDING review draft must not render, got:\n{screen}"
        );
        assert!(screen.contains("沒有留言"), "got:\n{screen}");
    }

    #[test]
    fn resolved_and_outdated_markers_appear_on_inline_comments() {
        let mut view = view_with_body("body".to_string());
        let mut resolved = review_comment("src/a.rs", Some(1), "fixed already");
        resolved.resolved = true;
        resolved.outdated = true;
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            timeline_page(
                vec![timeline_review(
                    "reviewer",
                    "COMMENTED",
                    Some("2026-08-30T12:00:00Z"),
                    "",
                    vec![resolved],
                )],
                None,
            ),
        );

        let screen = render_to_string(&mut view);
        assert!(screen.contains("(已解決)"), "got:\n{screen}");
        assert!(screen.contains("(已過時)"), "got:\n{screen}");
    }

    #[test]
    fn review_comment_overflow_shows_remaining_count() {
        let mut view = view_with_body("body".to_string());
        let mut review = timeline_review(
            "reviewer",
            "COMMENTED",
            Some("2026-08-30T12:00:00Z"),
            "",
            vec![review_comment("src/a.rs", Some(1), "one")],
        );
        // totalCount 比實際帶回的 nodes 多——GraphQL 端只給了前 N 則。
        if let GhTimelineItem::PullRequestReview { comments, .. } = &mut review {
            comments.total_count = 4;
        }
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            timeline_page(vec![review], None),
        );

        let screen = render_to_string(&mut view);
        assert!(screen.contains("(還有 3 則留言)"), "got:\n{screen}");
    }

    /// commit 集中成一個區塊：即使 API 回傳的時間序是 commit/comment 交錯，
    /// 展開模式下所有 commit 仍緊貼著彼此排在第一則留言之前，中間沒有
    /// 分隔線把它們切開。
    #[test]
    fn commits_are_grouped_together_before_the_first_comment() {
        let mut view = view_with_body("body".to_string());
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            timeline_page(
                vec![
                    timeline_commit("aaaaaaa", "SUCCESS"),
                    timeline_comment("a", "one"),
                    timeline_commit("bbbbbbb", "SUCCESS"),
                    timeline_commit("ccccccc", "SUCCESS"),
                ],
                None,
            ),
        );

        let (lines, _, _) = build_preview_content(&view.preview_input(40));
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect())
            .collect();

        let commit_positions: Vec<usize> = texts
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                t.contains("aaaaaaa") || t.contains("bbbbbbb") || t.contains("ccccccc")
            })
            .map(|(i, _)| i)
            .collect();
        assert_eq!(commit_positions.len(), 3, "got: {texts:?}");
        // 三個 commit 緊貼彼此：中間沒有其他列（分隔線或留言）插進來。
        assert!(
            commit_positions.windows(2).all(|w| w[1] == w[0] + 1),
            "commits must be contiguous, got: {texts:?}"
        );

        let comment_index = texts.iter().position(|t| t.contains("one")).unwrap();
        assert!(
            commit_positions.iter().all(|&i| i < comment_index),
            "all commits must come before the comment, got: {texts:?}"
        );
    }

    /// 只有 commit、沒有留言的 PR 不該顯示「沒有留言」——那是專門為「整條
    /// timeline 都沒有可渲染內容」保留的訊息。commit block 本身就是內容，
    /// 而且 timelineItems 是混合 connection，留言完全有可能落在下一頁。
    #[test]
    fn commits_without_comments_do_not_show_no_comments_notice() {
        let mut view = view_with_body("body".to_string());
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            timeline_page(vec![timeline_commit("aaaaaaa", "SUCCESS")], None),
        );

        let screen = render_to_string(&mut view);
        assert!(!screen.contains("沒有留言"), "got:\n{screen}");
    }

    #[test]
    fn preview_cache_invalidates_when_more_comments_start_loading() {
        let mut view = view_with_body("short".to_string());
        // 已載入一頁，且還有下一頁可以載入。
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            timeline_page(
                vec![timeline_comment("bob", "hi")],
                Some("cursor".to_string()),
            ),
        );
        let screen = render_to_string(&mut view);
        // 只取片段比對：在這個寬度下，完整的 footer 文字會折行。
        assert!(screen.contains("還有更多留言"), "got:\n{screen}");

        // 抓取下一頁只會改變 `loading_more`——項目數量沒變、stage 也沒變——
        // 所以 footer 只有在 key 有追蹤這個欄位時才會更新。
        view.preview_offset = usize::MAX / 2;
        view.maybe_load_more_timeline();
        let screen = render_to_string(&mut view);

        assert!(
            screen.contains("載入更多中"),
            "footer must follow loading_more, got:\n{screen}"
        );
    }

    fn view_with_issue(body: String) -> GitHubView<'static> {
        let issue = GhIssue {
            number: 1,
            title: "t".to_string(),
            state: "OPEN".to_string(),
            labels: Vec::new(),
            author: GhAuthor {
                login: "alice".to_string(),
            },
            created_at: String::new(),
            body,
            url: String::new(),
            closed_at: None,
            updated_at: String::new(),
            parent: None,
            sub_issues: Vec::new(),
        };

        let (tx, _rx) = Sender::channel_for_test();
        let data = GitHubData {
            issues: vec![issue],
            ..Default::default()
        };
        let mut view = GitHubView::new(View::Default, data, tx);
        view.active_tab = GitHubTab::Issues;
        view
    }

    /// Issues 分頁從 `TimelineEntry` 以下跟 PR 共用每一條程式碼路徑——
    /// 這個測試釘住了共用的 `timelineItems` 管線仍然跟 3b 之前一樣，
    /// 正確渲染 Issue 的 body 與留言。
    #[test]
    fn issue_timeline_renders_like_pr_timeline() {
        let mut view = view_with_issue("issue body".to_string());
        view.append_timeline_items(
            1,
            GhItemKind::Issue,
            None,
            timeline_page(vec![timeline_comment("carol", "issue comment")], None),
        );

        let screen = render_to_string(&mut view);
        assert!(screen.contains("issue body"), "got:\n{screen}");
        assert!(screen.contains("carol"), "got:\n{screen}");
        assert!(screen.contains("issue comment"), "got:\n{screen}");
    }

    #[test]
    fn empty_timeline_still_draws_the_body_divider() {
        let mut view = view_with_body("body".to_string());
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            timeline_page(Vec::new(), None),
        );

        let (lines, _, _) = build_preview_content(&view.preview_input(40));
        let has_body_divider = lines.iter().any(|l| {
            let text: String = l.spans.iter().map(|s| s.content.to_string()).collect();
            text.starts_with('─') && l.style.fg == Some(Section::Body.color())
        });
        assert!(
            has_body_divider,
            "loaded-but-empty timeline must still draw the body divider, got: {lines:?}"
        );
    }

    /// 一頁全部都是 `TimelineItem::from_gh` 會丟棄的節點（無法辨識的
    /// `__typename`）時，必須 fallback 成跟真正空頁一樣的行為——過濾動作
    /// 發生在 `entry.items.is_empty()` 已經判定「不是空的」之*後*。
    #[test]
    fn all_unknown_timeline_still_draws_the_body_divider() {
        let mut view = view_with_body("body".to_string());
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            timeline_page(vec![GhTimelineItem::Unknown, GhTimelineItem::Unknown], None),
        );

        let screen = render_to_string(&mut view);
        assert!(screen.contains("沒有留言"), "got:\n{screen}");

        let (lines, _, _) = build_preview_content(&view.preview_input(40));
        let has_body_divider = lines.iter().any(|l| {
            let text: String = l.spans.iter().map(|s| s.content.to_string()).collect();
            text.starts_with('─') && l.style.fg == Some(Section::Body.color())
        });
        assert!(
            has_body_divider,
            "an all-Unknown page must still draw the body divider, got: {lines:?}"
        );
    }

    fn rendered_mergeable_marker(view: &GitHubView<'_>) -> Option<(String, Option<Color>)> {
        let (lines, _, _) = build_preview_content(&view.preview_input(40));
        lines.iter().find_map(|l| {
            l.spans
                .iter()
                .find(|s| s.content.contains("可 merge") || s.content.contains("有衝突"))
                .map(|s| (s.content.to_string(), s.style.fg))
        })
    }

    #[test]
    fn mergeable_state_renders_into_the_base_head_line() {
        let mut view = view_with_body("body".to_string());
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            GhTimelinePage {
                mergeable: Some(Mergeable::Conflicting),
                ..timeline_page(Vec::new(), None)
            },
        );
        assert_eq!(
            rendered_mergeable_marker(&view),
            Some(("  (有衝突)".to_string(), Some(Color::Red)))
        );
    }

    /// 行數來自 PR 清單本身（`pr.diff_stat`），不是 timeline——不用先
    /// `append_timeline_items` 就該渲染出來。
    #[test]
    fn diff_stat_renders_into_the_base_head_line() {
        let mut view = view_with_body("body".to_string());
        view.pull_requests[0].diff_stat = DiffStat {
            additions: 600,
            deletions: 71,
        };
        let (lines, _, _) = build_preview_content(&view.preview_input(40));
        let rendered: String = lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content.contains("←")))
            .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect())
            .unwrap_or_default();
        assert!(
            rendered.contains("+600") && rendered.contains("-71") && rendered.contains("=671"),
            "base ← head line must carry the diff stat, got: {rendered:?}"
        );
    }

    #[test]
    fn collapsing_commits_replaces_them_with_a_summary() {
        let mut view = view_with_body("body".to_string());
        view.append_timeline_items(
            1,
            GhItemKind::PullRequest,
            None,
            timeline_page(
                vec![
                    timeline_commit("aaaaaaa", "SUCCESS"),
                    timeline_comment("carol", "one"),
                    timeline_commit("bbbbbbb", "SUCCESS"),
                ],
                None,
            ),
        );
        // 切換前先把 cache 熱好：冷 cache 一定會無條件重建，不管
        // `expand_commits` 有沒有被 cache key 追蹤，這種情況下就算追蹤機制
        // 壞了，這個測試也照樣會過。
        render_to_string(&mut view);

        view.toggle_commit_log();
        let screen = render_to_string(&mut view);

        assert!(!screen.contains("aaaaaaa"), "got:\n{screen}");
        assert!(!screen.contains("bbbbbbb"), "got:\n{screen}");
        assert!(screen.contains("2 個 commit"), "got:\n{screen}");
        // 中間夾的留言必須在收合後存活——只有 commit 會被折疊。
        assert!(screen.contains("one"), "got:\n{screen}");
    }

    #[test]
    fn toggle_commit_log_has_no_effect_on_issues_tab() {
        let mut view = view_with_issue("issue body".to_string());
        assert!(view.expand_commits);

        view.handle_preview_event(UserEvent::ToggleCommitLog, 1);
        assert!(view.expand_commits, "Issues tab must ignore the toggle");

        view.handle_list_event(UserEvent::ToggleCommitLog, 1);
        assert!(view.expand_commits, "Issues tab must ignore the toggle");

        assert!(
            !view
                .status_hints()
                .iter()
                .any(|(events, _)| events.contains(&UserEvent::ToggleCommitLog)),
            "no hint should be offered for a key that does nothing here"
        );
    }

    #[test]
    fn toggle_commit_log_flips_expand_commits_on_pr_tab() {
        let mut view = view_with_body("body".to_string());
        assert!(view.expand_commits);

        view.handle_preview_event(UserEvent::ToggleCommitLog, 1);
        assert!(!view.expand_commits);

        view.handle_list_event(UserEvent::ToggleCommitLog, 1);
        assert!(view.expand_commits);
    }

    /// 跟 `view_with_body` 共用同一個 PR fixture，只覆寫這個測試在乎的
    /// 兩個欄位——避免整段複製 `GhPullRequest` 建構，`GhPullRequest` 之後
    /// 加欄位時只有一處要跟著改。
    fn view_with_pr_labels(
        labels: Vec<crate::github::GhLabel>,
        title: &str,
    ) -> GitHubView<'static> {
        let mut view = view_with_body(String::new());
        view.pull_requests[0].labels = labels;
        view.pull_requests[0].title = title.to_string();
        view
    }

    /// #112：labels 一多就把標題擠到看不見。色塊模式（`t`）要顯示每個
    /// label 對應顏色的方塊、不顯示名稱，讓出來的欄寬要能讓標題露出更多。
    #[test]
    fn compact_label_mode_hides_names_and_frees_title_space() {
        let labels = vec![
            crate::github::GhLabel {
                name: "alpha".to_string(),
                color: Some("00ff00".to_string()),
            },
            crate::github::GhLabel {
                name: "beta".to_string(),
                color: Some("ff0000".to_string()),
            },
        ];
        let title = "This is a genuinely long pull request title that needs room";
        let mut view = view_with_pr_labels(labels, title);
        assert_eq!(view.label_mode, LabelMode::Names);

        // preview（`│` 右側）一律顯示完整 label 名稱與標題，不受這個設定
        // 影響——斷言只看 list 那一欄（`│` 左側），才是真正在測的東西。
        let list_row = |screen: &str| -> String {
            screen
                .lines()
                .find(|l| l.contains("#1") && l.contains("開啟"))
                .unwrap()
                .split('│')
                .next()
                .unwrap()
                .to_string()
        };

        let names_screen = render_to_string_at_width(&mut view, 100);
        let names_row = list_row(&names_screen);
        assert!(
            names_row.contains("alpha") && names_row.contains("beta"),
            "names mode must show label text in the list row:\n{names_row}"
        );
        assert!(
            !names_row.contains('▮'),
            "names mode must not draw swatches:\n{names_row}"
        );

        view.toggle_label_mode();
        assert_eq!(view.label_mode, LabelMode::Swatches);
        let swatch_screen = render_to_string_at_width(&mut view, 100);
        let swatch_row = list_row(&swatch_screen);
        assert!(
            !swatch_row.contains("alpha") && !swatch_row.contains("beta"),
            "swatch mode must hide label text in the list row:\n{swatch_row}"
        );
        assert!(
            swatch_row.contains("▮▮"),
            "swatch mode must draw one block per label, tight-packed:\n{swatch_row}"
        );

        // 色塊比 "[alpha, beta]" 短很多，讓出來的欄寬應該讓標題露出更多字。
        let title_prefix = &title[..10];
        assert!(
            !names_row.contains(title_prefix),
            "names mode is the regression case: title should still be cut off in the list row:\n{names_row}"
        );
        assert!(
            swatch_row.contains(title_prefix),
            "swatch mode should free up enough width to show more of the title in the list row:\n{swatch_row}"
        );
    }

    /// `t` 在 Issues／PRs 兩個分頁都要生效——不像 `z`（commit log）只對 PR
    /// 有意義，labels 精簡顯示兩個分頁都有需求。
    #[test]
    fn label_compact_toggle_flips_on_both_tabs_and_both_foci() {
        let mut pr_view = view_with_body("body".to_string());
        assert_eq!(pr_view.label_mode, LabelMode::Names);
        pr_view.handle_list_event(UserEvent::CreateTag, 1);
        assert_eq!(pr_view.label_mode, LabelMode::Swatches);
        pr_view.handle_preview_event(UserEvent::CreateTag, 1);
        assert_eq!(pr_view.label_mode, LabelMode::Names);

        let mut issue_view = view_with_issue("issue body".to_string());
        assert_eq!(issue_view.label_mode, LabelMode::Names);
        issue_view.handle_list_event(UserEvent::CreateTag, 1);
        assert_eq!(issue_view.label_mode, LabelMode::Swatches);
        issue_view.handle_preview_event(UserEvent::CreateTag, 1);
        assert_eq!(issue_view.label_mode, LabelMode::Names);
    }

    /// 搜尋 prompt 裡打字母 `t`：要進 search_input，不能被攔截去切換
    /// label 顯示模式——這是重用既有事件最容易踩的雷，`event.rs::handle_prompt_event`
    /// 的 fallback 分支理論上會處理對，這裡直接鎖住行為。
    #[test]
    fn typing_t_in_prompt_focus_reaches_search_input_not_label_toggle() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent};

        let mut view = view_with_body("body".to_string());
        view.focus = GitHubFocus::Prompt;
        assert_eq!(view.label_mode, LabelMode::Names);

        view.handle_event(
            UserEventWithCount::new(UserEvent::CreateTag, 1),
            KeyEvent::from(KeyCode::Char('t')),
        );

        assert_eq!(
            view.label_mode,
            LabelMode::Names,
            "typing inside the search prompt must not toggle label mode"
        );
        assert_eq!(view.search_input.value(), "t");
    }

    /// 色塊模式的提示只在使用者真的切換過去時才出現，平常（預設的名稱
    /// 模式）不佔狀態列空間。
    #[test]
    fn label_mode_hint_only_shown_in_swatch_mode() {
        let mut view = view_with_body("body".to_string());
        view.focus = GitHubFocus::List;

        let has_hint = |view: &GitHubView<'_>| {
            view.status_hints()
                .iter()
                .any(|(events, _)| events.contains(&UserEvent::CreateTag))
        };

        assert!(!has_hint(&view), "names mode must not show the hint");
        view.toggle_label_mode();
        assert!(has_hint(&view), "swatch mode must show the hint");
    }

    /// `r` 在 List 跟 Preview 兩個 focus 都要能觸發同一個刷新動作——
    /// Preview 之前完全沒有接 `UserEvent::Refresh`，在 gh 的 detail
    /// （PR/Issue 詳情，含留言）裡按 r 沒有任何反應。
    #[test]
    fn refresh_triggers_from_both_list_and_preview_focus() {
        let (mut view, rx) = view_with_body_and_rx("body".to_string());

        view.handle_preview_event(UserEvent::Refresh, 1);
        assert!(matches!(view.load_state, LoadState::Loading));
        let sent = rx.try_recv();
        assert!(
            matches!(sent, Ok(AppEvent::RefreshGitHub { .. })),
            "preview focus must send RefreshGitHub, got: {sent:?}"
        );

        view.load_state = LoadState::Idle;
        view.handle_list_event(UserEvent::Refresh, 1);
        assert!(matches!(view.load_state, LoadState::Loading));
        let sent = rx.try_recv();
        assert!(
            matches!(sent, Ok(AppEvent::RefreshGitHub { .. })),
            "list focus must keep sending RefreshGitHub, got: {sent:?}"
        );
    }

    /// Preview 的狀態列必須把 `refresh` 提示出來，使用者才知道 r 在這裡
    /// 有作用，不用去猜或翻說明頁。
    #[test]
    fn preview_status_hints_include_refresh() {
        let mut view = view_with_body("body".to_string());
        view.focus = GitHubFocus::Preview;
        assert!(
            view.status_hints()
                .iter()
                .any(|(events, _)| events.contains(&UserEvent::Refresh)),
            "got: {:?}",
            view.status_hints()
        );
    }

    // ── timeline debounce ──

    fn debounce_issue(number: u64, title: &str) -> GhIssue {
        GhIssue {
            number,
            title: title.to_string(),
            state: "OPEN".to_string(),
            labels: Vec::new(),
            author: GhAuthor {
                login: "alice".to_string(),
            },
            created_at: String::new(),
            body: String::new(),
            url: String::new(),
            closed_at: None,
            updated_at: String::new(),
            parent: None,
            sub_issues: Vec::new(),
        }
    }

    /// 三個 issue（#1 alpha、#2 beta、#3 gamma），選取在第一筆。
    fn debounce_view() -> (GitHubView<'static>, std::sync::mpsc::Receiver<AppEvent>) {
        let (tx, rx) = Sender::channel_for_test();
        let data = GitHubData {
            issues: vec![
                debounce_issue(1, "alpha"),
                debounce_issue(2, "beta"),
                debounce_issue(3, "gamma"),
            ],
            ..Default::default()
        };
        let view = GitHubView::new(View::Default, data, tx);
        (view, rx)
    }

    fn timeline_requests(rx: &std::sync::mpsc::Receiver<AppEvent>) -> Vec<u64> {
        rx.try_iter()
            .filter_map(|e| match e {
                AppEvent::LoadGitHubTimeline { number, .. } => Some(number),
                _ => None,
            })
            .collect()
    }

    fn press(view: &mut GitHubView<'_>, event: UserEvent) {
        use ratatui::crossterm::event::{KeyCode, KeyEvent};
        view.handle_event(
            UserEventWithCount::new(event, 1),
            KeyEvent::from(KeyCode::Null),
        );
    }

    #[test]
    fn on_tick_sends_timeline_request_only_at_the_debounce_boundary() {
        let (mut view, rx) = debounce_view();
        let t0 = Instant::now();
        view.schedule_timeline_for_selected(t0);

        view.on_tick(t0 + TIMELINE_DEBOUNCE - Duration::from_millis(1));
        assert!(timeline_requests(&rx).is_empty());

        view.on_tick(t0 + TIMELINE_DEBOUNCE);
        assert_eq!(timeline_requests(&rx), vec![1]);

        // 送出後排程已清掉，之後的 tick 不會再送
        view.on_tick(t0 + TIMELINE_DEBOUNCE * 2);
        assert!(timeline_requests(&rx).is_empty());
    }

    #[test]
    fn rapid_selection_changes_send_one_request_for_the_last_item() {
        let (mut view, rx) = debounce_view();
        let t0 = Instant::now();

        press(&mut view, UserEvent::NavigateDown);
        press(&mut view, UserEvent::NavigateDown);

        view.on_tick(t0 + Duration::from_millis(100));
        assert!(timeline_requests(&rx).is_empty(), "debounce 期間不該送");

        view.on_tick(Instant::now() + TIMELINE_DEBOUNCE);
        assert_eq!(timeline_requests(&rx), vec![3]);
    }

    #[test]
    fn debounced_tick_skips_items_whose_timeline_is_already_requested() {
        let (mut view, rx) = debounce_view();
        view.timeline
            .entry((GhItemKind::Issue, 1))
            .or_default()
            .state = TimelineLoad::Loaded;

        let t0 = Instant::now();
        view.schedule_timeline_for_selected(t0);
        view.on_tick(t0 + TIMELINE_DEBOUNCE);

        assert!(timeline_requests(&rx).is_empty());
    }

    /// 搜尋打字讓選到的項目換了、index 卻仍是 0——舊的 `(tab, index)`
    /// 比對會漏掉，preview 就卡在 loading。
    #[test]
    fn typing_in_search_requests_timeline_for_the_newly_selected_item() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent};

        let (mut view, rx) = debounce_view();
        view.focus = GitHubFocus::Prompt;

        view.handle_event(
            UserEventWithCount::new(UserEvent::Unknown, 1),
            KeyEvent::from(KeyCode::Char('g')),
        );
        assert_eq!(
            view.selected_number_and_kind(),
            Some((3, GhItemKind::Issue))
        );

        view.on_tick(Instant::now() + TIMELINE_DEBOUNCE);
        assert_eq!(timeline_requests(&rx), vec![3]);
    }

    /// 帶快取資料重開 view（`open_github` → `new`）時，初始選取項目的
    /// timeline 要在第一個 Tick 補送，不能卡在 loading 等使用者移動選取。
    #[test]
    fn reopen_with_cached_data_requests_timeline_on_first_tick() {
        let (mut view, rx) = debounce_view();
        assert!(timeline_requests(&rx).is_empty());

        view.on_tick(Instant::now());

        assert_eq!(timeline_requests(&rx), vec![1]);
    }

    #[test]
    fn jump_to_issue_requests_timeline() {
        let (mut view, rx) = debounce_view();

        assert!(view.jump_to_issue(2));

        assert_eq!(timeline_requests(&rx), vec![2]);
    }

    /// `update_data` 已立即送出首頁請求，之後到期的 debounce tick 不能再送第二次。
    #[test]
    fn expired_tick_after_immediate_request_does_not_duplicate() {
        let (mut view, rx) = debounce_view();
        let t0 = Instant::now();
        view.schedule_timeline_for_selected(t0);

        view.update_data(vec![debounce_issue(9, "new")], Vec::new(), None, None);
        assert_eq!(timeline_requests(&rx), vec![9]);

        view.on_tick(t0 + TIMELINE_DEBOUNCE);
        assert!(timeline_requests(&rx).is_empty());
    }
}

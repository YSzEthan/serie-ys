use std::{
    path::PathBuf,
    process::Child,
    rc::Rc,
    sync::{mpsc, Arc, Mutex},
    thread,
};

use ratatui::{
    crossterm::event::{KeyCode, KeyEvent, KeyEventKind},
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Padding, Paragraph},
    Frame,
};
use tui_input::{backend::crossterm::EventHandler, Input};

use crate::{
    app::{external_command_parameters, AppContext},
    config::CursorType,
    event::{AppEvent, Sender, UserEvent, UserEventWithCount},
    external::{exec_shell_command, expand_shell_command_line},
    git::{Commit, Ref},
    view::{ansi_output_to_lines, View},
    widget::{
        output_pane::{OutputPane, OutputPaneState},
        split_at_width,
    },
};

/// 輸出面板固定高度——沒有開 `ui.pane_height.shell` 設定，這是沒人要求過
/// 的旋鈕，有人抱怨再加。
pub(crate) const OUTPUT_PANE_HEIGHT: u16 = 20;

/// 顯示層的行數上限——記憶體層的上限是 `external::MAX_OUTPUT_BYTES`
/// （讀取階段就接住，不是等字串已經進了記憶體才 truncate）；這裡單純是
/// 避免幾萬行塞進 `OutputPane` 拖慢渲染。
const MAX_OUTPUT_LINES: usize = 50_000;

/// `render()` 與 `{{area_height}}` marker 的面積算式共用同一個來源——分開寫
/// 兩份曾經漂移過（marker 那份忘了扣輸入列吃掉的那一行，短終端下算多 1）。
/// 回傳的是輸出面板的 *外框* 高度；扣掉 `OutputPane` 自己的上邊框才是內容
/// 可用列數，呼叫端各自處理（`render()` 把它原樣交給 widget，widget 內部
/// 自己扣；marker 算式要自己扣一次）。
///
/// 這兩個數字是開啟命令列當下算好、存進 `ShellView` 的快照——終端機
/// resize 之後 `render()` 會用新的即時 `area`，但 `{{area_width}}`／
/// `{{area_height}}` marker 展開用的還是開啟當下那份，會跟畫面顯示的
/// 尺寸不同步。跟 user command 那條路徑一樣的取捨（`app::open_shell` /
/// `app::build_external_command_parameters` 也是開啟當下算一次），不是
/// 這裡才有的新問題。
pub(crate) fn output_pane_height(area_height: u16) -> u16 {
    OUTPUT_PANE_HEIGHT.min(area_height.saturating_sub(2))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellAction {
    Run,
    Close,
    HistPrev,
    HistNext,
    ScrollUp,
    ScrollDown,
    Text,
}

/// 預設鍵位把 `j k h l i m . q r c b v t d o g u f p x z y n` 幾乎整片
/// 字母佔走了，命令列必須能完整打出 `git`、`branch`。只有 Up/Down/PageUp/
/// PageDown 這四個是不可設定的原始鍵——因為 `j`/`k` 必須留給文字輸入，沒有
/// `UserEvent` 可以綁。Confirm／Cancel 仍走可設定的 `UserEvent`，但要排除
/// `y`/`n`：兩者的預設綁定含這兩個字元，比照 `CreateTagView` 讓它們在文字
/// 欄位裡維持文字語意。
fn resolve(event: UserEvent, key: KeyEvent) -> ShellAction {
    match key.code {
        KeyCode::Up => ShellAction::HistPrev,
        KeyCode::Down => ShellAction::HistNext,
        KeyCode::PageUp => ShellAction::ScrollUp,
        KeyCode::PageDown => ShellAction::ScrollDown,
        _ => match event {
            UserEvent::Confirm if key.code != KeyCode::Char('y') => ShellAction::Run,
            UserEvent::Cancel if key.code != KeyCode::Char('n') => ShellAction::Close,
            _ => ShellAction::Text,
        },
    }
}

#[derive(Debug)]
pub struct ShellView<'a> {
    before: View<'a>,
    /// `None` = 目前選取的是 Working changes（virtual row），沒有真正的
    /// commit 可以代入 `{{target_hash}}` 系列 marker——`run()` 只在指令
    /// 真的含這些 marker 時才報錯，不含（`git status` 這類）照跑。
    commit: Option<Commit>,
    refs: Vec<Ref>,
    area_width: u16,
    area_height: u16,
    repo_path: PathBuf,

    input: Input,
    history: Vec<String>,
    /// `None` = 目前在編輯新指令（不在瀏覽歷史）。
    history_index: Option<usize>,

    output_lines: Vec<Line<'static>>,
    output_pane_state: OutputPaneState,
    /// `Some` = 指令在背景執行緒跑，`try_recv` 拿得到結果就轉成 `None`。
    /// 這個欄位本身就是「執行中」旗標，不需要額外的 generation 計數——
    /// `ShellView` 被 drop（例如使用者中途按 Esc）時 `Receiver` 跟著被
    /// drop，背景 thread 的 `send` 會回 `Err`，舊結果自動失去去處。
    rx: Option<mpsc::Receiver<Vec<Line<'static>>>>,
    /// 背景 thread 目前跑的 child process——`Drop` 用它在使用者中途關閉
    /// （Esc）或整個 app 結束時砍掉還在跑的指令，不留孤兒程序握著 pipe
    /// （`git push` 卡認證、寫錯的無窮迴圈都會走到這裡）。每次 `run()`
    /// 重用同一個 `Arc`，不是每次重建——`rx.is_some()` 已經擋掉了重疊執行，
    /// 同一時間至多一個 child。
    child: Arc<Mutex<Option<Child>>>,
    /// git watcher 觸發的 `AppEvent::Refresh` 若直接重建 `App`，`ShellView`
    /// 會被整個炸掉（輸入、歷史、輸出全部消失）——這個功能的用途就是跑會
    /// 改動 repo 的指令，watcher 一秒內就會觸發。所以 `refresh()` 只設這個
    /// 旗標，實際刷新延後到 `close_shell` 還原 `before` 之後才補送。
    refresh_pending: bool,

    ctx: Rc<AppContext>,
    tx: Sender,
}

impl<'a> ShellView<'a> {
    pub fn new(
        before: View<'a>,
        commit: Option<Commit>,
        refs: Vec<Ref>,
        area_width: u16,
        area_height: u16,
        repo_path: PathBuf,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> ShellView<'a> {
        ShellView {
            before,
            commit,
            refs,
            area_width,
            area_height,
            repo_path,
            input: Input::default(),
            history: Vec::new(),
            history_index: None,
            output_lines: Vec::new(),
            output_pane_state: OutputPaneState::default(),
            rx: None,
            child: Arc::new(Mutex::new(None)),
            refresh_pending: false,
            ctx,
            tx,
        }
    }

    pub fn take_before_view(&mut self) -> View<'a> {
        std::mem::take(&mut self.before)
    }

    /// 下面四個方法單純委派給 `before`——`ShellView::render` 畫的就是
    /// `before`，跑馬燈與 graph clear 的旗標／狀態也活在它身上，見
    /// `View::marquee_id` 等呼叫端的文件註解。
    pub fn marquee_id(&self) -> Option<std::sync::Arc<str>> {
        self.before.marquee_id()
    }

    pub fn marquee_needed(&self) -> bool {
        self.before.marquee_needed()
    }

    pub fn take_graph_clear(&mut self) -> bool {
        self.before.take_graph_clear()
    }

    pub fn request_graph_clear(&mut self) {
        self.before.request_graph_clear();
    }

    /// `View::refresh()` 的 Shell arm 呼叫這裡——見上面 `refresh_pending`
    /// 的註解，這裡只記旗標，不能真的動 `before`（那會把它跟 `Refresh`
    /// event 的 `RefreshViewContext` 綁在一起，Shell 沒有對應的 context
    /// 變體）。故意不叫 `refresh`：其他 view 的 `refresh()` 是「立刻送出
    /// `AppEvent::Refresh`」，同名但語意不同容易混淆。
    pub fn mark_refresh_pending(&mut self) {
        self.refresh_pending = true;
    }

    pub fn take_refresh_pending(&mut self) -> bool {
        std::mem::take(&mut self.refresh_pending)
    }

    /// `AppEvent::ShellOutputReady` 抵達時呼叫。過期的喚醒（例如指令還沒
    /// 跑完使用者就關掉、又重開了一個新的 ShellView）打到新 view 的空
    /// `rx`，`try_recv` 拿不到東西，什麼都不做——正確地什麼都不做。
    pub fn poll_output(&mut self) {
        let Some(rx) = &self.rx else { return };
        match rx.try_recv() {
            Ok(lines) => {
                self.output_lines = lines;
                self.output_pane_state = OutputPaneState::default();
                self.rx = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            // 背景 thread panic 或提早結束、沒送出結果就掛了——沒有結果可
            // 拿，但「執行中」這個狀態不能繼續卡著：不清掉的話 `run()`
            // 會永遠在 `rx.is_some()` 那條早退，使用者只能關掉命令列重開。
            Err(mpsc::TryRecvError::Disconnected) => {
                self.rx = None;
            }
        }
    }

    pub fn handle_event(&mut self, event_with_count: UserEventWithCount, key: KeyEvent) {
        // Windows 上釋放 key 也會送事件；不濾掉的話 Enter 一次按壓
        // 會觸發兩次執行。
        if key.kind != KeyEventKind::Press {
            return;
        }

        match resolve(event_with_count.event, key) {
            ShellAction::Run => self.run(),
            ShellAction::Close => self.tx.send(AppEvent::CloseShell),
            ShellAction::HistPrev => self.history_prev(),
            ShellAction::HistNext => self.history_next(),
            ShellAction::ScrollUp => self.output_pane_state.scroll_page_up(),
            ShellAction::ScrollDown => self.output_pane_state.scroll_page_down(),
            ShellAction::Text => {
                self.input
                    .handle_event(&ratatui::crossterm::event::Event::Key(key));
            }
        }
    }

    fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let idx = match self.history_index {
            None => self.history.len() - 1,
            Some(i) => i.saturating_sub(1),
        };
        self.history_index = Some(idx);
        self.input = Input::new(self.history[idx].clone());
    }

    fn history_next(&mut self) {
        match self.history_index {
            None => {}
            Some(i) if i + 1 < self.history.len() => {
                self.history_index = Some(i + 1);
                self.input = Input::new(self.history[i + 1].clone());
            }
            Some(_) => {
                self.history_index = None;
                self.input.reset();
            }
        }
    }

    fn run(&mut self) {
        // 執行中拒收新命令——`rx` 本身就是「執行中」旗標。
        if self.rx.is_some() {
            return;
        }
        let value = self.input.value().trim().to_string();
        if value.is_empty() {
            return;
        }

        // 輸出面板從無到有才要清：不論等下執行成功或參數展開失敗顯示錯誤，
        // before_area（含圖表）都會被壓縮，不清會留殘影（跟 `app::open_shell`
        // 第一次壓縮同一個道理）；面板已經開著時版面不變，不必再清一次。
        if self.output_lines.is_empty() {
            self.request_graph_clear();
        }

        let params = self.commit.as_ref().map(|commit| {
            external_command_parameters(&[], commit, &self.refs, self.area_width, self.area_height)
        });
        let expanded = match expand_shell_command_line(
            &value,
            params.as_ref(),
            self.area_width,
            self.area_height,
        ) {
            Ok(s) => s,
            Err(err) => {
                self.output_lines = vec![Line::styled(
                    err,
                    Style::default().fg(self.ctx.color_theme.status_error_fg),
                )];
                self.output_pane_state = OutputPaneState::default();
                return;
            }
        };

        self.input.reset();
        self.history.push(value);
        self.history_index = None;

        let (result_tx, result_rx) = mpsc::channel();
        self.rx = Some(result_rx);

        let repo_path = self.repo_path.clone();
        let tab_width = self.ctx.core_config.user_command.tab_width;
        let shell = self.ctx.shell_command.clone();
        let child = self.child.clone();
        let app_tx = self.tx.clone();
        thread::spawn(move || {
            let output =
                exec_shell_command(&expanded, &repo_path, &shell, &child).unwrap_or_else(|e| e);
            let mut lines =
                ansi_output_to_lines(output, tab_width).unwrap_or_else(|e| vec![Line::raw(e)]);
            if lines.len() > MAX_OUTPUT_LINES {
                lines.truncate(MAX_OUTPUT_LINES);
                lines.push(Line::raw(format!(
                    "… 輸出過長，已截斷在 {MAX_OUTPUT_LINES} 行"
                )));
            }
            // ShellView 若已經被關掉／換掉，這裡的 rx 早就 drop 了，
            // send 會回 Err——靜靜丟棄過期結果就是正確行為。
            let _ = result_tx.send(lines);
            app_tx.send(AppEvent::ShellOutputReady);
        });
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect, marquee_frame: u64) {
        let running = self.rx.is_some();
        let show_output = running || !self.output_lines.is_empty();
        let output_height = if show_output {
            output_pane_height(area.height)
        } else {
            0
        };

        // Length(2)：一列 TOP 邊框當分隔線 + 一列輸入內容，跟 `status_line`
        // 自己那一列（app.rs 的 `[Min(0), Length(2)]`）同一個形狀。
        let [before_area, input_area, output_area] = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(2),
            Constraint::Length(output_height),
        ])
        .areas(area);

        self.before.render(f, before_area, marquee_frame);
        self.render_input(f, input_area, running);

        if show_output {
            let output_pane = OutputPane::new(&self.output_lines, self.ctx.clone());
            f.render_stateful_widget(output_pane, output_area, &mut self.output_pane_state);
        }
    }

    fn render_input(&mut self, f: &mut Frame, area: Rect, running: bool) {
        const PROMPT: &str = "$ ";
        let block = Block::default()
            .borders(Borders::TOP)
            .style(Style::default().fg(self.ctx.color_theme.divider_fg))
            .padding(Padding::horizontal(1));
        let inner = block.inner(area);

        let inner_width = inner.width.saturating_sub(PROMPT.len() as u16).max(1) as usize;
        // `visual_scroll` 回傳的是顯示寬度（tui-input 用 UnicodeWidthChar 累加），
        // 不是 byte offset——直接拿去切 `&value()[scroll..]` 在中文輸入下會切在
        // char 中間、當場 panic。`split_at_width` 就是用同一套寬度計算找 char
        // 邊界，兩者的 unit 對得上。
        let scroll = self.input.visual_scroll(inner_width);
        let (_, visible) = split_at_width(self.input.value(), scroll);

        let mut spans = vec![Span::raw(PROMPT), Span::raw(visible)];
        if running {
            spans.push(Span::styled(
                "  running…",
                Style::default().fg(self.ctx.color_theme.status_input_transient_fg),
            ));
        }

        let paragraph = Paragraph::new(Line::from(spans))
            .style(Style::default().fg(self.ctx.color_theme.fg))
            .block(block);
        f.render_widget(paragraph, area);

        if !running {
            let x = inner.x + PROMPT.len() as u16 + (self.input.visual_cursor() - scroll) as u16;
            let y = inner.y;
            match &self.ctx.ui_config.cursor_type {
                CursorType::Native => f.set_cursor_position((x, y)),
                CursorType::Virtual(cursor) => {
                    let style = Style::default().fg(self.ctx.color_theme.virtual_cursor_fg);
                    f.buffer_mut().set_string(x, y, cursor, style);
                }
            }
        }
    }
}

impl Drop for ShellView<'_> {
    /// 使用者按 Esc 關閉（`before` 被 `take_before_view` 取出後，`self.view`
    /// 被換成別的 view，這個 `Box<ShellView>` 就掉了）或整個 App 結束時都
    /// 會經過這裡——還在跑的指令（`git push` 卡認證、寫錯的無窮迴圈）不會
    /// 變成孤兒程序繼續握著 pipe。
    fn drop(&mut self) {
        if let Ok(mut guard) = self.child.lock() {
            if let Some(child) = guard.as_mut() {
                let _ = child.kill();
            }
        }
    }
}

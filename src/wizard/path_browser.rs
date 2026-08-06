use std::path::{Path, PathBuf};

use ratatui::{
    crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind},
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::Line,
    widgets::{ListItem, ListState, Paragraph},
    DefaultTerminal, Frame,
};

use crate::color::ColorTheme;

/// 把使用者給的路徑字串換成瀏覽器起點：`canonicalize` 不成（路徑不存在、
/// 權限不足）就退回目前工作目錄，不能拿原始字串本身當起點 —— 那樣
/// `read_dir` 一樣會失敗，清單空、`←` 也回不去，使用者會卡住出不來。
pub(crate) fn start_dir(raw: &str) -> PathBuf {
    Path::new(raw)
        .canonicalize()
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default())
}

pub(crate) struct DirEntryInfo {
    pub name: String,
    pub is_git_repo: bool,
}

/// 純函式，只碰檔案系統不碰 terminal —— 唯一能用 tempfile 直接單元測試的部分。
/// 只列子目錄（不列一般檔案，因為終究是選 git repo 路徑），過濾隱藏目錄（`.` 開頭），
/// 依名稱排序（大小寫不敏感）。`is_git_repo` 用 `.join(".git").exists()`，不是
/// `is_dir()`：worktree／submodule 的 `.git` 是檔案不是目錄。
/// 讀不到目錄（權限不足、目錄消失）時回傳空清單，不 panic 不報錯 —— 呼叫端把空清單
/// 當成「這裡沒有子目錄」處理即可，使用者還是能按 `←` 離開。
pub(crate) fn list_subdirs(dir: &Path) -> Vec<DirEntryInfo> {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<DirEntryInfo> = read_dir
        .filter_map(Result::ok)
        .filter_map(|entry| {
            // 名稱檢查是純字串比對，排在 `is_dir()`（syscall）前面先擋掉隱藏
            // 目錄；`entry.path()` 只算一次，不是每個判斷式各配一個 PathBuf。
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                return None;
            }
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }
            Some(DirEntryInfo {
                is_git_repo: path.join(".git").exists(),
                name,
            })
        })
        .collect();
    entries.sort_by_key(|entry| entry.name.to_lowercase());
    entries
}

pub(crate) enum Flow {
    Continue,
    /// Esc／Ctrl-C：整個放棄，不是「回上一層」。
    Abort,
    /// Enter：確認「目前所在目錄」（不是游標選中的子目錄）。
    Select,
}

pub(crate) struct BrowserState {
    current_dir: PathBuf,
    entries: Vec<DirEntryInfo>,
    list: ListState,
}

impl BrowserState {
    pub fn new(start_dir: PathBuf) -> Self {
        let mut state = Self {
            current_dir: start_dir,
            entries: Vec::new(),
            list: ListState::default(),
        };
        state.reload();
        state
    }

    pub fn current_dir(&self) -> &Path {
        &self.current_dir
    }

    fn reload(&mut self) {
        self.entries = list_subdirs(&self.current_dir);
        self.list.select(if self.entries.is_empty() {
            None
        } else {
            Some(0)
        });
    }

    fn move_selection(&mut self, delta: i32) {
        if self.entries.is_empty() {
            return;
        }
        let current = self.list.selected().unwrap_or(0) as i32;
        let next = (current + delta).clamp(0, self.entries.len() as i32 - 1);
        self.list.select(Some(next as usize));
    }

    fn enter_parent(&mut self) {
        if let Some(parent) = self.current_dir.parent() {
            self.current_dir = parent.to_path_buf();
            self.reload();
        }
    }

    fn enter_selected(&mut self) {
        let Some(entry) = self.list.selected().and_then(|i| self.entries.get(i)) else {
            return;
        };
        self.current_dir = self.current_dir.join(&entry.name);
        self.reload();
    }

    /// 零 I/O 的純狀態轉移，可以直接餵 `KeyEvent` 測試，不需要真的跑 terminal。
    pub fn on_key(&mut self, key: KeyEvent) -> Flow {
        if key.kind != KeyEventKind::Press {
            return Flow::Continue;
        }
        if super::is_abort_key(&key) {
            return Flow::Abort;
        }
        match key.code {
            KeyCode::Esc => Flow::Abort,
            KeyCode::Enter => Flow::Select,
            KeyCode::Up => {
                self.move_selection(-1);
                Flow::Continue
            }
            KeyCode::Down => {
                self.move_selection(1);
                Flow::Continue
            }
            KeyCode::Left => {
                self.enter_parent();
                Flow::Continue
            }
            KeyCode::Right => {
                self.enter_selected();
                Flow::Continue
            }
            _ => Flow::Continue,
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect, theme: &ColorTheme) {
        let [hint_area, path_area, list_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .areas(area);

        // 第一列固定操作說明 —— 跟本專案其餘對話框把提示放在底部的慣例相反，
        // 這是目錄瀏覽器特別要求的。
        let hint = crate::widget::build_hint_line(
            theme,
            &[
                ("↑↓", "移動"),
                ("←", "上層目錄"),
                ("→", "進入子目錄"),
                ("Enter", "選取目前目錄"),
                ("Esc", "取消"),
            ],
        );
        f.render_widget(Paragraph::new(hint), hint_area);

        f.render_widget(
            Paragraph::new(Line::raw(self.current_dir.to_string_lossy().into_owned()))
                .style(Style::default().fg(theme.fg)),
            path_area,
        );

        let items: Vec<ListItem> = if self.entries.is_empty() {
            vec![ListItem::new("（沒有子目錄）")
                .style(Style::default().fg(theme.status_input_transient_fg))]
        } else {
            self.entries
                .iter()
                .map(|entry| {
                    // `●` 標示這個子目錄本身是 git repo，方便使用者不用點進去看就知道。
                    let marker = if entry.is_git_repo { "● " } else { "  " };
                    ListItem::new(format!("{marker}{}/", entry.name))
                })
                .collect()
        };

        f.render_stateful_widget(super::styled_list(items, theme), list_area, &mut self.list);
    }
}

/// 接收「已經在跑」的 terminal（wizard 內呼叫沿用同一個，`-p` 獨立呼叫時外面自己
/// `ratatui::init()`/`ratatui::restore()`）。
pub(crate) fn run(
    terminal: &mut DefaultTerminal,
    start_dir: &Path,
    theme: &ColorTheme,
) -> crate::Result<Option<PathBuf>> {
    let mut state = BrowserState::new(start_dir.to_path_buf());
    loop {
        terminal.draw(|f| state.render(f, f.area(), theme))?;
        let Event::Key(key) = ratatui::crossterm::event::read()? else {
            continue;
        };
        match state.on_key(key) {
            Flow::Continue => {}
            Flow::Abort => return Ok(None),
            Flow::Select => return Ok(Some(state.current_dir().to_path_buf())),
        }
    }
}

/// `-p` 的獨立入口：自帶 init/restore。`restore()` 一定要在 `?` 傳播錯誤之前
/// 執行 —— 否則 `run()` 內的 `terminal.draw()`/`event::read()` 一旦回 `Err`，
/// 終端機會卡在 alt screen + raw mode 出不來，`panic` hook 救不了 `Err` return
/// 這條路徑。
pub(crate) fn run_standalone(raw_path: &str, theme: &ColorTheme) -> crate::Result<Option<PathBuf>> {
    let mut terminal = ratatui::init();
    let outcome = run(&mut terminal, &start_dir(raw_path), theme);
    ratatui::restore();
    outcome
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::KeyModifiers;

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl_key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    /// 建一棵固定形狀的暫存目錄樹，供本檔案所有測試共用：
    /// root/
    ///   ├── alpha/          （一般子目錄，沒有子目錄）
    ///   ├── beta/.git/      （beta 本身是 git repo，.git 是目錄）
    ///   ├── gamma/.git      （gamma 也是 git repo，但 .git 是「檔案」——
    ///   │                     worktree／submodule 的真實形狀）
    ///   ├── .hidden/        （隱藏目錄，應被濾掉）
    ///   └── a_file.txt      （一般檔案，應被濾掉）
    fn build_tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("alpha")).unwrap();
        std::fs::create_dir_all(dir.path().join("beta/.git")).unwrap();
        std::fs::create_dir(dir.path().join("gamma")).unwrap();
        std::fs::write(dir.path().join("gamma/.git"), "gitdir: ../real/.git").unwrap();
        std::fs::create_dir(dir.path().join(".hidden")).unwrap();
        std::fs::write(dir.path().join("a_file.txt"), "").unwrap();
        dir
    }

    #[test]
    fn list_subdirs_filters_hidden_and_files_marks_git_repos() {
        let dir = build_tree();
        let entries = list_subdirs(dir.path());
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["alpha", "beta", "gamma"],
            "隱藏目錄與檔案應被濾掉，依名稱排序"
        );
        assert!(!entries[0].is_git_repo, "alpha 不是 git repo");
        assert!(
            entries[1].is_git_repo,
            "beta 底下 .git 是目錄，應標記為 git repo"
        );
        assert!(
            entries[2].is_git_repo,
            "gamma 底下 .git 是檔案（worktree 形狀），一樣要標記為 git repo —— \
             這條測的正是 `is_git_repo` 用 `.exists()` 不是 `.is_dir()` 的理由"
        );
    }

    #[test]
    fn list_subdirs_on_missing_dir_returns_empty_not_panic() {
        let entries = list_subdirs(Path::new("/definitely/does/not/exist"));
        assert!(entries.is_empty());
    }

    #[test]
    fn right_enters_subdir_left_returns_to_parent() {
        let dir = build_tree();
        let mut state = BrowserState::new(dir.path().to_path_buf());
        assert_eq!(state.current_dir(), dir.path());

        // 游標預設停在第 0 項（alpha），→ 進去。
        assert!(matches!(state.on_key(key(KeyCode::Right)), Flow::Continue));
        assert_eq!(state.current_dir(), dir.path().join("alpha"));

        // alpha 底下沒有子目錄，← 回到 root。
        assert!(matches!(state.on_key(key(KeyCode::Left)), Flow::Continue));
        assert_eq!(state.current_dir(), dir.path());
    }

    #[test]
    fn left_at_filesystem_root_is_a_no_op() {
        // 用 tempdir 的父鏈一路往上走到底，確認不會 panic。
        let dir = build_tree();
        let mut state = BrowserState::new(dir.path().to_path_buf());
        for _ in 0..64 {
            state.on_key(key(KeyCode::Left));
        }
        let root = state.current_dir().to_path_buf();
        assert!(root.is_dir(), "不能退化成一個不存在的空路徑");

        // 真正的「no-op」是「再按一次，位置不變」——不是單看 parent() 是不是
        // None（那個對空字串路徑一樣成立，測不出退化成空路徑的 bug）。
        state.on_key(key(KeyCode::Left));
        assert_eq!(state.current_dir(), root, "已在根目錄，← 不再改變位置");
    }

    #[test]
    fn move_selection_clamps_at_both_ends() {
        let dir = build_tree();
        let mut state = BrowserState::new(dir.path().to_path_buf());
        state.on_key(key(KeyCode::Up)); // 已經在第 0 項，不會變成負數
        assert_eq!(state.list.selected(), Some(0));

        let last = state.entries.len() - 1;
        for _ in 0..last + 5 {
            state.on_key(key(KeyCode::Down));
        }
        assert_eq!(
            state.list.selected(),
            Some(last),
            "已經是最後一項，不會再往下"
        );
    }

    /// `move_selection` 開頭的空清單守衛是唯一擋住 panic 的東西——拿掉的話
    /// `(0 + delta).clamp(0, -1)` 會直接 panic。`alpha` 底下沒有子目錄，
    /// 進去之後刻意連按方向鍵，確認不會 panic、也不會偷偷移動位置。
    #[test]
    fn movement_in_an_empty_dir_does_not_panic_or_move() {
        let dir = build_tree();
        let mut state = BrowserState::new(dir.path().join("alpha"));
        assert_eq!(state.list.selected(), None);

        state.on_key(key(KeyCode::Down));
        state.on_key(key(KeyCode::Up));
        state.on_key(key(KeyCode::Right)); // 沒有選中項目可以進，不該動
        assert_eq!(state.list.selected(), None);
        assert_eq!(state.current_dir(), dir.path().join("alpha"));
    }

    #[test]
    fn enter_confirms_current_dir_not_highlighted_entry() {
        let dir = build_tree();
        let mut state = BrowserState::new(dir.path().to_path_buf());
        state.on_key(key(KeyCode::Down)); // 游標移到 beta，但還沒進去
        assert!(matches!(state.on_key(key(KeyCode::Enter)), Flow::Select));
        assert_eq!(
            state.current_dir(),
            dir.path(),
            "Enter 確認的是目前所在目錄（root），不是游標選中的 beta"
        );
    }

    #[test]
    fn esc_and_ctrl_c_both_abort() {
        let dir = build_tree();
        let mut state = BrowserState::new(dir.path().to_path_buf());
        assert!(matches!(state.on_key(key(KeyCode::Esc)), Flow::Abort));
        assert!(matches!(state.on_key(ctrl_key('c')), Flow::Abort));
        assert!(matches!(state.on_key(ctrl_key('d')), Flow::Abort));
    }

    #[test]
    fn plain_c_is_not_mistaken_for_ctrl_c() {
        let dir = build_tree();
        let mut state = BrowserState::new(dir.path().to_path_buf());
        assert!(matches!(
            state.on_key(key(KeyCode::Char('c'))),
            Flow::Continue
        ));
        assert_eq!(
            state.current_dir(),
            dir.path(),
            "沒有輸入框可以吃這個字元，位置也不該變"
        );
    }
}

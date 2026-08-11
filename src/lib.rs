pub mod color;
pub mod config;
pub mod git;
mod github;
pub mod graph;

mod app;
mod diff;
mod emoji;
mod event;
mod external;
mod fuzzy;
mod keybind;
mod update;
mod view;
mod widget;
mod wizard;

use std::{
    collections::VecDeque,
    io::{IsTerminal, Write},
    path::Path,
    rc::Rc,
};

use app::{App, Ret};
use clap::{CommandFactory, Parser, ValueEnum};
use graph::Graph;
use rustc_hash::FxHashSet;
use serde::Deserialize;
use update::{AutoRestart, UpdateMode};

/// ysgit - Git Graph in Terminal
#[derive(Parser)]
// `disable_help_flag`／`disable_version_flag`：關掉 clap 自動產生的英文 -h／-V，
// 改用下面手動定義的欄位取代，讓說明文字可以換成中文。
#[command(
    name = "ysgit",
    version,
    disable_help_flag = true,
    disable_version_flag = true
)]
struct Args {
    /// git 倉庫路徑 [default: current directory]
    // `hide_default_value`：上面的 doc comment 已經用文字寫出預設值了，
    // 若讓 clap 再自動附加 `[default: .]` 就會重複顯示兩次。
    #[arg(default_value = ".", hide_default_value = true)]
    path: String,

    /// 以互動式目錄瀏覽器選擇 [PATH]（類似 ranger；可搭配上面的路徑引數指定起始目錄）
    #[arg(short = 'p', long)]
    path_browser: bool,

    /// 要渲染的最大 commit 數量
    #[arg(short = 'n', long, value_name = "NUMBER")]
    max_count: Option<usize>,

    /// Commit 排序演算法 [default: chrono]
    #[arg(short, long, value_name = "TYPE")]
    order: Option<CommitOrderType>,

    /// Commit 圖形格子寬度 [default: auto]
    #[arg(short, long, value_name = "TYPE")]
    graph_width: Option<GraphWidthType>,

    /// 緊湊模式：commit 文字貼齊該列 graph 實際畫到的最右邊，不保留固定留白 [default: auto]
    #[arg(short = 'c', long, value_name = "TYPE")]
    compact: Option<CompactType>,

    /// Commit 圖形邊線風格 [default: rounded]
    #[arg(short = 's', long, value_name = "TYPE")]
    graph_style: Option<GraphStyle>,

    /// 初始選取的 commit [default: latest]
    #[arg(short, long, value_name = "TYPE")]
    initial_selection: Option<InitialSelection>,

    /// 自動更新檢查模式 [default: check]
    #[arg(long, value_name = "MODE")]
    update_mode: Option<UpdateMode>,

    /// 自動更新的檢查間隔，單位小時 [default: 6]
    #[arg(
        long,
        value_name = "HOURS",
        value_parser = clap::value_parser!(u64).range(
            update::MIN_INTERVAL_HOURS..=update::MAX_INTERVAL_HOURS
        )
    )]
    update_interval: Option<u64>,

    /// 更新完成後自動重啟（TUI）／開啟新版（CLI），不再詢問 [default: off]
    #[arg(long, value_name = "TYPE")]
    auto_restart: Option<AutoRestart>,

    /// 顯示說明
    #[arg(short = 'h', long, action = clap::ArgAction::Help)]
    help: Option<bool>,

    /// 顯示版本
    #[arg(short = 'V', long, action = clap::ArgAction::Version)]
    version: Option<bool>,

    /// 檢查 GitHub Release 並更新執行檔本身
    #[arg(short = 'U', long)]
    update: bool,
}

impl Args {
    /// 自我更新完成後，重啟時要帶的參數：序列化「命令列上實際給的值」（不是
    /// 與 config 檔合併後的結果——重啟後照樣會重新讀 config）。
    ///
    /// 故意不含 `-U`（重啟不該再更新一次）、`-p`（TUI 離開重啟這條路徑上，
    /// 選到的路徑已經在 `run()` 裡寫回 `path`，帶 path 就夠，也不會再彈一次
    /// 目錄瀏覽器；`-U` 這條路徑則是還沒跑到 `-p` 的處理就 early return 了，
    /// `-p` 本來就不會生效，跟這裡排除它是兩回事）；`help`／`version` 不必
    /// 特判——`ArgAction::Help`／`Version` 會中止 parse，成功 parse 出的
    /// `Args` 裡兩者恆為 `None`。三個更新設定跟 `order`／`compact` 那些
    /// 畫圖旋鈕同一個道理：命令列上明確給的值要保留，不是特例。
    ///
    /// 用解構式綁定：日後 `Args` 加欄位而忘了在這裡處理，編譯直接失敗——手寫的
    /// round-trip 測試擋不住這種漏。
    pub(crate) fn to_argv(&self) -> Vec<String> {
        let Args {
            path,
            path_browser: _,
            max_count,
            order,
            graph_width,
            compact,
            graph_style,
            initial_selection,
            update_mode,
            update_interval,
            auto_restart,
            help: _,
            version: _,
            update: _,
        } = self;

        let mut argv = vec!["ysgit".to_string()];
        for (flag, value) in [
            ("-n", max_count.map(|n| n.to_string())),
            ("-o", order.as_ref().map(wizard::variant_name)),
            ("-g", graph_width.as_ref().map(wizard::variant_name)),
            ("-c", compact.as_ref().map(wizard::variant_name)),
            ("-s", graph_style.as_ref().map(wizard::variant_name)),
            ("-i", initial_selection.as_ref().map(wizard::variant_name)),
            (
                "--update-mode",
                update_mode.as_ref().map(wizard::variant_name),
            ),
            ("--update-interval", update_interval.map(|h| h.to_string())),
            (
                "--auto-restart",
                auto_restart.as_ref().map(wizard::variant_name),
            ),
        ] {
            if let Some(value) = value {
                argv.push(flag.to_string());
                argv.push(value);
            }
        }
        if path != "." {
            // path 以 `-` 開頭時不加 `--` 會被當成旗標解析。
            if path.starts_with('-') {
                argv.push("--".to_string());
            }
            argv.push(path.clone());
        }
        argv
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommitOrderType {
    Chrono,
    Topo,
}

impl From<Option<CommitOrderType>> for git::SortCommit {
    fn from(order: Option<CommitOrderType>) -> Self {
        match order {
            Some(CommitOrderType::Chrono) => git::SortCommit::Chronological,
            Some(CommitOrderType::Topo) => git::SortCommit::Topological,
            None => git::SortCommit::Chronological,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GraphWidthType {
    Auto,
    Double,
    Single,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompactType {
    Auto,
    On,
    Off,
}

pub use graph::GraphStyle;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InitialSelection {
    Latest,
    Head,
}

impl From<Option<InitialSelection>> for app::InitialSelection {
    fn from(selection: Option<InitialSelection>) -> Self {
        match selection {
            Some(InitialSelection::Latest) => app::InitialSelection::Latest,
            Some(InitialSelection::Head) => app::InitialSelection::Head,
            None => app::InitialSelection::Latest,
        }
    }
}

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// 從所有本地 ref 開始執行 BFS，找出只能從遠端分支到達的 commit。
pub fn find_remote_only_commits(
    repository: &git::Repository,
    full_graph: &Graph,
) -> FxHashSet<git::CommitHash> {
    let all_hashes: FxHashSet<git::CommitHash> = full_graph.commit_hashes.iter().cloned().collect();

    // 收集 BFS 起始點：帶有本地 ref（Branch、Tag、Stash）的 commit + HEAD
    let mut seeds: Vec<git::CommitHash> = Vec::new();
    for (commit_hash, refs) in repository.refs_with_commits() {
        if !all_hashes.contains(commit_hash) {
            continue;
        }
        let has_local_ref = refs.iter().any(|r| {
            matches!(
                r,
                git::Ref::Branch { .. } | git::Ref::Tag { .. } | git::Ref::Stash { .. }
            )
        });
        if has_local_ref {
            seeds.push(commit_hash.clone());
        }
    }

    // 同時加入 HEAD 指向的目標
    if let git::Head::Detached { target } = repository.head() {
        if all_hashes.contains(target) {
            seeds.push(target.clone());
        }
    }

    // 從起始點開始執行 BFS，沿著 parent 連結走訪
    let mut reachable: FxHashSet<git::CommitHash> = FxHashSet::default();
    let mut queue: VecDeque<git::CommitHash> = VecDeque::new();
    for seed in seeds {
        if reachable.insert(seed.clone()) {
            queue.push_back(seed);
        }
    }
    while let Some(hash) = queue.pop_front() {
        for parent in repository.parents_hash(&hash) {
            if all_hashes.contains(parent) && reachable.insert(parent.clone()) {
                queue.push_back(parent.clone());
            }
        }
    }

    // Remote-only = 在圖形中但無法從本地 ref 到達
    all_hashes
        .into_iter()
        .filter(|h| !reachable.contains(h))
        .collect()
}

fn compute_filtered_graph_from(
    repository: &git::Repository,
    full_graph: &Graph,
    remote_only: &FxHashSet<git::CommitHash>,
) -> Option<Rc<Graph>> {
    if remote_only.is_empty() {
        return None;
    }

    let visible_hashes: FxHashSet<git::CommitHash> = full_graph
        .commit_hashes
        .iter()
        .filter(|h| !remote_only.contains(h))
        .cloned()
        .collect();

    let head = resolve_head_commit_hash(repository);
    Some(Rc::new(graph::calc_graph_filtered(
        repository,
        &visible_hashes,
        head.as_ref(),
        head_has_named_ref(repository),
    )))
}

fn build_graph_artifacts(
    repository: &git::Repository,
    graph: &Rc<Graph>,
) -> (Option<Rc<Graph>>, FxHashSet<git::CommitHash>) {
    let remote_only = find_remote_only_commits(repository, graph);
    let filtered = compute_filtered_graph_from(repository, graph, &remote_only);
    (filtered, remote_only)
}

/// 快速路徑輔助函式：若 ref 的變化導致 commit 在 local-reachable 與
/// remote-only 之間轉移，就重建過濾後的圖形。
/// 回傳是否真的發生了重建（若有，呼叫端會清空畫面）。
fn try_refresh_filtered_for_ref_change(
    repository: &git::Repository,
    graph: &Graph,
    remote_only_commits: &mut FxHashSet<git::CommitHash>,
    filtered_graph: &mut Option<Rc<Graph>>,
) -> bool {
    let new_remote_only = find_remote_only_commits(repository, graph);
    if &new_remote_only == remote_only_commits {
        return false;
    }
    *filtered_graph = compute_filtered_graph_from(repository, graph, &new_remote_only);
    *remote_only_commits = new_remote_only;
    true
}

/// 當 HEAD 指向帶有本地 branch 或 tag ref 的 commit 時回傳 true。
/// 用來決定圖形佈局中是否保留 col 0 — 沒有任何 ref 的 detached HEAD
/// 不該把其他分支擠出 col 0。
///
/// 這裡刻意排除 `RemoteBranch`：若 detached HEAD 停在只有遠端 ref 的
/// commit 上，使用者並沒有對應的本地把手可用，因此不把它視為值得保護的錨點。
fn head_has_named_ref(repository: &git::Repository) -> bool {
    match repository.head() {
        git::Head::Branch { .. } => true,
        git::Head::Detached { target } => repository
            .refs(target)
            .iter()
            .any(|r| matches!(r, git::Ref::Branch { .. } | git::Ref::Tag { .. })),
        git::Head::None => false,
    }
}

fn resolve_head_commit_hash(repository: &git::Repository) -> Option<git::CommitHash> {
    match repository.head() {
        git::Head::Branch { name } => {
            for (commit_hash, refs) in repository.refs_with_commits() {
                if refs
                    .iter()
                    .any(|r| matches!(r, git::Ref::Branch { name: n, .. } if n == name))
                {
                    return Some(commit_hash.clone());
                }
            }
            None
        }
        git::Head::Detached { target } => Some(target.clone()),
        git::Head::None => None,
    }
}

pub fn run() -> Result<()> {
    // ratatui::init() 裝的 panic hook 只還原 alt screen + raw mode，
    // 不會清 mouse capture — 先補一層 DisableMouseCapture。
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = ratatui::crossterm::execute!(
            std::io::stdout(),
            ratatui::crossterm::event::DisableMouseCapture
        );
        prev_hook(info);
    }));

    // 首次啟動生成預設設定檔。要排在 `Args::try_parse()` 之前——`-h` 會進
    // 互動式精靈，精靈要讀到這份剛生成的檔才能顯示真正的目前值。寫失敗
    // 不影響這裡的控制流：`ensure_config_file()` 自己吞掉錯誤只印一句，
    // 不回傳 `Result`，因為它不是任何後續步驟的必要條件。
    config::ensure_config_file();

    // 釘住這個 process 啟動時的執行檔身分（路徑＋inode），供
    // `update::exe_is_stale()` 判斷磁碟上的執行檔是否已被別的實例換掉。
    // 要排在這裡、`run_self_update()`（`-U`）的早退之前——那條路徑跟這裡
    // 共用同一個 `OnceLock`，晚一步初始化的話 `-U` 全程沒有快照，守衛
    // 靜默失效。
    update::snapshot_exe();

    // `-h`／`--help` 在真人終端機下改成互動選單，非 TTY（管線、CI）維持原本
    // `Args::parse()` 印靜態文字＋exit(0) 的行為。刻意不碰 Args 的 help/version
    // 欄位（維持 clap 原生的 ArgAction::Help/Version）：這是唯一能保證非 TTY 輸出、
    // 錯誤提示（"try '--help'"）、`-h` 後接無效值時的中止時機都跟改動前逐字不變
    // 的做法 —— `Args::parse()` 本來就是 `try_parse().unwrap_or_else(|e| e.exit())`，
    // 這裡只是在那個 unwrap 之前多插一個分支。
    let mut args = match Args::try_parse() {
        Ok(a) => a,
        Err(e)
            if e.kind() == clap::error::ErrorKind::DisplayHelp
                && std::io::stdout().is_terminal() =>
        {
            let Some(a) = wizard::run()? else {
                return Ok(()); // Esc／Ctrl-C：放棄，跟原本 --help 一樣 exit(0)
            };
            a
        }
        // `-V` 在真人終端機下順便接印完整選項清單——`-h` 被精靈接管後，CLI 上
        // 原本就看不到這份靜態說明。非 TTY 維持原本只印一行版本號：
        // `update::verify_binary` 就是拿 `.output()`（天然非 TTY）跑
        // `<新binary> --version`，版本不符時整段 stdout 會被塞進單行狀態列，
        // 若這裡不分 TTY 一律接印，那段錯誤訊息會變成幾十行灌進去。
        // `$(ysgit --version)` 這類 script 慣例也因此不受影響。
        Err(e)
            if e.kind() == clap::error::ErrorKind::DisplayVersion
                && std::io::stdout().is_terminal() =>
        {
            let mut cmd = Args::command();
            print!("{}", cmd.render_version());
            println!();
            cmd.print_help()?;
            return Ok(());
        }
        Err(e) => e.exit(),
    };

    // 不進 TUI、不開 repository——config 壞掉也不該擋住這條路徑（使用者
    // 可能就是為了拿掉會修掉那個壞 config 的新版才跑 -U）。
    if args.update {
        return run_self_update(&args);
    }

    let (core_config, ui_config, color_theme, keybind_patch) = config::load()?;
    let keybind = keybind::KeyBind::new(keybind_patch);

    let max_count = args.max_count.or(core_config.option.max_count);
    let order = args.order.or(core_config.option.order).into();
    let graph_width = args.graph_width.or(core_config.option.graph_width);
    let compact = args.compact.or(core_config.option.compact);
    let graph_style = args
        .graph_style
        .or(core_config.option.graph_style)
        .unwrap_or_default();
    let initial_selection = args
        .initial_selection
        .or(core_config.option.initial_selection)
        .into();
    let update_settings = update::resolve(
        update::UpdateOverrides {
            mode: args.update_mode,
            interval_hours: args.update_interval,
            auto_restart: args.auto_restart,
        },
        update::UpdateOverrides {
            mode: core_config.update.mode,
            interval_hours: core_config.update.interval_hours,
            auto_restart: core_config.update.auto_restart,
        },
    );

    let graph_color_set = color::GraphColorSet::new(&color_theme.graph);

    // -p：互動式目錄瀏覽器選 path。要在 EventController::init()（下面）之前跑，
    // 否則會有兩邊搶讀 stdin 的問題。Esc 只是放棄挑選，`args.path` 維持原樣
    // （預設 `.`）照樣往下啟動 —— 跟 -h wizard 的 Esc（整個放棄，等同原本
    // --help 離開）語意不同。
    if args.path_browser {
        if let Some(p) = wizard::path_browser::run_standalone(&args.path, &color_theme)? {
            args.path = p.to_string_lossy().into_owned();
        }
    }

    let ctx = Rc::new(app::AppContext {
        keybind,
        core_config,
        ui_config,
        color_theme,
        graph_style,
        graph_width,
        compact,
        update: update_settings,
    });

    let mut ec = event::EventController::init();
    // 這一輪迴圈每次 `Ret::Refresh` 都會重建 `App` 並重跑到這裡——檢查要
    // spawn 在迴圈外，只在整個 process 生命週期跑一次；放進 `App::run()`
    // 開頭的話，建 tag、刪 branch、checkout 這些觸發 refresh 的操作都會
    // 意外多 spawn 一次背景檢查。
    update::spawn_check(&ec, false, update_settings);
    // 排第一次週期檢查——`mode = Off` 就不排：這條鏈之後只會在
    // `AppEvent::PeriodicUpdateCheck` 的處理裡自我重新武裝，起點只有這裡
    // 一個，跟上面 `spawn_check` 一樣只在整個 process 生命週期跑一次。
    if update_settings.mode != UpdateMode::Off {
        ec.sender().send_after(
            event::AppEvent::PeriodicUpdateCheck,
            update_settings.interval,
        );
    }
    // 清掃上次啟動可能留下的自我更新殘檔——跟這次的 `mode` 無關（上次啟動
    // 沒關閉時留下的殘檔，不能因為這次關了就不清），背景執行不擋啟動。
    std::thread::spawn(update::cleanup_stale_temp_files);
    let mut refresh_view_context = None;
    let mut terminal = None;

    // 在 repo 根目錄啟動檔案監控，以便自動重新整理
    let repo_root = Path::new(&args.path)
        .canonicalize()
        .unwrap_or_else(|_| Path::new(&args.path).to_path_buf());
    if git::is_inside_work_tree(&repo_root) {
        ec.start_git_watcher(&repo_root);
    }

    let mut repository = git::Repository::load(Path::new(&args.path), order, max_count)?;
    let mut graph = Rc::new(graph::calc_graph(
        &repository,
        resolve_head_commit_hash(&repository).as_ref(),
        head_has_named_ref(&repository),
    ));
    let (mut filtered_graph, mut remote_only_commits) = build_graph_artifacts(&repository, &graph);

    let ret = loop {
        if terminal.is_none() {
            terminal = Some(ratatui::init());
            ratatui::crossterm::execute!(
                std::io::stdout(),
                ratatui::crossterm::event::EnableMouseCapture
            )
            .ok();
        }

        let mut app = App::new(
            &repository,
            &graph,
            filtered_graph,
            remote_only_commits,
            &graph_color_set,
            initial_selection,
            ctx.clone(),
            &ec,
            refresh_view_context.take(),
        );

        match app.run(terminal.as_mut().unwrap()) {
            Ok(Ret::Quit(restart)) => {
                break Ok(restart);
            }
            Ok(Ret::Refresh(request)) => {
                refresh_view_context = Some(request.context);

                let new_repo = git::Repository::load(Path::new(&args.path), order, max_count)?;

                let old_head = resolve_head_commit_hash(&repository);
                let new_head = resolve_head_commit_hash(&new_repo);
                let layout_inputs_same = old_head == new_head;
                if repository.same_commits(&new_repo) && layout_inputs_same {
                    // 快速路徑：commit 沒有變化 — 重用現有圖形，
                    // 讓畫面在 watcher 觸發的重新整理時不會閃爍。
                    // App 必須先釋放對 &repository 的借用，才能進行修改。
                    (filtered_graph, remote_only_commits) = app.into_parts();
                    repository.update_metadata_from(new_repo);
                    // 這裡不需要更新 head_commit_hash：App::new 會重新
                    // 從 `repository` 計算，而 update_metadata_from
                    // 剛把它更新到最新狀態（複製了 ref_map/head/working_changes）。

                    let filtered_changed = try_refresh_filtered_for_ref_change(
                        &repository,
                        &graph,
                        &mut remote_only_commits,
                        &mut filtered_graph,
                    );
                    if filtered_changed {
                        if let Some(t) = terminal.as_mut() {
                            t.clear()?;
                        }
                    }
                } else {
                    // 慢速路徑：commit 有變化 — 釋放 app、重建圖形，
                    // 並清空畫面區域以準備繪製新的一幀。
                    drop(app);
                    repository = new_repo;
                    graph = Rc::new(graph::calc_graph(
                        &repository,
                        resolve_head_commit_hash(&repository).as_ref(),
                        head_has_named_ref(&repository),
                    ));
                    (filtered_graph, remote_only_commits) =
                        build_graph_artifacts(&repository, &graph);

                    if let Some(t) = terminal.as_mut() {
                        t.clear()?;
                    }
                }

                continue;
            }
            Err(e) => {
                break Err(e);
            }
        }
    };

    ratatui::crossterm::execute!(
        std::io::stdout(),
        ratatui::crossterm::event::DisableMouseCapture
    )
    .ok();
    ratatui::restore();

    // exec 必須排在 DisableMouseCapture／ratatui::restore() 之後——新 process
    // 接手的終端機這時才是還原乾淨的，不然會接手一個還在 alt screen ＋
    // raw mode 的終端機。
    if let Some(exe) = ret? {
        if let Err(e) = update::exec_replacing_self(&exe, &args.to_argv()) {
            // 執行檔已經換好了，只是沒能自動重啟——印出來讓使用者知道要
            // 手動重開，不能回頭再跑一次 -U（新 process 已經是新版，
            // current_exe_checked 也不會再讓它跑）。
            eprintln!("{e}");
        }
    }
    Ok(())
}

/// 讀一行 y/n。空行（直接按 Enter）／`y`／`yes`（不分大小寫）算「是」，跟
/// TUI `yes_no_answer`（`src/app/status_line.rs`，`KeyCode::Enter` 映射到
/// `Answer::Confirm`）的 Enter=Confirm 一致；其餘（含 EOF）算「否」，
/// EOF（Ctrl-D）對齊 TUI 的 Esc。
fn confirm(prompt: &str) -> bool {
    print!("{prompt} ");
    let _ = std::io::stdout().flush();
    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).unwrap_or(0) == 0 {
        return false; // EOF：等同 Ctrl-D 取消
    }
    let ans = input.trim();
    ans.is_empty() || ans.eq_ignore_ascii_case("y") || ans.eq_ignore_ascii_case("yes")
}

/// `-U`／`--update`：查到新版才問（顯示「目前版本 → 最新版本」），更新完再問
/// 一次是否要就地開啟新版；繞過節流但一律標記已檢查——跟 `update::spawn_check`
/// 對「兩種情況都算檢查過一次」的認定一致。
///
/// 兩個問題都只在**互動環境**下問，非互動（cron／script／`ysgit -U >> log`）
/// 維持「查到就直接更新、不重啟、印訊息」，不會在無人值守的地方卡住等輸入。
/// 互動與否要 `stdin`／`stdout` **都**是 TTY 才算數，只看其中一個都不夠：
/// `stdout` 不是 TTY（`ysgit -U | tee log`）問題會卡在 pipe 的 block-buffer
/// 裡、使用者當下看不到；`stdin` 不是 TTY（`ysgit -U < /dev/null`）則問了也
/// 讀不到答案，`confirm()` 立刻收到 EOF，誤判成「否」。兩個都是 TTY，才能
/// 同時保證「問得出來、讀得到答案、真的要 exec 進 TUI 時新 process 也有活的
/// stdin」。
///
/// `settings.mode == Auto` 跳過第一問、`settings.auto_restart` 跳過第二問——
/// 兩個短路只取代對應的 `confirm()`，`interactive` 與
/// `git::is_inside_work_tree` 兩道守衛不受影響：前者是「有沒有活的 stdin／
/// stdout」，後者是「開下去會不會直接報錯」，跟使用者要不要被問是兩件事。
fn run_self_update(args: &Args) -> Result<()> {
    // 這條路徑排在 `run()` 的 `config::load()?` 之前（config 壞掉也不該擋住
    // `-U`——使用者可能就是為了拿掉會修掉那個壞 config 的新版才跑 -U），
    // 所以這裡要自己載入，且失敗不能靜默：直接用 `.ok()` 吞掉的話，
    // `update_mode = auto` 沒生效卻什麼都不會顯示，使用者無從得知。
    let core_update = match config::load() {
        Ok((core, ..)) => core.update,
        Err(e) => {
            eprintln!("設定檔載入失敗（{e}），本次 -U 使用預設更新設定");
            config::CoreConfig::default().update
        }
    };
    let settings = update::resolve(
        update::UpdateOverrides {
            mode: args.update_mode,
            interval_hours: args.update_interval,
            auto_restart: args.auto_restart,
        },
        update::UpdateOverrides {
            mode: core_update.mode,
            interval_hours: core_update.interval_hours,
            auto_restart: core_update.auto_restart,
        },
    );

    let latest = update::check_for_update()?;
    update::mark_checked();

    let Some(tag) = latest else {
        println!("Already up to date (v{})", env!("CARGO_PKG_VERSION"));
        return Ok(());
    };

    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    if interactive
        && settings.mode != UpdateMode::Auto
        && !confirm(&format!(
            "v{} → {tag}  Update? [Y/n]",
            env!("CARGO_PKG_VERSION")
        ))
    {
        println!("Update cancelled.");
        return Ok(());
    }

    println!("Updating to {tag}...");
    let exe = update::download_and_replace(&tag)?;

    // `git::is_inside_work_tree` 內部是 `Command::current_dir`，吃相對路徑就
    // 跟目前這個 process 的 cwd 一致，不需要先 canonicalize。不在 repo 裡就不
    // 問要不要開啟——`cd /tmp && ysgit -U` 這種呼叫，開下去只會看到一句
    // git 錯誤，使用者會誤以為更新失敗了。放在 `&&` 短路鏈最後一個，非互動
    // 環境（`interactive` 為 false）不會白 spawn 這個 git 子行程。
    if interactive
        && git::is_inside_work_tree(Path::new(&args.path))
        && (settings.auto_restart || confirm("Launch ysgit now? [Y/n]"))
    {
        if let Err(e) = update::exec_replacing_self(&exe, &args.to_argv()) {
            println!("Updated to {tag}, but restart failed: {e}");
            println!("Restart ysgit to use the new version.");
        }
        // exec 成功時 process image 已被換掉，不會執行到這裡。
    } else {
        println!("Updated to {tag}. Restart ysgit to use the new version.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_browser_flag_is_parsed() {
        let args = Args::try_parse_from(["ysgit", "-p"]).unwrap();
        assert!(args.path_browser);
        assert_eq!(args.path, ".");

        let args = Args::try_parse_from(["ysgit", "-p", "~/projects"]).unwrap();
        assert!(args.path_browser);
        assert_eq!(args.path, "~/projects");
    }

    #[test]
    fn removed_text_flag_is_rejected() {
        assert!(Args::try_parse_from(["ysgit", "-t"]).is_err());
    }

    #[test]
    fn to_argv_round_trips_non_default_fields() {
        let args = Args::try_parse_from([
            "ysgit",
            "-n",
            "50",
            "-o",
            "topo",
            "-g",
            "single",
            "-c",
            "on",
            "-s",
            "ascii",
            "-i",
            "head",
            "--update-mode",
            "auto",
            "--update-interval",
            "12",
            "--auto-restart",
            "on",
            "/some/repo",
        ])
        .unwrap();

        let argv = args.to_argv();
        let reparsed = Args::try_parse_from(&argv).unwrap();

        assert_eq!(reparsed.max_count, args.max_count);
        assert_eq!(reparsed.order, args.order);
        assert_eq!(reparsed.graph_width, args.graph_width);
        assert_eq!(reparsed.compact, args.compact);
        assert_eq!(reparsed.graph_style, args.graph_style);
        assert_eq!(reparsed.initial_selection, args.initial_selection);
        assert_eq!(reparsed.update_mode, args.update_mode);
        assert_eq!(reparsed.update_interval, args.update_interval);
        assert_eq!(reparsed.auto_restart, args.auto_restart);
        assert_eq!(reparsed.path, args.path);
    }

    #[test]
    fn to_argv_omits_update_and_path_browser() {
        let args = Args::try_parse_from(["ysgit", "-U", "-p"]).unwrap();
        let argv = args.to_argv();
        assert!(!argv.iter().any(|a| a == "-U" || a == "-p"));
    }

    #[test]
    fn to_argv_escapes_a_path_that_looks_like_a_flag() {
        let mut args = Args::try_parse_from(["ysgit"]).unwrap();
        args.path = "-foo".to_string();

        let argv = args.to_argv();
        assert_eq!(argv, vec!["ysgit", "--", "-foo"]);
        // 沒有這個 `--`，"-foo" 重新 parse 時會被當成不明旗標，
        // 而不是還原成 args.path。
        assert_eq!(Args::try_parse_from(&argv).unwrap().path, "-foo");
    }

    #[test]
    fn to_argv_of_defaults_is_just_the_program_name() {
        let args = Args::try_parse_from(["ysgit"]).unwrap();
        assert_eq!(args.to_argv(), vec!["ysgit"]);
    }

    #[test]
    fn update_interval_rejects_zero_and_values_above_max() {
        assert!(Args::try_parse_from(["ysgit", "--update-interval", "0"]).is_err());
        assert!(Args::try_parse_from(["ysgit", "--update-interval", "49"]).is_err());
    }

    #[test]
    fn update_interval_accepts_the_full_range() {
        assert!(Args::try_parse_from(["ysgit", "--update-interval", "1"]).is_ok());
        assert!(Args::try_parse_from(["ysgit", "--update-interval", "48"]).is_ok());
    }
}

pub mod color;
pub mod config;
pub mod git;
mod github;
pub mod graph;

mod app;
mod check;
mod emoji;
mod event;
mod external;
mod fuzzy;
mod keybind;
mod view;
mod widget;

use std::{collections::VecDeque, path::Path, rc::Rc};

use app::{App, Ret};
use clap::{Parser, ValueEnum};
use graph::Graph;
use rustc_hash::FxHashSet;
use serde::Deserialize;

/// ysgit - 在你的終端機中呈現豐富的 git commit 圖，宛如魔法 📚
#[derive(Parser)]
#[command(name = "ysgit", version)]
struct Args {
    /// git 倉庫路徑 [default: current directory]
    // `hide_default_value`：上面的 doc comment 已經用文字寫出預設值了，
    // 若讓 clap 再自動附加 `[default: .]` 就會重複顯示兩次。
    #[arg(default_value = ".", hide_default_value = true)]
    path: String,

    /// 要渲染的最大 commit 數量
    #[arg(short = 'n', long, value_name = "NUMBER")]
    max_count: Option<usize>,

    /// Commit 排序演算法 [default: chrono]
    #[arg(short, long, value_name = "TYPE")]
    order: Option<CommitOrderType>,

    /// Commit 圖形格子寬度 [default: auto]
    #[arg(short, long, value_name = "TYPE")]
    graph_width: Option<GraphWidthType>,

    /// Commit 圖形邊線風格 [default: rounded]
    #[arg(short = 's', long, value_name = "TYPE")]
    graph_style: Option<GraphStyle>,

    /// 初始選取的 commit [default: latest]
    #[arg(short, long, value_name = "TYPE")]
    initial_selection: Option<InitialSelection>,
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

    let args = Args::parse();
    let (core_config, ui_config, graph_config, color_theme, keybind_patch) = config::load()?;
    let keybind = keybind::KeyBind::new(keybind_patch);

    let max_count = args.max_count;
    let order = args.order.or(core_config.option.order).into();
    let graph_width = args.graph_width.or(core_config.option.graph_width);
    let graph_style = args
        .graph_style
        .or(core_config.option.graph_style)
        .unwrap_or_default();
    let initial_selection = args
        .initial_selection
        .or(core_config.option.initial_selection)
        .into();

    let graph_color_set = color::GraphColorSet::new(&graph_config.color);

    let ctx = Rc::new(app::AppContext {
        keybind,
        core_config,
        ui_config,
        color_theme,
        graph_style,
    });

    let mut ec = event::EventController::init();
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
    let mut cell_width_type = check::decide_cell_width_type(&graph, graph_width)?;
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
            cell_width_type,
            initial_selection,
            ctx.clone(),
            &ec,
            refresh_view_context.take(),
        );

        match app.run(terminal.as_mut().unwrap()) {
            Ok(Ret::Quit) => {
                break Ok(());
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
                    cell_width_type = check::decide_cell_width_type(&graph, graph_width)?;
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
    ret.map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removed_protocol_flag_is_rejected() {
        assert!(Args::try_parse_from(["ysgit", "-p", "kitty"]).is_err());
    }

    #[test]
    fn removed_text_flag_is_rejected() {
        assert!(Args::try_parse_from(["ysgit", "-t"]).is_err());
    }
}

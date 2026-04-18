pub mod color;
pub mod config;
pub mod git;
mod github;
pub mod graph;
pub mod protocol;

mod app;
mod check;
mod event;
mod external;
mod fuzzy;
mod keybind;
mod view;
mod widget;

use std::{collections::VecDeque, path::Path, rc::Rc};

use app::{App, Ret};
use clap::{Parser, ValueEnum};
use graph::{Graph, GraphImageManager};
use rustc_hash::FxHashSet;
use serde::Deserialize;

/// Serie - A rich git commit graph in your terminal, like magic 📚
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Path to git repository [default: current directory]
    #[arg(default_value = ".")]
    path: String,

    /// Maximum number of commits to render
    #[arg(short = 'n', long, value_name = "NUMBER")]
    max_count: Option<usize>,

    /// Image protocol to render graph [default: auto]
    #[arg(short, long, value_name = "TYPE")]
    protocol: Option<ImageProtocolType>,

    /// Commit ordering algorithm [default: chrono]
    #[arg(short, long, value_name = "TYPE")]
    order: Option<CommitOrderType>,

    /// Commit graph image cell width [default: auto]
    #[arg(short, long, value_name = "TYPE")]
    graph_width: Option<GraphWidthType>,

    /// Commit graph image edge style [default: rounded]
    #[arg(short = 's', long, value_name = "TYPE")]
    graph_style: Option<GraphStyle>,

    /// Initial selection of commit [default: latest]
    #[arg(short, long, value_name = "TYPE")]
    initial_selection: Option<InitialSelection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageProtocolType {
    Auto,
    Iterm,
    Kitty,
}

impl From<Option<ImageProtocolType>> for protocol::ImageProtocol {
    fn from(protocol: Option<ImageProtocolType>) -> Self {
        match protocol {
            Some(ImageProtocolType::Auto) => protocol::auto_detect(),
            Some(ImageProtocolType::Iterm) => protocol::ImageProtocol::Iterm2,
            Some(ImageProtocolType::Kitty) => protocol::ImageProtocol::Kitty,
            None => protocol::auto_detect(),
        }
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
pub enum GraphStyle {
    Rounded,
    Angular,
}

impl From<Option<GraphStyle>> for graph::GraphStyle {
    fn from(style: Option<GraphStyle>) -> Self {
        match style {
            Some(GraphStyle::Rounded) => graph::GraphStyle::Rounded,
            Some(GraphStyle::Angular) => graph::GraphStyle::Angular,
            None => graph::GraphStyle::Rounded,
        }
    }
}

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

pub struct FilteredGraphData {
    pub graph: Rc<Graph>,
    pub image_manager: GraphImageManager,
    pub cell_width: u16,
}

/// BFS from all local refs to find commits reachable only from remote branches.
pub fn find_remote_only_commits(
    repository: &git::Repository,
    full_graph: &Graph,
) -> FxHashSet<git::CommitHash> {
    let all_hashes: FxHashSet<git::CommitHash> = full_graph.commit_hashes.iter().cloned().collect();

    // Collect BFS seeds: commits with local refs (Branch, Tag, Stash) + HEAD
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

    // Also add HEAD target
    if let git::Head::Detached { target } = repository.head() {
        if all_hashes.contains(target) {
            seeds.push(target.clone());
        }
    }

    // BFS from seeds, walking parent links
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

    // Remote-only = in graph but not reachable from local refs
    all_hashes
        .into_iter()
        .filter(|h| !reachable.contains(h))
        .collect()
}

/// Rendering parameters shared by every graph/image builder.
#[derive(Clone, Copy)]
pub struct GraphRenderCtx<'a> {
    pub color_set: &'a color::GraphColorSet,
    pub cell_width_type: graph::CellWidthType,
    pub image_protocol: protocol::ImageProtocol,
    pub graph_style: graph::GraphStyle,
    pub selected_bg_color: image::Rgba<u8>,
}

pub fn compute_filtered_graph_from(
    repository: &git::Repository,
    full_graph: &Graph,
    remote_only: FxHashSet<git::CommitHash>,
    ctx: GraphRenderCtx<'_>,
    head_commit_hash: Option<git::CommitHash>,
) -> (Option<FilteredGraphData>, FxHashSet<git::CommitHash>) {
    if remote_only.is_empty() {
        return (None, remote_only);
    }

    let visible_hashes: FxHashSet<git::CommitHash> = full_graph
        .commit_hashes
        .iter()
        .filter(|h| !remote_only.contains(h))
        .cloned()
        .collect();

    let head = resolve_head_commit_hash(repository);
    let filtered = Rc::new(graph::calc_graph_filtered(
        repository,
        &visible_hashes,
        head.as_ref(),
    ));

    let cell_width = match ctx.cell_width_type {
        graph::CellWidthType::Double => (filtered.max_pos_x + 1) as u16 * 2,
        graph::CellWidthType::Single => (filtered.max_pos_x + 1) as u16,
    };

    let image_manager = GraphImageManager::new(
        Rc::clone(&filtered),
        ctx.color_set,
        ctx.cell_width_type,
        ctx.graph_style,
        ctx.image_protocol,
        head_commit_hash,
        ctx.selected_bg_color,
    );

    (
        Some(FilteredGraphData {
            graph: filtered,
            image_manager,
            cell_width,
        }),
        remote_only,
    )
}

fn ratatui_color_to_rgba(color: ratatui::style::Color) -> image::Rgba<u8> {
    match color::ratatui_color_to_rgb(color) {
        ratatui::style::Color::Rgb(r, g, b) => image::Rgba([r, g, b, 255]),
        _ => image::Rgba([0, 0, 0, 255]),
    }
}

fn build_graph_artifacts(
    repository: &git::Repository,
    graph: &Rc<Graph>,
    ctx: GraphRenderCtx<'_>,
) -> (
    GraphImageManager,
    Option<FilteredGraphData>,
    FxHashSet<git::CommitHash>,
) {
    let head_commit_hash = resolve_head_commit_hash(repository);
    let image_manager = GraphImageManager::new(
        Rc::clone(graph),
        ctx.color_set,
        ctx.cell_width_type,
        ctx.graph_style,
        ctx.image_protocol,
        head_commit_hash.clone(),
        ctx.selected_bg_color,
    );
    let remote_only = find_remote_only_commits(repository, graph);
    let (filtered, remote_only) =
        compute_filtered_graph_from(repository, graph, remote_only, ctx, head_commit_hash);
    (image_manager, filtered, remote_only)
}

/// Fast-path helper: if the refs changed in a way that shifts commits between
/// local-reachable and remote-only, rebuild the filtered graph + image manager.
/// Returns whether a rebuild actually happened (caller clears image if so).
fn try_refresh_filtered_for_ref_change(
    repository: &git::Repository,
    graph: &Graph,
    remote_only_commits: &mut FxHashSet<git::CommitHash>,
    filtered_graph: &mut Option<FilteredGraphData>,
    ctx: GraphRenderCtx<'_>,
    head_commit_hash: Option<git::CommitHash>,
) -> bool {
    let new_remote_only = find_remote_only_commits(repository, graph);
    if &new_remote_only == remote_only_commits {
        return false;
    }
    let (rebuilt_filtered, rebuilt_remote_only) =
        compute_filtered_graph_from(repository, graph, new_remote_only, ctx, head_commit_hash);
    *filtered_graph = rebuilt_filtered;
    *remote_only_commits = rebuilt_remote_only;
    true
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
    let image_protocol = args.protocol.or(core_config.option.protocol).into();
    let order = args.order.or(core_config.option.order).into();
    let graph_width = args.graph_width.or(core_config.option.graph_width);
    let graph_style = args.graph_style.or(core_config.option.graph_style).into();
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
        image_protocol,
    });

    let mut ec = event::EventController::init();
    let mut refresh_view_context = None;
    let mut terminal = None;

    // Start file watcher on repo root for auto-refresh
    let repo_root = Path::new(&args.path)
        .canonicalize()
        .unwrap_or_else(|_| Path::new(&args.path).to_path_buf());
    if git::is_inside_work_tree(&repo_root) {
        ec.start_git_watcher(&repo_root);
    }

    let selected_bg_color = ratatui_color_to_rgba(ctx.color_theme.list_selected_bg);

    let mut repository = git::Repository::load(Path::new(&args.path), order, max_count)?;
    let mut graph = Rc::new(graph::calc_graph(
        &repository,
        resolve_head_commit_hash(&repository).as_ref(),
    ));
    let mut cell_width_type = check::decide_cell_width_type(&graph, graph_width)?;
    let mut render_ctx = GraphRenderCtx {
        color_set: &graph_color_set,
        cell_width_type,
        image_protocol,
        graph_style,
        selected_bg_color,
    };
    let (mut graph_image_manager, mut filtered_graph, mut remote_only_commits) =
        build_graph_artifacts(&repository, &graph, render_ctx);

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
            graph_image_manager,
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
                    // Fast path: commits unchanged — reuse the existing image
                    // manager so the screen doesn't flicker on watcher refresh.
                    // App must release its &repository borrow before mutation.
                    (graph_image_manager, filtered_graph, remote_only_commits) = app.into_parts();
                    repository.update_metadata_from(new_repo);
                    graph_image_manager.update_head_commit_hash(new_head.clone());
                    if let Some(filtered) = filtered_graph.as_mut() {
                        filtered
                            .image_manager
                            .update_head_commit_hash(new_head.clone());
                    }

                    let filtered_changed = try_refresh_filtered_for_ref_change(
                        &repository,
                        &graph,
                        &mut remote_only_commits,
                        &mut filtered_graph,
                        render_ctx,
                        new_head,
                    );
                    if filtered_changed {
                        if let Some(t) = terminal.as_mut() {
                            let size = t.size()?;
                            app::clear_image_area(image_protocol, t, 0..size.height)?;
                        }
                    }
                } else {
                    // Slow path: commits changed — drop app, rebuild graph + image,
                    // and clear the on-screen image area for the new frame.
                    drop(app);
                    repository = new_repo;
                    graph = Rc::new(graph::calc_graph(
                        &repository,
                        resolve_head_commit_hash(&repository).as_ref(),
                    ));
                    cell_width_type = check::decide_cell_width_type(&graph, graph_width)?;
                    render_ctx.cell_width_type = cell_width_type;
                    (graph_image_manager, filtered_graph, remote_only_commits) =
                        build_graph_artifacts(&repository, &graph, render_ctx);

                    if let Some(t) = terminal.as_mut() {
                        let size = t.size()?;
                        app::clear_image_area(image_protocol, t, 0..size.height)?;
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

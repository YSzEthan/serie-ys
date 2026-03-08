pub mod color;
pub mod config;
pub mod git;
pub mod graph;
pub mod protocol;

mod app;
mod check;
mod event;
mod external;
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

    /// Preload all graph images
    #[arg(long, default_value = "false")]
    preload: bool,
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

pub struct FilteredGraphData<'a> {
    pub graph: Rc<Graph<'a>>,
    pub image_manager: GraphImageManager<'a>,
    pub cell_width: u16,
}

/// BFS from all local refs to find commits reachable only from remote branches.
pub fn find_remote_only_commits(
    repository: &git::Repository,
    full_graph: &Graph<'_>,
) -> FxHashSet<git::CommitHash> {
    // Collect all commit hashes in the graph
    let all_hashes: FxHashSet<git::CommitHash> = full_graph
        .commits
        .iter()
        .map(|c| c.commit_hash.clone())
        .collect();

    // Collect BFS seeds: targets of local refs (Branch, Tag, Stash) + HEAD
    let mut seeds: Vec<git::CommitHash> = Vec::new();
    for commit in &full_graph.commits {
        let refs = repository.refs(&commit.commit_hash);
        for r in &refs {
            match r {
                git::Ref::Branch { .. } | git::Ref::Tag { .. } | git::Ref::Stash { .. } => {
                    seeds.push(commit.commit_hash.clone());
                    break;
                }
                git::Ref::RemoteBranch { .. } => {}
            }
        }
    }

    // Also add HEAD target
    match repository.head() {
        git::Head::Branch { name } => {
            // Find commit for this branch
            for commit in &full_graph.commits {
                let refs = repository.refs(&commit.commit_hash);
                if refs
                    .iter()
                    .any(|r| matches!(r, git::Ref::Branch { name: n, .. } if n == name))
                {
                    seeds.push(commit.commit_hash.clone());
                    break;
                }
            }
        }
        git::Head::Detached { target } => {
            if all_hashes.contains(target) {
                seeds.push(target.clone());
            }
        }
        git::Head::None => {}
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

pub fn compute_filtered_graph<'a>(
    repository: &'a git::Repository,
    full_graph: &Graph<'a>,
    graph_color_set: &color::GraphColorSet,
    cell_width_type: graph::CellWidthType,
    image_protocol: protocol::ImageProtocol,
    preload: bool,
    graph_style: graph::GraphStyle,
) -> (Option<FilteredGraphData<'a>>, FxHashSet<git::CommitHash>) {
    let remote_only = find_remote_only_commits(repository, full_graph);

    if remote_only.is_empty() {
        return (None, remote_only);
    }

    // Build visible hash set: all commits minus remote-only
    let visible_hashes: FxHashSet<git::CommitHash> = full_graph
        .commits
        .iter()
        .map(|c| c.commit_hash.clone())
        .filter(|h| !remote_only.contains(h))
        .collect();

    let filtered = Rc::new(graph::calc_graph_filtered(repository, &visible_hashes));

    let cell_width = match cell_width_type {
        graph::CellWidthType::Double => (filtered.max_pos_x + 1) as u16 * 2,
        graph::CellWidthType::Single => (filtered.max_pos_x + 1) as u16,
    };

    let image_manager = GraphImageManager::new(
        Rc::clone(&filtered),
        graph_color_set,
        cell_width_type,
        graph_style,
        image_protocol,
        preload,
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

pub fn run() -> Result<()> {
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

    let (tx, mut rx) = event::init();
    let mut refresh_view_context = None;
    let mut terminal = None;

    let ret = loop {
        let repository = git::Repository::load(Path::new(&args.path), order, max_count)?;

        let graph = graph::calc_graph(&repository);

        let cell_width_type = check::decide_cell_width_type(&graph, graph_width)?;

        let graph = Rc::new(graph);

        let graph_image_manager = GraphImageManager::new(
            Rc::clone(&graph),
            &graph_color_set,
            cell_width_type,
            graph_style,
            image_protocol,
            args.preload,
        );

        // Compute filtered graph for remote-only commit hiding
        let (filtered_graph, remote_only_commits) = compute_filtered_graph(
            &repository,
            &graph,
            &graph_color_set,
            cell_width_type,
            image_protocol,
            args.preload,
            graph_style,
        );

        if terminal.is_none() {
            terminal = Some(ratatui::init());
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
            tx.clone(),
            refresh_view_context,
        );

        match app.run(terminal.as_mut().unwrap(), rx) {
            Ok(Ret::Quit) => {
                break Ok(());
            }
            Ok(Ret::Refresh(request)) => {
                rx = request.rx;
                refresh_view_context = Some(request.context);
                continue;
            }
            Err(e) => {
                break Err(e);
            }
        }
    };

    ratatui::restore();
    ret.map_err(Into::into)
}

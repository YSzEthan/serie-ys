//! 效能量測工具，見 #116 與 `CONTRIBUTING.md`「效能量測」一節。
//!
//! ```sh
//! cargo run --release --example perf -- <repo> [-o topo] [-n N]
//! ```
//!
//! 照 `ysgit::run()` 啟動時的順序量各階段：`load` → `calc_graph` →
//! `remote_only` → `filtered` → `stats`（統計兩張圖，不算進前面任何
//! 階段）→ `drop`（釋放全部資料，對照 Phase 6「舊資料丟背景釋放」）。
//! 只呼叫公開的 `git`、`graph` 模組，以及三個為了這支工具而開放的 crate
//! root helper（`resolve_head_commit_hash`、`head_has_named_ref`、
//! `compute_filtered_graph_from`）——它們就是 app 啟動時實際呼叫的那幾個
//! 函式，Phase 1 把 remote-only 改成 bitset 時，這裡會跟著改，量到的
//! 永遠是目前這一版演算法的數字。
//!
//! 每個階段印出耗時、RSS（`ps`，使用者實際感受到的記憶體）、以及計數
//! allocator 量到的目前 heap 用量與該階段內的峰值。RSS 在系統記憶體吃緊
//! 時會失真——linux 1.48M commit 的 `calc_graph` 曾經在 swap 狂跳時印出
//! RSS 2 GB，實際 edge 資料超過 32 GB；heap 計數不受這個影響。

use std::{
    alloc::{GlobalAlloc, Layout, System},
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicIsize, Ordering},
    time::Instant,
};

use clap::Parser;
use ysgit::{
    compute_filtered_graph_from, find_remote_only_commits,
    git::{Repository, SortCommit},
    graph::{self, Graph},
    head_has_named_ref, resolve_head_commit_hash, CommitOrderType,
};

// ---------------------------------------------------------------------
// 計數 allocator：包住 System，記錄目前配置的 bytes 與階段內峰值。
// 四個方法都原樣轉交給 System——預設的 `realloc` 會變成 alloc+copy+dealloc，
// `alloc_zeroed` 會自己 memset，兩者都會讓行為偏離真正的 app（大型 Vec
// 就地成長、calloc 拿零頁的好處都會消失），量出來的數字就不準了。
// ---------------------------------------------------------------------

static LIVE: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicIsize = AtomicIsize::new(0);

/// 只在有淨變化時更新 LIVE；PEAK 先用 load 比較，真的超過才 fetch_max，
/// 省掉多數呼叫（配置密集時，絕大部分不會刷新峰值）裡的一次額外 RMW。
fn track(delta: isize) {
    let now = LIVE.fetch_add(delta, Ordering::Relaxed) + delta;
    if now > PEAK.load(Ordering::Relaxed) {
        PEAK.fetch_max(now, Ordering::Relaxed);
    }
}

struct CountingAlloc;

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: 原樣轉交給 System；只在配置成功（非 null）時累計位元組數。
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            track(layout.size() as isize);
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: 同上；歸零交給 System 自己做，不在這裡重複一次。
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            track(layout.size() as isize);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: 呼叫端保證 ptr／layout 是這個 allocator 先前配置出來的。
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: 原樣轉交，讓 System 就地成長或 remap；成功時依新舊大小的
        // 差值調整，失敗（回傳 null）時原本的配置維持不變，不動計數。
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            track(new_size as isize - layout.size() as isize);
        }
        new_ptr
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

// ---------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------

/// 量測 ysgit 啟動路徑各階段的耗時與記憶體，見 CONTRIBUTING.md「效能量測」。
#[derive(Parser)]
struct Args {
    /// 要量測的 repo 路徑
    repo: PathBuf,

    /// commit 排序方式 [default: chrono]
    #[arg(short, long, value_name = "TYPE")]
    order: Option<CommitOrderType>,

    /// 對應 app 的 --max-count
    #[arg(short = 'n', long = "max-count", value_name = "NUMBER")]
    max_count: Option<usize>,
}

// ---------------------------------------------------------------------
// 階段計時／記憶體
// ---------------------------------------------------------------------

struct Stage {
    name: &'static str,
    start: Instant,
}

impl Stage {
    fn begin(name: &'static str) -> Self {
        PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
        Self {
            name,
            start: Instant::now(),
        }
    }

    fn end(self) {
        let elapsed = self.start.elapsed().as_secs_f64();
        let rss = rss_mib();
        let heap = mib(LIVE.load(Ordering::Relaxed) as f64);
        let peak = mib(PEAK.load(Ordering::Relaxed) as f64);
        let name = self.name;
        println!("{name:<12} {elapsed:>7.2} s {rss:>9} {heap:>9.1} {peak:>9.1}");
    }
}

fn print_start_row() {
    let name = "start";
    let time = "-";
    let rss = rss_mib();
    let heap = mib(LIVE.load(Ordering::Relaxed) as f64);
    let peak = "-";
    println!("{name:<12} {time:>9} {rss:>9} {heap:>9.1} {peak:>9}");
}

/// 跑一個外部指令，成功時回傳 trim 過的 stdout；指令跑不起來或非 0 結束都回 `None`。
/// `rss_mib`／`git_version`／`describe_self` 共用同一段「跑指令、取 stdout」的樣板。
fn stdout_of(cmd: &mut Command) -> Option<String> {
    let output = cmd.output().ok().filter(|o| o.status.success())?;
    String::from_utf8(output.stdout)
        .ok()
        .map(|s| s.trim().to_string())
}

fn rss_mib() -> String {
    let pid = std::process::id().to_string();
    let kib = stdout_of(Command::new("ps").args(["-o", "rss=", "-p", &pid]))
        .and_then(|s| s.parse::<f64>().ok());
    match kib {
        Some(kib) => format!("{:.1}", kib / 1024.0),
        None => "n/a".to_string(),
    }
}

fn mib(bytes: f64) -> f64 {
    bytes / (1024.0 * 1024.0)
}

/// 千分位分隔。用 `rchunks(3)` 從右往左分組再反轉順序，不寫 `x % 3 == 0`
/// ——那個寫法會撞上 stable clippy 的 `manual_is_multiple_of`。
fn thousands(n: usize) -> String {
    let s = n.to_string();
    s.as_bytes()
        .rchunks(3)
        .rev()
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect::<Vec<_>>()
        .join(",")
}

// ---------------------------------------------------------------------
// repo／graph 統計
// ---------------------------------------------------------------------

struct RepoStats {
    commits: usize,
    merges: usize,
    refs: usize,
    subject_bytes: usize,
    body_bytes: usize,
    head: String,
}

fn repo_stats(repo: &Repository) -> RepoStats {
    let commits = repo.all_commits();
    let head = match repo.head() {
        ysgit::git::Head::Branch { name } => {
            let hash = resolve_head_commit_hash(repo).map(|h| h.as_str().to_string());
            format!("{name} {}", hash.as_deref().unwrap_or("?"))
        }
        ysgit::git::Head::Detached { target } => format!("detached {}", target.as_str()),
        ysgit::git::Head::None => "none".to_string(),
    };
    RepoStats {
        commits: commits.len(),
        merges: commits
            .iter()
            .filter(|c| c.parent_commit_hashes.len() >= 2)
            .count(),
        refs: repo.all_refs().len(),
        subject_bytes: commits.iter().map(|c| c.subject.len()).sum(),
        body_bytes: commits.iter().map(|c| c.body.len()).sum(),
        head,
    }
}

struct GraphStats {
    rows: usize,
    cells: usize,
    edges: usize,
    width_p50: usize,
    width_p99: usize,
    width_max: usize,
}

/// 每一列的寬度是「該列 edge 最大的 pos_x」與「該列 commit 的欄位」取
/// 較大者再 +1——有 parent 或 child 的 commit，自己那一列、那一欄一定有
/// edge，所以取 commit 欄位不會改變任何一列的值，只補上孤立 commit
/// （沒有 parent 也沒有 child）那一列，順便讓取 max 的集合永遠非空。
fn graph_stats(graph: &Graph) -> GraphStats {
    let mut widths: Vec<usize> = (0..graph.row_count())
        .map(|y| {
            let commit_col = graph.col(y);
            let edge_col = graph
                .row_edges(y)
                .iter()
                .map(|e| e.pos_x)
                .max()
                .unwrap_or(0);
            commit_col.max(edge_col) + 1
        })
        .collect();
    let edges = (0..graph.row_count())
        .map(|y| graph.row_edges(y).len())
        .sum();
    let (width_p50, width_p99, width_max) = percentiles(&mut widths);
    GraphStats {
        rows: graph.row_count(),
        cells: graph.cell_count(),
        edges,
        width_p50,
        width_p99,
        width_max,
    }
}

/// 百分位數寫死成這兩個索引，跟 lane 原型與舊 bench 同一個公式：
/// 跟舊基準（`max_pos_x`／每列寬）比對時要注意這裡一律 +1。
fn percentiles(widths: &mut [usize]) -> (usize, usize, usize) {
    widths.sort_unstable();
    let n = widths.len();
    if n == 0 {
        return (0, 0, 0);
    }
    (widths[n / 2], widths[n * 99 / 100], widths[n - 1])
}

fn print_graph_line(label: &str, stats: &GraphStats) {
    let rows = thousands(stats.rows);
    let cells = stats.cells;
    let edges = thousands(stats.edges);
    let p50 = stats.width_p50;
    let p99 = stats.width_p99;
    let max = stats.width_max;
    println!(
        "{label:<8} rows {rows}  cells {cells}  edges {edges}  width p50/p99/max {p50}/{p99}/{max}"
    );
}

// ---------------------------------------------------------------------
// header
// ---------------------------------------------------------------------

fn git_version() -> String {
    stdout_of(Command::new("git").arg("--version"))
        .map(|s| s.trim_start_matches("git version ").to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// 標出這支 binary 是量的哪個版本的原始碼；`-dirty` 代表量的是還沒
/// commit 的工作區。`--tags`：這個 repo 的 release tag 是 lightweight，
/// `git describe` 預設只認 annotated tag，不加會跳過最近的版本號。
fn describe_self() -> String {
    stdout_of(
        Command::new("git")
            .args(["describe", "--tags", "--always", "--dirty"])
            .current_dir(env!("CARGO_MANIFEST_DIR")),
    )
    .unwrap_or_else(|| "unknown".to_string())
}

fn print_header(args: &Args) {
    let pkg_version = env!("CARGO_PKG_VERSION");
    let describe = describe_self();
    let git_ver = git_version();
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    println!("ysgit perf {pkg_version} ({describe})  git {git_ver}  {os}-{arch}");
    println!("repo     {}", args.repo.display());
    let order = match args.order {
        Some(CommitOrderType::Topo) => "topo",
        _ => "chrono",
    };
    let max_count = args
        .max_count
        .map(|n| n.to_string())
        .unwrap_or_else(|| "none".to_string());
    println!("options  order={order}  max-count={max_count}");
    println!();
    println!(
        "{:<12} {:>9} {:>9} {:>9} {:>9}  (MiB)",
        "stage", "time", "rss", "heap", "peak"
    );
}

// ---------------------------------------------------------------------
// main
// ---------------------------------------------------------------------

fn main() {
    let args = Args::parse();
    print_header(&args);
    print_start_row();

    let sort: SortCommit = args.order.into();

    let stage = Stage::begin("load");
    let repo = Repository::load(&args.repo, sort, args.max_count).expect("load 失敗");
    stage.end();
    let repo_summary = repo_stats(&repo);

    let stage = Stage::begin("calc_graph");
    let head = resolve_head_commit_hash(&repo);
    let reserve_head_col = head_has_named_ref(&repo);
    let graph = graph::calc_graph(&repo, head.as_ref(), reserve_head_col);
    stage.end();

    let stage = Stage::begin("remote_only");
    let remote_only = find_remote_only_commits(&repo);
    stage.end();
    let remote_only_count = remote_only.len();

    let filtered = if remote_only.is_empty() {
        println!("filtered     skipped（沒有 remote-only commit）");
        None
    } else {
        let stage = Stage::begin("filtered");
        let filtered = compute_filtered_graph_from(&repo, &remote_only);
        stage.end();
        filtered
    };

    let stage = Stage::begin("stats");
    let full_stats = graph_stats(&graph);
    let filtered_stats = filtered.as_ref().map(|g| graph_stats(g));
    stage.end();

    let stage = Stage::begin("drop");
    drop(filtered);
    drop(remote_only);
    drop(graph);
    drop(repo);
    stage.end();

    println!();
    let commits = thousands(repo_summary.commits);
    let merges = thousands(repo_summary.merges);
    let refs = repo_summary.refs;
    println!("commits  {commits}  merges {merges}  refs {refs}  remote-only {remote_only_count}");
    println!("head     {}", repo_summary.head);
    let subject_bytes = thousands(repo_summary.subject_bytes);
    let body_bytes = thousands(repo_summary.body_bytes);
    println!("text     subject {subject_bytes} bytes  body {body_bytes} bytes");
    print_graph_line("full", &full_stats);
    match &filtered_stats {
        Some(stats) => print_graph_line("filtered", stats),
        None => println!("filtered skipped（沒有 remote-only commit）"),
    }
}

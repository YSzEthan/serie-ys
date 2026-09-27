use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
};

use chrono::{DateTime, FixedOffset};
use clap::ValueEnum;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::Deserialize;

use crate::Result;

const GIT_EMPTY_TREE_HASH: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// 單一檔案 diff 輸出的行數上限。重新產生的 lockfile、minified bundle 等單一檔案
/// 就能撐出數萬行，不設上限會讓 UI 在 ansi 轉換與渲染時整個凍住。
const DIFF_OUTPUT_LINE_LIMIT: usize = 5000;

/// 使用 Arc<str> 以便宜複製並滿足 Send trait（`mpsc::Sender<AppEvent>` 所需）
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommitHash(Arc<str>);

impl CommitHash {
    pub fn as_short_hash(&self) -> &str {
        &self.0[0..7]
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn as_arc(&self) -> Arc<str> {
        self.0.clone()
    }
}

impl Default for CommitHash {
    fn default() -> Self {
        Self(Arc::from(""))
    }
}

impl From<&str> for CommitHash {
    fn from(s: &str) -> Self {
        Self(Arc::from(s))
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub enum CommitType {
    #[default]
    Commit,
    Stash,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Commit {
    pub commit_hash: CommitHash,
    /// `Arc<str>` 而非 `String`：linux 148 萬筆 commit 只有約 4 萬個不同作者，
    /// 載入時經 `intern_author_strings` 去重後，同一個作者的每一筆都共用
    /// 同一份配置。
    pub author_name: Arc<str>,
    pub author_email: Arc<str>,
    pub author_date: DateTime<FixedOffset>,
    pub subject: String,
    pub parent_commit_hashes: Vec<CommitHash>,
    pub commit_type: CommitType,
}

/// `committer_*` 與 `body`：只有 detail view 用得到，常駐在每筆 `Commit`
/// 上在大 repo（linux 148 萬筆）會白吃數百 MB，故延遲到 `commit_detail`
/// 才另外跑一次 `git show` 取得。
#[derive(Debug, Clone, Default)]
pub struct CommitExtra {
    pub committer_name: String,
    pub committer_email: String,
    pub committer_date: DateTime<FixedOffset>,
    pub body: String,
}

impl CommitExtra {
    /// `git show` 失敗時的退路：借用 author 的身分與日期，`body` 留空。
    /// `is_author_committer_different` 因此自然判定「相同」，畫面上就是
    /// 不顯示 Committer 那一行，跟真的拿不到資料時該有的行為一致。
    fn fallback(commit: &Commit) -> Self {
        Self {
            committer_name: commit.author_name.to_string(),
            committer_email: commit.author_email.to_string(),
            committer_date: commit.author_date,
            body: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefType {
    Tag,
    Branch,
    RemoteBranch,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Ref {
    Tag {
        name: String,
        target: CommitHash,
    },
    Branch {
        name: String,
        target: CommitHash,
    },
    RemoteBranch {
        name: String,
        target: CommitHash,
    },
    Stash {
        name: String,
        message: String,
        target: CommitHash,
    },
}

impl Ref {
    pub fn name(&self) -> &str {
        match self {
            Ref::Tag { name, .. } => name,
            Ref::Branch { name, .. } => name,
            Ref::RemoteBranch { name, .. } => name,
            Ref::Stash { name, .. } => name,
        }
    }

    pub fn target(&self) -> &CommitHash {
        match self {
            Ref::Tag { target, .. } => target,
            Ref::Branch { target, .. } => target,
            Ref::RemoteBranch { target, .. } => target,
            Ref::Stash { target, .. } => target,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Head {
    Branch { name: String },
    Detached { target: CommitHash },
    None,
}

#[derive(Debug, Clone, Copy)]
pub enum SortCommit {
    Chronological,
    Topological,
}

impl SortCommit {
    /// `load_commits_single` 與 `rev_list_hashes` 都要決定 commit 順序，
    /// 共用同一個對應，不要各寫一份 `match`。
    fn git_flag(self) -> &'static str {
        match self {
            SortCommit::Chronological => "--date-order",
            SortCommit::Topological => "--topo-order",
        }
    }
}

type CommitIndex = FxHashMap<CommitHash, usize>;

type RefMap = FxHashMap<CommitHash, Vec<Ref>>;

#[derive(Debug)]
pub struct Repository {
    path: PathBuf,
    commits: Vec<Commit>,
    commit_index: CommitIndex,
    /// parent 的 CSR 索引：第 i 個 commit 的 parent 在
    /// `parent_idx[parent_start[i]..parent_start[i + 1]]`，順序同
    /// `parent_commit_hashes`。沒載入的 parent（`max_count` 截斷、stash 的
    /// index／untracked commit）是 `PARENT_NOT_LOADED`。
    parent_start: Vec<u32>,
    parent_idx: Vec<u32>,

    ref_map: RefMap,
    head: Head,

    /// 全體 commit 的 author name 顯示寬度最大值，`commit_list` 的 Name
    /// 欄寬拿這個當上限。載入時算一次——用它必須是全體最大值，不能只看
    /// 目前捲動到的那幾列，否則欄寬會隨捲動跳動。
    name_cell_width: u16,
}

impl Repository {
    /// 不再載入 working changes——那是 `reload::Reloader` 背景執行緒的責任
    /// （只跑 `git status`，不必等一次完整的 `git log`）。`lib.rs` 啟動時
    /// 兩者平行跑，見 `reload::Reloader::spawn`。
    pub fn load(path: &Path, sort: SortCommit, max_count: Option<usize>) -> Result<Self> {
        check_git_repository(path)?;

        let (mut ref_map, head) = load_refs(path);

        let stashes = load_all_stashes(path);
        let commits = load_all_commits(path, sort, &head, &stashes, max_count);
        if commits.is_empty() {
            return Err("no commits in the repository".into());
        }

        let commits = merge_stashes_to_commits(commits, stashes);

        let stash_ref_map = load_stashes_as_refs(path);
        merge_ref_maps(&mut ref_map, stash_ref_map);

        Ok(Self::new(path.to_path_buf(), commits, ref_map, head))
    }

    /// 測試用：跳過 `git log` 等外部呼叫，直接用手造的 `Commit` 建
    /// `Repository`——`commit_index`／parent CSR／`name_cell_width` 這些
    /// 衍生資料照跑，跟 `load()` 走同一條 `new()`。
    #[cfg(test)]
    pub(crate) fn from_commits(commits: Vec<Commit>) -> Self {
        Self::new(PathBuf::new(), commits, RefMap::default(), Head::None)
    }

    fn new(path: PathBuf, mut commits: Vec<Commit>, ref_map: RefMap, head: Head) -> Self {
        let commit_index = build_commit_index(&commits);
        let (parent_start, parent_idx) = build_parent_csr(&mut commits, &commit_index);
        let name_cell_width = commits
            .iter()
            .map(|c| console::measure_text_width(&c.author_name) as u16)
            .max()
            .unwrap_or(0);
        Self {
            path,
            commits,
            commit_index,
            parent_start,
            parent_idx,
            ref_map,
            head,
            name_cell_width,
        }
    }

    pub fn commit(&self, commit_hash: &CommitHash) -> Option<&Commit> {
        self.commit_index
            .get(commit_hash)
            .map(|&i| &self.commits[i])
    }

    pub fn all_commits(&self) -> &[Commit] {
        &self.commits
    }

    /// `commit_hash` 在 `all_commits()` 裡的位置（raw index）。
    pub fn index_of(&self, commit_hash: &CommitHash) -> Option<usize> {
        self.commit_index.get(commit_hash).copied()
    }

    /// 全體 commit 的 author name 顯示寬度最大值，載入時算好，見欄位文件。
    pub fn name_cell_width(&self) -> u16 {
        self.name_cell_width
    }

    /// 第 `raw` 個 commit 有載入的 parent 的 raw index，順序同 `parent_commit_hashes`。
    pub fn loaded_parents(&self, raw: usize) -> impl Iterator<Item = usize> + '_ {
        let range = self.parent_start[raw] as usize..self.parent_start[raw + 1] as usize;
        self.parent_idx[range]
            .iter()
            .filter(|&&p| p != PARENT_NOT_LOADED)
            .map(|&p| p as usize)
    }

    /// raw 空間的 parent CSR：`parent_idx[parent_start[raw]..parent_start[raw + 1]]`
    /// 是第 `raw` 個 commit 的 parent raw index，順序同 `parent_commit_hashes`；
    /// 沒載入的 parent 是 `u32::MAX`。給 `graph::lanes::build` 用，主 graph
    /// 的 row 就是 raw，不需要另外轉換。
    pub fn parent_csr(&self) -> (&[u32], &[u32]) {
        (&self.parent_start, &self.parent_idx)
    }

    /// 這份 repository 的內容指紋：commit hash 序列（hash 已涵蓋 parent 與
    /// 內容）、`head`、每個 ref。Phase 6 背景重載拿它跟 App 手上那份比對——
    /// 相同就代表資料沒變，worker 直接丟掉這次載入結果、不觸發換資料，
    /// 取代舊的 `same_commits` fast path（見 `reload::Reloader`）。
    ///
    /// `ref_map` 是 `HashMap`，迭代順序不固定：每個 entry 各自算一份 hash，
    /// 用 `wrapping_add` 合併——加法跟順序無關，兩次迭代順序不同也不會誤判
    /// 成「變了」。
    pub fn fingerprint(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        for commit in &self.commits {
            commit.commit_hash.hash(&mut hasher);
        }
        self.head.hash(&mut hasher);

        let mut refs_acc: u64 = 0;
        for (hash, refs) in &self.ref_map {
            let mut entry_hasher = DefaultHasher::new();
            hash.hash(&mut entry_hasher);
            refs.hash(&mut entry_hasher);
            refs_acc = refs_acc.wrapping_add(entry_hasher.finish());
        }
        hasher.write_u64(refs_acc);

        hasher.finish()
    }

    pub fn refs(&self, commit_hash: &CommitHash) -> Vec<&Ref> {
        self.ref_map
            .get(commit_hash)
            .map(|refs| refs.iter().collect::<Vec<&Ref>>())
            .unwrap_or_default()
    }

    pub fn all_refs(&self) -> Vec<&Ref> {
        self.ref_map.values().flatten().collect()
    }

    pub fn refs_with_commits(&self) -> impl Iterator<Item = (&CommitHash, &[Ref])> {
        self.ref_map.iter().map(|(k, v)| (k, v.as_slice()))
    }

    pub fn head(&self) -> &Head {
        &self.head
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn commit_detail(
        &self,
        commit_hash: &CommitHash,
    ) -> (&Commit, CommitExtra, Vec<FileChange>) {
        let commit = self.commit(commit_hash).unwrap();
        let extra = load_commit_extra(&self.path, commit_hash)
            .unwrap_or_else(|| CommitExtra::fallback(commit));
        let changes = if commit.parent_commit_hashes.is_empty() {
            get_initial_commit_additions(&self.path, commit_hash)
        } else {
            get_diff_summary(&self.path, commit_hash)
        };
        (commit, extra, changes)
    }

    /// 回傳 commit 與其 refs，不會為了檔案變更而另外啟動 git 子行程。
    pub fn commit_refs(&self, commit_hash: &CommitHash) -> (&Commit, Vec<Ref>) {
        let commit = self.commit(commit_hash).unwrap();
        let refs = self.refs(commit_hash).into_iter().cloned().collect();
        (commit, refs)
    }

    /// 單一檔案的 diff（不含 ANSI 色碼——上色與行號、header 都交給 `crate::diff`
    /// 自己解析）。回傳值第二項是「是否被行數上限截斷」，供呼叫端在 title 上提示；
    /// 不再把這件事編碼成字串尾巴的 `... (truncated)` 讓呼叫端回頭解析。
    /// 初始 commit（無 parent）自動改跟 empty tree 比對，判斷邏輯與 `commit_detail` 同源。
    pub fn file_diff(&self, target: &DiffTarget) -> std::result::Result<(String, bool), String> {
        match target {
            DiffTarget::Commit { hash, path } => {
                let commit = self.commit(hash).unwrap();
                let base = if commit.parent_commit_hashes.is_empty() {
                    GIT_EMPTY_TREE_HASH.to_string()
                } else {
                    format!("{}^", hash.as_str())
                };
                run_diff(&self.path, &[&base, hash.as_str(), "--", path])
            }
            DiffTarget::Staged { path } => run_diff(&self.path, &["--cached", "--", path]),
            DiffTarget::Unstaged { path } => run_diff(&self.path, &["--", path]),
            // untracked 檔案不在 git 追蹤範圍內，`git diff -- <path>` 對它輸出空
            // 字串，必須改用 `--no-index` 跟 `/dev/null` 比較。
            DiffTarget::Untracked { path } => {
                run_diff(&self.path, &["--no-index", "--", "/dev/null", path])
            }
        }
    }
}

/// 一個可自我描述的 diff 目標。每個 variant 對應一種形狀不同的 git 呼叫。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DiffTarget {
    Commit { hash: CommitHash, path: String },
    Staged { path: String },
    Unstaged { path: String },
    Untracked { path: String },
}

impl DiffTarget {
    pub fn path(&self) -> &str {
        match self {
            DiffTarget::Commit { path, .. }
            | DiffTarget::Staged { path }
            | DiffTarget::Unstaged { path }
            | DiffTarget::Untracked { path } => path,
        }
    }
}

/// `args` 決定比較範圍與 pathspec，`Commit`／`Staged`／`Unstaged`／`Untracked`
/// 四種形狀共用同一條指令與 exit code 判斷：一般 diff 沒帶 `--exit-code`
/// 時只會回 0，只有 `--no-index` 會用 1 代表「有差異」（這裡的正常情況），
/// 對所有呼叫方統一接受 `0|1` 是安全的，其餘才是真的錯誤。
fn run_diff(path: &Path, args: &[&str]) -> std::result::Result<(String, bool), String> {
    let output = git_read(path)
        .args([
            "-c",
            "core.quotePath=false",
            // 上色與解析都交給 `crate::diff` 自己做，不再假手 git 的 ANSI 輸出；
            // 同理把 noprefix／mnemonicPrefix 都釘回 git 內建行為，`--no-ext-diff`
            // 停用外部 diff 工具 —— 否則設了這些的使用者，輸出會被我們的 parser
            // 解析成一團垃圾（原本「git 印什麼畫什麼」的做法對他們是無害的）。
            // 注意：停用外部 diff 不能用 `-c diff.external=`——空字串仍然是
            // 「設了一個外部 diff 程式，只是路徑是空的」，git 會嘗試執行它
            // 並失敗（`cannot run : No such file or directory`）；`--no-ext-diff`
            // 才是「不要用外部 diff」。
            "-c",
            "diff.noprefix=false",
            "-c",
            "diff.mnemonicPrefix=false",
            "--no-pager",
            "diff",
            "--no-ext-diff",
            "--color=never",
        ])
        .args(args)
        .output()
        .map_err(|e| format!("Failed to execute git diff: {e}"))?;

    match output.status.code() {
        Some(0 | 1) => Ok(truncate_diff_output(&output.stdout)),
        _ => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(format!("git diff failed: {stderr}"))
        }
    }
}

/// 回傳值第二項標示是否被行數上限截斷。截斷不再往內容尾端插入
/// `... (truncated)` 這種標記行——`crate::diff::parse_rows` 只認得
/// hunk／context／`+`／`-`／`\ No newline` 這幾種開頭，插進去的標記行會被
/// `_ => continue` 悄悄丟掉，等於白插；截斷的事實一律由這個布林值表達，
/// 呼叫端拿去組 title 上的 `truncated` 旗標。
fn truncate_diff_output(bytes: &[u8]) -> (String, bool) {
    let text = String::from_utf8_lossy(bytes);
    let mut lines = text.lines();
    let out = lines
        .by_ref()
        .take(DIFF_OUTPUT_LINE_LIMIT)
        .collect::<Vec<_>>()
        .join("\n");
    let truncated = lines.next().is_some();
    (out, truncated)
}

fn check_git_repository(path: &Path) -> Result<()> {
    if !is_inside_work_tree(path) && !is_bare_repository(path) {
        let msg = "not a git repository (or any of the parent directories)";
        return Err(msg.into());
    }
    Ok(())
}

/// 唯讀 git 呼叫共用的設定：`current_dir` + `GIT_OPTIONAL_LOCKS=0`。
///
/// `status`／`log`／`diff` 這些唯讀呼叫預設仍會嘗試更新 `index`（stat
/// cache），而寫 `index` 會觸發 watcher 自己（`event.rs::classify_event`
/// 把 `index` 歸類成 WorkingTree）——不關掉的話，主動 `git status` 會
/// 引發一次多餘的背景 reload。代價是 stat 過期的 repo 跑 `status` 會慢
/// 一點（VS Code 也做同樣的取捨），仍然比觸發背景 reload 划算。
///
/// 只給讀取路徑用：`create_tag`／`push_tag`／`background_command`（fetch／
/// checkout）等寫入類呼叫不經過這裡，它們本來就該正常拿鎖。
fn git_read(path: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(path).env("GIT_OPTIONAL_LOCKS", "0");
    cmd
}

pub fn is_inside_work_tree(path: &Path) -> bool {
    git_read(path)
        .arg("rev-parse")
        .arg("--is-inside-work-tree")
        .output()
        .map(|o| o.status.success() && o.stdout == b"true\n")
        .unwrap_or(false)
}

fn is_bare_repository(path: &Path) -> bool {
    git_read(path)
        .arg("rev-parse")
        .arg("--is-bare-repository")
        .output()
        .map(|o| o.status.success() && o.stdout == b"true\n")
        .unwrap_or(false)
}

/// 沒有 commit-graph 時，光是排序就要走完整段歷史；供 `lib.rs` 決定要不要
/// 在狀態列提示一次 `git commit-graph write --reachable`。不用
/// `GitDirs::resolve`：它靠 `--show-toplevel` 判斷，bare repo（例如
/// `linux.git`）沒有 toplevel 會回 `None`，這裡要在 bare repo 一樣能用。
pub fn has_commit_graph(path: &Path) -> bool {
    let Ok(output) = git_read(path)
        .arg("rev-parse")
        .arg("--git-path")
        .arg("objects/info/commit-graph")
        .arg("--git-path")
        .arg("objects/info/commit-graphs")
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| path.join(line).exists())
}

/// watcher 需要監看、也需要拿來分類事件路徑的三個目錄。`git_dir` 是「自己
/// 這個 worktree」的 git dir——linked worktree 底下是
/// `common_dir/worktrees/<name>`；`common_dir` 才是所有 worktree 共用、
/// 實際存放 `refs`／`objects` 的那個。bare repo 沒有 `toplevel`，這裡回傳
/// `None`（`lib.rs` 只在 `is_inside_work_tree` 成立時才啟動 watcher）。
#[derive(Debug, Clone)]
pub struct GitDirs {
    pub toplevel: PathBuf,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
}

impl GitDirs {
    /// 故意不加 `--path-format=absolute`：這個選項要 git 2.31 以上才有，
    /// 舊版 git 遇到不認得的 `--xxx` 選項不會報錯，而是把它原樣印成一行
    /// 輸出，讓後面每一行的對應整個錯位——「失敗就退回舊寫法」這種分支
    /// 永遠不會被觸發，等於沒修。
    ///
    /// 改成兩個版本都走同一條路：`--absolute-git-dir` 保證輸出絕對路徑，
    /// 但 `--git-common-dir`（沒有 `--path-format` 時）在舊版 git 可能印
    /// 相對路徑（例如子目錄裡的 `../../.git`）。這裡一律用 `path.join`
    /// 轉成絕對路徑——`Path::join` 遇到絕對路徑會直接取代，所以已經是
    /// 絕對路徑的輸出經過這個函式一樣不動，不需要另外判斷版本。
    pub fn resolve(path: &Path) -> Option<Self> {
        let output = git_read(path)
            .arg("rev-parse")
            .arg("--show-toplevel")
            .arg("--absolute-git-dir")
            .arg("--git-common-dir")
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let mut lines = text.lines();
        let to_abs = |line: &str| {
            path.join(line)
                .canonicalize()
                .unwrap_or_else(|_| path.join(line))
        };
        Some(Self {
            toplevel: to_abs(lines.next()?),
            git_dir: to_abs(lines.next()?),
            common_dir: to_abs(lines.next()?),
        })
    }
}

/// 平行載入失敗（rev-list 失敗、任一段 git 呼叫失敗）才退回這條單一
/// process 的路徑。
fn load_all_commits(
    path: &Path,
    sort: SortCommit,
    head: &Head,
    stashes: &[Commit],
    max_count: Option<usize>,
) -> Vec<Commit> {
    let mut commits = load_commits_parallel(path, sort, head, stashes, max_count)
        .unwrap_or_else(|| load_commits_single(path, sort, head, stashes, max_count));
    intern_author_strings(&mut commits);
    commits
}

/// 排除 stash 及其他 refs 之後，跟 `--branches --remotes --tags` 一起決定
/// 「要載入哪些 commit」的共用引數：`load_commits_single`（單一 `git log`）
/// 與 `rev_list_hashes`（平行路徑的第一步）都是同一組 commit，只是後面
/// 取得逐欄位內容的方式不同。
fn add_commit_revs(cmd: &mut Command, head: &Head, stashes: &[Commit], max_count: Option<usize>) {
    cmd.arg("--branches").arg("--remotes").arg("--tags");

    // 加入 stash 可以走到的 commits
    stashes.iter().for_each(|stash| {
        cmd.arg(stash.parent_commit_hashes[0].as_str());
    });

    if !matches!(head, Head::None) {
        cmd.arg("HEAD");
    }

    if let Some(n) = max_count {
        cmd.arg("--max-count").arg(n.to_string());
    }
}

fn load_commits_single(
    path: &Path,
    sort: SortCommit,
    head: &Head,
    stashes: &[Commit],
    max_count: Option<usize>,
) -> Vec<Commit> {
    let mut cmd = git_read(path);
    cmd.arg("log");

    cmd.arg(sort.git_flag())
        .arg(format!("--pretty={}", load_commits_format()))
        .arg("--date=iso-strict")
        .arg("-z"); // 用 NUL 作為分隔符

    add_commit_revs(&mut cmd, head, stashes, max_count);

    // stderr 設成 null：載入路徑上 git 的 warning（例如 dangling ref）
    // 不能直接印到 TUI 的 alternate screen 上把畫面弄花。
    cmd.stdout(Stdio::piped()).stderr(Stdio::null());

    let mut process = cmd.spawn().unwrap();

    let stdout = process.stdout.take().expect("failed to open stdout");

    let commits = parse_commit_stream(BufReader::new(stdout), CommitType::Commit).unwrap();

    process.wait().unwrap();

    commits
}

/// 平行讀取的最小 commit 數：小 repo 多開幾個 process 划不來，但仍然走
/// rev-list + stdin 這條路徑（只是段數壓成 1），不建另一條「小 repo 專用」
/// 分支。
const PARALLEL_LOAD_MIN_COMMITS: usize = 20_000;
/// 平行段數上限：issue #121 實測 8 段就已經吃滿收益，再多只是多開 process。
const PARALLEL_LOAD_MAX_SEGMENTS: usize = 8;

fn load_commits_parallel(
    path: &Path,
    sort: SortCommit,
    head: &Head,
    stashes: &[Commit],
    max_count: Option<usize>,
) -> Option<Vec<Commit>> {
    let available = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(PARALLEL_LOAD_MAX_SEGMENTS);
    load_commits_parallel_with_segments(path, sort, head, stashes, max_count, available)
}

/// `segments` 是「想要的」段數上限，實際段數會再壓到 `[1, 行數]`。獨立出
/// 這個參數是為了讓測試能對小 repo 強制切成多段，驗證平行路徑跟
/// `load_commits_single` 逐欄位相同。
fn load_commits_parallel_with_segments(
    path: &Path,
    sort: SortCommit,
    head: &Head,
    stashes: &[Commit],
    max_count: Option<usize>,
    segments: usize,
) -> Option<Vec<Commit>> {
    let revs = rev_list_hashes(path, sort, head, stashes, max_count)?;
    let line_ends = line_end_offsets(&revs);
    if line_ends.is_empty() {
        return Some(Vec::new());
    }

    let segments = if line_ends.len() < PARALLEL_LOAD_MIN_COMMITS {
        1
    } else {
        segments
    }
    .clamp(1, line_ends.len());
    let chunk_lines = line_ends.len().div_ceil(segments);
    let format = load_commits_format();

    let mut byte_start = 0;
    let chunks: Vec<&[u8]> = line_ends
        .chunks(chunk_lines)
        .map(|ends| {
            let byte_end = *ends.last().expect("chunks() 不會產生空的分組");
            let chunk = &revs[byte_start..byte_end];
            byte_start = byte_end;
            chunk
        })
        .collect();

    let results = std::thread::scope(|scope| {
        chunks
            .into_iter()
            .map(|chunk| {
                // 具名 thread：Phase 6 背景重載時，這段可能在 `lib.rs` 裝好的
                // 過濾 panic hook 底下跑，hook 靠名稱前綴判斷要不要靜音。
                // `spawn_scoped` 失敗（極端情況，例如 OS thread 用盡）就當這段
                // 失敗，跟下面 panic 走同一條 `unwrap_or(None)` 退回整批
                // `load_commits_single` 的路徑。
                std::thread::Builder::new()
                    .name(crate::QUIET_PANIC_THREAD_PREFIX.to_string())
                    .spawn_scoped(scope, || load_commits_segment(path, &format, chunk))
                    .ok()
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|handle| handle.and_then(|h| h.join().unwrap_or(None)))
            .collect::<Vec<_>>()
    });

    let mut commits = Vec::with_capacity(line_ends.len());
    for result in results {
        commits.extend(result?);
    }
    Some(commits)
}

/// 只決定「有哪些 commit、什麼排序」，逐欄位內容留給 `load_commits_segment`
/// 平行跑 `git log --stdin` 取得。`None` 代表 rev-list 本身失敗，呼叫端
/// 退回 `load_commits_single`。
fn rev_list_hashes(
    path: &Path,
    sort: SortCommit,
    head: &Head,
    stashes: &[Commit],
    max_count: Option<usize>,
) -> Option<Vec<u8>> {
    let mut cmd = git_read(path);
    cmd.arg("rev-list").arg(sort.git_flag());
    add_commit_revs(&mut cmd, head, stashes, max_count);
    cmd.stdout(Stdio::piped()).stderr(Stdio::null());

    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(output.stdout)
}

/// 每個換行符號後一個 byte 的位置，用來把 `rev_list_hashes` 的輸出切成
/// 「一行一個 commit hash」的連續區段，不需要另外配置字串陣列。
fn line_end_offsets(bytes: &[u8]) -> Vec<usize> {
    bytes
        .iter()
        .enumerate()
        .filter_map(|(i, &b)| (b == b'\n').then_some(i + 1))
        .collect()
}

/// 平行載入的一段：把這段的 rev 清單透過 stdin 交給 `git log --stdin`，讀
/// 它的 stdout 就地 parse。thread 裡的 spawn／write／read／wait 全部回
/// `Result`，不 `unwrap`——git 遇到壞物件會先 fatal 退出，這時寫 stdin
/// 會拿到 `BrokenPipe`，`unwrap` 會讓 panic 被 `thread::scope` 往上傳，
/// fallback 就形同虛設（外層的 `handle.join()` 另外接住萬一仍然發生的
/// panic，當作這段失敗）。
///
/// 先把整段 revs 寫完再讀 stdout 不會死鎖：git 的 `--stdin` 在
/// `setup_revisions` 就會把 stdin 讀到 EOF 才開始輸出。
fn load_commits_segment(path: &Path, format: &str, revs_chunk: &[u8]) -> Option<Vec<Commit>> {
    let mut cmd = git_read(path);
    cmd.arg("log")
        .arg("--no-walk=unsorted")
        .arg("--stdin")
        .arg("--no-show-signature")
        .arg(format!("--pretty={format}"))
        .arg("--date=iso-strict")
        .arg("-z")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let mut process = cmd.spawn().ok()?;
    let mut stdin = process.stdin.take()?;
    stdin.write_all(revs_chunk).ok()?;
    drop(stdin); // 送 EOF，git 才會開始輸出

    let stdout = process.stdout.take()?;
    let commits = parse_commit_stream(BufReader::new(stdout), CommitType::Commit).ok()?;

    let status = process.wait().ok()?;
    if !status.success() {
        return None;
    }

    debug_assert_eq!(
        commits.len(),
        revs_chunk.iter().filter(|&&b| b == b'\n').count(),
        "平行載入某段 parse 出來的筆數跟送進去的行數不同"
    );

    Some(commits)
}

/// 同一批載入呼叫內，把 `author_name`／`author_email` 相同內容的 `Arc<str>`
/// 收斂成同一份配置。`load_all_commits` 在合併完平行段落之後才呼叫一次
/// （不是每段各自呼叫）：這樣才能跨段落去重，而且這個函式本來就要單執行緒
/// 走完全部 commit，跟平行段落各自呼叫的成本一樣，沒有理由犧牲去重效果。
/// `load_all_stashes` 另外自己呼叫一次，不跟主要 commits 共用——stash 通常
/// 只有幾筆，共用與否差異可忽略。
fn intern_author_strings(commits: &mut [Commit]) {
    fn intern(pool: &mut FxHashSet<Arc<str>>, s: &Arc<str>) -> Arc<str> {
        if let Some(existing) = pool.get(s.as_ref()) {
            return existing.clone();
        }
        pool.insert(s.clone());
        s.clone()
    }

    let mut names: FxHashSet<Arc<str>> = FxHashSet::default();
    let mut emails: FxHashSet<Arc<str>> = FxHashSet::default();
    for commit in commits {
        commit.author_name = intern(&mut names, &commit.author_name);
        commit.author_email = intern(&mut emails, &commit.author_email);
    }
}

fn parse_commit_line(s: &str, commit_type: CommitType) -> Option<Commit> {
    let mut parts = s.splitn(6, '\x1f');
    let commit_hash = parts.next()?;
    let author_name = parts.next()?;
    let author_email = parts.next()?;
    let author_date = parts.next()?;
    let subject = parts.next()?;
    let parents = parts.next()?;
    Some(Commit {
        commit_hash: commit_hash.into(),
        author_name: author_name.into(),
        author_email: author_email.into(),
        author_date: parse_iso_date(author_date),
        subject: crate::emoji::expand(subject).into_owned(),
        parent_commit_hashes: parse_parent_commit_hashes(parents),
        commit_type,
    })
}

/// `load_commits_single`／`load_all_stashes`／`load_commits_segment` 共用：
/// 從 `-z` 輸出的 stdout 依 NUL 切開再逐行 `parse_commit_line`。呼叫端各自
/// 決定怎麼處理讀取錯誤（`unwrap` 或 `?`），這裡只回傳 `io::Result`。
fn parse_commit_stream(
    reader: impl BufRead,
    commit_type: CommitType,
) -> std::io::Result<Vec<Commit>> {
    let mut commits = Vec::new();
    for bytes in reader.split(b'\0') {
        let bytes = bytes?;
        let s = String::from_utf8_lossy(&bytes);
        if let Some(commit) = parse_commit_line(&s, commit_type.clone()) {
            commits.push(commit);
        }
    }
    Ok(commits)
}

/// `commit_detail` 專用：`Commit` 列表格式拿掉的 `committer_*` 與 `body`，
/// 只在使用者真的打開一個 commit 的 detail view 時才跑。跟 `%aN/%aE` 同理，
/// 大寫的 `%cN/%cE` 會經 `.mailmap` 解析身分。
fn load_commit_extra(path: &Path, commit_hash: &CommitHash) -> Option<CommitExtra> {
    let format = ["%cN", "%cE", "%cd", "%b"].join("%x1f");
    let output = git_read(path)
        .arg("show")
        .arg("-s")
        .arg("--no-show-signature")
        .arg(format!("--format={format}"))
        .arg("--date=iso-strict")
        .arg(commit_hash.as_str())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let text = text.strip_suffix('\n').unwrap_or(&text);
    let mut parts = text.splitn(4, '\x1f');
    let committer_name = parts.next()?;
    let committer_email = parts.next()?;
    let committer_date = parts.next()?;
    let body = parts.next()?;
    Some(CommitExtra {
        committer_name: committer_name.into(),
        committer_email: committer_email.into(),
        committer_date: parse_iso_date(committer_date),
        body: crate::emoji::expand(body).into_owned(),
    })
}

fn load_all_stashes(path: &Path) -> Vec<Commit> {
    let mut cmd = git_read(path)
        .arg("stash")
        .arg("list")
        .arg(format!("--pretty={}", load_commits_format()))
        .arg("--date=iso-strict")
        .arg("-z") // 用 NUL 作為分隔符
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdout = cmd.stdout.take().expect("failed to open stdout");

    let mut commits = parse_commit_stream(BufReader::new(stdout), CommitType::Stash).unwrap();

    cmd.wait().unwrap();

    intern_author_strings(&mut commits);

    commits
}

fn load_commits_format() -> String {
    // 大寫的 %aN/%aE 會經 .mailmap 解析身分；沒有 .mailmap 時輸出跟小寫版
    // 完全相同，故不需要另外開 config 開關。committer 身分與 %b 拿掉了——
    // 見 `load_commit_extra`。
    ["%H", "%aN", "%aE", "%ad", "%s", "%P"].join("%x1f") // 用 Unit Separator 作為分隔符
}

fn parse_iso_date(s: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(s).expect("git --format=%aI should always produce valid RFC3339")
}

fn parse_parent_commit_hashes(s: &str) -> Vec<CommitHash> {
    if s.is_empty() {
        return Vec::new();
    }
    s.split(' ').map(|s| s.into()).collect()
}

fn build_commit_index(commits: &[Commit]) -> CommitIndex {
    commits
        .iter()
        .enumerate()
        .map(|(i, commit)| (commit.commit_hash.clone(), i))
        .collect()
}

const PARENT_NOT_LOADED: u32 = u32::MAX;

/// 一併把每個已載入的 parent hash 換成該 parent commit 自己的 `CommitHash`
/// clone（同一份 `Arc` 配置），不用另外多掃一遍 parent 清單。
fn build_parent_csr(commits: &mut [Commit], commit_index: &CommitIndex) -> (Vec<u32>, Vec<u32>) {
    let mut parent_start = Vec::with_capacity(commits.len() + 1);
    let mut parent_idx = Vec::with_capacity(commits.len());
    parent_start.push(0);
    for i in 0..commits.len() {
        for j in 0..commits[i].parent_commit_hashes.len() {
            match commit_index.get(&commits[i].parent_commit_hashes[j]) {
                Some(&p) => {
                    parent_idx.push(p as u32);
                    commits[i].parent_commit_hashes[j] = commits[p].commit_hash.clone();
                }
                None => parent_idx.push(PARENT_NOT_LOADED),
            }
        }
        parent_start.push(parent_idx.len() as u32);
    }
    (parent_start, parent_idx)
}

fn merge_stashes_to_commits(commits: Vec<Commit>, stashes: Vec<Commit>) -> Vec<Commit> {
    // stash commit 有多個 parent commit，但第一個 parent commit 才是建立該 stash 時所在的 commit。
    // 如果找不到第一個 parent commit，該 stash commit 就會被忽略。
    let mut ret = Vec::new();
    let mut statsh_map: FxHashMap<CommitHash, Vec<Commit>> =
        stashes
            .into_iter()
            .fold(FxHashMap::default(), |mut acc, commit| {
                let parent = commit.parent_commit_hashes[0].clone();
                acc.entry(parent).or_default().push(commit);
                acc
            });
    for commit in commits {
        if let Some(stashes) = statsh_map.remove(&commit.commit_hash) {
            for stash in stashes {
                ret.push(stash);
            }
        }
        ret.push(commit);
    }
    ret
}

fn load_refs(path: &Path) -> (RefMap, Head) {
    let mut cmd = git_read(path)
        .arg("show-ref")
        .arg("--head")
        .arg("--dereference")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdout = cmd.stdout.take().expect("failed to open stdout");

    let reader = BufReader::new(stdout);

    let mut ref_map = RefMap::default();
    let mut tag_map: FxHashMap<String, Ref> = FxHashMap::default();
    let mut head: Head = Head::None;

    for line in reader.lines() {
        let line = line.unwrap();

        let Some((hash, refs)) = line.split_once(' ') else {
            panic!("unexpected format: [{line}]");
        };

        if refs == "HEAD" {
            head = if let Some(branch) = get_current_branch(path) {
                Head::Branch { name: branch }
            } else {
                Head::Detached {
                    target: hash.into(),
                }
            };
        } else if let Some(r) = parse_branch_refs(hash, refs) {
            ref_map.entry(hash.into()).or_default().push(r);
        } else if let Some(r) = parse_tag_refs(hash, refs) {
            // 若存在 annotated tag，會被同一個 tag 後面那一行覆蓋
            // 這樣可以讓 tag 指向 annotated tag 實際指到的 commit
            tag_map.insert(r.name().into(), r);
        }
    }

    for tag in tag_map.into_values() {
        ref_map.entry(tag.target().clone()).or_default().push(tag);
    }

    ref_map.values_mut().for_each(|refs| refs.sort());

    cmd.wait().unwrap();

    (ref_map, head)
}

fn load_stashes_as_refs(path: &Path) -> RefMap {
    let format = ["%gd", "%H", "%s"].join("%x1f"); // 用 Unit Separator 作為分隔符
    let mut cmd = git_read(path)
        .arg("stash")
        .arg("list")
        .arg(format!("--format={format}"))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdout = cmd.stdout.take().expect("failed to open stdout");

    let reader = BufReader::new(stdout);

    let mut ref_map = RefMap::default();

    for line in reader.lines() {
        let line = line.unwrap();

        let mut parts = line.splitn(3, '\x1f');
        let Some(name) = parts.next() else { continue };
        let Some(hash) = parts.next() else { continue };
        let Some(subject) = parts.next() else {
            continue;
        };

        let r = Ref::Stash {
            name: name.into(),
            message: crate::emoji::expand(subject).into_owned(),
            target: hash.into(),
        };

        ref_map.entry(hash.into()).or_default().push(r);
    }

    cmd.wait().unwrap();

    ref_map
}

fn merge_ref_maps(m1: &mut RefMap, m2: RefMap) {
    for (k, v) in m2 {
        m1.entry(k).or_default().extend(v);
    }
}

fn parse_branch_refs(hash: &str, refs: &str) -> Option<Ref> {
    if refs.starts_with("refs/heads/") {
        let name = refs.trim_start_matches("refs/heads/");
        Some(Ref::Branch {
            name: name.into(),
            target: hash.into(),
        })
    } else if refs.starts_with("refs/remotes/") {
        let name = refs.trim_start_matches("refs/remotes/");
        Some(Ref::RemoteBranch {
            name: name.into(),
            target: hash.into(),
        })
    } else {
        None
    }
}

fn parse_tag_refs(hash: &str, refs: &str) -> Option<Ref> {
    if refs.starts_with("refs/tags/") {
        let name = refs.trim_start_matches("refs/tags/");
        let name = name.trim_end_matches("^{}");
        Some(Ref::Tag {
            name: name.into(),
            target: hash.into(),
        })
    } else {
        None
    }
}

fn get_current_branch(path: &Path) -> Option<String> {
    let mut cmd = git_read(path)
        .arg("branch")
        .arg("--show-current")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdout = cmd.stdout.take().expect("failed to open stdout");

    let reader = BufReader::new(stdout);

    let branch = if let Some(line) = reader.lines().next() {
        line.ok()
    } else {
        None
    };

    cmd.wait().unwrap();

    branch
}

#[derive(Debug, Clone)]
pub enum FileChange {
    Add {
        path: String,
        stats: Option<(usize, usize)>,
    },
    Modify {
        path: String,
        stats: Option<(usize, usize)>,
    },
    Delete {
        path: String,
        stats: Option<(usize, usize)>,
    },
    /// git 完全不追蹤的新檔案。與 `Add`（已 staged 的新增）分開記，因為兩者要
    /// 走不同的 `git diff` 呼叫形狀（`--no-index` vs 一般範圍比較）。
    Untracked {
        path: String,
        stats: Option<(usize, usize)>,
    },
}

impl FileChange {
    pub fn path(&self) -> &str {
        match self {
            FileChange::Add { path, .. }
            | FileChange::Modify { path, .. }
            | FileChange::Delete { path, .. }
            | FileChange::Untracked { path, .. } => path,
        }
    }

    pub fn stats(&self) -> Option<(usize, usize)> {
        match self {
            FileChange::Add { stats, .. }
            | FileChange::Modify { stats, .. }
            | FileChange::Delete { stats, .. }
            | FileChange::Untracked { stats, .. } => *stats,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct WorkingChanges {
    pub staged: Vec<FileChange>,
    pub unstaged: Vec<FileChange>,
}

impl WorkingChanges {
    pub fn is_empty(&self) -> bool {
        self.staged.is_empty() && self.unstaged.is_empty()
    }

    pub fn file_count(&self) -> usize {
        self.staged.len() + self.unstaged.len()
    }
}

/// 只跑 `git status`，不含檔案行數增減——`reload::Reloader` 的背景 worker
/// 每次存檔都要跑這個，多兩個 `diff --numstat` 子行程不划算。行數只有真的
/// 開著 working changes detail 才看得到，需要時呼叫 `fill_working_changes_stats`
/// 另外補。
pub fn load_working_changes(path: &Path) -> Result<WorkingChanges> {
    let mut cmd = git_read(path)
        .arg("-c")
        .arg("core.quotePath=false")
        .arg("status")
        .arg("--porcelain=v1")
        .arg("--untracked-files=all")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to spawn git status: {e}"))?;

    let stdout = cmd
        .stdout
        .take()
        .ok_or("failed to open git status stdout")?;
    let reader = BufReader::new(stdout);

    let mut staged = Vec::new();
    let mut unstaged = Vec::new();

    for line in reader.lines() {
        let Ok(line) = line else { continue };
        if line.len() < 4 {
            continue;
        }
        let x = line.as_bytes()[0]; // staged 狀態
        let y = line.as_bytes()[1]; // unstaged 狀態
        let file_path = &line[3..];

        // 未追蹤檔案一律顯示為 `??` —— 兩個欄位必須一起處理，
        // 因為單獨一個 `?` 沒有獨立的欄位意義。
        if x == b'?' && y == b'?' {
            unstaged.push(FileChange::Untracked {
                path: file_path.into(),
                stats: None,
            });
            continue;
        }

        // 解析 staged 變更（X 欄）
        staged.extend(parse_status_char(x, file_path));

        // 解析 unstaged 變更（Y 欄）
        unstaged.extend(parse_status_char(y, file_path));
    }

    let status = cmd
        .wait()
        .map_err(|e| format!("failed to wait for git status: {e}"))?;
    if !status.success() {
        return Err("git status exited with a non-zero status".into());
    }

    Ok(WorkingChanges { staged, unstaged })
}

/// 把檔案行數增減（`diff --numstat`）補進已經跑完 `git status` 的
/// `WorkingChanges`。只有使用者真的開著 working changes detail、看得到
/// 行數時才值得呼叫——見 `App::open_detail`／`reload::Reloader::set_want_stats`。
pub fn fill_working_changes_stats(path: &Path, working_changes: &mut WorkingChanges) {
    let unstaged_stats = get_diff_numstat(path, &[]);
    apply_numstat(&mut working_changes.unstaged, &unstaged_stats);

    let staged_stats = get_diff_numstat(path, &["--cached"]);
    apply_numstat(&mut working_changes.staged, &staged_stats);
}

fn rename_to_changes(old_path: &str, new_path: &str) -> Vec<FileChange> {
    vec![
        FileChange::Delete {
            path: old_path.into(),
            stats: None,
        },
        FileChange::Add {
            path: new_path.into(),
            stats: None,
        },
    ]
}

fn parse_status_char(status: u8, file_path: &str) -> Vec<FileChange> {
    match status {
        b'A' => vec![FileChange::Add {
            path: file_path.into(),
            stats: None,
        }],
        b'M' => vec![FileChange::Modify {
            path: file_path.into(),
            stats: None,
        }],
        b'D' => vec![FileChange::Delete {
            path: file_path.into(),
            stats: None,
        }],
        b'R' => {
            let parts: Vec<&str> = file_path.splitn(2, " -> ").collect();
            if parts.len() == 2 {
                rename_to_changes(parts[0], parts[1])
            } else {
                vec![FileChange::Modify {
                    path: file_path.into(),
                    stats: None,
                }]
            }
        }
        _ => vec![],
    }
}

fn get_diff_numstat(path: &Path, args: &[&str]) -> FxHashMap<String, (usize, usize)> {
    let mut cmd_args = vec!["-c", "core.quotePath=false", "diff", "--numstat"];
    cmd_args.extend_from_slice(args);

    let mut cmd = git_read(path)
        .args(&cmd_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdout = cmd.stdout.take().expect("failed to open stdout");
    let reader = BufReader::new(stdout);

    let mut stats = FxHashMap::default();

    for line in reader.lines() {
        let line = line.unwrap();
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() >= 3 {
            let additions = parts[0].parse::<usize>().unwrap_or(0);
            let deletions = parts[1].parse::<usize>().unwrap_or(0);
            // 對於 rename，numstat 顯示的是目的路徑
            // 處理 numstat 中的 "from => to" 格式
            let file_path = parts[2..].join("\t");
            stats.insert(file_path, (additions, deletions));
        }
    }

    cmd.wait().unwrap();

    stats
}

fn apply_numstat(changes: &mut [FileChange], stats: &FxHashMap<String, (usize, usize)>) {
    for change in changes.iter_mut() {
        let key = change.path();
        if let Some(&s) = stats.get(key) {
            match change {
                FileChange::Add { stats: st, .. }
                | FileChange::Modify { stats: st, .. }
                | FileChange::Delete { stats: st, .. }
                | FileChange::Untracked { stats: st, .. } => {
                    *st = Some(s);
                }
            }
        }
    }
}

pub fn get_diff_summary(path: &Path, commit_hash: &CommitHash) -> Vec<FileChange> {
    let parent_arg = format!("{}^", commit_hash.as_str());
    let mut cmd = git_read(path)
        .arg("-c")
        .arg("core.quotePath=false")
        .arg("diff")
        .arg("--name-status")
        .arg(&parent_arg)
        .arg(commit_hash.as_str())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdout = cmd.stdout.take().expect("failed to open stdout");

    let reader = BufReader::new(stdout);

    let mut changes = Vec::new();

    for line in reader.lines() {
        let line = line.unwrap();
        let parts: Vec<&str> = line.split('\t').collect();

        match &parts[0][0..1] {
            "A" => changes.push(FileChange::Add {
                path: parts[1].into(),
                stats: None,
            }),
            "M" => changes.push(FileChange::Modify {
                path: parts[1].into(),
                stats: None,
            }),
            "D" => changes.push(FileChange::Delete {
                path: parts[1].into(),
                stats: None,
            }),
            "R" => {
                changes.extend(rename_to_changes(parts[1], parts[2]));
            }
            _ => {}
        }
    }

    cmd.wait().unwrap();

    let numstat = get_diff_numstat(path, &[&parent_arg, commit_hash.as_str()]);
    apply_numstat(&mut changes, &numstat);

    changes
}

pub fn get_initial_commit_additions(path: &Path, commit_hash: &CommitHash) -> Vec<FileChange> {
    let mut cmd = git_read(path)
        .arg("-c")
        .arg("core.quotePath=false")
        .arg("ls-tree")
        .arg("--name-status")
        .arg("-r")
        .arg(commit_hash.as_str())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdout = cmd.stdout.take().expect("failed to open stdout");

    let reader = BufReader::new(stdout);

    let mut changes = Vec::new();

    for line in reader.lines() {
        let line = line.unwrap();
        changes.push(FileChange::Add {
            path: line,
            stats: None,
        });
    }

    cmd.wait().unwrap();

    // 用 empty tree hash 取得初始 commit 的 numstat
    let numstat = get_diff_numstat(path, &[GIT_EMPTY_TREE_HASH, commit_hash.as_str()]);
    apply_numstat(&mut changes, &numstat);

    changes
}

/// 用 `git check-ref-format` 驗證 git ref 名稱。
fn validate_ref_name(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() {
        return Err("Ref name cannot be empty".into());
    }
    let output = Command::new("git")
        .args(["check-ref-format", "--allow-onelevel", name])
        .output()
        .map_err(|e| format!("Failed to validate ref name: {e}"))?;
    if !output.status.success() {
        return Err(format!("Invalid ref name: '{name}'"));
    }
    Ok(())
}

fn run_git_command(
    path: &Path,
    args: &[&str],
    error_prefix: &str,
) -> std::result::Result<(), String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .map_err(|e| format!("Failed to execute git {}: {e}", args[0]))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("{error_prefix}: {stderr}"));
    }
    Ok(())
}

/// 會打網路（`fetch`／`ls-remote`／`checkout` 觸發的 smudge/clean filter 等）
/// 且跑在背景 thread 的 git 指令共用的設定：`app.rs::spawn_git_task`（手動
/// `f`／checkout）跟 `auto_fetch.rs`（背景輪詢）都需要同一套硬化。
///
/// `raw mode + alternate screen` 的終端機上，沒有可用 credential helper 時
/// git／ssh 會直接對 controlling tty 寫提示把畫面弄爛，子行程卡到逾時才
/// 收；`fetch` 又會觸發 `gc --auto`，背景長出一個重量級 gc 不是好事。
///
/// `-c gc.auto=0` 是全域選項，必須在子指令（`fetch`／`checkout`／
/// `ls-remote`）之前，所以這裡直接建構整個 `Command`，不是事後 `.arg()`
/// 附加——附加在子指令之後 git 會把它當成該子指令的位置參數解析，不是
/// 全域設定。
pub(crate) fn background_command<I, S>(path: &Path, args: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut cmd = Command::new("git");
    cmd.args(["-c", "gc.auto=0"])
        .args(args)
        .current_dir(path)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    cmd
}

/// `fetch --all` 要不要多帶 `--prune`，手動 `f`（`app.rs::fetch_all`）與背景
/// auto-fetch（`auto_fetch.rs::spawn_due_fetch`）共用同一個開關。
///
/// `Off` **不是** `--no-prune`：不傳任何 prune 相關旗標，讓使用者
/// `fetch.prune`／`remote.<name>.prune` 的 gitconfig 設定照常生效——這樣手動
/// `f` 在新增這個開關之後，對沒有動過這個設定的人是逐位元組不變的行為。
/// 若改傳 `--no-prune`，會反過來蓋掉那些已經設了 `fetch.prune = true` 的人
/// 的既有行為，是這個功能不該附帶的副作用。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FetchPrune {
    #[default]
    Off,
    On,
}

impl FetchPrune {
    /// 唯一的指令組裝點：`fetch_all()`（手動 `f`）與 `spawn_due_fetch()`
    /// （auto-fetch）都呼叫這裡拿參數，不各自拼字面量。
    pub(crate) fn fetch_all_args(self) -> &'static [&'static str] {
        match self {
            FetchPrune::Off => &["fetch", "--all"],
            FetchPrune::On => &["fetch", "--all", "--prune"],
        }
    }
}

pub fn create_tag(
    path: &Path,
    name: &str,
    commit_hash: &CommitHash,
    message: Option<&str>,
) -> std::result::Result<(), String> {
    validate_ref_name(name)?;
    let mut cmd = Command::new("git");
    cmd.arg("tag");
    if let Some(msg) = message {
        if !msg.is_empty() {
            cmd.arg("-a").arg("-m").arg(msg);
        }
    }
    cmd.arg(name).arg(commit_hash.as_str()).current_dir(path);

    let output = cmd
        .output()
        .map_err(|e| format!("Failed to execute git tag: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Failed to create tag: {stderr}"));
    }
    Ok(())
}

pub fn push_tag(path: &Path, tag_name: &str) -> std::result::Result<(), String> {
    run_git_command(path, &["push", "origin", tag_name], "Failed to push tag")
}

pub fn delete_tag(path: &Path, tag_name: &str) -> std::result::Result<(), String> {
    run_git_command(path, &["tag", "-d", tag_name], "Failed to delete tag")
}

pub fn delete_remote_tag(path: &Path, tag_name: &str) -> std::result::Result<(), String> {
    run_git_command(
        path,
        &["push", "origin", "--delete", tag_name],
        "Failed to delete remote tag",
    )
}

pub fn delete_branch(path: &Path, branch_name: &str) -> std::result::Result<(), String> {
    run_git_command(
        path,
        &["branch", "-d", branch_name],
        "Failed to delete branch",
    )
}

pub fn delete_branch_force(path: &Path, branch_name: &str) -> std::result::Result<(), String> {
    run_git_command(
        path,
        &["branch", "-D", branch_name],
        "Failed to force delete branch",
    )
}

pub fn delete_remote_branch(path: &Path, branch_name: &str) -> std::result::Result<(), String> {
    let parts: Vec<&str> = branch_name.splitn(2, '/').collect();
    if parts.len() != 2 {
        return Err(format!("Invalid remote branch name format: {branch_name}"));
    }
    let (remote, branch) = (parts[0], parts[1]);
    run_git_command(
        path,
        &["push", remote, "--delete", branch],
        "Failed to delete remote branch",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 只釘不變式，不逐字重抄 `fetch_all_args` 的兩個分支（那樣等於沒測）：
    /// Off 不含任何 prune 相關字串——守住「不是 `--no-prune`」這個決定。
    #[test]
    fn off_never_mentions_prune() {
        assert!(!FetchPrune::Off
            .fetch_all_args()
            .iter()
            .any(|a| a.contains("prune")));
        assert!(FetchPrune::On.fetch_all_args().contains(&"--prune"));
    }

    fn commit_with_parents(hash: &str, parents: &str) -> Commit {
        let date = "2026-07-31T10:00:00+08:00";
        let line = [hash, "A", "a@example.com", date, "s", parents].join("\x1f");
        parse_commit_line(&line, CommitType::Commit).unwrap()
    }

    /// 沒載入的 parent（`max_count` 截掉的、stash 的 index／untracked commit）
    /// 在 CSR 裡是 `PARENT_NOT_LOADED`，位置跟 `parent_commit_hashes` 對齊。
    #[test]
    fn parent_csr_marks_unloaded_parents() {
        let mut commits = vec![
            commit_with_parents("s", "b idx untracked"),
            commit_with_parents("b", "a"),
            commit_with_parents("a", "cut"),
        ];
        let index = build_commit_index(&commits);
        let (start, idx) = build_parent_csr(&mut commits, &index);

        assert_eq!(start, [0, 3, 4, 5]);
        assert_eq!(
            idx,
            [
                1,
                PARENT_NOT_LOADED,
                PARENT_NOT_LOADED,
                2,
                PARENT_NOT_LOADED
            ]
        );

        // 有載入的 parent（"s" 的 parent "b"、"b" 的 parent "a"）改成跟該
        // parent commit 自己的 `commit_hash` 共用同一份 `Arc` 配置。
        assert!(Arc::ptr_eq(
            &commits[0].parent_commit_hashes[0].as_arc(),
            &commits[1].commit_hash.as_arc()
        ));
        assert!(Arc::ptr_eq(
            &commits[1].parent_commit_hashes[0].as_arc(),
            &commits[2].commit_hash.as_arc()
        ));
    }

    /// 同一次載入呼叫內，相同作者的 `author_name`／`author_email` 收斂成
    /// 同一份 `Arc` 配置——linux 148 萬筆 commit 只有約 4 萬個不同作者。
    #[test]
    fn intern_author_strings_shares_arc_for_same_author() {
        let mut commits = vec![commit_with_parents("a", ""), commit_with_parents("b", "")];
        intern_author_strings(&mut commits);

        assert!(Arc::ptr_eq(
            &commits[0].author_name,
            &commits[1].author_name
        ));
        assert!(Arc::ptr_eq(
            &commits[0].author_email,
            &commits[1].author_email
        ));
    }

    #[test]
    fn parse_commit_line_expands_emoji_shortcodes() {
        let date = "2026-07-31T10:00:00+08:00";
        let line = [
            "abc1234",
            "Alice",
            "alice@example.com",
            date,
            ":tada: 上線",
            "",
        ]
        .join("\x1f");

        let commit = parse_commit_line(&line, CommitType::Commit).unwrap();

        assert_eq!(commit.subject, "🎉 上線");
    }

    // ── 平行載入：跟單一 process 逐欄位相同 ──

    fn git_env(path: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> std::process::Output {
        let mut cmd = std::process::Command::new("git");
        cmd.args(args)
            .current_dir(path)
            .env("GIT_CONFIG_NOSYSTEM", "true")
            // 比照 tests/mailmap.rs：擋掉開發者 global config 的 commit.gpgsign
            // 之類的設定，不然這個測試在他機器上會紅、在 CI 上才綠。
            .env("HOME", "/dev/null")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@example.com");
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let out = cmd
            .output()
            .unwrap_or_else(|e| panic!("failed to run git {}: {e}", args.join(" ")));
        assert!(
            out.status.success(),
            "git {} 失敗: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    fn commit_at(path: &Path, msg: &str, date: &str) {
        git_env(
            path,
            &["commit", "--allow-empty", "-m", msg],
            &[("GIT_AUTHOR_DATE", date), ("GIT_COMMITTER_DATE", date)],
        );
    }

    /// 寫進 `-F` 檔案再 commit：`std::fs::write` 可以塞任意 bytes，不像
    /// command-line 引數在非 Unix 平台上受 `OsStr` 編碼限制。
    fn commit_with_raw_message(path: &Path, date: &str, message: &[u8]) {
        let msg_file = path.join(".msg");
        std::fs::write(&msg_file, message).unwrap();
        git_env(
            path,
            &["commit", "--allow-empty", "-F", ".msg"],
            &[("GIT_AUTHOR_DATE", date), ("GIT_COMMITTER_DATE", date)],
        );
        std::fs::remove_file(&msg_file).unwrap();
    }

    /// main: A(t1) 分出 b1: B(t2) C(t4) 與 b2: D(t3)，b1／b2 互不是彼此
    /// 祖先。實測過這個結構會讓 `--date-order`（C D B A）跟 `--topo-order`
    /// （C B D A）排出不同順序，兩種排序都能真的被測到。結束時停在 main。
    fn build_branching_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        git_env(path, &["init", "-q", "-b", "main"], &[]);
        commit_at(path, "A", "2026-01-01T00:00:01+00:00");
        git_env(path, &["branch", "b1"], &[]);
        git_env(path, &["branch", "b2"], &[]);
        git_env(path, &["checkout", "-q", "b1"], &[]);
        commit_at(path, "B", "2026-01-01T00:00:02+00:00");
        commit_at(path, "C", "2026-01-01T00:00:04+00:00");
        git_env(path, &["checkout", "-q", "b2"], &[]);
        commit_at(path, "D", "2026-01-01T00:00:03+00:00");
        git_env(path, &["checkout", "-q", "main"], &[]);
        dir
    }

    /// `segments = 1` 與 `segments = 3` 都跟 `load_commits_single` 逐欄位
    /// 相同（`Commit`／`CommitType` 的 `PartialEq` 是為了這個比較加的）。
    fn assert_parallel_matches_single(
        path: &Path,
        sort: SortCommit,
        head: &Head,
        max_count: Option<usize>,
    ) {
        let baseline = load_commits_single(path, sort, head, &[], max_count);
        for segments in [1, 3] {
            let parallel =
                load_commits_parallel_with_segments(path, sort, head, &[], max_count, segments)
                    .expect("平行路徑不應該失敗");
            assert_eq!(
                parallel, baseline,
                "segments = {segments} 的結果跟單一 process 不同"
            );
        }
    }

    #[test]
    fn parallel_load_matches_single_process_across_sort_orders() {
        let dir = build_branching_repo();
        let path = dir.path();
        let (_, head) = load_refs(path);

        for sort in [SortCommit::Chronological, SortCommit::Topological] {
            assert_parallel_matches_single(path, sort, &head, None);
        }
    }

    /// `max_count` 小於段數：`segments = 3` 但只有 2 筆，效果上會被壓成
    /// `segments = 2`——守住「空段一律不 spawn」，不然空的 stdin 會讓
    /// `git log --stdin` 自己補上一筆 HEAD。
    #[test]
    fn parallel_load_with_max_count_smaller_than_segments() {
        let dir = build_branching_repo();
        let path = dir.path();
        let (_, head) = load_refs(path);

        assert_parallel_matches_single(path, SortCommit::Chronological, &head, Some(2));
    }

    #[test]
    fn parallel_load_matches_single_process_with_detached_head() {
        let dir = build_branching_repo();
        let path = dir.path();
        let rev = git_env(path, &["rev-parse", "b1"], &[]);
        let target = String::from_utf8_lossy(&rev.stdout).trim().to_string();
        git_env(path, &["checkout", "-q", &target], &[]);

        let (_, head) = load_refs(path);
        assert!(matches!(head, Head::Detached { .. }), "head = {head:?}");
        assert_parallel_matches_single(path, SortCommit::Chronological, &head, None);
    }

    /// unborn 分支（`--orphan`，還沒有任何 commit）加上其他有 commit 的
    /// 分支：`show-ref --head` 印不出 HEAD 那一行，`load_refs` 因此判定
    /// `Head::None`，但 `--branches` 仍然吃得到 main／b1／b2。
    #[test]
    fn parallel_load_matches_single_process_with_unborn_head() {
        let dir = build_branching_repo();
        let path = dir.path();
        git_env(path, &["checkout", "-q", "--orphan", "unborn"], &[]);

        let (_, head) = load_refs(path);
        assert!(matches!(head, Head::None), "head = {head:?}");
        assert_parallel_matches_single(path, SortCommit::Chronological, &head, None);
    }

    /// commit subject 帶一個單獨的 latin1 byte（0xE9），不是合法 UTF-8。
    /// `parse_commit_line` 用 `String::from_utf8_lossy`，平行段落跟單一
    /// process 呼叫的是同一個函式，這裡釘住兩條路徑對非法 byte 的處理
    /// 逐欄位相同。
    #[test]
    fn parallel_load_matches_single_process_with_non_utf8_subject() {
        let dir = build_branching_repo();
        let path = dir.path();
        let mut message = b"latin1-".to_vec();
        message.push(0xE9);
        message.extend_from_slice(b"-subject");
        commit_with_raw_message(path, "2026-01-01T00:00:05+00:00", &message);

        let (_, head) = load_refs(path);
        assert_parallel_matches_single(path, SortCommit::Chronological, &head, None);
    }

    #[test]
    fn has_commit_graph_reflects_whether_it_was_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        git_env(path, &["init", "-q", "-b", "main"], &[]);
        commit_at(path, "A", "2026-01-01T00:00:01+00:00");

        assert!(!has_commit_graph(path));

        git_env(path, &["commit-graph", "write", "--reachable"], &[]);

        assert!(has_commit_graph(path));
    }

    // ── fingerprint：Phase 6 背景重載拿它判斷「資料沒變」 ──

    #[test]
    fn fingerprint_is_stable_across_reloads_of_the_same_state() {
        let dir = build_branching_repo();
        let path = dir.path();
        let repo1 = Repository::load(path, SortCommit::Chronological, None).unwrap();
        let repo2 = Repository::load(path, SortCommit::Chronological, None).unwrap();
        assert_eq!(repo1.fingerprint(), repo2.fingerprint());
    }

    #[test]
    fn fingerprint_changes_when_a_tag_is_added() {
        let dir = build_branching_repo();
        let path = dir.path();
        let before = Repository::load(path, SortCommit::Chronological, None)
            .unwrap()
            .fingerprint();

        git_env(path, &["tag", "v1.0"], &[]);

        let after = Repository::load(path, SortCommit::Chronological, None)
            .unwrap()
            .fingerprint();
        assert_ne!(before, after, "多一個 tag，指紋不該不變");
    }

    #[test]
    fn fingerprint_changes_when_head_moves() {
        let dir = build_branching_repo();
        let path = dir.path();
        let before = Repository::load(path, SortCommit::Chronological, None)
            .unwrap()
            .fingerprint();

        git_env(path, &["checkout", "-q", "b1"], &[]);

        let after = Repository::load(path, SortCommit::Chronological, None)
            .unwrap()
            .fingerprint();
        assert_ne!(before, after, "HEAD 換了分支，指紋不該不變");
    }

    #[test]
    fn fingerprint_changes_when_a_commit_is_added() {
        let dir = build_branching_repo();
        let path = dir.path();
        let before = Repository::load(path, SortCommit::Chronological, None)
            .unwrap()
            .fingerprint();

        commit_at(path, "E", "2026-01-01T00:00:05+00:00");

        let after = Repository::load(path, SortCommit::Chronological, None)
            .unwrap()
            .fingerprint();
        assert_ne!(before, after, "多一個 commit，指紋不該不變");
    }
}

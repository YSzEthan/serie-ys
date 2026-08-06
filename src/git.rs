use std::{
    hash::Hash,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
};

use chrono::{DateTime, FixedOffset};
use rustc_hash::FxHashMap;

use crate::Result;

const GIT_EMPTY_TREE_HASH: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

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

#[derive(Debug, Default, Clone)]
pub enum CommitType {
    #[default]
    Commit,
    Stash,
}

#[derive(Debug, Default, Clone)]
pub struct Commit {
    pub commit_hash: CommitHash,
    pub author_name: String,
    pub author_email: String,
    pub author_date: DateTime<FixedOffset>,
    pub committer_name: String,
    pub committer_email: String,
    pub committer_date: DateTime<FixedOffset>,
    pub subject: String,
    pub body: String,
    pub parent_commit_hashes: Vec<CommitHash>,
    pub commit_type: CommitType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefType {
    Tag,
    Branch,
    RemoteBranch,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
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

#[derive(Debug, Clone)]
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

type CommitIndex = FxHashMap<CommitHash, usize>;
type CommitsMap = FxHashMap<CommitHash, Vec<CommitHash>>;

type RefMap = FxHashMap<CommitHash, Vec<Ref>>;

#[derive(Debug)]
pub struct Repository {
    path: PathBuf,
    commits: Vec<Commit>,
    commit_index: CommitIndex,

    children_map: CommitsMap,

    ref_map: RefMap,
    head: Head,
    working_changes: WorkingChanges,
}

impl Repository {
    pub fn load(path: &Path, sort: SortCommit, max_count: Option<usize>) -> Result<Self> {
        check_git_repository(path)?;

        let (mut ref_map, head) = load_refs(path);

        let stashes = load_all_stashes(path);
        let commits = load_all_commits(path, sort, &head, &stashes, max_count);
        if commits.is_empty() {
            return Err("no commits in the repository".into());
        }

        let commits = merge_stashes_to_commits(commits, stashes);

        let children_map = build_children_map(&commits);
        let commit_index = build_commit_index(&commits);

        let stash_ref_map = load_stashes_as_refs(path);
        merge_ref_maps(&mut ref_map, stash_ref_map);

        let working_changes = load_working_changes(path)?;

        Ok(Self::new(
            path.to_path_buf(),
            commits,
            commit_index,
            children_map,
            ref_map,
            head,
            working_changes,
        ))
    }

    pub fn new(
        path: PathBuf,
        commits: Vec<Commit>,
        commit_index: CommitIndex,
        children_map: CommitsMap,
        ref_map: RefMap,
        head: Head,
        working_changes: WorkingChanges,
    ) -> Self {
        Self {
            path,
            commits,
            commit_index,
            children_map,
            ref_map,
            head,
            working_changes,
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

    /// 比較 commit hash 序列，檢查 commit 圖是否有變化。
    pub fn same_commits(&self, other: &Self) -> bool {
        self.commits
            .iter()
            .map(|c| &c.commit_hash)
            .eq(other.commits.iter().map(|c| &c.commit_hash))
    }

    /// 從另一個 repository 更新 refs、head 與 working changes，
    /// commits 與衍生資料（index、children_map）維持不變。
    pub fn update_metadata_from(&mut self, other: Self) {
        self.ref_map = other.ref_map;
        self.head = other.head;
        self.working_changes = other.working_changes;
    }

    pub fn parents_hash(&self, commit_hash: &CommitHash) -> Vec<&CommitHash> {
        self.commit(commit_hash)
            .map(|c| c.parent_commit_hashes.iter().collect())
            .unwrap_or_default()
    }

    pub fn children_hash(&self, commit_hash: &CommitHash) -> Vec<&CommitHash> {
        self.children_map
            .get(commit_hash)
            .map(|hs| hs.iter().collect::<Vec<&CommitHash>>())
            .unwrap_or_default()
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

    pub fn working_changes(&self) -> &WorkingChanges {
        &self.working_changes
    }

    pub fn commit_detail(&self, commit_hash: &CommitHash) -> (&Commit, Vec<FileChange>) {
        let commit = self.commit(commit_hash).unwrap();
        let changes = if commit.parent_commit_hashes.is_empty() {
            get_initial_commit_additions(&self.path, commit_hash)
        } else {
            get_diff_summary(&self.path, commit_hash)
        };
        (commit, changes)
    }

    /// 回傳 commit 與其 refs，不會為了檔案變更而另外啟動 git 子行程。
    pub fn commit_refs(&self, commit_hash: &CommitHash) -> (&Commit, Vec<Ref>) {
        let commit = self.commit(commit_hash).unwrap();
        let refs = self.refs(commit_hash).into_iter().cloned().collect();
        (commit, refs)
    }
}

fn check_git_repository(path: &Path) -> Result<()> {
    if !is_inside_work_tree(path) && !is_bare_repository(path) {
        let msg = "not a git repository (or any of the parent directories)";
        return Err(msg.into());
    }
    Ok(())
}

pub fn is_inside_work_tree(path: &Path) -> bool {
    Command::new("git")
        .arg("rev-parse")
        .arg("--is-inside-work-tree")
        .current_dir(path)
        .output()
        .map(|o| o.status.success() && o.stdout == b"true\n")
        .unwrap_or(false)
}

fn is_bare_repository(path: &Path) -> bool {
    Command::new("git")
        .arg("rev-parse")
        .arg("--is-bare-repository")
        .current_dir(path)
        .output()
        .map(|o| o.status.success() && o.stdout == b"true\n")
        .unwrap_or(false)
}

fn load_all_commits(
    path: &Path,
    sort: SortCommit,
    head: &Head,
    stashes: &[Commit],
    max_count: Option<usize>,
) -> Vec<Commit> {
    let mut cmd = Command::new("git");
    cmd.arg("log");

    cmd.arg(match sort {
        SortCommit::Chronological => "--date-order",
        SortCommit::Topological => "--topo-order",
    })
    .arg(format!("--pretty={}", load_commits_format()))
    .arg("--date=iso-strict")
    .arg("-z"); // 用 NUL 作為分隔符

    // 排除 stash 及其他 refs
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

    cmd.current_dir(path).stdout(Stdio::piped());

    let mut process = cmd.spawn().unwrap();

    let stdout = process.stdout.take().expect("failed to open stdout");

    let reader = BufReader::new(stdout);

    let mut commits = Vec::new();

    for bytes in reader.split(b'\0') {
        let bytes = bytes.unwrap();
        let s = String::from_utf8_lossy(&bytes);

        if let Some(commit) = parse_commit_line(&s, CommitType::Commit) {
            commits.push(commit);
        }
    }

    process.wait().unwrap();

    commits
}

fn parse_commit_line(s: &str, commit_type: CommitType) -> Option<Commit> {
    let mut parts = s.splitn(10, '\x1f');
    let commit_hash = parts.next()?;
    let author_name = parts.next()?;
    let author_email = parts.next()?;
    let author_date = parts.next()?;
    let committer_name = parts.next()?;
    let committer_email = parts.next()?;
    let committer_date = parts.next()?;
    let subject = parts.next()?;
    let body = parts.next()?;
    let parents = parts.next()?;
    Some(Commit {
        commit_hash: commit_hash.into(),
        author_name: author_name.into(),
        author_email: author_email.into(),
        author_date: parse_iso_date(author_date),
        committer_name: committer_name.into(),
        committer_email: committer_email.into(),
        committer_date: parse_iso_date(committer_date),
        subject: crate::emoji::expand(subject).into_owned(),
        body: crate::emoji::expand(body).into_owned(),
        parent_commit_hashes: parse_parent_commit_hashes(parents),
        commit_type,
    })
}

fn load_all_stashes(path: &Path) -> Vec<Commit> {
    let mut cmd = Command::new("git")
        .arg("stash")
        .arg("list")
        .arg(format!("--pretty={}", load_commits_format()))
        .arg("--date=iso-strict")
        .arg("-z") // 用 NUL 作為分隔符
        .current_dir(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdout = cmd.stdout.take().expect("failed to open stdout");

    let reader = BufReader::new(stdout);

    let mut commits = Vec::new();

    for bytes in reader.split(b'\0') {
        let bytes = bytes.unwrap();
        let s = String::from_utf8_lossy(&bytes);

        if let Some(commit) = parse_commit_line(&s, CommitType::Stash) {
            commits.push(commit);
        }
    }

    cmd.wait().unwrap();

    commits
}

fn load_commits_format() -> String {
    [
        "%H", "%an", "%ae", "%ad", "%cn", "%ce", "%cd", "%s", "%b", "%P",
    ]
    .join("%x1f") // 用 Unit Separator 作為分隔符
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

fn build_children_map(commits: &[Commit]) -> CommitsMap {
    let mut children_map: CommitsMap = FxHashMap::default();
    for commit in commits {
        let hash = &commit.commit_hash;
        for parent_hash in &commit.parent_commit_hashes {
            children_map
                .entry(parent_hash.clone())
                .or_default()
                .push(hash.clone());
        }
    }
    children_map
}

fn build_commit_index(commits: &[Commit]) -> CommitIndex {
    commits
        .iter()
        .enumerate()
        .map(|(i, commit)| (commit.commit_hash.clone(), i))
        .collect()
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
    let mut cmd = Command::new("git")
        .arg("show-ref")
        .arg("--head")
        .arg("--dereference")
        .current_dir(path)
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
    let mut cmd = Command::new("git")
        .arg("stash")
        .arg("list")
        .arg(format!("--format={format}"))
        .current_dir(path)
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
    let mut cmd = Command::new("git")
        .arg("branch")
        .arg("--show-current")
        .current_dir(path)
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
}

impl FileChange {
    pub fn path(&self) -> &str {
        match self {
            FileChange::Add { path, .. }
            | FileChange::Modify { path, .. }
            | FileChange::Delete { path, .. } => path,
        }
    }

    pub fn stats(&self) -> Option<(usize, usize)> {
        match self {
            FileChange::Add { stats, .. }
            | FileChange::Modify { stats, .. }
            | FileChange::Delete { stats, .. } => *stats,
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

pub fn load_working_changes(path: &Path) -> Result<WorkingChanges> {
    let mut cmd = Command::new("git")
        .arg("status")
        .arg("--porcelain=v1")
        .arg("--untracked-files=all")
        .current_dir(path)
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
            unstaged.push(FileChange::Add {
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

    let _ = cmd.wait();

    // 取得 unstaged 變更的 numstat
    let unstaged_stats = get_diff_numstat(path, &[]);
    apply_numstat(&mut unstaged, &unstaged_stats);

    // 取得 staged 變更的 numstat
    let staged_stats = get_diff_numstat(path, &["--cached"]);
    apply_numstat(&mut staged, &staged_stats);

    Ok(WorkingChanges { staged, unstaged })
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
    let mut cmd_args = vec!["diff", "--numstat"];
    cmd_args.extend_from_slice(args);

    let mut cmd = Command::new("git")
        .args(&cmd_args)
        .current_dir(path)
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
                | FileChange::Delete { stats: st, .. } => {
                    *st = Some(s);
                }
            }
        }
    }
}

pub fn get_diff_summary(path: &Path, commit_hash: &CommitHash) -> Vec<FileChange> {
    let parent_arg = format!("{}^", commit_hash.as_str());
    let mut cmd = Command::new("git")
        .arg("diff")
        .arg("--name-status")
        .arg(&parent_arg)
        .arg(commit_hash.as_str())
        .current_dir(path)
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
    let mut cmd = Command::new("git")
        .arg("ls-tree")
        .arg("--name-status")
        .arg("-r")
        .arg(commit_hash.as_str())
        .current_dir(path)
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

    #[test]
    fn parse_commit_line_expands_emoji_shortcodes() {
        let date = "2026-07-31T10:00:00+08:00";
        let line = [
            "abc1234",
            "Alice",
            "alice@example.com",
            date,
            "Alice",
            "alice@example.com",
            date,
            ":tada: 上線",
            "細節 :+1:",
            "",
        ]
        .join("\x1f");

        let commit = parse_commit_line(&line, CommitType::Commit).unwrap();

        assert_eq!(commit.subject, "🎉 上線");
        assert_eq!(commit.body, "細節 👍");
    }
}

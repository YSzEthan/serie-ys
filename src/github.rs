use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::Duration;

use rust_i18n::t;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Deserializer};

use crate::process::run_with_timeout;

/// 反序列化時就把 emoji shortcode 展開。掛在欄位定義上而不是在 `into_gh_*` 裡逐處
/// 賦值：`GhRelatedIssue` / `GhCommit` 是共用型別，一個宣告覆蓋所有引用點，
/// `parse_timeline_graphql` 這種直接把 serde 產物遞出去的路徑也不必改。
fn de_expand_emoji<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let s = String::deserialize(d)?;
    Ok(crate::emoji::expand(&s).into_owned())
}

// ── 分頁回傳 ──

pub struct GhPage<T> {
    pub items: Vec<T>,
    /// Some(cursor) 代表還有下一頁；None 代表已到底
    pub next_cursor: Option<String>,
}

// ── 項目種類 ──

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GhItemKind {
    Issue,
    PullRequest,
}

impl GhItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            GhItemKind::Issue => "issue",
            GhItemKind::PullRequest => "pr",
        }
    }
}

// ── 查詢狀態篩選 ──

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StateFilter {
    #[default]
    Open,
    Closed,
    All,
}

impl StateFilter {
    pub fn next(self) -> Self {
        match self {
            StateFilter::Open => StateFilter::Closed,
            StateFilter::Closed => StateFilter::All,
            StateFilter::All => StateFilter::Open,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            StateFilter::Open => "open",
            StateFilter::Closed => "closed",
            StateFilter::All => "all",
        }
    }

    /// 顯示用文字。`as_str` 是 gh argv 的邏輯值，不能拿來顯示。
    pub fn label(self) -> Cow<'static, str> {
        match self {
            StateFilter::Open => t!("github.state.open"),
            StateFilter::Closed => t!("github.state.closed"),
            StateFilter::All => t!("github.state.all"),
        }
    }
}

// ── 列表項目 ──

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GhLabel {
    pub name: String,
    pub color: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GhAuthor {
    pub login: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GhRelatedIssue {
    pub number: u64,
    #[serde(default, deserialize_with = "de_expand_emoji")]
    pub title: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub url: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
pub struct GhIssue {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub labels: Vec<GhLabel>,
    pub author: GhAuthor,
    pub created_at: String,
    pub body: String,
    pub url: String,
    pub closed_at: Option<String>,
    pub updated_at: String,
    pub parent: Option<GhRelatedIssue>,
    pub sub_issues: Vec<GhRelatedIssue>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GhPullRequest {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub labels: Vec<GhLabel>,
    pub author: GhAuthor,
    pub head_ref_name: String,
    /// PR head branch 目前指向的 commit id（GraphQL `headRefOid`）。用來判斷
    /// 本地同名分支的 tip 是不是就是這次被 merge 的版本——相同才能安全強刪
    /// 本地分支，不必依賴 `git branch -d` 自己的 reachability 判斷（見
    /// `app::local_branch_delete_check`）。
    pub head_ref_oid: String,
    pub base_ref_name: String,
    pub is_draft: bool,
    pub head_branch_deletable: bool,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub closed_at: Option<String>,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default, rename = "closingIssuesReferences")]
    pub linked_issues: Vec<GhRelatedIssue>,
    pub diff_stat: DiffStat,
}

/// PR 的變更行數，跟 `baseRefName` 一樣是 PR 本身的屬性，隨 PR 清單查詢
/// 一起拿——不像 `mergeable` 是 GitHub 事後才算出來、會變動的狀態，不需要
/// 額外走 timeline 查詢。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffStat {
    pub additions: u32,
    pub deletions: u32,
}

/// GitHub issue／PR 列表的完整快照，`App` 與 `GitHubView` 之間交接資料用的
/// 唯一封包。同一時刻只有一個擁有者——view 開著時資料活在 `GitHubView`
/// 自己的欄位裡，關閉時 take 出來裝進這個結構體暫存，兩邊不會同時各存一份。
#[derive(Debug, Default)]
pub struct GitHubData {
    pub issues: Vec<GhIssue>,
    pub pull_requests: Vec<GhPullRequest>,
    pub state_filter: StateFilter,
    pub issues_next_cursor: Option<String>,
    pub prs_next_cursor: Option<String>,
}

// ── CLI 包裝 ──

const GH_TIMEOUT: Duration = Duration::from_secs(20);

fn run_gh(path: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("gh");
    cmd.args(args).current_dir(path);

    let output = run_with_timeout(cmd, None, GH_TIMEOUT)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(t!("github.error.gh_failed", detail = stderr).into_owned());
    }

    String::from_utf8(output.stdout)
        .map_err(|e| t!("github.error.invalid_utf8", detail = e).into_owned())
}

pub fn list_issues(
    path: &Path,
    state: &str,
    after: Option<&str>,
) -> Result<GhPage<GhIssue>, String> {
    let (owner, name) = fetch_repo_name_with_owner(path)?;
    let states = match state {
        "open" => "[OPEN]",
        "closed" => "[CLOSED]",
        _ => "[OPEN, CLOSED]",
    };
    let query = format!(
        r#"query($owner:String!,$name:String!,$after:String){{
            repository(owner:$owner,name:$name){{
                issues(first:50,after:$after,states:{states},orderBy:{{field:CREATED_AT,direction:DESC}}){{
                    pageInfo {{ hasNextPage endCursor }}
                    nodes {{
                        number title state body url createdAt closedAt updatedAt
                        author {{ login }}
                        labels(first:20) {{ nodes {{ name color }} }}
                        parent {{ number title state url }}
                        subIssues(first:20) {{ nodes {{ number title state url }} }}
                    }}
                }}
            }}
        }}"#
    );
    let owner_f = format!("owner={owner}");
    let name_f = format!("name={name}");
    let query_f = format!("query={query}");
    let mut args = vec![
        "api", "graphql", "-F", &owner_f, "-F", &name_f, "-f", &query_f,
    ];
    let after_f;
    if let Some(cursor) = after {
        after_f = format!("after={cursor}");
        args.push("-f");
        args.push(&after_f);
    }
    let json = run_gh(path, &args)?;
    parse_issues_graphql(&json)
}

/// key 是 repo path，value 是 `(owner, name)`。只快取成功結果——第一次
/// 因為沒網路失敗時不能把失敗記起來，否則這個 process 之後再也不會重試。
///
/// 用 map 而不是 `OnceLock`：實務上這個 process 只服務一個 repo
/// （`lib.rs` 只 `Repository::load` 一次），但 `path` 是這個函式的參數，
/// 用 `OnceLock` 等於對簽名說謊——哪天真的有第二個 path 進來會安靜地
/// 回錯的值。
static REPO_NAME_CACHE: LazyLock<Mutex<FxHashMap<PathBuf, (String, String)>>> =
    LazyLock::new(|| Mutex::new(FxHashMap::default()));

/// 不用 `expect`：快取不需要 poisoning 語意，毒化了照樣可以用。用
/// `expect` 的話，任何一次 panic 都會讓這個 process 之後每一次 GitHub
/// 呼叫都 panic，GitHub 功能永久壞掉還看起來像網路問題。
fn repo_name_cache() -> MutexGuard<'static, FxHashMap<PathBuf, (String, String)>> {
    REPO_NAME_CACHE.lock().unwrap_or_else(|e| e.into_inner())
}

/// **絕不持鎖橫跨 `run_gh`**：`run_gh` 現在最長會跑滿 `GH_TIMEOUT`
/// （20 秒），持鎖跑它的話網路死掉時第二個 caller 要先等 20 秒拿鎖、
/// 再跑自己的 20 秒，最壞情況直接翻倍——這個函式存在的目的就是不讓
/// 等待時間被放大。代價：process 生命週期內最多一次多餘的
/// `gh repo view`；快取在 process 生命週期內不失效（中途改 base repo
/// 要重啟才生效，罕見動作，可接受）。
fn fetch_repo_name_with_owner(path: &Path) -> Result<(String, String), String> {
    if let Some(hit) = repo_name_cache().get(path) {
        return Ok(hit.clone());
    }

    let out = run_gh(
        path,
        &[
            "repo",
            "view",
            "--json",
            "nameWithOwner",
            "--jq",
            ".nameWithOwner",
        ],
    )?;
    let s = out.trim();
    let (owner, name) = s
        .split_once('/')
        .ok_or_else(|| t!("github.error.unexpected_repo", value = s).into_owned())?;
    let entry = (owner.to_string(), name.to_string());

    repo_name_cache().insert(path.to_path_buf(), entry.clone());
    Ok(entry)
}

/// 解析 gh GraphQL 回傳的 JSON；失敗統一包成 `github.error.json_parse`。
fn parse_json<T: serde::de::DeserializeOwned>(json: &str) -> Result<T, String> {
    serde_json::from_str(json).map_err(|e| t!("github.error.json_parse", detail = e).into_owned())
}

fn parse_issues_graphql(json: &str) -> Result<GhPage<GhIssue>, String> {
    let resp: GqlIssuesResp = parse_json(json)?;
    let list = resp.data.repository.issues;
    let next_cursor = list.page_info.next_cursor();
    Ok(GhPage {
        items: list
            .nodes
            .into_iter()
            .map(GqlIssueNode::into_gh_issue)
            .collect(),
        next_cursor,
    })
}

// ── GraphQL 回應包裝型別 ──

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct GqlPageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

impl GqlPageInfo {
    /// 只有 `has_next_page` 為真才回游標：`end_cursor` 在最後一頁也可能是非 null。
    fn next_cursor(self) -> Option<String> {
        self.has_next_page.then_some(self.end_cursor).flatten()
    }
}

#[derive(Deserialize)]
struct GqlIssuesResp {
    data: GqlIssuesData,
}
#[derive(Deserialize)]
struct GqlIssuesData {
    repository: GqlIssuesRepo,
}
#[derive(Deserialize)]
struct GqlIssuesRepo {
    issues: GqlConnection<GqlIssueNode>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlIssueNode {
    number: u64,
    #[serde(deserialize_with = "de_expand_emoji")]
    title: String,
    state: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    url: Option<String>,
    created_at: String,
    #[serde(default)]
    closed_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
    author: Option<GhAuthor>,
    labels: GqlConnection<GhLabel>,
    #[serde(default)]
    parent: Option<GhRelatedIssue>,
    sub_issues: GqlConnection<GhRelatedIssue>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlConnection<T> {
    /// 沒查 `pageInfo` 的 connection（labels 等）拿到 `has_next_page = false`。
    /// `nodes` 刻意不加 default：查詢漏寫 `nodes` 要報錯，不要悄悄變空。
    #[serde(default)]
    page_info: GqlPageInfo,
    nodes: Vec<T>,
}
/// 手寫而非 `#[derive(Default)]`：derive 會加上多餘的 `T: Default` bound，
/// 但一個空 `Vec<T>` 從不需要 `T` 本身可以是預設值。
impl<T> Default for GqlConnection<T> {
    fn default() -> Self {
        GqlConnection {
            page_info: GqlPageInfo::default(),
            nodes: Vec::new(),
        }
    }
}

impl GqlIssueNode {
    fn into_gh_issue(self) -> GhIssue {
        GhIssue {
            number: self.number,
            title: self.title,
            state: self.state,
            labels: self.labels.nodes,
            author: self.author.unwrap_or(GhAuthor {
                login: "ghost".to_string(),
            }),
            created_at: self.created_at,
            body: self.body.unwrap_or_default(),
            url: self.url.unwrap_or_default(),
            closed_at: self.closed_at,
            updated_at: self.updated_at.unwrap_or_default(),
            parent: self.parent,
            sub_issues: self.sub_issues.nodes,
        }
    }
}

pub fn list_pull_requests(
    path: &Path,
    state: &str,
    after: Option<&str>,
) -> Result<GhPage<GhPullRequest>, String> {
    let (owner, name) = fetch_repo_name_with_owner(path)?;
    let states = match state {
        "open" => "[OPEN]",
        "closed" => "[CLOSED, MERGED]",
        _ => "[OPEN, CLOSED, MERGED]",
    };
    let query = format!(
        r#"query($owner:String!,$name:String!,$after:String){{
            repository(owner:$owner,name:$name){{
                defaultBranchRef {{ name }}
                pullRequests(first:50,after:$after,states:{states},orderBy:{{field:CREATED_AT,direction:DESC}}){{
                    pageInfo {{ hasNextPage endCursor }}
                    nodes {{
                        number title state body url closedAt updatedAt headRefName headRefOid baseRefName isDraft isCrossRepository additions deletions
                        author {{ login }}
                        labels(first:20) {{ nodes {{ name color }} }}
                        closingIssuesReferences(first:20) {{ nodes {{ number title state url }} }}
                    }}
                }}
            }}
        }}"#
    );
    let owner_f = format!("owner={owner}");
    let name_f = format!("name={name}");
    let query_f = format!("query={query}");
    let mut args = vec![
        "api", "graphql", "-F", &owner_f, "-F", &name_f, "-f", &query_f,
    ];
    let after_f;
    if let Some(cursor) = after {
        after_f = format!("after={cursor}");
        args.push("-f");
        args.push(&after_f);
    }
    let json = run_gh(path, &args)?;
    parse_prs_graphql(&json)
}

fn parse_prs_graphql(json: &str) -> Result<GhPage<GhPullRequest>, String> {
    let resp: GqlPrsResp = parse_json(json)?;
    let repo = resp.data.repository;
    let default_branch = repo.default_branch_ref.map(|r| r.name);
    let list = repo.pull_requests;
    let next_cursor = list.page_info.next_cursor();
    Ok(GhPage {
        items: list
            .nodes
            .into_iter()
            .map(|node| node.into_gh_pr(default_branch.as_deref()))
            .collect(),
        next_cursor,
    })
}

/// head branch 可刪的條件。`head != base` 在同 repo PR 下永遠成立
/// （GitHub 不接受 head == base 的 PR），留著是防禦性斷言，不是走得到的路。
fn head_branch_deletable(
    is_cross: bool,
    head: &str,
    base: &str,
    default_branch: Option<&str>,
) -> bool {
    !is_cross && head != base && default_branch != Some(head)
}

// ── GraphQL PR 回應包裝型別 ──

#[derive(Deserialize)]
struct GqlPrsResp {
    data: GqlPrsData,
}
#[derive(Deserialize)]
struct GqlPrsData {
    repository: GqlPrsRepo,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlPrsRepo {
    #[serde(default)]
    default_branch_ref: Option<GqlRefName>,
    pull_requests: GqlConnection<GqlPrNode>,
}
#[derive(Deserialize)]
struct GqlRefName {
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlPrNode {
    number: u64,
    #[serde(deserialize_with = "de_expand_emoji")]
    title: String,
    state: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    url: Option<String>,
    head_ref_name: String,
    head_ref_oid: String,
    base_ref_name: String,
    is_draft: bool,
    is_cross_repository: bool,
    #[serde(default)]
    closed_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
    author: Option<GhAuthor>,
    labels: GqlConnection<GhLabel>,
    closing_issues_references: GqlConnection<GhRelatedIssue>,
    #[serde(flatten)]
    diff_stat: DiffStat,
}

impl GqlPrNode {
    fn into_gh_pr(self, default_branch: Option<&str>) -> GhPullRequest {
        let head_branch_deletable = head_branch_deletable(
            self.is_cross_repository,
            &self.head_ref_name,
            &self.base_ref_name,
            default_branch,
        );
        GhPullRequest {
            number: self.number,
            title: self.title,
            state: self.state,
            labels: self.labels.nodes,
            author: self.author.unwrap_or(GhAuthor {
                login: "ghost".to_string(),
            }),
            head_ref_name: self.head_ref_name,
            head_ref_oid: self.head_ref_oid,
            base_ref_name: self.base_ref_name,
            is_draft: self.is_draft,
            head_branch_deletable,
            body: self.body.unwrap_or_default(),
            url: self.url.unwrap_or_default(),
            closed_at: self.closed_at,
            updated_at: self.updated_at.unwrap_or_default(),
            linked_issues: self.closing_issues_references.nodes,
            diff_stat: self.diff_stat,
        }
    }
}

// ── Timeline（留言與 commit，依 GitHub 原生順序交錯排列）──

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "__typename")]
pub enum GhTimelineItem {
    IssueComment {
        #[serde(default)]
        body: String,
        #[serde(default, rename = "createdAt")]
        created_at: String,
        author: Option<GhAuthor>,
    },
    PullRequestCommit {
        commit: GhCommit,
    },
    PullRequestReview {
        #[serde(default)]
        state: String,
        #[serde(default)]
        body: String,
        /// PENDING（尚未 submit 的草稿，只有作者自己看得到）時是 `None`——
        /// 過濾靠這個欄位，不比對 `state` 字串。
        #[serde(default, rename = "submittedAt")]
        submitted_at: Option<String>,
        author: Option<GhAuthor>,
        #[serde(default)]
        comments: GhReviewCommentConn,
    },
    /// `itemTypes` 理論上只會產生上面三種 variant，但把整頁的反序列化都賭在
    /// GitHub 永遠不會新增第四種上並不值得 —— 否則一個未知的 `__typename` 會讓
    /// 每個 node 都失敗，而不只是這一個。
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GhCommit {
    pub abbreviated_oid: String,
    #[serde(deserialize_with = "de_expand_emoji")]
    pub message_headline: String,
    #[serde(default)]
    pub status_check_rollup: Option<GhStatusCheckRollup>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GhStatusCheckRollup {
    pub state: String,
}

/// 單一 CI check 的結果。宣告順序即排序：fail → pending → pass，收合截斷時優先丟掉綠色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CheckState {
    Failed,
    Pending,
    Passed,
}

/// PR head commit 上的一個 check。`head_ci_checks` 已經去重、判定狀態、
/// 排序完畢，view 層拿到的就是最終清單，不再判斷狀態字串。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhCheck {
    pub name: String,
    pub state: CheckState,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GhReviewCommentConn {
    #[serde(default)]
    pub total_count: usize,
    #[serde(default)]
    pub nodes: Vec<GhReviewComment>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GhReviewComment {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub path: String,
    /// 留言所在行被後續 commit 改掉時是 `None`（PR 一 rebase 就常見）；
    /// `parse_timeline_graphql` 會 fallback 到 `original_line`，所以
    /// view 層看到的已經是收斂後的單一 `Option<u32>`。
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default, rename = "originalLine")]
    pub original_line: Option<u32>,
    #[serde(default)]
    pub outdated: bool,
    #[serde(default)]
    pub body: String,
    /// 是否已標記解決——GraphQL 本身不在這個型別上提供，是
    /// `parse_timeline_graphql` 拿同一次查詢多帶的 `reviewThreads` 回填的。
    #[serde(default)]
    pub resolved: bool,
}

/// PR 目前是否可以合併。`UNKNOWN`（GitHub 惰性計算出的第三種狀態，
/// 例如仍在檢查中，或該 PR 已經合併／關閉）與「根本不是 PR」
/// 兩種情況都會併入 `None` —— 都代表「不要顯示標記」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mergeable {
    Mergeable,
    Conflicting,
}

impl Mergeable {
    fn from_api(state: &str) -> Option<Self> {
        match state {
            "MERGEABLE" => Some(Mergeable::Mergeable),
            "CONFLICTING" => Some(Mergeable::Conflicting),
            _ => None,
        }
    }
}

#[derive(Debug, Default)]
pub struct GhTimelinePage {
    pub items: Vec<GhTimelineItem>,
    pub next_cursor: Option<String>,
    pub mergeable: Option<Mergeable>,
    /// PR head commit 的 check 清單，只有首頁（`after` 為 `None`）才查：
    /// 載入更多的頁面與 Issue 一律是空的，呼叫端只該在首頁採用，不然會把
    /// 既有清單清掉。首頁為空代表尚無 CI（例如 force-push 後的新 head）。
    pub ci_checks: Vec<GhCheck>,
}

/// 每頁 contexts 筆數（connection 上限 100），首頁與續頁共用。
const CONTEXTS_PAGE_SIZE: usize = 100;

/// 首頁之後最多再追幾頁（CI contexts、review threads 各自算）。超過就截斷：
/// 這是確定性的，重試也不會變，回 `Err` 只會讓那個 PR 的 timeline 永遠載不出來。
const MAX_EXTRA_PAGES: usize = 10;

/// 每頁 review thread 筆數（connection 上限 100），首頁與續頁共用。
const THREADS_PAGE_SIZE: usize = 100;

/// review thread 要取的欄位，首頁與續頁查詢共用，不會漂移。只取 comment id：
/// `PullRequestReviewComment` 沒有 resolved 欄位，只能靠 thread 反查。
///
/// thread 內的留言取前 100 則，單一 thread 超過這個數量時，多出來的留言會被
/// 當成未 resolved（實測最長的 thread 只有 6 則）。
const REVIEW_THREADS_SELECTION: &str = r#"pageInfo { hasNextPage endCursor }
    nodes { isResolved comments(first:100) { nodes { id } } }"#;

/// 每筆 context 要取的欄位，首頁與續頁查詢共用，不會漂移。
const CHECK_CONTEXT_NODES: &str = r#"__typename
    ... on CheckRun {
        name conclusion databaseId
        checkSuite { app { slug } workflowRun { event workflow { name } } }
    }
    ... on StatusContext { context state }"#;

/// PR head commit 的 CI 查詢片段。`oid` 讓續頁能釘住同一個 commit。
fn head_commit_ci_fragment() -> String {
    format!(
        "commits(last:1) {{ nodes {{ commit {{ oid statusCheckRollup {{
            contexts(first:{CONTEXTS_PAGE_SIZE}) {{
                pageInfo {{ hasNextPage endCursor }}
                nodes {{ {CHECK_CONTEXT_NODES} }}
            }}
        }} }} }} }}"
    )
}

/// head commit 剩下的 contexts。用 `object(oid:)` 而非 `pullRequest.commits`：
/// 追頁途中被 force-push 時，游標才不會跑到另一個 commit 上。
/// 游標是 offset 式，所以追頁期間新增的 check 造成的位移防不了：重複會被
/// `head_ci_checks` 去重吃掉，漏掉的等下次重抓補回來。
fn build_contexts_query() -> String {
    format!(
        r#"query($owner:String!,$name:String!,$oid:GitObjectID!,$after:String){{
            repository(owner:$owner,name:$name){{
                object(oid:$oid){{
                    ... on Commit {{ statusCheckRollup {{
                        contexts(first:{CONTEXTS_PAGE_SIZE},after:$after) {{
                            pageInfo {{ hasNextPage endCursor }}
                            nodes {{ {CHECK_CONTEXT_NODES} }}
                        }}
                    }} }}
                }}
            }}
        }}"#
    )
}

/// 剩下的 review threads，以 PR 編號定位：thread 屬於 PR，不像 CI 會被
/// force-push 換掉，沒有東西需要釘住。游標以 `(createdAt, id)` 為鍵，追頁期間
/// 新增或刪除 thread 不會造成重複或漏抓。
fn build_review_threads_query() -> String {
    format!(
        r#"query($owner:String!,$name:String!,$number:Int!,$after:String){{
            repository(owner:$owner,name:$name){{
                pullRequest(number:$number){{
                    reviewThreads(first:{THREADS_PAGE_SIZE},after:$after) {{
                        {REVIEW_THREADS_SELECTION}
                    }}
                }}
            }}
        }}"#
    )
}

/// issue 與 PR 的 timeline 是兩個不同的 union：`Issue.timelineItems` 給的是
/// `IssueTimelineItems`，裡面既沒有 `PullRequestCommit` 也沒有 `mergeable`。
/// 對 issue 送出這些片段會讓整個查詢被 GraphQL 擋下（"Fragment on
/// PullRequestCommit can't be spread inside IssueTimelineItems"），而不是安靜地
/// 回傳空結果 —— 所以四處分岔綁在同一個 match 上，漏掉其中一項就編不過。
///
/// CI（`commits(last:1)`）只有首頁要：`first_page` 為假時省略，載入更多不必
/// 每次重抓一遍最多 100 筆 check。分岔一樣綁在 `PullRequest` arm 裡，不會
/// 產生「Issue 帶 CI」這種會被整個退回的組合。
fn build_timeline_query(kind: GhItemKind, first_page: bool) -> String {
    let (item_field, item_types, pr_only_fields, pr_fragments) = match kind {
        GhItemKind::Issue => ("issue", "ISSUE_COMMENT", String::new(), ""),
        GhItemKind::PullRequest => (
            "pullRequest",
            "ISSUE_COMMENT, PULL_REQUEST_COMMIT, PULL_REQUEST_REVIEW",
            // reviewThreads 是 resolved 狀態唯一的來源——`PullRequestReviewComment`
            // 本身沒有這個欄位，只在 `PullRequestReviewThread` 上。跟 timelineItems
            // 平行查詢，回應裡靠 comment id 對應回去（見 parse_timeline_graphql）。
            format!(
                r#"mergeable
                    reviewThreads(first:{THREADS_PAGE_SIZE}) {{ {REVIEW_THREADS_SELECTION} }}
                    {ci}"#,
                ci = if first_page {
                    head_commit_ci_fragment()
                } else {
                    String::new()
                }
            ),
            r#"... on PullRequestCommit { commit {
                                abbreviatedOid messageHeadline
                                statusCheckRollup { state }
                            } }
                            ... on PullRequestReview {
                                state body submittedAt author { login }
                                comments(first:20) {
                                    totalCount
                                    nodes { id path line originalLine outdated body }
                                }
                            }"#,
        ),
    };
    // 用 100（connection 上限）而非 50：commit 只佔一行視覺高度，
    // 而留言常常佔好幾行，混在同一頁會讓一頁實際能看到的內容打對折。
    format!(
        r#"query($owner:String!,$name:String!,$number:Int!,$after:String){{
            repository(owner:$owner,name:$name){{
                {item_field}(number:$number){{
                    {pr_only_fields}
                    timelineItems(first:100,after:$after,itemTypes:[{item_types}]){{
                        pageInfo {{ hasNextPage endCursor }}
                        nodes {{
                            __typename
                            ... on IssueComment {{ body createdAt author {{ login }} }}
                            {pr_fragments}
                        }}
                    }}
                }}
            }}
        }}"#
    )
}

pub fn get_timeline(
    path: &Path,
    number: u64,
    kind: GhItemKind,
    after: Option<&str>,
) -> Result<GhTimelinePage, String> {
    let (owner, name) = fetch_repo_name_with_owner(path)?;
    let query = build_timeline_query(kind, after.is_none());
    let owner_f = format!("owner={owner}");
    let name_f = format!("name={name}");
    let number_f = format!("number={number}");
    let query_f = format!("query={query}");
    let mut args = vec![
        "api", "graphql", "-F", &owner_f, "-F", &name_f, "-F", &number_f, "-f", &query_f,
    ];
    let after_f;
    if let Some(cursor) = after {
        after_f = format!("after={cursor}");
        args.push("-f");
        args.push(&after_f);
    }
    let json = run_gh(path, &args)?;
    // 續頁的 owner／name／oid／after 一律 `-f`：`-F` 會把純數字轉成整數，oid 有可能
    // 全是數字。唯獨 threads 的 `$number:Int!` 需要整數，那個變數用 `-F`。
    parse_timeline_graphql(&json, kind, |target, cursor| {
        let (query, flag, var) = match target {
            Target::Contexts { oid } => (build_contexts_query(), "-f", format!("oid={oid}")),
            Target::ReviewThreads => (build_review_threads_query(), "-F", number_f.clone()),
        };
        let query_f = format!("query={query}");
        let cursor_f = format!("after={cursor}");
        run_gh(
            path,
            &[
                "api", "graphql", "-f", &owner_f, "-f", &name_f, flag, &var, "-f", &cursor_f, "-f",
                &query_f,
            ],
        )
    })
}

/// 續頁要抓什麼。游標 `after` 每種都有，所以不放進 variant。
enum Target<'a> {
    /// head commit 剩下的 CI contexts；`oid` 釘住首頁那個 commit。
    Contexts { oid: &'a str },
    /// 剩下的 review threads。
    ReviewThreads,
}

/// `fetch_more(target, after)` 回傳續頁的原始 JSON，只在 head commit 的 contexts
/// 或 review threads 超過一頁時才會被呼叫。注入而非直接呼叫 `gh`，分頁邏輯才測得到。
fn parse_timeline_graphql(
    json: &str,
    kind: GhItemKind,
    mut fetch_more: impl FnMut(Target<'_>, &str) -> Result<String, String>,
) -> Result<GhTimelinePage, String> {
    let resp: GqlTimelineResp = parse_json(json)?;
    let container = match kind {
        GhItemKind::Issue => resp.data.repository.issue,
        GhItemKind::PullRequest => resp.data.repository.pull_request,
    };
    let Some(container) = container else {
        return Ok(GhTimelinePage::default());
    };
    let conn = container.timeline_items;
    let next_cursor = conn.page_info.next_cursor();
    // resolved 狀態與 line/originalLine 的收斂都在這裡做一次，view 層因此
    // 只需要認識收斂後的單一 `Option<u32>` 與 `bool`，不必知道 reviewThreads
    // 這條平行查詢的存在。
    let mut items = conn.nodes;
    let threads = collect_review_threads(container.review_threads, &items, &mut fetch_more)?;
    let resolved_ids = resolved_comment_ids(&threads);
    for item in &mut items {
        if let GhTimelineItem::PullRequestReview { comments, .. } = item {
            for c in &mut comments.nodes {
                c.line = c.line.or(c.original_line);
                c.resolved = resolved_ids.contains(c.id.as_str());
            }
        }
    }
    let ci_checks = head_ci_checks(&collect_head_contexts(container.commits, &mut fetch_more)?);
    Ok(GhTimelinePage {
        items,
        next_cursor,
        mergeable: container.mergeable.as_deref().and_then(Mergeable::from_api),
        ci_checks,
    })
}

#[derive(Deserialize)]
struct GqlTimelineResp {
    data: GqlTimelineData,
}
#[derive(Deserialize)]
struct GqlTimelineData {
    repository: GqlTimelineRepo,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlTimelineRepo {
    #[serde(default)]
    issue: Option<GqlTimelineContainer>,
    #[serde(default)]
    pull_request: Option<GqlTimelineContainer>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlTimelineContainer {
    #[serde(default)]
    mergeable: Option<String>,
    timeline_items: GqlConnection<GhTimelineItem>,
    #[serde(default)]
    review_threads: GqlConnection<GqlReviewThread>,
    #[serde(default)]
    commits: GqlConnection<GqlHeadCommit>,
}

/// resolved 狀態不在 `PullRequestReviewComment` 上，只在
/// `PullRequestReviewThread` 上——這裡把已 resolved 的 thread 底下每則
/// 留言的 id 收集起來，讓 `parse_timeline_graphql` 拿去回填。
fn resolved_comment_ids(threads: &[GqlReviewThread]) -> FxHashSet<&str> {
    threads
        .iter()
        .filter(|t| t.is_resolved)
        .flat_map(|t| t.comments.nodes.iter().map(|c| c.id.as_str()))
        .collect()
}

/// 續頁回應的 `data.repository` 外殼，底下那層（`R`）依查詢而異。
#[derive(Deserialize)]
struct GqlRepoResp<R> {
    data: GqlRepoData<R>,
}
#[derive(Deserialize)]
struct GqlRepoData<R> {
    repository: R,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlThreadsRepo {
    #[serde(default)]
    pull_request: Option<GqlThreadsPr>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlThreadsPr {
    review_threads: GqlConnection<GqlReviewThread>,
}

/// 首頁 review threads 加上追頁。回填只針對這一頁**已送出**的 review 留言
/// （view 會丟掉 PENDING，這裡也不算），所以追到這些留言的 thread 都找到就停：
/// 一則留言只屬於一個 thread，找到之後後面的頁不可能改變它的狀態。沒有這種
/// 留言時一頁都不追；找不到的 id（例如 thread 超過 100 則留言）會追到
/// `MAX_EXTRA_PAGES` 為止。
///
/// 失敗（fetch、解析、PR 找不到）是暫時性的，回 `Err` 讓使用者按 `r` 重試，
/// 訊息加前綴——不然畫面上的 `(comments failed: …)` 會誤導成留言載入失敗。
fn collect_review_threads(
    threads: GqlConnection<GqlReviewThread>,
    items: &[GhTimelineItem],
    fetch_more: &mut impl FnMut(Target<'_>, &str) -> Result<String, String>,
) -> Result<Vec<GqlReviewThread>, String> {
    let mut pending: FxHashSet<&str> = items
        .iter()
        .filter_map(|item| match item {
            GhTimelineItem::PullRequestReview {
                comments,
                submitted_at: Some(_),
                ..
            } => Some(comments.nodes.iter().map(|c| c.id.as_str())),
            _ => None,
        })
        .flatten()
        .collect();
    let need_more = |page: &[GqlReviewThread]| {
        for c in page.iter().flat_map(|t| &t.comments.nodes) {
            pending.remove(c.id.as_str());
        }
        !pending.is_empty()
    };
    collect_pages(
        &t!("github.what.review_threads"),
        threads,
        need_more,
        |after| {
            let json = fetch_more(Target::ReviewThreads, after)?;
            let resp: GqlRepoResp<GqlThreadsRepo> = parse_json(&json)?;
            resp.data
                .repository
                .pull_request
                .map(|pr| pr.review_threads)
                .ok_or_else(|| t!("github.error.pr_not_found").into_owned())
        },
    )
}

/// 依序抓完一個分頁 connection：`first` 是首頁，`fetch(after)` 抓下一頁，
/// `need_more` 看過剛收到的那一頁後決定要不要繼續（用不到提早停止就傳
/// `|_| true`）。停止條件是正規化後的游標（`GqlPageInfo::next_cursor`）加上
/// `MAX_EXTRA_PAGES`；失敗訊息以 `what` 與頁碼為前綴，首頁是第 1 頁。
fn collect_pages<T>(
    what: &str,
    first: GqlConnection<T>,
    mut need_more: impl FnMut(&[T]) -> bool,
    mut fetch: impl FnMut(&str) -> Result<GqlConnection<T>, String>,
) -> Result<Vec<T>, String> {
    let mut next = first
        .page_info
        .next_cursor()
        .filter(|_| need_more(&first.nodes));
    let mut all = first.nodes;
    for page in 2..=1 + MAX_EXTRA_PAGES {
        let Some(after) = next.take() else { break };
        let conn = fetch(&after).map_err(|e| {
            t!(
                "github.error.page_failed",
                what = what,
                page = page,
                detail = e
            )
            .into_owned()
        })?;
        next = conn
            .page_info
            .next_cursor()
            .filter(|_| need_more(&conn.nodes));
        all.extend(conn.nodes);
    }
    Ok(all)
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlReviewThread {
    #[serde(default)]
    is_resolved: bool,
    #[serde(default)]
    comments: GqlConnection<GqlReviewThreadCommentId>,
}
#[derive(Deserialize)]
struct GqlReviewThreadCommentId {
    #[serde(default)]
    id: String,
}

// head commit 的 check 清單：`commits(last:1)` 只會有 0 或 1 個 node。
#[derive(Deserialize)]
struct GqlHeadCommit {
    commit: GqlHeadCommitInner,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlHeadCommitInner {
    /// 續頁用來釘住 commit（見 `build_contexts_query`）。
    #[serde(default)]
    oid: String,
    #[serde(default)]
    status_check_rollup: Option<GqlRollupContexts>,
}
#[derive(Deserialize)]
struct GqlRollupContexts {
    /// 首頁與續頁共用：contexts 是分頁的 connection，`nodes` 是含重複的原始筆數。
    #[serde(default)]
    contexts: GqlConnection<GqlCheckContext>,
}

#[derive(Deserialize)]
struct GqlContextsRepo {
    /// oid 不存在（或不是 Commit）時是 null；續頁沒查 `oid`，會是 `""`。
    #[serde(default)]
    object: Option<GqlHeadCommitInner>,
}

/// 首頁 contexts 加上追頁，回傳 head commit 全部的原始 context。
///
/// 失敗（fetch、解析、oid 找不到）是暫時性的，回 `Err` 讓使用者按 `r`
/// 重試，訊息加前綴——不然畫面上的 `(comments failed: …)` 會誤導成
/// 留言載入失敗。達到 `MAX_EXTRA_PAGES` 則截斷、回 `Ok`。
fn collect_head_contexts(
    commits: GqlConnection<GqlHeadCommit>,
    fetch_more: &mut impl FnMut(Target<'_>, &str) -> Result<String, String>,
) -> Result<Vec<GqlCheckContext>, String> {
    let Some(head) = commits.nodes.into_iter().next() else {
        return Ok(Vec::new());
    };
    let oid = head.commit.oid;
    let Some(rollup) = head.commit.status_check_rollup else {
        return Ok(Vec::new());
    };
    collect_pages(
        &t!("github.what.ci_checks"),
        rollup.contexts,
        |_| true,
        |after| {
            let json = fetch_more(Target::Contexts { oid: &oid }, after)?;
            let resp: GqlRepoResp<GqlContextsRepo> = parse_json(&json)?;
            resp.data
                .repository
                .object
                .and_then(|o| o.status_check_rollup)
                .map(|r| r.contexts)
                .ok_or_else(|| t!("github.error.commit_not_found", oid = oid).into_owned())
        },
    )
}

/// 不能在 enum 上用 `rename_all`：那會改 variant 名，`__typename` 就對不上，
/// 每一筆都會悄悄落進 `Unknown`。欄位各自 `rename`。
#[derive(Deserialize)]
#[serde(tag = "__typename")]
enum GqlCheckContext {
    CheckRun {
        #[serde(default)]
        name: String,
        #[serde(default)]
        conclusion: Option<String>,
        #[serde(default, rename = "databaseId")]
        database_id: u64,
        #[serde(default, rename = "checkSuite")]
        check_suite: Option<GqlCheckSuite>,
    },
    StatusContext {
        #[serde(default)]
        context: String,
        #[serde(default)]
        state: String,
    },
    #[serde(other)]
    Unknown,
}
#[derive(Deserialize)]
struct GqlCheckSuite {
    #[serde(default)]
    app: Option<GqlApp>,
    #[serde(default, rename = "workflowRun")]
    workflow_run: Option<GqlWorkflowRun>,
}
impl GqlCheckSuite {
    /// 去重範圍 `(app, workflow, event)`，缺的欄位都是 `""`：同名 check 在範圍
    /// 相同時才算同一個。Actions 的 job 靠 workflow 分，第三方 App 沒有
    /// `workflowRun`，靠 `app` 分；同一個 workflow 被 push 與 pull_request
    /// 各觸發一輪，靠 `event` 分。
    fn scope(&self) -> (&str, &str, &str) {
        let app = self.app.as_ref().map_or("", |a| a.slug.as_str());
        let (workflow, event) = self
            .workflow_run
            .as_ref()
            .map_or(("", ""), |w| (w.workflow.name.as_str(), w.event.as_str()));
        (app, workflow, event)
    }
}
#[derive(Deserialize)]
struct GqlApp {
    #[serde(default)]
    slug: String,
}
#[derive(Deserialize)]
struct GqlWorkflowRun {
    #[serde(default)]
    event: String,
    workflow: GqlWorkflow,
}
#[derive(Deserialize)]
struct GqlWorkflow {
    name: String,
}

/// `None` = 不顯示（skipped／neutral／stale）。CANCELLED 算 fail：GitHub 的
/// rollup 也把它算 FAILURE，commit 行出現紅 ✗ 時，清單要找得到對應的紅色。
/// 被取消後又重跑的舊 run 已在 `head_ci_checks` 去重時被蓋掉，不會出現。
fn check_run_state(conclusion: Option<&str>) -> Option<CheckState> {
    match conclusion {
        Some("SUCCESS") => Some(CheckState::Passed),
        Some("FAILURE" | "TIMED_OUT" | "STARTUP_FAILURE" | "ACTION_REQUIRED" | "CANCELLED") => {
            Some(CheckState::Failed)
        }
        // 排隊或執行中時 conclusion 是 null
        None => Some(CheckState::Pending),
        Some(_) => None,
    }
}

fn status_context_state(state: &str) -> Option<CheckState> {
    match state {
        "SUCCESS" => Some(CheckState::Passed),
        "FAILURE" | "ERROR" => Some(CheckState::Failed),
        "PENDING" | "EXPECTED" => Some(CheckState::Pending),
        _ => None,
    }
}

/// head commit 的 check 清單。順序不能換：
///
/// 1. **去重**：CheckRun 以 `(GqlCheckSuite::scope, name)` 為 key，只留
///    `databaseId` 最大的一筆——PR 標題改一次，`pr-title.yml` 就在同一個 commit
///    上多跑一輪，GitHub 的 rollup 不會去重。StatusContext 由 GitHub 保證
///    每個 context 只留最新一筆。要在所有頁合併之後才做：同一個 key 的新舊
///    run 可能落在不同頁。
/// 2. **判定狀態**並丟掉 `None`。必須在去重之後：先濾會讓「最新一筆跑中」
///    被濾掉，留下舊的 ✗。
/// 3. **排序** `(state, name)`：展開與收合共用，收合截斷時優先丟掉綠色。
///
/// commit 行上的 rollup 標記沒有去重，所以可能出現 commit 行是 ✗、清單卻
/// 全是 ✓ 的情況；跟 GitHub 網頁一致，不為 head commit 另寫特例。
fn head_ci_checks(contexts: &[GqlCheckContext]) -> Vec<GhCheck> {
    type Scope<'a> = (&'a str, &'a str, &'a str);
    let mut latest: FxHashMap<(Scope, &str), (u64, Option<&str>)> = FxHashMap::default();
    let mut checks = Vec::new();
    for ctx in contexts {
        match ctx {
            GqlCheckContext::CheckRun {
                name,
                conclusion,
                database_id,
                check_suite,
            } => {
                let scope = check_suite.as_ref().map_or(("", "", ""), |s| s.scope());
                let entry = latest
                    .entry((scope, name.as_str()))
                    .or_insert((*database_id, conclusion.as_deref()));
                if *database_id > entry.0 {
                    *entry = (*database_id, conclusion.as_deref());
                }
            }
            GqlCheckContext::StatusContext { context, state } => {
                if let Some(state) = status_context_state(state) {
                    checks.push(GhCheck {
                        name: context.clone(),
                        state,
                    });
                }
            }
            GqlCheckContext::Unknown => {}
        }
    }
    checks.extend(
        latest
            .into_iter()
            .filter_map(|((_scope, name), (_, conclusion))| {
                check_run_state(conclusion).map(|state| GhCheck {
                    name: name.to_string(),
                    state,
                })
            }),
    );
    checks.sort_by(|a, b| (a.state, &a.name).cmp(&(b.state, &b.name)));
    checks
}

// ── Checkbox／工作清單 ──

#[derive(Debug, Clone)]
pub struct CheckboxItem {
    pub index: usize,
    pub checked: bool,
    pub label: String,
    pub(crate) byte_offset: usize,
}

pub fn get_body(path: &Path, number: u64, kind: GhItemKind) -> Result<String, String> {
    run_gh(
        path,
        &[
            kind.as_str(),
            "view",
            &number.to_string(),
            "--json",
            "body",
            "--jq",
            ".body",
        ],
    )
}

pub fn parse_checkboxes(body: &str) -> Vec<CheckboxItem> {
    let mut items = Vec::new();
    let mut idx = 0usize;
    let mut byte_pos = 0usize;

    for line in body.lines() {
        let trimmed = line.trim_start();
        let has_unchecked = trimmed.starts_with("- [ ] ");
        let has_checked = trimmed.starts_with("- [x] ") || trimmed.starts_with("- [X] ");

        if has_unchecked || has_checked {
            let leading = line.len() - trimmed.len();
            // '[' 位於 "- " (2 bytes) 之後
            let byte_offset = byte_pos + leading + 2;

            // label 純顯示用（回寫走的是 byte_offset 與重抓的原文），在這裡展開就不必
            // 讓每個顯示端各自補做。
            let label = crate::emoji::expand(&trimmed[6..]).into_owned();

            items.push(CheckboxItem {
                index: idx,
                checked: has_checked,
                label,
                byte_offset,
            });
            idx += 1;
        }

        // 跳過該行內容
        byte_pos += line.len();
        // 跳過行分隔符號
        let rest = body.as_bytes();
        if byte_pos < rest.len() && rest[byte_pos] == b'\r' {
            byte_pos += 1;
        }
        if byte_pos < rest.len() && rest[byte_pos] == b'\n' {
            byte_pos += 1;
        }
    }

    items
}

pub fn toggle_checkboxes(body: &str, indices: &[usize]) -> String {
    let items = parse_checkboxes(body);
    let mut result = body.to_string();
    // 從後往前處理，避免 byte offset 錯位
    let mut targets: Vec<&CheckboxItem> = items
        .iter()
        .filter(|item| indices.contains(&item.index))
        .collect();
    targets.sort_by_key(|t| std::cmp::Reverse(t.byte_offset));
    for item in targets {
        let replacement = if item.checked { "[ ]" } else { "[x]" };
        result.replace_range(item.byte_offset..item.byte_offset + 3, replacement);
    }
    result
}

pub fn update_body(path: &Path, number: u64, kind: GhItemKind, body: &str) -> Result<(), String> {
    let num_str = number.to_string();
    let mut cmd = Command::new("gh");
    cmd.args([kind.as_str(), "edit", &num_str, "--body-file", "-"])
        .current_dir(path);

    let output = run_with_timeout(cmd, Some(body.as_bytes()), GH_TIMEOUT)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(t!("github.error.edit_failed", detail = stderr).into_owned());
    }
    Ok(())
}

pub fn is_merge_conflict_error(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    lower.contains("conflict") || lower.contains("not mergeable")
}

// ── Issue／PR 狀態切換 ──

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateAction {
    Close,
    Reopen,
}

impl StateAction {
    /// 對某個狀態該執行的切換，`None` 代表不可切換。
    ///
    /// 這是「狀態 → 動作」的唯一來源：hint 與實際動作都吃它，就不可能指向
    /// 相反的方向，也不會有一邊擋了 MERGED 另一邊沒擋。
    pub fn for_state(state: &str) -> Option<Self> {
        match state {
            "OPEN" => Some(StateAction::Close),
            "CLOSED" => Some(StateAction::Reopen),
            // MERGED 的 PR：GitHub 不允許 reopen
            _ => None,
        }
    }

    fn verb(self) -> &'static str {
        match self {
            StateAction::Close => "close",
            StateAction::Reopen => "reopen",
        }
    }

    pub fn prompt(self, kind: GhItemKind, number: u64) -> String {
        match (self, kind) {
            (StateAction::Close, GhItemKind::Issue) => {
                t!("github.state_action.close_issue.prompt", number = number)
            }
            (StateAction::Reopen, GhItemKind::Issue) => {
                t!("github.state_action.reopen_issue.prompt", number = number)
            }
            (StateAction::Close, GhItemKind::PullRequest) => {
                t!("github.state_action.close_pr.prompt", number = number)
            }
            (StateAction::Reopen, GhItemKind::PullRequest) => {
                t!("github.state_action.reopen_pr.prompt", number = number)
            }
        }
        .into_owned()
    }

    pub fn pending(self, kind: GhItemKind, number: u64) -> String {
        match (self, kind) {
            (StateAction::Close, GhItemKind::Issue) => {
                t!("github.state_action.close_issue.pending", number = number)
            }
            (StateAction::Reopen, GhItemKind::Issue) => {
                t!("github.state_action.reopen_issue.pending", number = number)
            }
            (StateAction::Close, GhItemKind::PullRequest) => {
                t!("github.state_action.close_pr.pending", number = number)
            }
            (StateAction::Reopen, GhItemKind::PullRequest) => {
                t!("github.state_action.reopen_pr.pending", number = number)
            }
        }
        .into_owned()
    }

    pub fn success(self, kind: GhItemKind, number: u64) -> String {
        match (self, kind) {
            (StateAction::Close, GhItemKind::Issue) => {
                t!("github.state_action.close_issue.success", number = number)
            }
            (StateAction::Reopen, GhItemKind::Issue) => {
                t!("github.state_action.reopen_issue.success", number = number)
            }
            (StateAction::Close, GhItemKind::PullRequest) => {
                t!("github.state_action.close_pr.success", number = number)
            }
            (StateAction::Reopen, GhItemKind::PullRequest) => {
                t!("github.state_action.reopen_pr.success", number = number)
            }
        }
        .into_owned()
    }

    pub fn hint_label(self, kind: GhItemKind) -> Cow<'static, str> {
        match (self, kind) {
            (StateAction::Close, GhItemKind::Issue) => t!("github.state_action.close_issue.hint"),
            (StateAction::Reopen, GhItemKind::Issue) => t!("github.state_action.reopen_issue.hint"),
            (StateAction::Close, GhItemKind::PullRequest) => {
                t!("github.state_action.close_pr.hint")
            }
            (StateAction::Reopen, GhItemKind::PullRequest) => {
                t!("github.state_action.reopen_pr.hint")
            }
        }
    }
}

pub fn set_item_state(
    path: &Path,
    kind: GhItemKind,
    number: u64,
    action: StateAction,
) -> Result<(), String> {
    run_gh(path, &[kind.as_str(), action.verb(), &number.to_string()])?;
    Ok(())
}

// ── PR 合併 ──

#[derive(Debug, Clone, Copy)]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

impl MergeMethod {
    pub fn as_flag(self) -> &'static str {
        match self {
            MergeMethod::Merge => "--merge",
            MergeMethod::Squash => "--squash",
            MergeMethod::Rebase => "--rebase",
        }
    }

    pub fn display(self) -> &'static str {
        match self {
            MergeMethod::Merge => "merge",
            MergeMethod::Squash => "squash",
            MergeMethod::Rebase => "rebase",
        }
    }
}

pub fn merge_pr(path: &Path, number: u64, method: &str) -> Result<(), String> {
    run_gh(path, &["pr", "merge", &number.to_string(), method])?;
    Ok(())
}

/// 只刪遠端的 head branch，不動本地。owner/repo 走 `fetch_repo_name_with_owner`
/// 而非 gh 的 `{owner}/{repo}` placeholder——列表也是走這個函式解析 repo，
/// 兩套機制解析同一件事就有機會給出不同答案（快取在 process 生命週期內不失效，
/// 期間若有人 `gh repo set-default`，就會列出 A repo 的 PR、卻刪到 B repo 的分支）。
pub fn delete_remote_branch(path: &Path, head_ref: &str) -> Result<(), String> {
    let (owner, name) = fetch_repo_name_with_owner(path)?;
    let endpoint = ref_delete_endpoint(&owner, &name, head_ref);
    match run_gh(path, &["api", "-X", "DELETE", &endpoint]) {
        // repo 開了 auto-delete head branches 時 merge 當下遠端就沒了，不是失敗
        Err(e) if is_ref_missing_error(&e) => Ok(()),
        other => other.map(|_| ()),
    }
}

fn ref_delete_endpoint(owner: &str, name: &str, head_ref: &str) -> String {
    format!(
        "repos/{owner}/{name}/git/refs/heads/{}",
        encode_ref_path(head_ref)
    )
}

/// branch 名塞進 URL path 前跳脫——`/` 原樣留（endpoint 本來就吃斜線），
/// 只處理會壞掉 URL 的字元。git 允許分支名含 `#`，不編碼會被當成 fragment 截斷。
/// `%` 必須先換，否則後面插入的 `%23`／`%3F` 會被二次編碼。
fn encode_ref_path(head_ref: &str) -> String {
    head_ref
        .replace('%', "%25")
        .replace('#', "%23")
        .replace('?', "%3F")
}

/// 只認明確的「ref 不存在」，裸 404 不吞——沒權限、repo 解析錯、endpoint
/// 打錯也會是 404，全吞掉就等於靜默失敗。
fn is_ref_missing_error(msg: &str) -> bool {
    msg.contains("Reference does not exist")
}

// ── PR draft 切換 ──

#[derive(Debug, Clone, Copy)]
pub enum PrDraftAction {
    MarkReady,
    ConvertToDraft,
}

impl PrDraftAction {
    /// 對一個 draft／非 draft PR 該執行的切換方向。集中在這裡，UI 提示與實際
    /// 動作就不可能指向相反的方向。
    pub fn for_pr(is_draft: bool) -> Self {
        if is_draft {
            PrDraftAction::MarkReady
        } else {
            PrDraftAction::ConvertToDraft
        }
    }

    pub fn prompt(self, number: u64) -> String {
        match self {
            PrDraftAction::MarkReady => t!("github.draft.mark_ready.prompt", number = number),
            PrDraftAction::ConvertToDraft => t!("github.draft.to_draft.prompt", number = number),
        }
        .into_owned()
    }

    pub fn pending(self, number: u64) -> String {
        match self {
            PrDraftAction::MarkReady => t!("github.draft.mark_ready.pending", number = number),
            PrDraftAction::ConvertToDraft => t!("github.draft.to_draft.pending", number = number),
        }
        .into_owned()
    }

    pub fn success(self, number: u64) -> String {
        match self {
            PrDraftAction::MarkReady => t!("github.draft.mark_ready.success", number = number),
            PrDraftAction::ConvertToDraft => t!("github.draft.to_draft.success", number = number),
        }
        .into_owned()
    }

    pub fn hint_label(self) -> Cow<'static, str> {
        match self {
            PrDraftAction::MarkReady => t!("github.draft.mark_ready.hint"),
            PrDraftAction::ConvertToDraft => t!("github.draft.to_draft.hint"),
        }
    }

    /// 動作成功後 PR 應有的 draft 狀態，供列表 in-place 更新使用。
    /// 這也正是 `gh pr ready --undo` 的語意，所以 `set_pr_draft` 直接用它。
    pub fn result_is_draft(self) -> bool {
        matches!(self, PrDraftAction::ConvertToDraft)
    }
}

pub fn set_pr_draft(path: &Path, number: u64, action: PrDraftAction) -> Result<(), String> {
    let num_str = number.to_string();
    let mut args = vec!["pr", "ready", &num_str];
    if action.result_is_draft() {
        args.push("--undo");
    }
    run_gh(path, &args)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 單頁 fixture 不該觸發任何續頁（CI contexts 或 review threads）；
    /// 追了就是解析或 fixture 有誤。
    fn parse(json: &str, kind: GhItemKind) -> Result<GhTimelinePage, String> {
        parse_timeline_graphql(json, kind, |_, _| panic!("unexpected continuation fetch"))
    }

    /// 狀態 → 動作的映射是 hint 與實際動作的唯一來源，錯了兩邊會一起錯。
    #[test]
    fn state_action_for_state() {
        assert_eq!(StateAction::for_state("OPEN"), Some(StateAction::Close));
        assert_eq!(StateAction::for_state("CLOSED"), Some(StateAction::Reopen));
        // merged 的 PR 不能 reopen —— None 同時擋掉動作與 hint
        assert_eq!(StateAction::for_state("MERGED"), None);
    }

    /// argv 第 0 格接的是 `kind.as_str()`。這裡打錯會讓 `gh issue close` 被送去
    /// 關一個 PR（或反過來），而文案看起來完全正常。
    #[test]
    fn state_action_verb_and_kind_argv() {
        assert_eq!(GhItemKind::Issue.as_str(), "issue");
        assert_eq!(GhItemKind::PullRequest.as_str(), "pr");
        assert_eq!(StateAction::Close.verb(), "close");
        assert_eq!(StateAction::Reopen.verb(), "reopen");
    }

    #[test]
    fn state_action_messages_name_the_right_kind() {
        assert_eq!(
            StateAction::Close.prompt(GhItemKind::PullRequest, 12),
            "關閉 PR #12？ "
        );
        assert_eq!(
            StateAction::Close.prompt(GhItemKind::Issue, 12),
            "關閉 issue #12？ "
        );
        assert_eq!(
            StateAction::Reopen.success(GhItemKind::PullRequest, 12),
            "已重新開啟 PR #12"
        );
        assert_eq!(
            StateAction::Close.pending(GhItemKind::Issue, 12),
            "正在關閉 issue #12…"
        );
    }

    /// `next()` 三段循環要回得到起點，否則 UI 上的 filter 快捷鍵會卡住或跳號。
    #[test]
    fn state_filter_next_cycles_through_all_three_states() {
        assert_eq!(StateFilter::Open.next(), StateFilter::Closed);
        assert_eq!(StateFilter::Closed.next(), StateFilter::All);
        assert_eq!(StateFilter::All.next(), StateFilter::Open);
    }

    /// `as_str()` 是 gh CLI argv 的唯一輸出端——`StateFilter` 全程留在型別裡，
    /// 只在真正呼叫 `gh` 的邊界才轉成字串，不會有轉回來的 round-trip。
    #[test]
    fn state_filter_as_str_matches_gh_cli_argv() {
        assert_eq!(StateFilter::Open.as_str(), "open");
        assert_eq!(StateFilter::Closed.as_str(), "closed");
        assert_eq!(StateFilter::All.as_str(), "all");
    }

    #[test]
    fn parse_graphql_issue_with_relations() {
        let json = r#"{
            "data": {
                "repository": {
                    "issues": {
                        "nodes": [{
                            "number": 7,
                            "title": "Epic",
                            "state": "OPEN",
                            "body": "parent body",
                            "url": "https://github.com/o/r/issues/7",
                            "createdAt": "2026-01-01T00:00:00Z",
                            "closedAt": null,
                            "updatedAt": "2026-01-02T00:00:00Z",
                            "author": {"login": "alice"},
                            "labels": {"nodes": [{"name": "bug", "color": "ff0000"}]},
                            "parent": null,
                            "subIssues": {"nodes": [
                                {"number": 10, "title": "First", "state": "OPEN"},
                                {"number": 11, "title": "Second", "state": "CLOSED"}
                            ]}
                        }]
                    }
                }
            }
        }"#;
        let page = parse_issues_graphql(json).unwrap();
        let issues = page.items;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].number, 7);
        assert!(issues[0].parent.is_none());
        assert_eq!(issues[0].sub_issues.len(), 2);
        assert_eq!(issues[0].sub_issues[0].number, 10);
        assert_eq!(issues[0].sub_issues[1].state, "CLOSED");
    }

    #[test]
    fn parse_graphql_issue_with_parent_no_children() {
        let json = r#"{
            "data": {
                "repository": {
                    "issues": {
                        "nodes": [{
                            "number": 10,
                            "title": "Child",
                            "state": "OPEN",
                            "body": "",
                            "url": "",
                            "createdAt": "2026-01-01T00:00:00Z",
                            "closedAt": null,
                            "updatedAt": null,
                            "author": null,
                            "labels": {"nodes": []},
                            "parent": {"number": 7, "title": "Epic", "state": "OPEN"},
                            "subIssues": {"nodes": []}
                        }]
                    }
                }
            }
        }"#;
        let page = parse_issues_graphql(json).unwrap();
        let issues = page.items;
        assert_eq!(issues[0].parent.as_ref().unwrap().number, 7);
        assert!(issues[0].sub_issues.is_empty());
        assert_eq!(issues[0].author.login, "ghost");
    }

    #[test]
    fn parse_timeline_issue_with_next_page() {
        let json = r#"{
            "data": {
                "repository": {
                    "issue": {
                        "timelineItems": {
                            "pageInfo": {"hasNextPage": true, "endCursor": "C2"},
                            "nodes": [
                                {
                                    "__typename": "IssueComment",
                                    "body": "first",
                                    "createdAt": "2026-01-01T00:00:00Z",
                                    "url": "https://github.com/o/r/issues/1#issuecomment-1",
                                    "author": {"login": "alice"}
                                },
                                {
                                    "__typename": "IssueComment",
                                    "body": "second",
                                    "createdAt": "2026-01-02T00:00:00Z",
                                    "url": "https://github.com/o/r/issues/1#issuecomment-2",
                                    "author": null
                                }
                            ]
                        }
                    }
                }
            }
        }"#;
        let page = parse(json, GhItemKind::Issue).unwrap();
        assert_eq!(page.next_cursor.as_deref(), Some("C2"));
        assert_eq!(page.items.len(), 2);
        match &page.items[0] {
            GhTimelineItem::IssueComment { body, author, .. } => {
                assert_eq!(body, "first");
                assert_eq!(author.as_ref().unwrap().login, "alice");
            }
            other => panic!("expected IssueComment, got {other:?}"),
        }
        match &page.items[1] {
            GhTimelineItem::IssueComment { author, .. } => assert!(author.is_none()),
            other => panic!("expected IssueComment, got {other:?}"),
        }
    }

    #[test]
    fn parse_timeline_pr_no_next_page() {
        let json = r#"{
            "data": {
                "repository": {
                    "pullRequest": {
                        "timelineItems": {
                            "pageInfo": {"hasNextPage": false, "endCursor": null},
                            "nodes": []
                        }
                    }
                }
            }
        }"#;
        let page = parse(json, GhItemKind::PullRequest).unwrap();
        assert!(page.next_cursor.is_none());
        assert!(page.items.is_empty());
    }

    #[test]
    fn graphql_issue_titles_expand_emoji_shortcodes() {
        let json = r#"{
            "data": {
                "repository": {
                    "issues": {
                        "nodes": [{
                            "number": 7,
                            "title": ":tada: 上線",
                            "state": "OPEN",
                            "body": ":tada: 內文保持原文",
                            "url": "",
                            "createdAt": "2026-01-01T00:00:00Z",
                            "closedAt": null,
                            "updatedAt": null,
                            "author": null,
                            "labels": {"nodes": []},
                            "parent": {"number": 1, "title": ":rocket: 母議題", "state": "OPEN"},
                            "subIssues": {"nodes": [
                                {"number": 10, "title": ":bug: 子議題", "state": "OPEN"}
                            ]}
                        }]
                    }
                }
            }
        }"#;
        let issues = parse_issues_graphql(json).unwrap().items;

        assert_eq!(issues[0].title, "🎉 上線");
        assert_eq!(issues[0].parent.as_ref().unwrap().title, "🚀 母議題");
        assert_eq!(issues[0].sub_issues[0].title, "🐛 子議題");
        // body 走 markdown renderer 展開，才能保住 code fence 內的原文。
        assert_eq!(issues[0].body, ":tada: 內文保持原文");
    }

    #[test]
    fn parse_timeline_expands_commit_headline_but_not_comment_body() {
        let json = r#"{
            "data": {
                "repository": {
                    "pullRequest": {
                        "timelineItems": {
                            "pageInfo": {"hasNextPage": false, "endCursor": null},
                            "nodes": [
                                {
                                    "__typename": "IssueComment",
                                    "body": ":tada: 留言",
                                    "createdAt": "2026-01-01T00:00:00Z",
                                    "author": {"login": "alice"}
                                },
                                {
                                    "__typename": "PullRequestCommit",
                                    "commit": {
                                        "abbreviatedOid": "62f3c11",
                                        "messageHeadline": ":sparkles: 新功能",
                                        "statusCheckRollup": null
                                    }
                                }
                            ]
                        }
                    }
                }
            }
        }"#;
        let items = parse(json, GhItemKind::PullRequest).unwrap().items;

        match &items[0] {
            GhTimelineItem::IssueComment { body, .. } => assert_eq!(body, ":tada: 留言"),
            other => panic!("expected IssueComment, got {other:?}"),
        }
        match &items[1] {
            GhTimelineItem::PullRequestCommit { commit } => {
                assert_eq!(commit.message_headline, "✨ 新功能");
            }
            other => panic!("expected PullRequestCommit, got {other:?}"),
        }
    }

    #[test]
    fn parse_timeline_pull_request_commit_with_ci_state() {
        let json = r#"{
            "data": {
                "repository": {
                    "pullRequest": {
                        "timelineItems": {
                            "pageInfo": {"hasNextPage": false, "endCursor": null},
                            "nodes": [
                                {
                                    "__typename": "PullRequestCommit",
                                    "commit": {
                                        "abbreviatedOid": "62f3c11",
                                        "messageHeadline": "fix(pricing): add gate",
                                        "committedDate": "2026-01-03T00:00:00Z",
                                        "statusCheckRollup": {"state": "SUCCESS"}
                                    }
                                },
                                {
                                    "__typename": "PullRequestCommit",
                                    "commit": {
                                        "abbreviatedOid": "aaaaaaa",
                                        "messageHeadline": "no ci here",
                                        "committedDate": "2026-01-04T00:00:00Z",
                                        "statusCheckRollup": null
                                    }
                                }
                            ]
                        }
                    }
                }
            }
        }"#;
        let page = parse(json, GhItemKind::PullRequest).unwrap();
        assert_eq!(page.items.len(), 2);
        match &page.items[0] {
            GhTimelineItem::PullRequestCommit { commit } => {
                assert_eq!(commit.abbreviated_oid, "62f3c11");
                assert_eq!(
                    commit.status_check_rollup.as_ref().unwrap().state,
                    "SUCCESS"
                );
            }
            other => panic!("expected PullRequestCommit, got {other:?}"),
        }
        match &page.items[1] {
            GhTimelineItem::PullRequestCommit { commit } => {
                assert!(commit.status_check_rollup.is_none());
            }
            other => panic!("expected PullRequestCommit, got {other:?}"),
        }
    }

    fn timeline_json(mergeable: Option<&str>) -> String {
        let mergeable_field =
            mergeable.map_or(String::new(), |s| format!(r#""mergeable": "{s}","#));
        format!(
            r#"{{
                "data": {{
                    "repository": {{
                        "pullRequest": {{
                            {mergeable_field}
                            "timelineItems": {{
                                "pageInfo": {{"hasNextPage": false, "endCursor": null}},
                                "nodes": []
                            }}
                        }}
                    }}
                }}
            }}"#
        )
    }

    /// `MERGEABLE` / `CONFLICTING` 會對應到一個標記；`UNKNOWN`（仍在計算中，
    /// 或該 PR 已合併／關閉）與缺少該欄位（issue 根本沒有這個欄位）
    /// 兩種情況都會併入 `None` —— 不顯示標記。
    #[test]
    fn parse_timeline_mergeable_states() {
        let json = timeline_json(Some("MERGEABLE"));
        let page = parse(&json, GhItemKind::PullRequest).unwrap();
        assert_eq!(page.mergeable, Some(Mergeable::Mergeable));

        let json = timeline_json(Some("CONFLICTING"));
        let page = parse(&json, GhItemKind::PullRequest).unwrap();
        assert_eq!(page.mergeable, Some(Mergeable::Conflicting));

        let json = timeline_json(Some("UNKNOWN"));
        let page = parse(&json, GhItemKind::PullRequest).unwrap();
        assert_eq!(page.mergeable, None);

        let json = timeline_json(None);
        let page = parse(&json, GhItemKind::PullRequest).unwrap();
        assert_eq!(page.mergeable, None);
    }

    /// issue 的 timeline union 不含 `PullRequestCommit`，也沒有 `mergeable`。
    /// 只要有一項漏了分岔，GitHub 就會退回整個查詢，detail 畫面只剩
    /// "comments failed"。
    #[test]
    fn timeline_query_omits_pr_only_pieces_for_issues() {
        for first_page in [true, false] {
            let q = build_timeline_query(GhItemKind::Issue, first_page);
            assert!(q.contains("issue(number:$number)"));
            assert!(!q.contains("PullRequestCommit"));
            assert!(!q.contains("PULL_REQUEST_COMMIT"));
            assert!(!q.contains("mergeable"));
            assert!(!q.contains("PullRequestReview"));
            assert!(!q.contains("PULL_REQUEST_REVIEW"));
            assert!(!q.contains("reviewThreads"));
            assert!(!q.contains("commits(last:1)"));
        }

        let q = build_timeline_query(GhItemKind::PullRequest, true);
        assert!(q.contains("pullRequest(number:$number)"));
        assert!(q.contains("PullRequestCommit"));
        assert!(q.contains("PULL_REQUEST_COMMIT"));
        assert!(q.contains("mergeable"));
        assert!(q.contains("PullRequestReview"));
        assert!(q.contains("PULL_REQUEST_REVIEW"));
        assert!(q.contains("reviewThreads"));
        assert!(q.contains("commits(last:1)"));
    }

    /// CI 只有首頁要：首頁帶 oid（續頁釘 commit 用）、`app`、`event` 與
    /// contexts 的 `pageInfo`；載入更多不重抓，但 `reviewThreads`／`mergeable`
    /// 每頁都要（resolved 回填靠它）。
    #[test]
    fn timeline_query_carries_ci_only_on_the_first_page() {
        let first = build_timeline_query(GhItemKind::PullRequest, true);
        assert!(first.contains("commit { oid statusCheckRollup"));
        assert!(first.contains(&format!("contexts(first:{CONTEXTS_PAGE_SIZE})")));
        assert!(first.contains(&head_commit_ci_fragment()));
        assert!(first.contains("app { slug }"));
        assert!(first.contains("event workflow { name }"));

        let more = build_timeline_query(GhItemKind::PullRequest, false);
        assert!(!more.contains("commits(last:1)"));
        assert!(!more.contains("contexts("));
        assert!(more.contains("reviewThreads"));
        assert!(more.contains("mergeable"));
        // commit 行上的 rollup 標記是 timeline item 的一部分，每頁都要
        assert!(more.contains("statusCheckRollup { state }"));
    }

    #[test]
    fn contexts_query_pins_the_commit_and_shares_the_node_selection() {
        let q = build_contexts_query();
        assert!(q.contains("$oid:GitObjectID!"));
        assert!(q.contains("object(oid:$oid)"));
        assert!(q.contains("after:$after"));
        assert!(q.contains(CHECK_CONTEXT_NODES));
        assert!(build_timeline_query(GhItemKind::PullRequest, true).contains(CHECK_CONTEXT_NODES));
    }

    /// resolved 回填每頁都要（載入更多的頁面也有自己的 review 留言），
    /// 首頁與續頁查詢共用同一份 thread 選取。
    #[test]
    fn review_threads_query_shares_the_selection_with_every_pr_page() {
        for first_page in [true, false] {
            let q = build_timeline_query(GhItemKind::PullRequest, first_page);
            assert!(
                q.contains(REVIEW_THREADS_SELECTION),
                "first_page={first_page}"
            );
            assert!(q.contains(&format!("reviewThreads(first:{THREADS_PAGE_SIZE})")));
        }
        let q = build_review_threads_query();
        assert!(q.contains("$number:Int!"));
        assert!(q.contains("pullRequest(number:$number)"));
        assert!(q.contains("after:$after"));
        assert!(q.contains(REVIEW_THREADS_SELECTION));
    }

    /// `itemTypes` 理論上只會產生兩種已知的 variant，但這個測試釘住了
    /// 退回機制：未知的 `__typename` 不得讓整頁的反序列化失敗。
    #[test]
    fn parse_timeline_unknown_typename_does_not_fail_the_page() {
        let json = r#"{
            "data": {
                "repository": {
                    "issue": {
                        "timelineItems": {
                            "pageInfo": {"hasNextPage": false, "endCursor": null},
                            "nodes": [
                                {"__typename": "ClosedEvent"},
                                {
                                    "__typename": "IssueComment",
                                    "body": "still here",
                                    "createdAt": "2026-01-01T00:00:00Z",
                                    "url": "",
                                    "author": {"login": "bob"}
                                }
                            ]
                        }
                    }
                }
            }
        }"#;
        let page = parse(json, GhItemKind::Issue).unwrap();
        assert_eq!(page.items.len(), 2);
        assert!(matches!(page.items[0], GhTimelineItem::Unknown));
        assert!(matches!(page.items[1], GhTimelineItem::IssueComment { .. }));
    }

    /// 三個會讓「刪錯 branch」重演的條件：fork（同名分支會打到 base repo）、
    /// head == base（GitHub 不接受，防禦性斷言）、head 是 default branch
    /// （GitHub 會拒刪，UI 不該先問一件做不到的事）。
    #[test]
    fn head_branch_deletable_blocks_fork_default_and_self_merge() {
        assert!(head_branch_deletable(
            false,
            "feature/x",
            "main",
            Some("main")
        ));
        assert!(!head_branch_deletable(
            true,
            "feature/x",
            "main",
            Some("main")
        ));
        assert!(!head_branch_deletable(false, "main", "main", Some("main")));
        assert!(!head_branch_deletable(
            false,
            "main",
            "develop",
            Some("main")
        ));
        assert!(head_branch_deletable(false, "feature/x", "main", None));
    }

    /// 只認明確的「ref 不存在」；裸 404（沒權限、repo 解析錯、endpoint 打錯
    /// 都會是 404）絕不能被吞成「成功」，否則刪除失敗會靜默過關。
    #[test]
    fn ref_missing_error_only_matches_explicit_message() {
        assert!(is_ref_missing_error(
            "gh command failed: HTTP 422: Reference does not exist"
        ));
        assert!(!is_ref_missing_error(
            "gh command failed: HTTP 404: Not Found"
        ));
        assert!(!is_ref_missing_error(
            "gh command failed: HTTP 403: Forbidden"
        ));
        assert!(!is_ref_missing_error("gh command failed: Merge conflict"));
    }

    #[test]
    fn ref_delete_endpoint_builds_expected_path() {
        assert_eq!(
            ref_delete_endpoint("owner", "repo", "feature/x"),
            "repos/owner/repo/git/refs/heads/feature/x"
        );
    }

    /// `/` 原樣留（endpoint 本來就吃斜線），但 `%` `#` `?` 會壞掉 URL 或被
    /// 當成 fragment／query 截斷，git 允許分支名含這些字元，不編碼就是地雷。
    #[test]
    fn encode_ref_path_escapes_url_special_chars_but_keeps_slashes() {
        assert_eq!(encode_ref_path("feature/x"), "feature/x");
        assert_eq!(encode_ref_path("fix#1"), "fix%231");
        assert_eq!(encode_ref_path("100%done"), "100%25done");
        assert_eq!(encode_ref_path("what?"), "what%3F");
    }

    /// `defaultBranchRef` 與 `isCrossRepository` 要能從同一支 query 解析出來，
    /// 且要在 parse 當下就算進 `head_branch_deletable`，而不是留給呼叫端。
    #[test]
    fn parse_prs_graphql_computes_head_branch_deletable() {
        let json = r#"{
            "data": {
                "repository": {
                    "defaultBranchRef": {"name": "main"},
                    "pullRequests": {
                        "pageInfo": {"hasNextPage": false, "endCursor": null},
                        "nodes": [
                            {
                                "number": 1,
                                "title": "same repo",
                                "state": "OPEN",
                                "headRefName": "feature/x",
                                "headRefOid": "aaa111",
                                "baseRefName": "main",
                                "isDraft": false,
                                "isCrossRepository": false,
                                "author": {"login": "alice"},
                                "labels": {"nodes": []},
                                "closingIssuesReferences": {"nodes": []},
                                "additions": 600,
                                "deletions": 71
                            },
                            {
                                "number": 2,
                                "title": "fork",
                                "state": "OPEN",
                                "headRefName": "feature/y",
                                "headRefOid": "bbb222",
                                "baseRefName": "main",
                                "isDraft": false,
                                "isCrossRepository": true,
                                "author": {"login": "bob"},
                                "labels": {"nodes": []},
                                "closingIssuesReferences": {"nodes": []},
                                "additions": 0,
                                "deletions": 0
                            },
                            {
                                "number": 3,
                                "title": "head is default branch",
                                "state": "OPEN",
                                "headRefName": "main",
                                "headRefOid": "ccc333",
                                "baseRefName": "release/1.x",
                                "isDraft": false,
                                "isCrossRepository": false,
                                "author": {"login": "carol"},
                                "labels": {"nodes": []},
                                "closingIssuesReferences": {"nodes": []},
                                "additions": 0,
                                "deletions": 0
                            }
                        ]
                    }
                }
            }
        }"#;
        let page = parse_prs_graphql(json).unwrap();
        assert!(page.items[0].head_branch_deletable);
        assert!(!page.items[1].head_branch_deletable);
        assert!(!page.items[2].head_branch_deletable);
        assert_eq!(
            page.items[0].diff_stat,
            DiffStat {
                additions: 600,
                deletions: 71
            }
        );
    }

    /// resolved 狀態不在 `PullRequestReviewComment` 上，是這裡拿平行查詢的
    /// `reviewThreads` 靠 comment id 回填的。一個 review 帶兩則行內留言，
    /// 只有其中一個的 id 出現在已 resolved 的 thread 裡。
    #[test]
    fn parse_timeline_backfills_resolved_from_review_threads() {
        let json = r#"{
            "data": {
                "repository": {
                    "pullRequest": {
                        "mergeable": "MERGEABLE",
                        "reviewThreads": {
                            "nodes": [
                                {
                                    "isResolved": true,
                                    "comments": { "nodes": [{"id": "C1"}] }
                                },
                                {
                                    "isResolved": false,
                                    "comments": { "nodes": [{"id": "C2"}] }
                                }
                            ]
                        },
                        "timelineItems": {
                            "pageInfo": {"hasNextPage": false, "endCursor": null},
                            "nodes": [
                                {
                                    "__typename": "PullRequestReview",
                                    "state": "COMMENTED",
                                    "body": "",
                                    "submittedAt": "2026-01-01T00:00:00Z",
                                    "author": {"login": "carol"},
                                    "comments": {
                                        "totalCount": 2,
                                        "nodes": [
                                            {"id": "C1", "path": "a.rs", "line": 5, "originalLine": 5, "outdated": false, "body": "x"},
                                            {"id": "C2", "path": "b.rs", "line": 9, "originalLine": 9, "outdated": false, "body": "y"}
                                        ]
                                    }
                                }
                            ]
                        }
                    }
                }
            }
        }"#;
        let page = parse(json, GhItemKind::PullRequest).unwrap();
        let GhTimelineItem::PullRequestReview { comments, .. } = &page.items[0] else {
            panic!("expected a review item, got {:?}", page.items[0]);
        };
        assert!(comments.nodes[0].resolved, "C1's thread is resolved");
        assert!(!comments.nodes[1].resolved, "C2's thread is not resolved");
    }

    /// `line` 為 null（留言所在行被後續 commit 改掉）時 fallback 到
    /// `originalLine`；兩者皆 null 時保持 `None`，view 層只印 path。
    #[test]
    fn parse_timeline_falls_back_to_original_line_when_line_is_null() {
        let json = r#"{
            "data": {
                "repository": {
                    "pullRequest": {
                        "mergeable": null,
                        "reviewThreads": { "nodes": [] },
                        "timelineItems": {
                            "pageInfo": {"hasNextPage": false, "endCursor": null},
                            "nodes": [
                                {
                                    "__typename": "PullRequestReview",
                                    "state": "COMMENTED",
                                    "body": "",
                                    "submittedAt": "2026-01-01T00:00:00Z",
                                    "author": {"login": "carol"},
                                    "comments": {
                                        "totalCount": 2,
                                        "nodes": [
                                            {"id": "C1", "path": "a.rs", "line": null, "originalLine": 5, "outdated": true, "body": "x"},
                                            {"id": "C2", "path": "b.rs", "line": null, "originalLine": null, "outdated": true, "body": "y"}
                                        ]
                                    }
                                }
                            ]
                        }
                    }
                }
            }
        }"#;
        let page = parse(json, GhItemKind::PullRequest).unwrap();
        let GhTimelineItem::PullRequestReview { comments, .. } = &page.items[0] else {
            panic!("expected a review item, got {:?}", page.items[0]);
        };
        assert_eq!(comments.nodes[0].line, Some(5));
        assert_eq!(comments.nodes[1].line, None);
    }

    fn run(workflow: &str, name: &str, id: u64, conclusion: Option<&str>) -> String {
        let conclusion = conclusion.map_or("null".to_string(), |c| format!(r#""{c}""#));
        format!(
            r#"{{"__typename":"CheckRun","name":"{name}","conclusion":{conclusion},
                "databaseId":{id},
                "checkSuite":{{"workflowRun":{{"workflow":{{"name":"{workflow}"}}}}}}}}"#
        )
    }

    fn status(context: &str, state: &str) -> String {
        format!(r#"{{"__typename":"StatusContext","context":"{context}","state":"{state}"}}"#)
    }

    fn checks_from(contexts: &[String]) -> Vec<GhCheck> {
        let json = format!(
            r#"{{"data":{{"repository":{{"pullRequest":{{
                "commits":{{"nodes":[{{"commit":{{"statusCheckRollup":
                    {{"contexts":{{"nodes":[{}]}}}}}}}}]}},
                "timelineItems":{{"pageInfo":{{"hasNextPage":false,"endCursor":null}},
                    "nodes":[]}}}}}}}}}}"#,
            contexts.join(",")
        );
        parse(&json, GhItemKind::PullRequest).unwrap().ci_checks
    }

    fn check(name: &str, state: CheckState) -> GhCheck {
        GhCheck {
            name: name.to_string(),
            state,
        }
    }

    /// PR 標題改一次，`pr-title.yml` 就在同一個 commit 上多跑一輪，
    /// GitHub 的 rollup 不去重——同 `(workflow, name)` 只留 `databaseId` 最大的。
    #[test]
    fn head_checks_keep_only_latest_run_per_workflow_and_name() {
        let checks = checks_from(&[
            run("PR Title", "check", 1, Some("FAILURE")),
            run("PR Title", "check", 3, Some("SUCCESS")),
            run("PR Title", "check", 2, Some("CANCELLED")),
        ]);
        assert_eq!(checks, vec![check("check", CheckState::Passed)]);
    }

    /// 去重必須在丟掉非 pass/fail 之前：最新一筆還在跑，就不能讓舊的 ✗ 浮上來。
    #[test]
    fn head_checks_rerun_in_progress_hides_stale_result() {
        let checks = checks_from(&[
            run("Build", "lint", 1, Some("FAILURE")),
            run("Build", "lint", 2, None),
        ]);
        assert_eq!(checks, vec![check("lint", CheckState::Pending)]);
    }

    #[test]
    fn head_checks_same_name_in_different_workflows_are_kept_apart() {
        let checks = checks_from(&[
            run("A", "build", 1, Some("SUCCESS")),
            run("B", "build", 2, Some("FAILURE")),
        ]);
        assert_eq!(
            checks,
            vec![
                check("build", CheckState::Failed),
                check("build", CheckState::Passed)
            ]
        );
    }

    #[test]
    fn head_checks_state_table() {
        let checks = checks_from(&[
            run("W", "success", 1, Some("SUCCESS")),
            run("W", "failure", 2, Some("FAILURE")),
            run("W", "timed_out", 3, Some("TIMED_OUT")),
            run("W", "startup", 4, Some("STARTUP_FAILURE")),
            run("W", "action", 5, Some("ACTION_REQUIRED")),
            run("W", "cancelled", 6, Some("CANCELLED")),
            run("W", "running", 7, None),
            run("W", "skipped", 8, Some("SKIPPED")),
            run("W", "neutral", 9, Some("NEUTRAL")),
            run("W", "stale", 10, Some("STALE")),
            status("ctx-ok", "SUCCESS"),
            status("ctx-bad", "ERROR"),
            status("ctx-fail", "FAILURE"),
            status("ctx-wait", "PENDING"),
            status("ctx-exp", "EXPECTED"),
            r#"{"__typename":"SomethingNew"}"#.to_string(),
        ]);
        let state_of = |n: &str| checks.iter().find(|c| c.name == n).map(|c| c.state);
        for n in ["success", "ctx-ok"] {
            assert_eq!(state_of(n), Some(CheckState::Passed), "{n}");
        }
        for n in [
            "failure",
            "timed_out",
            "startup",
            "action",
            "cancelled",
            "ctx-bad",
            "ctx-fail",
        ] {
            assert_eq!(state_of(n), Some(CheckState::Failed), "{n}");
        }
        for n in ["running", "ctx-wait", "ctx-exp"] {
            assert_eq!(state_of(n), Some(CheckState::Pending), "{n}");
        }
        for n in ["skipped", "neutral", "stale"] {
            assert_eq!(state_of(n), None, "{n}");
        }
        assert_eq!(checks.len(), 12);
    }

    /// fail 在前、綠色在後：收合截斷時優先丟掉綠色。
    #[test]
    fn head_checks_sorted_failed_then_pending_then_passed_then_name() {
        let checks = checks_from(&[
            run("W", "b", 1, Some("SUCCESS")),
            run("W", "a", 2, Some("SUCCESS")),
            run("W", "z", 3, Some("FAILURE")),
            run("W", "m", 4, None),
        ]);
        let order: Vec<_> = checks.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(order, ["z", "m", "a", "b"]);
    }

    #[test]
    fn head_checks_empty_when_commit_has_no_rollup() {
        let json = r#"{"data":{"repository":{"pullRequest":{
            "commits":{"nodes":[{"commit":{"statusCheckRollup":null}}]},
            "timelineItems":{"pageInfo":{"hasNextPage":false,"endCursor":null},
                "nodes":[]}}}}}"#;
        let page = parse(json, GhItemKind::PullRequest).unwrap();
        assert!(page.ci_checks.is_empty());
    }

    /// 第三方 App 的 check 沒有 `workflowRun`，只能靠 `app.slug` 區分。
    fn app_run(app: &str, name: &str, id: u64, conclusion: Option<&str>) -> String {
        serde_json::json!({
            "__typename": "CheckRun", "name": name, "conclusion": conclusion,
            "databaseId": id, "checkSuite": {"app": {"slug": app}, "workflowRun": null},
        })
        .to_string()
    }

    fn event_run(
        workflow: &str,
        event: &str,
        name: &str,
        id: u64,
        conclusion: Option<&str>,
    ) -> String {
        serde_json::json!({
            "__typename": "CheckRun", "name": name, "conclusion": conclusion,
            "databaseId": id,
            "checkSuite": {"workflowRun": {"event": event, "workflow": {"name": workflow}}},
        })
        .to_string()
    }

    /// 兩個 App 各報一個叫 `build` 的 check：紅燈不能被另一個 App 的綠燈蓋掉。
    #[test]
    fn head_checks_same_name_from_different_apps_are_kept_apart() {
        let checks = checks_from(&[
            app_run("codeql", "build", 9, Some("SUCCESS")),
            app_run("other-ci", "build", 1, Some("FAILURE")),
        ]);
        assert_eq!(
            checks,
            vec![
                check("build", CheckState::Failed),
                check("build", CheckState::Passed)
            ]
        );
    }

    /// 同一個 workflow 被 push 與 pull_request 各觸發一輪：兩輪都要留著；
    /// 同一個 event 重跑仍然只留最新一筆。
    #[test]
    fn head_checks_same_workflow_from_different_events_are_kept_apart() {
        let checks = checks_from(&[
            event_run("CI", "push", "test", 1, Some("FAILURE")),
            event_run("CI", "pull_request", "test", 2, Some("SUCCESS")),
            event_run("CI", "push", "test", 3, Some("FAILURE")),
        ]);
        assert_eq!(
            checks,
            vec![
                check("test", CheckState::Failed),
                check("test", CheckState::Passed)
            ]
        );

        let checks = checks_from(&[
            event_run("CI", "push", "test", 1, Some("FAILURE")),
            event_run("CI", "push", "test", 2, Some("SUCCESS")),
        ]);
        assert_eq!(checks, vec![check("test", CheckState::Passed)]);
    }

    fn page_info(has_next: bool, cursor: Option<&str>) -> serde_json::Value {
        serde_json::json!({"hasNextPage": has_next, "endCursor": cursor})
    }

    fn raw_nodes(contexts: &[String]) -> Vec<serde_json::Value> {
        contexts
            .iter()
            .map(|c| serde_json::from_str(c).unwrap())
            .collect()
    }

    /// PR 首頁回應，head commit 的 contexts 帶著指定的 `pageInfo`。
    fn head_timeline(oid: &str, contexts: &[String], info: serde_json::Value) -> String {
        serde_json::json!({"data": {"repository": {"pullRequest": {
            "commits": {"nodes": [{"commit": {"oid": oid, "statusCheckRollup": {
                "contexts": {"pageInfo": info, "nodes": raw_nodes(contexts)}
            }}}]},
            "timelineItems": {"pageInfo": page_info(false, None), "nodes": []},
        }}}})
        .to_string()
    }

    /// `object(oid:)` 續頁查詢的回應。
    fn contexts_page(contexts: &[String], info: serde_json::Value) -> String {
        serde_json::json!({"data": {"repository": {"object": {"statusCheckRollup": {
            "contexts": {"pageInfo": info, "nodes": raw_nodes(contexts)}
        }}}}})
        .to_string()
    }

    /// 續頁要拿首頁的 oid 與上一頁的游標去抓，且去重在所有頁合併*之後*：
    /// 同一個 check 的舊 run 在首頁、新 run 在續頁，只能留新的。
    #[test]
    fn head_contexts_follow_pagination_and_dedupe_across_pages() {
        let first = head_timeline(
            "abc123",
            &[
                run("W", "a", 1, Some("FAILURE")),
                run("W", "b", 2, Some("SUCCESS")),
            ],
            page_info(true, Some("c1")),
        );
        let mut calls = Vec::new();
        let page = parse_timeline_graphql(&first, GhItemKind::PullRequest, |target, after| {
            let Target::Contexts { oid } = target else {
                panic!("expected a contexts fetch");
            };
            calls.push((oid.to_string(), after.to_string()));
            Ok(contexts_page(
                &[run("W", "a", 3, Some("SUCCESS"))],
                page_info(false, None),
            ))
        })
        .unwrap();

        assert_eq!(calls, [("abc123".to_string(), "c1".to_string())]);
        assert_eq!(
            page.ci_checks,
            vec![
                check("a", CheckState::Passed),
                check("b", CheckState::Passed)
            ]
        );
    }

    /// 最後一頁的 `endCursor` 也可能非 null：停不停只看 `hasNextPage`。
    #[test]
    fn head_contexts_do_not_fetch_when_has_next_page_is_false() {
        let json = head_timeline(
            "abc123",
            &[run("W", "a", 1, Some("SUCCESS"))],
            page_info(false, Some("stale")),
        );
        let page = parse(&json, GhItemKind::PullRequest).unwrap();
        assert_eq!(page.ci_checks, vec![check("a", CheckState::Passed)]);
    }

    /// 上限是確定性的：到頂就截斷、仍回 `Ok`，不然那個 PR 永遠載不出來。
    #[test]
    fn head_contexts_stop_at_the_page_cap_and_still_succeed() {
        let first = head_timeline(
            "abc123",
            &[run("W", "job0", 1, Some("SUCCESS"))],
            page_info(true, Some("c0")),
        );
        let mut calls = 0;
        let page = parse_timeline_graphql(&first, GhItemKind::PullRequest, |_, _| {
            calls += 1;
            Ok(contexts_page(
                &[run(
                    "W",
                    &format!("job{calls}"),
                    calls as u64 + 1,
                    Some("SUCCESS"),
                )],
                page_info(true, Some("more")),
            ))
        })
        .unwrap();

        assert_eq!(calls, MAX_EXTRA_PAGES);
        assert_eq!(page.ci_checks.len(), 1 + MAX_EXTRA_PAGES);
    }

    /// 失敗訊息要指出是 CI 續頁，不然畫面上的 `(comments failed: …)` 會誤導。
    #[test]
    fn head_contexts_failures_name_the_ci_page() {
        let first = head_timeline(
            "abc123",
            &[run("W", "a", 1, Some("SUCCESS"))],
            page_info(true, Some("c1")),
        );
        let fail = |fetch: fn(Target<'_>, &str) -> Result<String, String>| {
            parse_timeline_graphql(&first, GhItemKind::PullRequest, fetch).unwrap_err()
        };

        let e = fail(|_, _| Err("boom".to_string()));
        assert!(e.contains("CI check 第 2 頁") && e.contains("boom"), "{e}");

        let e = fail(|_, _| Ok("not json".to_string()));
        assert!(e.contains("CI check 第 2 頁：JSON 解析失敗"), "{e}");

        // oid 不存在（或不是 Commit）
        let e = fail(|_, _| Ok(r#"{"data":{"repository":{"object":null}}}"#.to_string()));
        assert!(
            e.contains("CI check 第 2 頁") && e.contains("找不到"),
            "{e}"
        );
    }

    /// 帶行內留言的 review node；`submitted_at` 為 `None` 是還沒送出的草稿。
    fn review_node(comment_ids: &[&str], submitted_at: Option<&str>) -> serde_json::Value {
        let nodes: Vec<_> = comment_ids
            .iter()
            .map(|id| {
                serde_json::json!({"id": id, "path": "a.rs", "line": 1, "originalLine": 1,
                    "outdated": false, "body": "x"})
            })
            .collect();
        serde_json::json!({"__typename": "PullRequestReview", "state": "COMMENTED", "body": "",
            "submittedAt": submitted_at, "author": {"login": "carol"},
            "comments": {"totalCount": nodes.len(), "nodes": nodes}})
    }

    fn thread(resolved: bool, comment_ids: &[&str]) -> serde_json::Value {
        let nodes: Vec<_> = comment_ids
            .iter()
            .map(|id| serde_json::json!({"id": id}))
            .collect();
        serde_json::json!({"isResolved": resolved, "comments": {"nodes": nodes}})
    }

    /// PR 首頁回應：threads 帶指定的 `pageInfo`，timeline 只有給定的 review。
    fn threads_timeline(
        threads: Vec<serde_json::Value>,
        info: serde_json::Value,
        reviews: Vec<serde_json::Value>,
    ) -> String {
        serde_json::json!({"data": {"repository": {"pullRequest": {
            "reviewThreads": {"pageInfo": info, "nodes": threads},
            "timelineItems": {"pageInfo": page_info(false, None), "nodes": reviews},
        }}}})
        .to_string()
    }

    /// `pullRequest.reviewThreads` 續頁查詢的回應。
    fn threads_page(threads: Vec<serde_json::Value>, info: serde_json::Value) -> String {
        serde_json::json!({"data": {"repository": {"pullRequest": {
            "reviewThreads": {"pageInfo": info, "nodes": threads}
        }}}})
        .to_string()
    }

    /// 所有 review 的留言 `(id, resolved)`。
    fn resolved_of(page: &GhTimelinePage) -> Vec<(String, bool)> {
        page.items
            .iter()
            .filter_map(|item| match item {
                GhTimelineItem::PullRequestReview { comments, .. } => Some(&comments.nodes),
                _ => None,
            })
            .flatten()
            .map(|c| (c.id.clone(), c.resolved))
            .collect()
    }

    fn resolved(id: &str, resolved: bool) -> (String, bool) {
        (id.to_string(), resolved)
    }

    /// 第 101 個之後的 thread 也要收到：C2、C3 的 thread 在續頁，
    /// C2 的 thread 已 resolved、C3 沒有。
    #[test]
    fn review_threads_follow_pagination_and_backfill_resolved() {
        let first = threads_timeline(
            vec![thread(true, &["C1"])],
            page_info(true, Some("t1")),
            vec![review_node(
                &["C1", "C2", "C3"],
                Some("2026-01-01T00:00:00Z"),
            )],
        );
        let mut calls = Vec::new();
        let page = parse_timeline_graphql(&first, GhItemKind::PullRequest, |target, after| {
            assert!(matches!(target, Target::ReviewThreads));
            calls.push(after.to_string());
            Ok(threads_page(
                vec![thread(true, &["C2"]), thread(false, &["C3"])],
                page_info(false, None),
            ))
        })
        .unwrap();

        assert_eq!(calls, ["t1"]);
        assert_eq!(
            resolved_of(&page),
            [
                resolved("C1", true),
                resolved("C2", true),
                resolved("C3", false)
            ]
        );
    }

    /// 這一頁的留言都找到 thread 之後就停：一則留言只屬於一個 thread，
    /// 後面的頁不可能改變它的狀態。
    #[test]
    fn review_threads_stop_once_every_comment_is_located() {
        let all_on_first_page = threads_timeline(
            vec![thread(true, &["C1"])],
            page_info(true, Some("t1")),
            vec![review_node(&["C1"], Some("2026-01-01T00:00:00Z"))],
        );
        // `parse` 的 fetch 會 panic：追了頁就是錯
        let page = parse(&all_on_first_page, GhItemKind::PullRequest).unwrap();
        assert_eq!(resolved_of(&page), [resolved("C1", true)]);

        let second_page_has_more = threads_timeline(
            vec![thread(false, &["C1"])],
            page_info(true, Some("t1")),
            vec![review_node(&["C1", "C2"], Some("2026-01-01T00:00:00Z"))],
        );
        let mut calls = 0;
        let page =
            parse_timeline_graphql(&second_page_has_more, GhItemKind::PullRequest, |_, _| {
                calls += 1;
                Ok(threads_page(
                    vec![thread(true, &["C2"])],
                    page_info(true, Some("t2")),
                ))
            })
            .unwrap();
        assert_eq!(calls, 1, "C2 在第 2 頁找到，不該再抓第 3 頁");
        assert_eq!(
            resolved_of(&page),
            [resolved("C1", false), resolved("C2", true)]
        );
    }

    /// 沒有已送出的 review 留言就不需要 resolved 狀態，一頁都不追；
    /// 使用者自己的 PENDING 草稿 view 也不顯示，不能拿來觸發追頁。
    #[test]
    fn review_threads_are_not_fetched_without_submitted_review_comments() {
        for reviews in [
            Vec::new(),
            vec![review_node(&["C1"], None)],
            vec![review_node(&[], Some("2026-01-01T00:00:00Z"))],
        ] {
            let json = threads_timeline(
                vec![thread(false, &["Z"])],
                page_info(true, Some("t1")),
                reviews,
            );
            parse(&json, GhItemKind::PullRequest).unwrap();
        }
    }

    /// 找不到的 id 會一路追到上限：到頂就截斷、仍回 `Ok`。
    #[test]
    fn review_threads_stop_at_the_page_cap_and_still_succeed() {
        let first = threads_timeline(
            vec![thread(false, &["A"])],
            page_info(true, Some("t0")),
            vec![review_node(&["missing"], Some("2026-01-01T00:00:00Z"))],
        );
        let mut calls = 0;
        let page = parse_timeline_graphql(&first, GhItemKind::PullRequest, |_, _| {
            calls += 1;
            Ok(threads_page(
                vec![thread(false, &["B"])],
                page_info(true, Some("more")),
            ))
        })
        .unwrap();

        assert_eq!(calls, MAX_EXTRA_PAGES);
        assert_eq!(resolved_of(&page), [resolved("missing", false)]);
    }

    #[test]
    fn review_threads_failures_name_the_page() {
        let first = threads_timeline(
            vec![thread(false, &["A"])],
            page_info(true, Some("t1")),
            vec![review_node(&["C1"], Some("2026-01-01T00:00:00Z"))],
        );
        let fail = |fetch: fn(Target<'_>, &str) -> Result<String, String>| {
            parse_timeline_graphql(&first, GhItemKind::PullRequest, fetch).unwrap_err()
        };

        let e = fail(|_, _| Err("boom".to_string()));
        assert!(
            e.contains("Review thread 第 2 頁") && e.contains("boom"),
            "{e}"
        );

        let e = fail(|_, _| Ok("not json".to_string()));
        assert!(e.contains("Review thread 第 2 頁：JSON 解析失敗"), "{e}");

        let e = fail(|_, _| Ok(r#"{"data":{"repository":{"pullRequest":null}}}"#.to_string()));
        assert!(
            e.contains("Review thread 第 2 頁") && e.contains("找不到"),
            "{e}"
        );
    }

    /// 同一頁 contexts 與 threads 都有下一頁：兩個 collector 共用同一個
    /// `fetch_more`，各自恰好收到自己的那一次續頁。
    #[test]
    fn contexts_and_threads_continuations_are_dispatched_to_the_right_query() {
        let first = serde_json::json!({"data": {"repository": {"pullRequest": {
            "reviewThreads": {"pageInfo": page_info(true, Some("t1")),
                "nodes": [thread(false, &["C1"])]},
            "commits": {"nodes": [{"commit": {"oid": "abc123", "statusCheckRollup": {
                "contexts": {"pageInfo": page_info(true, Some("c1")),
                    "nodes": raw_nodes(&[run("W", "a", 1, Some("SUCCESS"))])}
            }}}]},
            "timelineItems": {"pageInfo": page_info(false, None),
                "nodes": [review_node(&["C1", "C2"], Some("2026-01-01T00:00:00Z"))]},
        }}}})
        .to_string();

        let mut calls = Vec::new();
        let page =
            parse_timeline_graphql(
                &first,
                GhItemKind::PullRequest,
                |target, after| match target {
                    Target::Contexts { oid } => {
                        calls.push(format!("contexts:{oid}:{after}"));
                        Ok(contexts_page(
                            &[run("W", "b", 2, Some("FAILURE"))],
                            page_info(false, None),
                        ))
                    }
                    Target::ReviewThreads => {
                        calls.push(format!("threads:{after}"));
                        Ok(threads_page(
                            vec![thread(true, &["C2"])],
                            page_info(false, None),
                        ))
                    }
                },
            )
            .unwrap();

        calls.sort();
        assert_eq!(calls, ["contexts:abc123:c1", "threads:t1"]);
        assert_eq!(
            resolved_of(&page),
            [resolved("C1", false), resolved("C2", true)]
        );
        assert_eq!(
            page.ci_checks,
            vec![
                check("b", CheckState::Failed),
                check("a", CheckState::Passed)
            ]
        );
    }
}

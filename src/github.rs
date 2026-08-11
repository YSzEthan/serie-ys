use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{mpsc, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use rustc_hash::FxHashMap;
use serde::{Deserialize, Deserializer};

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

    pub fn display_name(self) -> &'static str {
        match self {
            GhItemKind::Issue => "Issue",
            GhItemKind::PullRequest => "Pull Request",
        }
    }

    /// 句中名詞，例如 "Close PR #12?"。與 [`Self::as_str`]（gh argv）和
    /// [`Self::display_name`]（標題式標籤）是三條各自獨立的輸出通道 ——
    /// 合併任兩條就會讓文案的改動洩漏到 argv 上。
    pub fn noun(self) -> &'static str {
        match self {
            GhItemKind::Issue => "issue",
            GhItemKind::PullRequest => "PR",
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
    pub base_ref_name: String,
    pub is_draft: bool,
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
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// 把 pipe 讀到底丟進 channel；呼叫端只能用 `recv_timeout` 有界地等，
/// 不能 `join`（理由見 `run_with_timeout`）。
fn spawn_reader<R: Read + Send + 'static>(mut pipe: R) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    rx
}

/// 逾時就砍掉的子行程執行，`run_gh`（純讀，`stdin_data = None`）跟
/// `update_body`（要把 body 灌進 stdin，`stdin_data = Some(body)`）共用。
///
/// **函式內部唯一的等待機制是 `deadline`，沒有任何 `join()`**——`gh` 常會
/// fork 出 `git` 之類的孫行程，`kill()` 只殺得掉 `gh` 自己，孫行程繼承的
/// pipe 寫端沒關，reader thread 永遠等不到 EOF，`join()` 會卡死——這正好
/// 是「有子行程掛在網路上」的情境，也就是這個函式最該生效的那個情境。
/// 改用 `spawn_reader` + `recv_timeout(剩餘預算)`，不管子行程是被我們
/// kill、還是自己結束但孫行程仍握著 pipe，都保證在 `timeout` 內返回。
fn run_with_timeout(
    mut cmd: Command,
    stdin_data: Option<&[u8]>,
    timeout: Duration,
) -> Result<Output, String> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    cmd.stdin(if stdin_data.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to execute {program}: {e}"))?;

    if let Some(data) = stdin_data {
        // 射後不理：唯一的副作用是寫完 drop 掉 `stdin`，讓子行程收到
        // EOF。不能等它——它可能跟 reader thread 一樣卡在孫行程手上。
        let mut stdin = child
            .stdin
            .take()
            .expect("stdin is piped when stdin_data is Some");
        let data = data.to_vec();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&data);
        });
    }

    let stdout_rx = spawn_reader(child.stdout.take().expect("stdout is piped"));
    let stderr_rx = spawn_reader(child.stderr.take().expect("stderr is piped"));

    let deadline = Instant::now() + timeout;
    let status = loop {
        let failure = match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(POLL_INTERVAL);
                continue;
            }
            Ok(None) => format!(
                "{program} command timed out after {}s (network issue?)",
                timeout.as_secs()
            ),
            // try_wait 本身失敗（極罕見）也走同一條收尾。
            Err(e) => format!("Failed to wait for {program}: {e}"),
        };
        let _ = child.kill();
        let _ = child.wait(); // reap，不留 zombie；不等 reader thread
        return Err(failure);
    };

    // 子行程自己結束了（不是被我們 kill），但孫行程可能仍握著 pipe 寫端
    // 不放——用剩餘的 timeout 預算等 reader，逾時就當空輸出，不能無界等
    // （這條路徑對應 `Command::output()`／`wait_with_output()` 今天就有
    // 的同一個理論限制，這裡額外把它也收進同一個 deadline 預算裡）。
    //
    // 每次都要重算剩餘預算：算一次餵給兩個 `recv_timeout` 的話，子行程
    // 一結束就 break（剩餘 ≈ 整個 timeout）時兩次各可耗滿，總共變成
    // 2×timeout。（`Receiver::recv_deadline()` 能一步到位，但還是
    // unstable，rust-lang/rust#46316，不能用。）
    //
    // 逾時當空輸出的後果：`run_gh` 會把空字串餵給 `parse_issues_graphql`，
    // 使用者看到的是 JSON parse error 而不是 timed out 訊息。不改成回報
    // 錯誤是因為 `update_body` 根本不需要 stdout，強制報錯會對「其實
    // 成功了」的編輯回報假錯誤，而 checkbox toggle 不冪等，使用者重試
    // 會把狀態弄反——回假成功比回假失敗安全。
    let remaining = || deadline.saturating_duration_since(Instant::now());
    let stdout = stdout_rx.recv_timeout(remaining()).unwrap_or_default();
    let stderr = stderr_rx.recv_timeout(remaining()).unwrap_or_default();

    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn run_gh(path: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("gh");
    cmd.args(args).current_dir(path);

    let output = run_with_timeout(cmd, None, GH_TIMEOUT)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("gh command failed: {stderr}"));
    }

    String::from_utf8(output.stdout).map_err(|e| format!("Invalid UTF-8: {e}"))
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
        .ok_or_else(|| format!("Unexpected nameWithOwner: {s}"))?;
    let entry = (owner.to_string(), name.to_string());

    repo_name_cache().insert(path.to_path_buf(), entry.clone());
    Ok(entry)
}

fn parse_issues_graphql(json: &str) -> Result<GhPage<GhIssue>, String> {
    let resp: GqlIssuesResp =
        serde_json::from_str(json).map_err(|e| format!("JSON parse error: {e}"))?;
    let list = resp.data.repository.issues;
    let next_cursor = if list.page_info.has_next_page {
        list.page_info.end_cursor
    } else {
        None
    };
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
    issues: GqlIssueList,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlIssueList {
    #[serde(default)]
    page_info: GqlPageInfo,
    nodes: Vec<GqlIssueNode>,
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
struct GqlConnection<T> {
    nodes: Vec<T>,
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
                pullRequests(first:50,after:$after,states:{states},orderBy:{{field:CREATED_AT,direction:DESC}}){{
                    pageInfo {{ hasNextPage endCursor }}
                    nodes {{
                        number title state body url closedAt updatedAt headRefName baseRefName isDraft
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
    let resp: GqlPrsResp =
        serde_json::from_str(json).map_err(|e| format!("JSON parse error: {e}"))?;
    let list = resp.data.repository.pull_requests;
    let next_cursor = if list.page_info.has_next_page {
        list.page_info.end_cursor
    } else {
        None
    };
    Ok(GhPage {
        items: list.nodes.into_iter().map(GqlPrNode::into_gh_pr).collect(),
        next_cursor,
    })
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
    pull_requests: GqlPrList,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlPrList {
    #[serde(default)]
    page_info: GqlPageInfo,
    nodes: Vec<GqlPrNode>,
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
    base_ref_name: String,
    is_draft: bool,
    #[serde(default)]
    closed_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
    author: Option<GhAuthor>,
    labels: GqlConnection<GhLabel>,
    closing_issues_references: GqlConnection<GhRelatedIssue>,
}

impl GqlPrNode {
    fn into_gh_pr(self) -> GhPullRequest {
        GhPullRequest {
            number: self.number,
            title: self.title,
            state: self.state,
            labels: self.labels.nodes,
            author: self.author.unwrap_or(GhAuthor {
                login: "ghost".to_string(),
            }),
            head_ref_name: self.head_ref_name,
            base_ref_name: self.base_ref_name,
            is_draft: self.is_draft,
            body: self.body.unwrap_or_default(),
            url: self.url.unwrap_or_default(),
            closed_at: self.closed_at,
            updated_at: self.updated_at.unwrap_or_default(),
            linked_issues: self.closing_issues_references.nodes,
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
    /// `itemTypes` 理論上只會產生上面兩種 variant，但把整頁的反序列化都賭在
    /// GitHub 永遠不會新增第三種上並不值得 —— 否則一個未知的 `__typename` 會讓
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
}

/// issue 與 PR 的 timeline 是兩個不同的 union：`Issue.timelineItems` 給的是
/// `IssueTimelineItems`，裡面既沒有 `PullRequestCommit` 也沒有 `mergeable`。
/// 對 issue 送出這些片段會讓整個查詢被 GraphQL 擋下（"Fragment on
/// PullRequestCommit can't be spread inside IssueTimelineItems"），而不是安靜地
/// 回傳空結果 —— 所以四處分岔綁在同一個 match 上，漏掉其中一項就編不過。
fn build_timeline_query(kind: GhItemKind) -> String {
    let (item_field, item_types, mergeable_field, commit_fragment) = match kind {
        GhItemKind::Issue => ("issue", "ISSUE_COMMENT", "", ""),
        GhItemKind::PullRequest => (
            "pullRequest",
            "ISSUE_COMMENT, PULL_REQUEST_COMMIT",
            "mergeable",
            r#"... on PullRequestCommit { commit {
                                abbreviatedOid messageHeadline
                                statusCheckRollup { state }
                            } }"#,
        ),
    };
    // 用 100（connection 上限）而非 50：commit 只佔一行視覺高度，
    // 而留言常常佔好幾行，混在同一頁會讓一頁實際能看到的內容打對折。
    format!(
        r#"query($owner:String!,$name:String!,$number:Int!,$after:String){{
            repository(owner:$owner,name:$name){{
                {item_field}(number:$number){{
                    {mergeable_field}
                    timelineItems(first:100,after:$after,itemTypes:[{item_types}]){{
                        pageInfo {{ hasNextPage endCursor }}
                        nodes {{
                            __typename
                            ... on IssueComment {{ body createdAt author {{ login }} }}
                            {commit_fragment}
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
    let query = build_timeline_query(kind);
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
    parse_timeline_graphql(&json, kind)
}

fn parse_timeline_graphql(json: &str, kind: GhItemKind) -> Result<GhTimelinePage, String> {
    let resp: GqlTimelineResp =
        serde_json::from_str(json).map_err(|e| format!("JSON parse error: {e}"))?;
    let container = match kind {
        GhItemKind::Issue => resp.data.repository.issue,
        GhItemKind::PullRequest => resp.data.repository.pull_request,
    };
    let Some(container) = container else {
        return Ok(GhTimelinePage::default());
    };
    let conn = container.timeline_items;
    let next_cursor = if conn.page_info.has_next_page {
        conn.page_info.end_cursor
    } else {
        None
    };
    Ok(GhTimelinePage {
        items: conn.nodes,
        next_cursor,
        mergeable: container.mergeable.as_deref().and_then(Mergeable::from_api),
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
    timeline_items: GqlTimelineConn,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlTimelineConn {
    #[serde(default)]
    page_info: GqlPageInfo,
    nodes: Vec<GhTimelineItem>,
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
        return Err(format!("gh edit failed: {stderr}"));
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
        let verb = match self {
            StateAction::Close => "Close",
            StateAction::Reopen => "Reopen",
        };
        format!("{verb} {} #{number}? ", kind.noun())
    }

    pub fn pending(self, kind: GhItemKind, number: u64) -> String {
        let verb = match self {
            StateAction::Close => "Closing",
            StateAction::Reopen => "Reopening",
        };
        format!("{verb} {} #{number}...", kind.noun())
    }

    pub fn success(self, kind: GhItemKind, number: u64) -> String {
        let verb = match self {
            StateAction::Close => "Closed",
            StateAction::Reopen => "Reopened",
        };
        format!("{verb} {} #{number}", kind.noun())
    }

    pub fn hint_label(self, kind: GhItemKind) -> &'static str {
        match (self, kind) {
            (StateAction::Close, GhItemKind::Issue) => "close issue",
            (StateAction::Reopen, GhItemKind::Issue) => "reopen issue",
            (StateAction::Close, GhItemKind::PullRequest) => "close PR",
            (StateAction::Reopen, GhItemKind::PullRequest) => "reopen PR",
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

pub fn merge_pr(path: &Path, number: u64, method: &str, delete_branch: bool) -> Result<(), String> {
    let num_str = number.to_string();
    let mut args = vec!["pr", "merge", &num_str, method];
    if delete_branch {
        args.push("--delete-branch");
    }
    run_gh(path, &args)?;
    Ok(())
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
            PrDraftAction::MarkReady => format!("Mark PR #{number} ready for review? "),
            PrDraftAction::ConvertToDraft => format!("Convert PR #{number} back to draft? "),
        }
    }

    pub fn pending(self, number: u64) -> String {
        match self {
            PrDraftAction::MarkReady => format!("Marking PR #{number} ready..."),
            PrDraftAction::ConvertToDraft => format!("Converting PR #{number} to draft..."),
        }
    }

    pub fn success(self, number: u64) -> String {
        match self {
            PrDraftAction::MarkReady => format!("PR #{number} is ready for review"),
            PrDraftAction::ConvertToDraft => format!("PR #{number} converted to draft"),
        }
    }

    pub fn hint_label(self) -> &'static str {
        match self {
            PrDraftAction::MarkReady => "ready for review",
            PrDraftAction::ConvertToDraft => "back to draft",
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

    // ── run_with_timeout() ──
    //
    // 所有逾時測試共用的預算與斷言上限。上限抓 3 倍而不是「反正很大」的
    // 2 秒：這幾條測試唯一的價值就是證明「有界」，斷言鬆到跟預算無關就
    // 什麼都沒證明。

    const BUDGET: Duration = Duration::from_millis(150);
    const MAX_ELAPSED: Duration = Duration::from_millis(450);

    #[test]
    fn run_with_timeout_pipes_stdin_and_captures_stdout() {
        let output = run_with_timeout(
            Command::new("cat"),
            Some(b"hello\n"),
            Duration::from_secs(5),
        )
        .expect("cat should succeed");
        assert!(output.status.success());
        assert_eq!(output.stdout, b"hello\n");
    }

    #[test]
    fn run_with_timeout_kills_a_directly_hanging_child() {
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        let start = Instant::now();
        let err = run_with_timeout(cmd, None, BUDGET).expect_err("sleep 30 should time out");
        assert!(err.contains("timed out"), "{err}");
        assert!(start.elapsed() < MAX_ELAPSED, "took {:?}", start.elapsed());
    }

    #[test]
    fn run_with_timeout_does_not_hang_when_a_killed_childs_orphan_still_holds_the_pipe() {
        // sh 自己因為 `wait` 卡住 30 秒（觸發 kill），但它背景起的 sleep
        // 繼承了同一組 stdout/stderr 寫端——kill 掉 sh 不會連帶殺掉 sleep，
        // 這正是 join() 版本會卡死的情境，即使子行程本身已經被砍掉、reap 掉。
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 30 & wait"]);
        let start = Instant::now();
        let err = run_with_timeout(cmd, None, BUDGET)
            .expect_err("should time out even though a grandchild still holds the pipe open");
        assert!(err.contains("timed out"), "{err}");
        assert!(start.elapsed() < MAX_ELAPSED, "took {:?}", start.elapsed());
    }

    #[test]
    fn run_with_timeout_does_not_exceed_budget_when_child_exits_but_orphan_holds_the_pipe() {
        // sh 沒有 `wait`，立刻結束（成功路徑，不會觸發 kill）；但背景的
        // sleep 一樣繼承了 pipe 寫端還活著。這條是「remaining 算一次用
        // 兩次」的迴歸測試：那個寫法在這裡會花掉 2×BUDGET，每次重算才會
        // 落在 BUDGET 附近。
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 30 &"]);
        let start = Instant::now();
        let output = run_with_timeout(cmd, None, BUDGET)
            .expect("sh itself exits immediately, this should not error");
        assert!(output.status.success());
        assert!(start.elapsed() < MAX_ELAPSED, "took {:?}", start.elapsed());
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

    /// 三條輸出通道各自獨立：argv 用小寫 `pr`，句中名詞用 `PR`，標籤用完整名稱。
    #[test]
    fn item_kind_has_three_distinct_channels() {
        let pr = GhItemKind::PullRequest;
        assert_eq!(
            (pr.as_str(), pr.noun(), pr.display_name()),
            ("pr", "PR", "Pull Request")
        );
        let issue = GhItemKind::Issue;
        assert_eq!(
            (issue.as_str(), issue.noun(), issue.display_name()),
            ("issue", "issue", "Issue")
        );
    }

    #[test]
    fn state_action_messages_name_the_right_kind() {
        assert_eq!(
            StateAction::Close.prompt(GhItemKind::PullRequest, 12),
            "Close PR #12? "
        );
        assert_eq!(
            StateAction::Close.prompt(GhItemKind::Issue, 12),
            "Close issue #12? "
        );
        assert_eq!(
            StateAction::Reopen.success(GhItemKind::PullRequest, 12),
            "Reopened PR #12"
        );
        assert_eq!(
            StateAction::Close.pending(GhItemKind::Issue, 12),
            "Closing issue #12..."
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
        let page = parse_timeline_graphql(json, GhItemKind::Issue).unwrap();
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
        let page = parse_timeline_graphql(json, GhItemKind::PullRequest).unwrap();
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
        let items = parse_timeline_graphql(json, GhItemKind::PullRequest)
            .unwrap()
            .items;

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
        let page = parse_timeline_graphql(json, GhItemKind::PullRequest).unwrap();
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
        let page = parse_timeline_graphql(&json, GhItemKind::PullRequest).unwrap();
        assert_eq!(page.mergeable, Some(Mergeable::Mergeable));

        let json = timeline_json(Some("CONFLICTING"));
        let page = parse_timeline_graphql(&json, GhItemKind::PullRequest).unwrap();
        assert_eq!(page.mergeable, Some(Mergeable::Conflicting));

        let json = timeline_json(Some("UNKNOWN"));
        let page = parse_timeline_graphql(&json, GhItemKind::PullRequest).unwrap();
        assert_eq!(page.mergeable, None);

        let json = timeline_json(None);
        let page = parse_timeline_graphql(&json, GhItemKind::PullRequest).unwrap();
        assert_eq!(page.mergeable, None);
    }

    /// issue 的 timeline union 不含 `PullRequestCommit`，也沒有 `mergeable`。
    /// 只要有一項漏了分岔，GitHub 就會退回整個查詢，detail 畫面只剩
    /// "comments failed"。
    #[test]
    fn timeline_query_omits_pr_only_pieces_for_issues() {
        let q = build_timeline_query(GhItemKind::Issue);
        assert!(q.contains("issue(number:$number)"));
        assert!(!q.contains("PullRequestCommit"));
        assert!(!q.contains("PULL_REQUEST_COMMIT"));
        assert!(!q.contains("mergeable"));

        let q = build_timeline_query(GhItemKind::PullRequest);
        assert!(q.contains("pullRequest(number:$number)"));
        assert!(q.contains("PullRequestCommit"));
        assert!(q.contains("PULL_REQUEST_COMMIT"));
        assert!(q.contains("mergeable"));
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
        let page = parse_timeline_graphql(json, GhItemKind::Issue).unwrap();
        assert_eq!(page.items.len(), 2);
        assert!(matches!(page.items[0], GhTimelineItem::Unknown));
        assert!(matches!(page.items[1], GhTimelineItem::IssueComment { .. }));
    }
}

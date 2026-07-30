use std::path::Path;
use std::process::{Command, Stdio};

use serde::Deserialize;

// ── 分頁回傳 ──

pub struct GhPage<T> {
    pub items: Vec<T>,
    /// Some(cursor) 代表還有下一頁；None 代表已到底
    pub next_cursor: Option<String>,
}

// ── Item Kind ──

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
    #[serde(default)]
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

// ── CLI 包裝 ──

fn run_gh(path: &Path, args: &[&str], force_tty: bool) -> Result<String, String> {
    let mut cmd = Command::new("gh");
    cmd.args(args).current_dir(path).stdin(Stdio::null());
    if force_tty {
        cmd.env("GH_FORCE_TTY", "200");
    }

    let output = cmd
        .output()
        .map_err(|e| format!("Failed to execute gh: {e}"))?;

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
    let json = run_gh(path, &args, false)?;
    parse_issues_graphql(&json)
}

fn fetch_repo_name_with_owner(path: &Path) -> Result<(String, String), String> {
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
        false,
    )?;
    let s = out.trim();
    let (owner, name) = s
        .split_once('/')
        .ok_or_else(|| format!("Unexpected nameWithOwner: {s}"))?;
    Ok((owner.to_string(), name.to_string()))
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

// ── GraphQL response wrapper types ──

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
    let json = run_gh(path, &args, false)?;
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

// ── GraphQL PR response wrapper types ──

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

// ── Timeline (comments + commits, interleaved in GitHub's own order) ──

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
    /// `itemTypes` should only ever produce the two variants above, but
    /// betting the whole page's deserialization on GitHub never adding a
    /// third is not worth it — an unrecognized `__typename` would otherwise
    /// fail every node instead of just this one.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GhCommit {
    pub abbreviated_oid: String,
    pub message_headline: String,
    #[serde(default)]
    pub status_check_rollup: Option<GhStatusCheckRollup>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GhStatusCheckRollup {
    pub state: String,
}

pub fn get_timeline(
    path: &Path,
    number: u64,
    kind: GhItemKind,
    after: Option<&str>,
) -> Result<GhPage<GhTimelineItem>, String> {
    let (owner, name) = fetch_repo_name_with_owner(path)?;
    let item_field = match kind {
        GhItemKind::Issue => "issue",
        GhItemKind::PullRequest => "pullRequest",
    };
    // Issues have no commits — asking for PULL_REQUEST_COMMIT there is a
    // GraphQL validation error, not an empty result.
    let item_types = match kind {
        GhItemKind::Issue => "ISSUE_COMMENT",
        GhItemKind::PullRequest => "ISSUE_COMMENT, PULL_REQUEST_COMMIT",
    };
    // 100 (the connection max) rather than 50: commits only take one visual
    // line while comments often take many, so mixing them into one page
    // halves how far a page actually gets you.
    let query = format!(
        r#"query($owner:String!,$name:String!,$number:Int!,$after:String){{
            repository(owner:$owner,name:$name){{
                {item_field}(number:$number){{
                    timelineItems(first:100,after:$after,itemTypes:[{item_types}]){{
                        pageInfo {{ hasNextPage endCursor }}
                        nodes {{
                            __typename
                            ... on IssueComment {{ body createdAt author {{ login }} }}
                            ... on PullRequestCommit {{ commit {{
                                abbreviatedOid messageHeadline
                                statusCheckRollup {{ state }}
                            }} }}
                        }}
                    }}
                }}
            }}
        }}"#
    );
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
    let json = run_gh(path, &args, false)?;
    parse_timeline_graphql(&json, kind)
}

fn parse_timeline_graphql(json: &str, kind: GhItemKind) -> Result<GhPage<GhTimelineItem>, String> {
    let resp: GqlTimelineResp =
        serde_json::from_str(json).map_err(|e| format!("JSON parse error: {e}"))?;
    let conn = match kind {
        GhItemKind::Issue => resp.data.repository.issue.map(|i| i.timeline_items),
        GhItemKind::PullRequest => resp.data.repository.pull_request.map(|p| p.timeline_items),
    };
    let Some(conn) = conn else {
        return Ok(GhPage {
            items: Vec::new(),
            next_cursor: None,
        });
    };
    let next_cursor = if conn.page_info.has_next_page {
        conn.page_info.end_cursor
    } else {
        None
    };
    Ok(GhPage {
        items: conn.nodes,
        next_cursor,
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
    timeline_items: GqlTimelineConn,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlTimelineConn {
    #[serde(default)]
    page_info: GqlPageInfo,
    nodes: Vec<GhTimelineItem>,
}

// ── Checkbox / Task List ──

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
        false,
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

            let label = trimmed[6..].to_string();

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
    let output = Command::new("gh")
        .args([kind.as_str(), "edit", &num_str, "--body-file", "-"])
        .current_dir(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                stdin.write_all(body.as_bytes())?;
            }
            child.wait_with_output()
        })
        .map_err(|e| format!("Failed to execute gh: {e}"))?;

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

// ── Issue／PR state toggle ──

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
    run_gh(
        path,
        &[kind.as_str(), action.verb(), &number.to_string()],
        false,
    )?;
    Ok(())
}

pub fn merge_pr(path: &Path, number: u64, method: &str, delete_branch: bool) -> Result<(), String> {
    let num_str = number.to_string();
    let mut args = vec!["pr", "merge", &num_str, method];
    if delete_branch {
        args.push("--delete-branch");
    }
    run_gh(path, &args, false)?;
    Ok(())
}

// ── PR draft toggle ──

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
    run_gh(path, &args, false)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// `itemTypes` should only ever produce the two known variants, but this
    /// pins down the fallback: an unrecognized `__typename` must not fail
    /// deserialization of the whole page.
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

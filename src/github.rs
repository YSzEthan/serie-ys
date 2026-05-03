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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    cmd.args(args).current_dir(path);
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

pub fn merge_pr(path: &Path, number: u64, method: &str, delete_branch: bool) -> Result<(), String> {
    let num_str = number.to_string();
    let mut args = vec!["pr", "merge", &num_str, method];
    if delete_branch {
        args.push("--delete-branch");
    }
    run_gh(path, &args, false)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

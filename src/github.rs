use std::path::Path;
use std::process::Command;

use serde::Deserialize;

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

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GhIssue {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub labels: Vec<GhLabel>,
    pub author: GhAuthor,
    pub created_at: String,
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
    pub is_draft: bool,
}

// ── CLI 包裝 ──

fn run_gh(path: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("gh")
        .args(args)
        .current_dir(path)
        .output()
        .map_err(|e| format!("Failed to execute gh: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("gh command failed: {stderr}"));
    }

    String::from_utf8(output.stdout).map_err(|e| format!("Invalid UTF-8: {e}"))
}

fn run_gh_rendered(path: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("gh")
        .args(args)
        .current_dir(path)
        .env("GH_FORCE_TTY", "200")
        .output()
        .map_err(|e| format!("Failed to execute gh: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("gh command failed: {stderr}"));
    }

    String::from_utf8(output.stdout).map_err(|e| format!("Invalid UTF-8: {e}"))
}

pub fn list_issues(path: &Path, state: &str) -> Result<Vec<GhIssue>, String> {
    let json = run_gh(
        path,
        &[
            "issue",
            "list",
            "--state",
            state,
            "--limit",
            "50",
            "--json",
            "number,title,state,labels,author,createdAt",
        ],
    )?;
    serde_json::from_str(&json).map_err(|e| format!("JSON parse error: {e}"))
}

pub fn list_pull_requests(path: &Path, state: &str) -> Result<Vec<GhPullRequest>, String> {
    let json = run_gh(
        path,
        &[
            "pr",
            "list",
            "--state",
            state,
            "--limit",
            "50",
            "--json",
            "number,title,state,labels,author,headRefName,isDraft",
        ],
    )?;
    serde_json::from_str(&json).map_err(|e| format!("JSON parse error: {e}"))
}

pub fn view_issue_rendered(path: &Path, number: u64) -> Result<String, String> {
    let mut rendered =
        run_gh_rendered(path, &["issue", "view", &number.to_string(), "--comments"])?;

    // 從 JSON 取得原始 body + comments，提取圖片 URL
    if let Ok(json) = run_gh(
        path,
        &[
            "issue",
            "view",
            &number.to_string(),
            "--json",
            "body,comments",
        ],
    ) {
        append_image_urls_from_json(&mut rendered, &json);
    }

    Ok(rendered)
}

pub fn view_pr_rendered(path: &Path, number: u64) -> Result<String, String> {
    let mut rendered = run_gh_rendered(path, &["pr", "view", &number.to_string(), "--comments"])?;

    // 從 JSON 取得原始 body + comments，提取圖片 URL
    if let Ok(json) = run_gh(
        path,
        &["pr", "view", &number.to_string(), "--json", "body,comments"],
    ) {
        append_image_urls_from_json(&mut rendered, &json);
    }

    Ok(rendered)
}

fn append_image_urls_from_json(rendered: &mut String, json: &str) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return;
    };

    let mut all_markdown = String::new();
    if let Some(body) = value.get("body").and_then(|v| v.as_str()) {
        all_markdown.push_str(body);
    }
    if let Some(comments) = value.get("comments").and_then(|v| v.as_array()) {
        for comment in comments {
            if let Some(body) = comment.get("body").and_then(|v| v.as_str()) {
                all_markdown.push('\n');
                all_markdown.push_str(body);
            }
        }
    }

    let urls = extract_image_urls(&all_markdown);
    if !urls.is_empty() {
        rendered.push_str("\n── Images ──\n");
        for url in &urls {
            rendered.push_str(url);
            rendered.push('\n');
        }
    }
}

/// 從 markdown 文字中提取圖片 URL（支援 `![alt](url)` 和 `<img src="...">` 格式）
fn extract_image_urls(markdown: &str) -> Vec<String> {
    let mut urls = Vec::new();

    // 提取 markdown ![alt](url)
    let mut rest = markdown;
    while let Some(pos) = rest.find("![") {
        rest = &rest[pos + 2..];
        if let Some(bracket_end) = rest.find("](") {
            let url_start = bracket_end + 2;
            if let Some(paren_end) = rest[url_start..].find(')') {
                let url = &rest[url_start..url_start + paren_end];
                if !url.is_empty() {
                    urls.push(url.to_string());
                }
                rest = &rest[url_start + paren_end + 1..];
                continue;
            }
        }
    }

    // 提取 HTML <img src="..."> 或 <img src='...'>
    rest = markdown;
    while let Some(img_pos) = rest.find("<img") {
        rest = &rest[img_pos + 4..];
        // 在該標籤結束（'>'）之前尋找 src 屬性
        let tag_end = rest.find('>').unwrap_or(rest.len());
        let tag_content = &rest[..tag_end];
        if let Some(src_pos) = tag_content.find("src=") {
            let after_src = &tag_content[src_pos + 4..];
            let quote = after_src.as_bytes().first().copied().unwrap_or(0);
            if quote == b'"' || quote == b'\'' {
                let after_quote = &after_src[1..];
                if let Some(end) = after_quote.find(quote as char) {
                    let url = &after_quote[..end];
                    if !url.is_empty() && !urls.contains(&url.to_string()) {
                        urls.push(url.to_string());
                    }
                }
            }
        }
    }

    urls
}

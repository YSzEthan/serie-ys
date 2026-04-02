use std::path::Path;
use std::process::{Command, Stdio};

use serde::Deserialize;

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
        false,
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
        false,
    )?;
    serde_json::from_str(&json).map_err(|e| format!("JSON parse error: {e}"))
}

pub fn view_item_rendered(path: &Path, number: u64, kind: GhItemKind) -> Result<String, String> {
    let cmd = kind.as_str();
    let num = number.to_string();
    let mut rendered = run_gh(path, &[cmd, "view", &num, "--comments"], true)?;

    // 從 JSON 取得原始 body + comments，提取圖片 URL
    if let Ok(json) = run_gh(path, &[cmd, "view", &num, "--json", "body,comments"], false) {
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
    targets.sort_by(|a, b| b.byte_offset.cmp(&a.byte_offset));
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

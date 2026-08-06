use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::github::{GhTimelineItem, Mergeable};

use super::Section;

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) enum TimelineLoad {
    #[default]
    NotRequested,
    Loading,
    Loaded,
    Error(String),
}

#[derive(Debug, Default)]
pub(super) struct TimelineEntry {
    pub(super) state: TimelineLoad,
    pub(super) items: Vec<GhTimelineItem>,
    pub(super) next_cursor: Option<String>,
    pub(super) loading_more: bool,
    /// 對 Issue 是 `None`，對 GitHub 還沒算完的 PR（`UNKNOWN`）也是
    /// `None`——兩者都代表「沒有標記」。每一頁都帶著自己的副本，所以後面
    /// 的頁面只是冪等地覆蓋掉這個值。
    pub(super) mergeable: Option<Mergeable>,
}

/// 把 `TimelineEntry` 可能處於的每種狀態——pending、failed、loaded（空或
/// 非空）、分頁中——攤平成一份可渲染列的清單。走訪結果的渲染迴圈本身
/// 沒有任何分支：「我現在是什麼狀態」這個問題只在這裡回答一次。
///
/// 回傳借用的項目而非 owned 複本，所以 `None`/`NotRequested` 這種
/// entry（沒東西可借）必須在 `Loaded` 這個 match arm 之前處理，不能靠
/// local 預設值折疊進去——那樣會產生 dangling reference。
pub(super) fn build_timeline(
    entry: Option<&TimelineEntry>,
    expand_commits: bool,
) -> Vec<TimelineItem<'_>> {
    let Some(entry) = entry else {
        return vec![TimelineItem::notice("(loading comments…)", Color::DarkGray)];
    };

    match &entry.state {
        TimelineLoad::NotRequested | TimelineLoad::Loading => {
            vec![TimelineItem::notice("(loading comments…)", Color::DarkGray)]
        }
        TimelineLoad::Error(e) => vec![TimelineItem::notice(
            format!("(comments failed: {e})"),
            Color::Red,
        )],
        TimelineLoad::Loaded => {
            let mut items: Vec<TimelineItem<'_>> = entry
                .items
                .iter()
                .filter_map(TimelineItem::from_gh)
                .collect();
            if !expand_commits {
                items = collapse_commits(items);
            }
            // 判斷用的是*過濾/收合後*的清單，不是 `entry.items`：一頁全是
            // `Unknown` 節點時，仍然要 fallback 到提示訊息，而不是渲染出
            // 零列（也就沒有任何分隔線）——而且收合絕不會把非空清單變空。
            if items.is_empty() {
                items.push(TimelineItem::notice("(no comments)", Color::DarkGray));
            } else if entry.next_cursor.is_some() {
                let text = if entry.loading_more {
                    "(loading more…)"
                } else {
                    "(more comments — scroll down to load)"
                };
                items.push(TimelineItem::notice(text, Color::DarkGray));
            }
            items
        }
    }
}

/// 把每一筆獨立的 `Commit` 列取代成一行摘要，放在 timeline 最前面、留言
/// 之前——對應網頁版 UI 的「N commits」收合檢視，而不是在每個 commit
/// 原本所在的位置留下空隙。
fn collapse_commits(mut items: Vec<TimelineItem<'_>>) -> Vec<TimelineItem<'_>> {
    let mut commit_count = 0;
    items.retain(|item| {
        let is_commit = matches!(item, TimelineItem::Commit { .. });
        commit_count += usize::from(is_commit);
        !is_commit
    });
    if commit_count > 0 {
        items.insert(0, TimelineItem::CollapsedCommits(commit_count));
    }
    items
}

/// timeline 的一列可渲染內容：留言、commit、收合後的 commit 數量摘要，
/// 或是代替以上任何一種的狀態提示（載入中／錯誤／空／分頁 footer）。
/// 借用自建置它的那個 `TimelineEntry`——每次 cache miss 都會從頭重建，
/// 所以渲染之後不需要保留任何東西。
pub(super) enum TimelineItem<'a> {
    Comment {
        author: &'a str,
        created_at: &'a str,
        body: &'a str,
    },
    Commit {
        oid: &'a str,
        headline: &'a str,
        ci_state: Option<&'a str>,
    },
    CollapsedCommits(usize),
    Notice(Line<'static>),
}

impl<'a> TimelineItem<'a> {
    fn notice(text: impl Into<String>, color: Color) -> Self {
        TimelineItem::Notice(Line::styled(text.into(), Style::default().fg(color)))
    }

    /// `Unknown` 節點——`itemTypes` 本不該產生的 `__typename`——會被直接
    /// 丟棄，而不是渲染成錯誤。一個無法辨識的節點不該在原本正常的
    /// timeline 中間跳出一則嚇人的訊息。
    fn from_gh(item: &'a GhTimelineItem) -> Option<Self> {
        match item {
            GhTimelineItem::IssueComment {
                body,
                created_at,
                author,
            } => Some(TimelineItem::Comment {
                author: author.as_ref().map_or("ghost", |a| a.login.as_str()),
                created_at,
                body,
            }),
            GhTimelineItem::PullRequestCommit { commit } => Some(TimelineItem::Commit {
                oid: &commit.abbreviated_oid,
                headline: &commit.message_headline,
                ci_state: commit
                    .status_check_rollup
                    .as_ref()
                    .map(|r| r.state.as_str()),
            }),
            GhTimelineItem::Unknown => None,
        }
    }

    pub(super) fn section(&self) -> Section {
        match self {
            TimelineItem::Comment { .. } | TimelineItem::Notice(_) => Section::Comment,
            TimelineItem::Commit { .. } | TimelineItem::CollapsedCommits(_) => Section::Commit,
        }
    }

    pub(super) fn render(self, lines: &mut Vec<Line<'static>>, width: usize) {
        match self {
            TimelineItem::Comment {
                author,
                created_at,
                body,
            } => {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("@{author}"),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  {created_at}"),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
                lines.extend(crate::view::markdown::render(body, width));
            }
            TimelineItem::Commit {
                oid,
                headline,
                ci_state,
            } => {
                lines.push(commit_line(oid, headline, ci_state, width));
            }
            TimelineItem::CollapsedCommits(n) => {
                lines.push(Line::styled(
                    format!("▸ {n} commits"),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            TimelineItem::Notice(line) => lines.push(line),
        }
    }
}

/// commit CI 狀態的標記字元 + 顏色。完全沒有 rollup 時 fallback 成兩個
/// 空格，讓 oid 欄位跟有 rollup 的 commit 保持對齊。
fn commit_ci_marker(state: Option<&str>) -> (&'static str, Color) {
    match state {
        Some("SUCCESS") => ("✓ ", Color::Green),
        Some("FAILURE" | "ERROR") => ("✗ ", Color::Red),
        Some("PENDING" | "EXPECTED") => ("● ", Color::Yellow),
        _ => ("  ", Color::DarkGray),
    }
}

/// `base ← head` 那一列的合併狀態標記文字 + 顏色。`None`（不是 PR，或
/// GitHub 回傳 `UNKNOWN`）代表完全沒有標記。
pub(super) fn mergeable_marker(state: Option<Mergeable>) -> Option<(&'static str, Color)> {
    match state {
        Some(Mergeable::Mergeable) => Some(("  (mergeable)", Color::Green)),
        Some(Mergeable::Conflicting) => Some(("  (conflicts)", Color::Red)),
        None => None,
    }
}

fn commit_line(oid: &str, headline: &str, ci_state: Option<&str>, width: usize) -> Line<'static> {
    let (marker, marker_color) = commit_ci_marker(ci_state);
    let prefix_width = console::measure_text_width(marker) + console::measure_text_width(oid) + 2;
    let headline =
        console::truncate_str(headline, width.saturating_sub(prefix_width), "…").to_string();
    Line::from(vec![
        Span::styled(marker, Style::default().fg(marker_color)),
        Span::styled(oid.to_string(), Style::default().fg(Color::DarkGray)),
        Span::raw("  "),
        Span::raw(headline),
    ])
}

/// preview 會走 `append_comment_lines` 的哪個分支。從 `TimelineLoad`
/// 推導而來、而非直接重用它，這樣 key 才能維持 `Copy`/`Eq`，不用拖著
/// 錯誤字串。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TimelineStage {
    Pending,
    Ready,
    Failed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mergeable_marker_covers_all_states() {
        assert_eq!(
            mergeable_marker(Some(Mergeable::Mergeable)),
            Some(("  (mergeable)", Color::Green))
        );
        assert_eq!(
            mergeable_marker(Some(Mergeable::Conflicting)),
            Some(("  (conflicts)", Color::Red))
        );
        // GitHub 的 `UNKNOWN` 跟「這是 Issue 不是 PR」都會變成 `None`——
        // 兩者都不該顯示標記。
        assert_eq!(mergeable_marker(None), None);
    }

    #[test]
    fn commit_ci_marker_covers_all_states() {
        assert_eq!(commit_ci_marker(Some("SUCCESS")).0, "✓ ");
        assert_eq!(commit_ci_marker(Some("FAILURE")).0, "✗ ");
        assert_eq!(commit_ci_marker(Some("ERROR")).0, "✗ ");
        assert_eq!(commit_ci_marker(Some("PENDING")).0, "● ");
        assert_eq!(commit_ci_marker(Some("EXPECTED")).0, "● ");
        // 完全沒有 rollup（API 回傳 null）：用兩個空格，而不是讓標記欄位
        // 直接消失，這樣 oid 在各個 commit 之間才能保持對齊。
        assert_eq!(commit_ci_marker(None).0, "  ");
    }

    #[test]
    fn commit_line_truncates_long_headline_to_width() {
        let width = 20;
        let line = commit_line("abc1234", &"x".repeat(100), Some("SUCCESS"), width);
        let rendered: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(
            console::measure_text_width(&rendered) <= width,
            "line must not exceed width {width}, got {} cells: {rendered:?}",
            console::measure_text_width(&rendered)
        );
    }
}

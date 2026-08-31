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
    /// 背景重抓（按 r 刷新）進行中。刻意獨立於 `state`——`build_timeline`
    /// 對 `NotRequested`/`Loading` 完全無視 `items`，若拿 `state` 表示
    /// 「重抓中」，畫面會塌成 loading 提示，違背刷新無感的目的。
    pub(super) refreshing: bool,
    /// 對 Issue 是 `None`，對 GitHub 還沒算完的 PR（`UNKNOWN`）也是
    /// `None`——兩者都代表「沒有標記」。每一頁都帶著自己的副本，所以後面
    /// 的頁面只是冪等地覆蓋掉這個值。
    pub(super) mergeable: Option<Mergeable>,
    /// 每次有新一頁資料落地（不管是首頁替換還是續接）就 +1。`PreviewKey`
    /// 靠這個欄位偵測「commit 數／mergeable 都沒變，但內容變了」——CI 狀態
    /// 從 PENDING 換成 SUCCESS 正是這種情況，其他既有欄位偵測不到。
    pub(super) rev: u64,
}

/// timeline 攤平後的一個可渲染區塊：同一個 section 底下連續出現的
/// item（一整組 commit、單一則留言、或單一則狀態提示）。分隔線只畫在
/// block 與 block 之間，所以同一個 commit block 裡的每一筆 commit 都
/// 緊貼著彼此，不會像逐 item 判斷時那樣被切成一條一條。
pub(super) struct TimelineBlock<'a> {
    pub(super) section: Section,
    pub(super) items: Vec<TimelineItem<'a>>,
}

impl<'a> TimelineBlock<'a> {
    /// 一個 gh timeline node 對應 0 或 1 個 block。`Unknown` 節點——`itemTypes`
    /// 本不該產生的 `__typename`——被直接丟棄，不渲染成錯誤：一個無法辨識的
    /// 節點不該在原本正常的 timeline 中間跳出一則嚇人的訊息。PENDING review
    /// （`submitted_at` 為 `None`，尚未送出的草稿，只有作者自己看得到）同樣
    /// 丟棄，理由相同——不該讓半成品永遠掛在別人也看得到的 timeline 上。
    ///
    /// 一則 review 連同它的行內留言算*一個* block：`TimelineBlock` 的用途
    /// 就是「同一個 section 底下連續出現的 item」，行內留言緊接在總結留言
    /// 之後正是這個形狀，不需要為此另立機制。
    fn from_gh(item: &'a GhTimelineItem) -> Option<Self> {
        match item {
            GhTimelineItem::IssueComment {
                body,
                created_at,
                author,
            } => Some(TimelineBlock {
                section: Section::Comment,
                items: vec![TimelineItem::Comment {
                    author: author.as_ref().map_or("ghost", |a| a.login.as_str()),
                    created_at,
                    body,
                }],
            }),
            GhTimelineItem::PullRequestCommit { commit } => Some(TimelineBlock {
                section: Section::Commit,
                items: vec![TimelineItem::Commit {
                    oid: &commit.abbreviated_oid,
                    headline: &commit.message_headline,
                    ci_state: commit
                        .status_check_rollup
                        .as_ref()
                        .map(|r| r.state.as_str()),
                }],
            }),
            GhTimelineItem::PullRequestReview {
                state,
                body,
                submitted_at,
                author,
                comments,
            } => {
                let submitted_at = submitted_at.as_deref()?;
                let mut items = vec![TimelineItem::Review {
                    state,
                    author: author.as_ref().map_or("ghost", |a| a.login.as_str()),
                    submitted_at,
                    body,
                }];
                items.extend(comments.nodes.iter().map(|c| TimelineItem::ReviewComment {
                    path: &c.path,
                    line: c.line,
                    outdated: c.outdated,
                    resolved: c.resolved,
                    body: &c.body,
                }));
                if comments.total_count > comments.nodes.len() {
                    items.push(TimelineItem::notice(
                        format!(
                            "(+{} more comments)",
                            comments.total_count - comments.nodes.len()
                        ),
                        Color::DarkGray,
                    ));
                }
                Some(TimelineBlock {
                    section: Section::Review,
                    items,
                })
            }
            GhTimelineItem::Unknown => None,
        }
    }
}

/// 把 `TimelineEntry` 可能處於的每種狀態——pending、failed、loaded（空或
/// 非空）、分頁中——攤平成一份可渲染 block 的清單。走訪結果的渲染迴圈本身
/// 沒有任何分支：「我現在是什麼狀態」這個問題只在這裡回答一次。
///
/// 回傳借用的項目而非 owned 複本，所以 `None`/`NotRequested` 這種
/// entry（沒東西可借）必須在 `Loaded` 這個 match arm 之前處理，不能靠
/// local 預設值折疊進去——那樣會產生 dangling reference。
pub(super) fn build_timeline(
    entry: Option<&TimelineEntry>,
    expand_commits: bool,
) -> Vec<TimelineBlock<'_>> {
    let Some(entry) = entry else {
        return vec![notice_block("(loading comments…)", Color::DarkGray)];
    };

    match &entry.state {
        TimelineLoad::NotRequested | TimelineLoad::Loading => {
            vec![notice_block("(loading comments…)", Color::DarkGray)]
        }
        TimelineLoad::Error(e) => {
            vec![notice_block(format!("(comments failed: {e})"), Color::Red)]
        }
        TimelineLoad::Loaded => {
            let (commit_blocks, rest): (Vec<_>, Vec<_>) = entry
                .items
                .iter()
                .filter_map(TimelineBlock::from_gh)
                .partition(|b| b.section == Section::Commit);

            let mut blocks = Vec::new();
            if !commit_blocks.is_empty() {
                let commit_items: Vec<_> =
                    commit_blocks.into_iter().flat_map(|b| b.items).collect();
                let items = if expand_commits {
                    commit_items
                } else {
                    vec![TimelineItem::CollapsedCommits(commit_items.len())]
                };
                blocks.push(TimelineBlock {
                    section: Section::Commit,
                    items,
                });
            }
            blocks.extend(rest);

            // 判斷用的是*過濾/分組後*的 block 清單，不是 `entry.items`：一頁
            // 全是 `Unknown` 節點時，仍然要 fallback 到提示訊息，而不是渲染
            // 出零列（也就沒有任何分隔線）。判空主體刻意維持整條 timeline，
            // 不是只看 comments——`timelineItems` 是一條混合 connection，
            // 前 100 筆全是 commit、留言落在下一頁的長命 PR 很常見，只看
            // comments 會在那種情況印出騙人的「沒有留言」。
            if blocks.is_empty() {
                blocks.push(notice_block("(no comments)", Color::DarkGray));
            } else if entry.next_cursor.is_some() {
                let text = if entry.loading_more {
                    "(loading more…)"
                } else {
                    "(more comments — scroll down to load)"
                };
                // `next_cursor` 代表「還有 timelineItems」，不是「還有留言」
                // ——文案沿用舊字樣，即使下一頁其實是更多 commit 也一樣，
                // 避免為了措辭精確而長出第二種 footer。
                blocks.push(notice_block(text, Color::DarkGray));
            }
            blocks
        }
    }
}

fn notice_block(text: impl Into<String>, color: Color) -> TimelineBlock<'static> {
    TimelineBlock {
        section: Section::Comment,
        items: vec![TimelineItem::notice(text, color)],
    }
}

/// commit block 在 timeline 裡佔的視覺行數：沒有 commit 就是 0（不畫這個
/// block，也不畫它前面那條分隔線）；否則是 1 條分隔線，加上展開時每個
/// commit 各一行、收合時固定一行的收合摘要。`append_timeline_items` 用
/// 它算分頁載入前後的差值來補償 `preview_offset`——commit block 插在
/// timeline 最前面，插入的視覺行數必須讓捲動位置跟著往下位移，畫面才不會
/// 因為視窗上方多出內容而往回跳。
pub(super) fn commit_block_height(items: &[GhTimelineItem], expand_commits: bool) -> usize {
    let count = items
        .iter()
        .filter(|item| matches!(item, GhTimelineItem::PullRequestCommit { .. }))
        .count();
    match (count, expand_commits) {
        (0, _) => 0,
        (n, true) => 1 + n,
        (_, false) => 2,
    }
}

/// timeline 的一列可渲染內容：留言、commit、review 總結、review 行內留言、
/// 收合後的 commit 數量摘要，或是代替以上任何一種的狀態提示
/// （載入中／錯誤／空／分頁 footer）。
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
    Review {
        state: &'a str,
        author: &'a str,
        submitted_at: &'a str,
        body: &'a str,
    },
    /// 前面必定緊接著同一個 block 裡的 `Review`（或另一個 `ReviewComment`）
    /// ——渲染時自帶一條前置空行當視覺分隔，見 `render`。
    ReviewComment {
        path: &'a str,
        line: Option<u32>,
        outdated: bool,
        resolved: bool,
        body: &'a str,
    },
    Notice(Line<'static>),
}

impl<'a> TimelineItem<'a> {
    fn notice(text: impl Into<String>, color: Color) -> Self {
        TimelineItem::Notice(Line::styled(text.into(), Style::default().fg(color)))
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
            TimelineItem::Review {
                state,
                author,
                submitted_at,
                body,
            } => {
                let (marker, marker_color) = review_state_marker(state);
                lines.push(Line::from(vec![
                    Span::styled(marker, Style::default().fg(marker_color)),
                    Span::styled(
                        format!("@{author}"),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  {}", state.to_lowercase()),
                        Style::default().fg(marker_color),
                    ),
                    Span::styled(
                        format!("  {submitted_at}"),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
                lines.extend(crate::view::markdown::render(body, width));
            }
            TimelineItem::ReviewComment {
                path,
                line,
                outdated,
                resolved,
                body,
            } => {
                lines.push(Line::raw(""));
                let mut header = match line {
                    Some(l) => format!("{path}:{l}"),
                    None => path.to_string(),
                };
                if outdated {
                    header.push_str("  (outdated)");
                }
                if resolved {
                    header.push_str("  (resolved)");
                }
                lines.push(Line::styled(header, Style::default().fg(Color::DarkGray)));
                lines.extend(crate::view::markdown::render(body, width));
            }
            TimelineItem::Notice(line) => lines.push(line),
        }
    }
}

/// review 總結留言 state 的標記字元 + 顏色。完全沒有對應狀態時 fallback
/// 成兩個空格，讓 `@author` 欄位跟其他 review 保持對齊——理由同
/// `commit_ci_marker`。
fn review_state_marker(state: &str) -> (&'static str, Color) {
    match state {
        "APPROVED" => ("✓ ", Color::Green),
        "CHANGES_REQUESTED" => ("✗ ", Color::Red),
        "COMMENTED" => ("● ", Color::Blue),
        "DISMISSED" => ("- ", Color::DarkGray),
        _ => ("  ", Color::DarkGray),
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
    fn review_state_marker_covers_all_states() {
        assert_eq!(review_state_marker("APPROVED").0, "✓ ");
        assert_eq!(review_state_marker("CHANGES_REQUESTED").0, "✗ ");
        assert_eq!(review_state_marker("COMMENTED").0, "● ");
        assert_eq!(review_state_marker("DISMISSED").0, "- ");
        // 未知 state（理論上不會發生，但 fallback 用兩個空格保持 @author
        // 欄位跟其他 review 對齊，而不是讓標記欄位消失。
        assert_eq!(review_state_marker("PENDING").0, "  ");
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

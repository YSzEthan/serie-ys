use std::borrow::Cow;

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use rust_i18n::t;

use crate::github::{CheckState, DiffStat, GhCheck, GhTimelineItem, Mergeable};

use super::{render::SWATCH, Section};

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
    /// PR head commit 的 check 清單，`head_ci_checks` 已去重排序。只有首頁
    /// （首次載入與刷新）會覆寫；載入更多的頁面不查 CI，不會動它。
    pub(super) ci_checks: Vec<GhCheck>,
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
                        t!(
                            "github.timeline.more_review_comments",
                            n = comments.total_count - comments.nodes.len()
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
pub(super) fn build_timeline(entry: Option<&TimelineEntry>) -> Vec<TimelineBlock<'_>> {
    let Some(entry) = entry else {
        return vec![notice_block(t!("github.timeline.loading"), Color::DarkGray)];
    };

    match &entry.state {
        TimelineLoad::NotRequested | TimelineLoad::Loading => {
            vec![notice_block(t!("github.timeline.loading"), Color::DarkGray)]
        }
        TimelineLoad::Error(e) => {
            vec![notice_block(
                t!("github.timeline.failed", detail = e),
                Color::Red,
            )]
        }
        TimelineLoad::Loaded => {
            let mut blocks: Vec<_> = entry
                .items
                .iter()
                .filter(|item| !is_commit(item))
                .filter_map(TimelineBlock::from_gh)
                .collect();

            // `next_cursor` 代表「還有 timelineItems」，不是「還有留言」：
            // 長命 PR 常見前 100 筆全是 commit（畫在 header 區，不在這裡）、
            // 留言落在下一頁，這時 `blocks` 是空的，卻不能印出騙人的「沒有
            // 留言」，所以先判斷還有沒有下一頁，才判斷空。文案沿用舊字樣，
            // 即使下一頁其實是更多 commit 也一樣，避免為了措辭精確而長出
            // 第二種 footer。
            if entry.next_cursor.is_some() {
                let text = if entry.loading_more {
                    t!("github.timeline.loading_more")
                } else {
                    t!("github.timeline.more")
                };
                blocks.push(notice_block(text, Color::DarkGray));
            } else if blocks.is_empty() {
                blocks.push(notice_block(t!("github.timeline.empty"), Color::DarkGray));
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

fn is_commit(item: &GhTimelineItem) -> bool {
    matches!(item, GhTimelineItem::PullRequestCommit { .. })
}

/// 所有 commit 併成一個 block；收合時只剩一行計數。沒有 commit 就沒有 block。
fn commit_block(entry: &TimelineEntry, expand_commits: bool) -> Option<TimelineBlock<'_>> {
    let commit_items: Vec<_> = entry
        .items
        .iter()
        .filter(|item| is_commit(item))
        .filter_map(TimelineBlock::from_gh)
        .flat_map(|b| b.items)
        .collect();
    if commit_items.is_empty() {
        return None;
    }
    let items = if expand_commits {
        commit_items
    } else {
        vec![TimelineItem::CollapsedCommits(commit_items.len())]
    };
    Some(TimelineBlock {
        section: Section::Commit,
        items,
    })
}

/// head commit 的 CI check；收合時只剩一行計數加色塊。沒有 check 就沒有 block。
fn ci_block(entry: &TimelineEntry, expand_commits: bool) -> Option<TimelineBlock<'_>> {
    if entry.ci_checks.is_empty() {
        return None;
    }
    let items = if expand_commits {
        entry.ci_checks.iter().map(TimelineItem::Check).collect()
    } else {
        vec![TimelineItem::CollapsedChecks(&entry.ci_checks)]
    };
    Some(TimelineBlock {
        section: Section::Ci,
        items,
    })
}

/// 畫在 Body 之前、緊接 header 的兩個 block，依畫面順序（CI、commit）。
/// 畫面順序只在這裡寫一次：渲染、高度、`remap` 都由這個陣列推導。
/// 非 `Loaded` 時畫面上是提示訊息，沒有這兩個 block。
pub(super) fn leading(
    entry: &TimelineEntry,
    expand_commits: bool,
) -> [Option<TimelineBlock<'_>>; LEADING_BLOCKS] {
    if entry.state != TimelineLoad::Loaded {
        return [None, None];
    }
    [
        ci_block(entry, expand_commits),
        commit_block(entry, expand_commits),
    ]
}

pub(super) const LEADING_BLOCKS: usize = 2;

/// `leading` 各 block 佔的視覺行數。每個 block 是 1 條分隔線加上每個 item
/// 各一行（收合時 item 只有一個摘要），前提是這兩種 block 的每個 item 都只
/// 渲染一行（有測試固定）。
pub(super) type LeadingHeights = [usize; LEADING_BLOCKS];

/// `append_timeline_items` 用落地前後的兩份高度來位移 `preview_offset`：
/// 這些 block 插在 Body 之前，行數變化會把 Body 以下整段推動，必須讓捲動位置
/// 跟著位移，畫面才不會因為視窗上方的內容變動而跳動。
pub(super) fn leading_heights(entry: &TimelineEntry, expand_commits: bool) -> LeadingHeights {
    leading(entry, expand_commits).map(|b| b.map_or(0, |b| 1 + b.items.len()))
}

/// 視窗頂端在 leading 起點之下第 `rel` 行；內容從 `old` 變成 `new` 之後，
/// 同一段內容的新位置。每一段用同一條規則：新區段起點加上
/// `min(區段內位移, 新區段長度)`——區段縮小到頂端之上時落在區段末端，
/// 不會越界進下一段。最後一段（Body 以下）整段平移。
///
/// 只保住位置、保不住內容：CI 依狀態重新排序時，頂端那一行可能換成
/// 另一個 check。超出底端的部分交給 `render_preview` 的 clamp。
pub(super) fn remap(old: LeadingHeights, new: LeadingHeights, mut rel: usize) -> usize {
    let mut base = 0;
    for (o, n) in old.into_iter().zip(new) {
        if rel < o {
            return base + rel.min(n);
        }
        rel -= o;
        base += n;
    }
    base + rel
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
    Check(&'a GhCheck),
    /// 收合後的 CI 摘要：計數加每個 check 一個色塊，永遠只佔一行。
    CollapsedChecks(&'a [GhCheck]),
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
                    t!("github.timeline.collapsed_commits", n = n),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            TimelineItem::Check(check) => {
                let (marker, color) = check_state_marker(check.state);
                let name_width = width.saturating_sub(console::measure_text_width(marker));
                let name = console::truncate_str(&check.name, name_width, "…").to_string();
                lines.push(Line::from(vec![
                    Span::styled(marker, Style::default().fg(color)),
                    Span::raw(name),
                ]));
            }
            TimelineItem::CollapsedChecks(checks) => {
                let label = t!("github.timeline.collapsed_checks", n = checks.len());
                // 色塊只取塞得進剩餘寬度的數量：保證單行，捲動補償才算得準。
                // `checks` 依 fail → pending → pass 排序，截斷時優先丟掉綠色。
                let room = width.saturating_sub(console::measure_text_width(&label));
                let mut spans = vec![Span::styled(label, Style::default().fg(Color::DarkGray))];
                spans.extend(checks.iter().take(room).map(|c| {
                    Span::styled(SWATCH, Style::default().fg(check_state_marker(c.state).1))
                }));
                lines.push(Line::from(spans));
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
                        format!("  {}", review_state_label(state)),
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
                    header.push_str(&t!("github.timeline.outdated"));
                }
                if resolved {
                    header.push_str(&t!("github.timeline.resolved"));
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

/// CI check 狀態的標記字元 + 顏色，展開列與收合色塊共用。
fn check_state_marker(state: CheckState) -> (&'static str, Color) {
    match state {
        CheckState::Failed => ("✗ ", Color::Red),
        CheckState::Pending => ("● ", Color::DarkGray),
        CheckState::Passed => ("✓ ", Color::Green),
    }
}

/// `base ← head` 那一列的合併狀態標記文字 + 顏色。`None`（不是 PR，或
/// GitHub 回傳 `UNKNOWN`）代表完全沒有標記。
pub(super) fn mergeable_marker(state: Option<Mergeable>) -> Option<(Cow<'static, str>, Color)> {
    match state {
        Some(Mergeable::Mergeable) => Some((t!("github.timeline.mergeable"), Color::Green)),
        Some(Mergeable::Conflicting) => Some((t!("github.timeline.conflicting"), Color::Red)),
        None => None,
    }
}

/// review state（GitHub API 值）的顯示文字。未知的值原樣轉小寫顯示。
fn review_state_label(state: &str) -> Cow<'static, str> {
    match state {
        "APPROVED" => t!("github.review.approved"),
        "CHANGES_REQUESTED" => t!("github.review.changes_requested"),
        "COMMENTED" => t!("github.review.commented"),
        "DISMISSED" => t!("github.review.dismissed"),
        other => Cow::Owned(other.to_lowercase()),
    }
}

/// 總和達到這個門檻，代表這個 PR 通常該拆分——用紅色加粗標示出來。
const DIFF_STAT_DANGER: u32 = 10_000;

/// `base ← head` 那一列的變更行數：`+新增 -刪除 =總和`。總和達到
/// [`DIFF_STAT_DANGER`] 時改紅色加粗。
pub(super) fn diff_stat_spans(stat: DiffStat) -> [Span<'static>; 3] {
    let total = stat.additions.saturating_add(stat.deletions);
    let total_style = if total >= DIFF_STAT_DANGER {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
    [
        Span::styled(
            format!("  +{}", stat.additions),
            Style::default().fg(Color::Green),
        ),
        Span::styled(
            format!(" -{}", stat.deletions),
            Style::default().fg(Color::Red),
        ),
        Span::styled(format!(" ={total}"), total_style),
    ]
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
            Some((t!("github.timeline.mergeable"), Color::Green))
        );
        assert_eq!(
            mergeable_marker(Some(Mergeable::Conflicting)),
            Some((t!("github.timeline.conflicting"), Color::Red))
        );
        // GitHub 的 `UNKNOWN` 跟「這是 Issue 不是 PR」都會變成 `None`——
        // 兩者都不該顯示標記。
        assert_eq!(mergeable_marker(None), None);
    }

    #[test]
    fn diff_stat_spans_colors_additions_and_deletions() {
        let spans = diff_stat_spans(DiffStat {
            additions: 600,
            deletions: 71,
        });
        assert_eq!(spans[0].content, "  +600");
        assert_eq!(spans[0].style.fg, Some(Color::Green));
        assert_eq!(spans[1].content, " -71");
        assert_eq!(spans[1].style.fg, Some(Color::Red));
        assert_eq!(spans[2].content, " =671");
    }

    #[test]
    fn diff_stat_spans_total_below_danger_threshold_is_white() {
        let spans = diff_stat_spans(DiffStat {
            additions: 9999,
            deletions: 0,
        });
        assert_eq!(spans[2].content, " =9999");
        assert_eq!(spans[2].style.fg, Some(Color::White));
        assert!(!spans[2].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn diff_stat_spans_total_at_danger_threshold_is_red_and_bold() {
        let spans = diff_stat_spans(DiffStat {
            additions: 9000,
            deletions: 1000,
        });
        assert_eq!(spans[2].content, " =10000");
        assert_eq!(spans[2].style.fg, Some(Color::Red));
        assert!(spans[2].style.add_modifier.contains(Modifier::BOLD));
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

    fn gh_check(name: &str, state: CheckState) -> GhCheck {
        GhCheck {
            name: name.to_string(),
            state,
        }
    }

    fn loaded_entry(items: Vec<GhTimelineItem>, ci_checks: Vec<GhCheck>) -> TimelineEntry {
        TimelineEntry {
            state: TimelineLoad::Loaded,
            items,
            ci_checks,
            ..Default::default()
        }
    }

    fn commit_item() -> GhTimelineItem {
        GhTimelineItem::PullRequestCommit {
            commit: crate::github::GhCommit {
                abbreviated_oid: "abc1234".to_string(),
                message_headline: "headline".to_string(),
                status_check_rollup: None,
            },
        }
    }

    fn three_checks() -> Vec<GhCheck> {
        vec![
            gh_check("lint", CheckState::Failed),
            gh_check("build", CheckState::Pending),
            gh_check("test", CheckState::Passed),
        ]
    }

    fn render_block(block: TimelineBlock<'_>, width: usize) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for item in block.items {
            item.render(&mut lines, width);
        }
        lines
    }

    fn text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// 畫面順序只寫在 `leading` 一處：CI 在前、commit 在後。
    #[test]
    fn leading_blocks_are_ci_then_commit() {
        let entry = loaded_entry(vec![commit_item()], three_checks());
        let sections: Vec<_> = leading(&entry, true)
            .into_iter()
            .flatten()
            .map(|b| b.section)
            .collect();
        assert_eq!(sections, [Section::Ci, Section::Commit]);
    }

    /// commit 與 CI 已搬到 header 區，不再出現在 timeline 的 block 清單裡。
    #[test]
    fn build_timeline_no_longer_emits_commit_or_ci_blocks() {
        let entry = loaded_entry(vec![commit_item()], three_checks());
        assert!(build_timeline(Some(&entry))
            .iter()
            .all(|b| !matches!(b.section, Section::Commit | Section::Ci)));
    }

    #[test]
    fn expanded_ci_block_is_one_line_per_check() {
        let entry = loaded_entry(vec![commit_item()], three_checks());
        let [ci, _] = leading(&entry, true);

        let lines = render_block(ci.expect("has checks"), 40);
        let rendered: Vec<_> = lines.iter().map(text).collect();
        assert_eq!(rendered, ["✗ lint", "● build", "✓ test"]);
        let colors: Vec<_> = lines.iter().map(|l| l.spans[0].style.fg).collect();
        assert_eq!(
            colors,
            [Some(Color::Red), Some(Color::DarkGray), Some(Color::Green)]
        );
    }

    #[test]
    fn collapsed_ci_block_is_one_line_with_colored_swatches() {
        let entry = loaded_entry(vec![commit_item()], three_checks());
        let [ci, _] = leading(&entry, false);
        let lines = render_block(ci.expect("has checks"), 40);
        assert_eq!(lines.len(), 1);
        assert_eq!(text(&lines[0]), "▸ 3 個 check ▮▮▮");
        let swatch_colors: Vec<_> = lines[0].spans[1..].iter().map(|s| s.style.fg).collect();
        assert_eq!(
            swatch_colors,
            [Some(Color::Red), Some(Color::DarkGray), Some(Color::Green)]
        );
    }

    /// 窄寬度下色塊被截斷而不是換行；排在前面的 fail 留下，被丟掉的是排在後面的。
    #[test]
    fn collapsed_ci_swatches_truncate_to_width_keeping_failures() {
        let entry = loaded_entry(Vec::new(), three_checks());
        let [ci, _] = leading(&entry, false);
        let width = console::measure_text_width("▸ 3 個 check ") + 1;
        let lines = render_block(ci.expect("has checks"), width);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].width() <= width);
        assert_eq!(lines[0].spans.len(), 2);
        assert_eq!(lines[0].spans[1].style.fg, Some(Color::Red));
    }

    #[test]
    fn long_check_name_is_truncated_to_width() {
        let entry = loaded_entry(
            Vec::new(),
            vec![gh_check(&"x".repeat(100), CheckState::Passed)],
        );
        let [ci, _] = leading(&entry, true);
        let lines = render_block(ci.expect("has checks"), 20);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].width() <= 20);
    }

    #[test]
    fn no_ci_block_without_checks() {
        let entry = loaded_entry(vec![commit_item()], Vec::new());
        let [ci, commit] = leading(&entry, true);
        assert!(ci.is_none());
        assert!(commit.is_some());
    }

    #[test]
    fn leading_is_empty_until_the_timeline_is_loaded() {
        let entry = TimelineEntry {
            ci_checks: three_checks(),
            items: vec![commit_item()],
            ..Default::default()
        };
        let [ci, commit] = leading(&entry, true);
        assert!(ci.is_none() && commit.is_none());
    }

    #[test]
    fn leading_heights_counts_ci_and_commit_blocks_separately() {
        let entry = loaded_entry(vec![commit_item()], three_checks());
        // 展開：CI（分隔線 + 3）、commit（分隔線 + 1）
        assert_eq!(leading_heights(&entry, true), [4, 2]);
        // 收合：兩個 block 各是分隔線 + 一行摘要
        assert_eq!(leading_heights(&entry, false), [2, 2]);

        let no_ci = loaded_entry(vec![commit_item()], Vec::new());
        assert_eq!(leading_heights(&no_ci, true), [0, 2]);
        assert_eq!(leading_heights(&no_ci, false), [0, 2]);

        let no_commit = loaded_entry(Vec::new(), three_checks());
        assert_eq!(leading_heights(&no_commit, true), [4, 0]);

        // 還沒載入完：畫面上只有「loading」提示，不算 CI／commit
        assert_eq!(leading_heights(&TimelineEntry::default(), true), [0, 0]);
    }

    /// 高度與實際渲染出的行數（分隔線不在這裡畫，所以加 1）必須一致，
    /// 版面規則才真的只有 `leading` 一個來源。
    #[test]
    fn leading_heights_match_the_rendered_blocks() {
        let entry = loaded_entry(vec![commit_item(), commit_item()], three_checks());
        for expand in [true, false] {
            let rendered =
                leading(&entry, expand).map(|b| b.map_or(0, |b| 1 + render_block(b, 40).len()));
            assert_eq!(leading_heights(&entry, expand), rendered, "expand={expand}");
        }
    }

    /// 三段（CI、commit、其後）各取邊界：區段第一行、最後一行、剛越過去的一行。
    #[test]
    fn remap_keeps_the_top_content_in_place_across_all_three_segments() {
        // commit 2→4 行，CI 3 行不變
        let (old, new) = ([3, 2], [3, 4]);
        // CI 區塊內：上面的內容沒動
        assert_eq!(remap(old, new, 0), 0);
        assert_eq!(remap(old, new, 2), 2);
        // commit 區塊內：CI 沒變，起點沒動
        assert_eq!(remap(old, new, 3), 3);
        assert_eq!(remap(old, new, 4), 4);
        // 兩區塊之下（Body 以下）：跟著總和位移
        assert_eq!(remap(old, new, 5), 7);
        assert_eq!(remap(old, new, 9), 11);

        // CI 3→5 行：commit 區塊與其下整段被往下推
        let (old, new) = ([3, 2], [5, 2]);
        assert_eq!(remap(old, new, 2), 2);
        assert_eq!(remap(old, new, 3), 5);
        assert_eq!(remap(old, new, 5), 7);
    }

    #[test]
    fn remap_clamps_when_a_segment_shrinks_above_the_top() {
        // CI 3→0 行（force-push 後新 head 沒有 check）：頂端原本是 commit 的
        // 第一行，現在 commit 區塊從 0 開始
        assert_eq!(remap([3, 2], [0, 2], 3), 0);
        // 頂端在 CI 最後一行，CI 縮成 2 行：夾在 CI 區塊末端，也就是 commit 的起點
        assert_eq!(remap([3, 2], [2, 2], 2), 2);
        // commit 4→2 行（重抓截回第一頁），頂端在 commit 內第 3 行：夾在
        // commit 區塊末端，也就是 Body 分隔線
        assert_eq!(remap([3, 4], [3, 2], 6), 5);
        // CI 消失，頂端原本在 CI 內
        assert_eq!(remap([3, 2], [0, 2], 1), 0);
    }

    #[test]
    fn remap_handles_missing_leading_blocks() {
        // 一開始什麼都沒有（rel 一定落在最後一段）：整段位移新增的高度。
        // 首次載入就是這個情境，使用者可能已經往下讀 Body。
        assert_eq!(remap([0, 0], [4, 2], 0), 6);
        assert_eq!(remap([0, 0], [4, 2], 5), 11);
        // 全部消失：往回位移
        assert_eq!(remap([4, 2], [0, 0], 8), 2);
        // CI 是 0 行時，rel = 0 就是 commit 的第一行
        assert_eq!(remap([0, 3], [2, 3], 0), 2);
    }

    /// `leading_heights` 以「每個 item 一行」為前提，這裡固定住它。
    #[test]
    fn every_commit_and_ci_item_renders_exactly_one_line() {
        let entry = loaded_entry(vec![commit_item(), commit_item()], three_checks());
        for expand in [true, false] {
            for block in leading(&entry, expand).into_iter().flatten() {
                let n = block.items.len();
                assert_eq!(render_block(block, 40).len(), n, "expand={expand}");
            }
        }
    }

    fn only_notice(entry: &TimelineEntry) -> String {
        let mut blocks = build_timeline(Some(entry));
        assert_eq!(blocks.len(), 1, "expected a single notice block");
        text(&render_block(blocks.remove(0), 60)[0])
    }

    /// 前 100 筆全是 commit、留言在下一頁：不能印「沒有留言」，要顯示還有更多。
    #[test]
    fn only_commits_with_a_next_page_shows_the_more_footer_not_empty() {
        let mut entry = loaded_entry(vec![commit_item()], Vec::new());
        entry.next_cursor = Some("c".to_string());
        assert_eq!(only_notice(&entry), "（還有更多留言——往下捲動載入）");
    }

    /// 只有 commit、沒有下一頁：commit 已畫在 header 區，timeline 確實沒有留言。
    #[test]
    fn only_commits_without_a_next_page_shows_empty() {
        let entry = loaded_entry(vec![commit_item()], Vec::new());
        assert_eq!(only_notice(&entry), "（沒有留言）");
    }
}

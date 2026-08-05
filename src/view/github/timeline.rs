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
    /// `None` for Issues and for PRs GitHub hasn't finished computing this
    /// for yet (`UNKNOWN`) — both mean "no marker". Every page carries its
    /// own copy, so a later page just overwrites this idempotently.
    pub(super) mergeable: Option<Mergeable>,
}

/// Flattens every state a `TimelineEntry` can be in — pending, failed,
/// loaded (empty or not), paginating — into one list of renderable rows.
/// The render loop that walks the result has no branches of its own: every
/// "what state am I in" question is answered once, here.
///
/// Returns borrowed items rather than an owned copy, so a `None`/`NotRequested`
/// entry (nothing to borrow from) has to be handled before the `Loaded` match
/// arm rather than folded into it via a local default — that would dangle.
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
            // Checked on the *filtered/collapsed* list, not `entry.items`: a
            // page of nothing but `Unknown` nodes must still fall back to a
            // notice instead of rendering zero rows (and thus no divider at
            // all) — and collapsing never turns a non-empty list empty.
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

/// Replaces every individual `Commit` row with one summary line at the front
/// of the timeline, ahead of the comments — matching the web UI's "N commits"
/// collapsed view rather than leaving a gap where each commit used to sit.
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

/// One renderable row of the timeline: a comment, a commit, a collapsed
/// commit-count summary, or a status notice standing in for any of them
/// (loading/error/empty/pagination footer). Borrows from the `TimelineEntry`
/// it was built from — this is rebuilt from scratch on every cache miss, so
/// there's nothing to hold onto past render.
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

    /// `Unknown` nodes — an `__typename` `itemTypes` wasn't supposed to
    /// produce — are dropped rather than rendered as an error. One
    /// unrecognized node shouldn't put a scary message in the middle of an
    /// otherwise normal timeline.
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

/// Marker + colour for a commit's CI state. The two-space fallback when
/// there's no rollup at all keeps the oid column aligned with commits that
/// do have one.
fn commit_ci_marker(state: Option<&str>) -> (&'static str, Color) {
    match state {
        Some("SUCCESS") => ("✓ ", Color::Green),
        Some("FAILURE" | "ERROR") => ("✗ ", Color::Red),
        Some("PENDING" | "EXPECTED") => ("● ", Color::Yellow),
        _ => ("  ", Color::DarkGray),
    }
}

/// Label + colour for the `base ← head` line's merge-state marker. `None`
/// (not a PR, or GitHub's `UNKNOWN`) means no marker at all.
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

/// Which of `append_comment_lines`' branches the preview will take. Derived
/// from `TimelineLoad` rather than reusing it, so the key stays `Copy`/`Eq`
/// without dragging the error string along.
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
        // GitHub's `UNKNOWN` and "this is an Issue, not a PR" both arrive as
        // `None` — neither should show a marker.
        assert_eq!(mergeable_marker(None), None);
    }

    #[test]
    fn commit_ci_marker_covers_all_states() {
        assert_eq!(commit_ci_marker(Some("SUCCESS")).0, "✓ ");
        assert_eq!(commit_ci_marker(Some("FAILURE")).0, "✗ ");
        assert_eq!(commit_ci_marker(Some("ERROR")).0, "✗ ");
        assert_eq!(commit_ci_marker(Some("PENDING")).0, "● ");
        assert_eq!(commit_ci_marker(Some("EXPECTED")).0, "● ");
        // No rollup at all (null in the API): two spaces, not the marker
        // column collapsing, so the oid stays aligned across commits.
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

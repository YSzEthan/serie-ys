use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::github::Mergeable;

use super::{
    render::{label_spans, state_color},
    timeline::{build_timeline, mergeable_marker, TimelineEntry, TimelineLoad, TimelineStage},
    GitHubTab, Section,
};

pub(super) fn build_preview_content(
    input: &PreviewInput,
) -> (Vec<Line<'static>>, Option<PreviewOverlay>) {
    let mut overlay = None;
    let width = input.width as usize;
    let number = input.number;
    let Some(item) = input.item.as_ref() else {
        return (
            vec![Line::styled(
                "(no item selected)",
                Style::default().fg(Color::DarkGray),
            )],
            overlay,
        );
    };

    let mut lines = Vec::new();

    // Header: #number title  (#N hyperlink overlay)
    if !item.url.is_empty() {
        overlay = Some(PreviewOverlay {
            url: item.url.to_string(),
            label: format!("#{number}"),
        });
    }
    lines.push(Line::from(vec![
        Span::styled(format!("#{number} "), Style::default().fg(Color::DarkGray)),
        Span::styled(
            item.title.to_string(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    let mut meta_spans = vec![
        Span::styled(
            item.state.to_lowercase(),
            Style::default().fg(state_color(item.state)),
        ),
        Span::styled(
            format!("  @{}", item.author),
            Style::default().fg(Color::DarkGray),
        ),
    ];
    if !item.labels.is_empty() {
        meta_spans.push(Span::raw("  "));
        meta_spans.extend(label_spans(item.labels));
    }
    lines.push(Line::from(meta_spans));

    if let SelectedItemExtra::PullRequest {
        base_ref_name,
        head_ref_name,
    } = item.extra
    {
        let mut spans = vec![
            Span::styled(base_ref_name.to_string(), Style::default().fg(Color::Cyan)),
            Span::styled("  ←  ", Style::default().fg(Color::DarkGray)),
            Span::styled(head_ref_name.to_string(), Style::default().fg(Color::Cyan)),
        ];
        if let Some((text, color)) = mergeable_marker(input.entry.and_then(|e| e.mergeable)) {
            spans.push(Span::styled(text, Style::default().fg(color)));
        }
        lines.push(Line::from(spans));
    }

    lines.push(crate::view::markdown::rule_line(width));

    if let SelectedItemExtra::Issue { parent, sub_issues } = item.extra {
        append_relation_lines(&mut lines, parent, sub_issues, width);
    }

    if item.body.is_empty() {
        lines.push(Line::styled(
            "(no body)",
            Style::default().fg(Color::DarkGray),
        ));
    } else {
        lines.extend(crate::view::markdown::render(item.body, width));
    }

    append_comment_lines(&mut lines, input.entry, input.expand_commits, width);

    (lines, overlay)
}

pub(super) fn append_comment_lines(
    lines: &mut Vec<Line<'static>>,
    entry: Option<&TimelineEntry>,
    expand_commits: bool,
    width: usize,
) {
    let mut prev = Section::Body;
    for item in build_timeline(entry, expand_commits) {
        let section = item.section();
        lines.push(prev.divider(width));
        item.render(lines, width);
        prev = section;
    }
}

/// OSC 8 hyperlink drawn over the preview's header line (`#N`) — the only
/// preview row an overlay is ever attached to; see `render_preview`.
#[derive(Debug, Clone)]
pub(super) struct PreviewOverlay {
    pub(super) url: String,
    pub(super) label: String,
}

/// Everything `build_preview_content` reads, borrowed from the selected
/// issue/PR and its timeline entry, plus the width the result is wrapped
/// against. `cache_key` reads the same struct, so it cannot silently miss a
/// field that content-building depends on.
pub(super) struct PreviewInput<'v> {
    pub(super) tab: GitHubTab,
    pub(super) number: u64,
    pub(super) width: u16,
    pub(super) body_rev: u64,
    pub(super) entry: Option<&'v TimelineEntry>,
    pub(super) expand_commits: bool,
    pub(super) item: Option<SelectedItem<'v>>,
}

impl PreviewInput<'_> {
    pub(super) fn cache_key(&self) -> PreviewKey {
        // Count alone is not enough: "loaded but empty" and "still loading"
        // both have zero items yet render differently. Exhaustive on purpose —
        // a new `TimelineLoad` variant must not silently fold into Pending and
        // freeze the preview.
        let stage = match self.entry.map(|e| &e.state) {
            None | Some(TimelineLoad::NotRequested | TimelineLoad::Loading) => {
                TimelineStage::Pending
            }
            Some(TimelineLoad::Loaded) => TimelineStage::Ready,
            Some(TimelineLoad::Error(_)) => TimelineStage::Failed,
        };
        PreviewKey {
            tab: self.tab,
            number: self.number,
            stage,
            item_count: self.entry.map_or(0, |e| e.items.len()),
            has_more: self.entry.is_some_and(|e| e.next_cursor.is_some()),
            loading_more: self.entry.is_some_and(|e| e.loading_more),
            mergeable: self.entry.and_then(|e| e.mergeable),
            expand_commits: self.expand_commits,
            body_rev: self.body_rev,
            width: self.width,
        }
    }
}

/// Borrowed fields of the selected issue/PR, common to both plus whichever
/// extra bits are specific to the tab it came from.
#[derive(Clone, Copy)]
pub(super) struct SelectedItem<'v> {
    pub(super) title: &'v str,
    pub(super) state: &'v str,
    pub(super) author: &'v str,
    pub(super) labels: &'v [crate::github::GhLabel],
    pub(super) body: &'v str,
    pub(super) url: &'v str,
    pub(super) extra: SelectedItemExtra<'v>,
}

#[derive(Clone, Copy)]
pub(super) enum SelectedItemExtra<'v> {
    Issue {
        parent: Option<&'v crate::github::GhRelatedIssue>,
        sub_issues: &'v [crate::github::GhRelatedIssue],
    },
    PullRequest {
        base_ref_name: &'v str,
        head_ref_name: &'v str,
    },
}

/// Everything the preview content depends on. Equal key ⇒ identical output, so
/// the cache can be reused. A content key rather than a dirty flag: there are
/// ~20 sites that reset `preview_offset`, and relying on each to also mark the
/// cache stale would eventually miss one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PreviewKey {
    tab: GitHubTab,
    number: u64,
    stage: TimelineStage,
    item_count: usize,
    /// Drives the footer between "(loading more…)" and "(more comments — …)".
    has_more: bool,
    loading_more: bool,
    mergeable: Option<Mergeable>,
    expand_commits: bool,
    body_rev: u64,
    width: u16,
}

#[derive(Debug)]
pub(super) struct PreviewCache {
    pub(super) key: PreviewKey,
    pub(super) lines: Vec<Line<'static>>,
    pub(super) overlay: Option<PreviewOverlay>,
    /// Line count *after* wrapping — what `preview_offset` is measured in.
    pub(super) visual_len: usize,
}

/// Re-borrow cached lines instead of cloning them: `Paragraph` needs an owned
/// `Text`, but the spans can point at the cache's strings, so only the `Vec`s
/// are allocated per frame — no string copies.
pub(super) fn borrow_lines<'a>(lines: &'a [Line<'static>]) -> Vec<Line<'a>> {
    lines
        .iter()
        // Struct literal, not `Line::from(..)` plus assignments: a field added
        // upstream then fails to compile instead of being silently dropped.
        .map(|l| Line {
            spans: l
                .spans
                .iter()
                .map(|s| Span::styled(s.content.as_ref(), s.style))
                .collect(),
            style: l.style,
            alignment: l.alignment,
        })
        .collect()
}

#[derive(Debug, Clone)]
pub(super) struct RowData {
    pub(super) line: Line<'static>,
    pub(super) url: String,
    pub(super) number: u64,
}

fn related_issue_line(indent: &'static str, r: &crate::github::GhRelatedIssue) -> Line<'static> {
    Line::from(vec![
        Span::raw(indent),
        Span::styled(
            format!("#{} ", r.number),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(r.title.clone()),
        Span::raw(" "),
        Span::styled(
            format!("({})", r.state.to_lowercase()),
            Style::default().fg(state_color(&r.state)),
        ),
    ])
}

fn append_relation_lines(
    lines: &mut Vec<Line<'static>>,
    parent: Option<&crate::github::GhRelatedIssue>,
    sub_issues: &[crate::github::GhRelatedIssue],
    width: usize,
) {
    if let Some(parent) = parent {
        let prefix = "Parent: ";
        lines.push(Line::from(vec![
            Span::styled(prefix, Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("#{} ", parent.number),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw(parent.title.clone()),
            Span::raw(" "),
            Span::styled(
                format!("({})", parent.state.to_lowercase()),
                Style::default().fg(state_color(&parent.state)),
            ),
        ]));
    }
    if !sub_issues.is_empty() {
        let indent = "  ";
        lines.push(Line::styled(
            format!("Sub-issues ({}):", sub_issues.len()),
            Style::default().fg(Color::DarkGray),
        ));
        for sub in sub_issues {
            lines.push(related_issue_line(indent, sub));
        }
    }
    if parent.is_some() || !sub_issues.is_empty() {
        lines.push(crate::view::markdown::rule_line(width));
    }
}

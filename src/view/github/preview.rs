use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
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
    fn cache_key(&self) -> PreviewKey {
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
struct PreviewKey {
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

/// 一次建置的產物。完全不對外可見——連 `pub(super)` 都不需要，
/// `PreviewCache` 是它唯一的存取入口。
#[derive(Debug)]
struct CachedPreview {
    key: PreviewKey,
    lines: Vec<Line<'static>>,
    overlay: Option<PreviewOverlay>,
    /// Line count *after* wrapping — what `preview_offset` is measured in.
    visual_len: usize,
}

#[derive(Debug, Default)]
pub(super) struct PreviewCache {
    cached: Option<CachedPreview>,
    /// Rebuild counter, test-only. `build_preview_content` is pure, so two
    /// calls with an unchanged `PreviewInput` produce identical output
    /// whether the second one actually reused the cache or quietly rebuilt
    /// it — comparing output alone can't tell "reused" apart from "rebuilt
    /// to the same result". This is the only way to observe the difference
    /// from outside `get_or_build`.
    #[cfg(test)]
    build_count: usize,
}

impl PreviewCache {
    /// Rebuild the preview only when its inputs changed, returning the
    /// wrapped line count either way. `render_preview` runs at the marquee
    /// tick rate (10 Hz) whenever the selected row overflows, and both
    /// `markdown::render` and `line_count` walk the entire body plus every
    /// comment — so recomputing per frame burns CPU while idle.
    ///
    /// Wrapping is left to `Paragraph` rather than reusing
    /// `commit_detail::wrap_line_spans`: that one breaks mid-word, which
    /// would mangle the English prose common in PR bodies.
    pub(super) fn get_or_build(&mut self, input: &PreviewInput) -> usize {
        let key = input.cache_key();
        if let Some(c) = self.cached.as_ref().filter(|c| c.key == key) {
            return c.visual_len;
        }
        #[cfg(test)]
        {
            self.build_count += 1;
        }
        let (lines, overlay) = build_preview_content(input);
        let visual_len = Paragraph::new(borrow_lines(&lines))
            .wrap(Wrap { trim: false })
            .line_count(input.width);
        self.cached = Some(CachedPreview {
            key,
            lines,
            overlay,
            visual_len,
        });
        visual_len
    }

    /// 冷快取回傳空切片；`render_preview` 一定先呼叫 `get_or_build`。
    pub(super) fn lines(&self) -> &[Line<'static>] {
        self.cached.as_ref().map_or(&[], |c| &c.lines)
    }

    pub(super) fn overlay(&self) -> Option<&PreviewOverlay> {
        self.cached.as_ref().and_then(|c| c.overlay.as_ref())
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded_entry(mergeable: Option<Mergeable>) -> TimelineEntry {
        TimelineEntry {
            state: TimelineLoad::Loaded,
            mergeable,
            ..Default::default()
        }
    }

    fn pr_input(entry: &TimelineEntry) -> PreviewInput<'_> {
        PreviewInput {
            tab: GitHubTab::PullRequests,
            number: 1,
            width: 40,
            body_rev: 0,
            entry: Some(entry),
            expand_commits: true,
            item: Some(SelectedItem {
                title: "t",
                state: "OPEN",
                author: "alice",
                labels: &[],
                body: "body",
                url: "",
                extra: SelectedItemExtra::PullRequest {
                    base_ref_name: "main",
                    head_ref_name: "topic",
                },
            }),
        }
    }

    #[test]
    fn get_or_build_reuses_cache_for_an_unchanged_input() {
        let entry = loaded_entry(None);
        let input = pr_input(&entry);
        let mut cache = PreviewCache::default();

        let first_len = cache.get_or_build(&input);
        assert_eq!(cache.build_count, 1);

        // Why build_count and not just comparing output: see its field doc.
        let second_len = cache.get_or_build(&input);
        assert_eq!(
            cache.build_count, 1,
            "unchanged input must not trigger a rebuild"
        );
        assert_eq!(first_len, second_len);
    }

    /// mergeable 掛在 TimelineEntry 上，不是 selected item 的獨立欄位——這
    /// 釘住 cache key 有獨立追蹤它，不是靠 stage 變化順便觸發。兩個 entry
    /// 都是 Loaded，兩個 input 之間唯一的差異就是 mergeable 本身。
    #[test]
    fn get_or_build_rebuilds_when_mergeable_changes() {
        let mut cache = PreviewCache::default();

        cache.get_or_build(&pr_input(&loaded_entry(None)));
        cache.get_or_build(&pr_input(&loaded_entry(Some(Mergeable::Mergeable))));

        assert_eq!(
            cache.build_count, 2,
            "mergeable change must invalidate the cache"
        );
        assert!(
            cache
                .lines()
                .iter()
                .any(|l| l.spans.iter().any(|s| s.content.contains("(mergeable)"))),
            "rebuilt content must reflect the new mergeable state, got: {:?}",
            cache.lines()
        );
    }

    /// `body_rev` 是唯一在 `PreviewKey` 裡代表「body 內容變了」的欄位——
    /// `PreviewKey` 本身不存 body 文字（太貴）。兩個 input 的 body 文字
    /// 故意給一樣的，只讓 body_rev 不同：光比對畫面內容區分不出「有沒有
    /// 重建」，因為就算重建，輸出也會長得一模一樣，只能靠 build_count。
    #[test]
    fn get_or_build_rebuilds_when_body_rev_changes() {
        let entry = loaded_entry(None);
        let mut cache = PreviewCache::default();

        cache.get_or_build(&pr_input(&entry));
        cache.get_or_build(&PreviewInput {
            body_rev: 1,
            ..pr_input(&entry)
        });

        assert_eq!(
            cache.build_count, 2,
            "body_rev change must invalidate the cache"
        );
    }
}

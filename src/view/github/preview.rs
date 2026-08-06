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

    // Header：#number title（#N 超連結疊加）
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

/// 畫在 preview header 那一列（`#N`）上的 OSC 8 超連結——這是唯一會被
/// 附加疊加層的 preview 列；參見 `render_preview`。
#[derive(Debug, Clone)]
pub(super) struct PreviewOverlay {
    pub(super) url: String,
    pub(super) label: String,
}

/// `build_preview_content` 讀取的所有東西，借用自選取的 issue/PR 與其
/// timeline entry，再加上結果要折行的寬度。`cache_key` 讀的是同一個
/// struct，所以不會悄悄漏掉內容建置所依賴的欄位。
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
        // 光靠數量不夠：「已載入但是空的」跟「還在載入中」都是零筆項目，
        // 但渲染結果不同。刻意寫成窮舉——新加的 `TimelineLoad` 變體不能
        // 悄悄被歸進 Pending，讓 preview 卡住不動。
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

/// 選取的 issue/PR 借用欄位，兩者共通的部分，加上依所屬分頁而異的額外欄位。
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

/// preview 內容依賴的所有東西。key 相等 ⇒ 輸出必然相同，因此可以重用 cache。
/// 用內容 key 而非 dirty flag：有將近 20 個地方會重置 `preview_offset`，
/// 依賴每一處都同步標記 cache 過期，遲早會漏掉一個。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PreviewKey {
    tab: GitHubTab,
    number: u64,
    stage: TimelineStage,
    item_count: usize,
    /// 決定 footer 要顯示「(loading more…)」還是「(more comments — …)」。
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
    /// 折行*之後*的行數——`preview_offset` 就是以這個為單位量測。
    visual_len: usize,
}

#[derive(Debug, Default)]
pub(super) struct PreviewCache {
    cached: Option<CachedPreview>,
    /// 重建計數器，僅供測試用。`build_preview_content` 是 pure function，
    /// 所以用同一個未變的 `PreviewInput` 呼叫兩次，不管第二次是真的重用了
    /// cache 還是悄悄重建，輸出都一樣——光比對輸出沒辦法分辨「重用」跟
    /// 「重建出相同結果」。這是從 `get_or_build` 外部觀察兩者差異的唯一方法。
    #[cfg(test)]
    build_count: usize,
}

impl PreviewCache {
    /// 只在輸入改變時才重建 preview，不論哪種情況都回傳折行後的行數。
    /// 只要選取列有溢出，`render_preview` 就會以跑馬燈的更新頻率
    /// （10 Hz）執行，而 `markdown::render` 跟 `line_count` 都會走訪整個
    /// body 加上每則留言——所以閒置時每一 frame 都重算會白白燒 CPU。
    ///
    /// 折行交給 `Paragraph` 處理，不重用 `commit_detail::wrap_line_spans`：
    /// 那個函式會在字中間斷行，會把 PR body 裡常見的英文散文弄得亂七八糟。
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

/// 重新借用已快取的行，而不是複製它們：`Paragraph` 需要一個 owned 的
/// `Text`，但 span 可以直接指向 cache 裡的字串，所以每個 frame 只需配置
/// `Vec`——不用複製字串。
pub(super) fn borrow_lines<'a>(lines: &'a [Line<'static>]) -> Vec<Line<'a>> {
    lines
        .iter()
        // 用 struct literal，不是 `Line::from(..)` 加賦值：上游多加一個欄位
        // 時會編譯失敗，而不是被悄悄漏掉。
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

        // 為什麼用 build_count 而不是直接比對輸出：見該欄位的文件註解。
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

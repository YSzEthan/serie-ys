use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use rust_i18n::t;

use crate::{github::GhBranch, widget::truncate_line};

use super::timeline::diff_stat_spans;

/// 卡片高度：上下邊框 2 列 + 內容 2 列。branch 名放在上框的 title，不佔內容列。
pub(super) const CARD_HEIGHT: u16 = 4;

/// 一張卡片的最小寬度；內容區寬度除以它決定一列放幾張。容得下
/// `hash · 日期 · @作者` 加上 `+N -N =N` 的常見長度。
const MIN_CARD_WIDTH: usize = 36;

/// 一列放幾張卡片：寬度夠就多欄，至少一欄。
pub(super) fn columns(width: usize) -> usize {
    (width / MIN_CARD_WIDTH).max(1)
}

/// 邊框以外的文字一律用亮灰色（`Color::Gray`），不用 `DarkGray`，深色背景下才看得清楚。
const TEXT_COLOR: Color = Color::Gray;

/// 邊框 2 格 + title 兩側各 1 格空白。
const TITLE_CHROME: usize = 4;

pub(super) fn render_card(f: &mut Frame, area: Rect, branch: &GhBranch, selected: bool) {
    let (border_color, title_style) = if selected {
        (Color::Cyan, Style::default().fg(Color::Cyan).bold())
    } else {
        (Color::DarkGray, Style::default().fg(TEXT_COLOR))
    };

    let title = truncate_line(
        // 樣式要放在 span 上：`Line::styled` 只設整行樣式，下面取出 `spans`
        // 組 title 時會丟掉，標題就退回邊框的顏色。
        Line::from(Span::styled(branch.name.as_str(), title_style)),
        (area.width as usize).saturating_sub(TITLE_CHROME),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(Line::from(
            [vec![Span::raw(" ")], title.spans, vec![Span::raw(" ")]].concat(),
        ));

    f.render_widget(Paragraph::new(card_body(branch)).block(block), area);
}

fn card_body(branch: &GhBranch) -> Vec<Line<'static>> {
    let short_oid: String = branch.head_oid.chars().take(7).collect();
    let meta = Line::styled(
        format!(
            " {short_oid} · {} · @{}",
            branch.committer_date, branch.author
        ),
        Style::default().fg(TEXT_COLOR),
    );
    // `diff_stat_spans` 自帶開頭兩格空白，剛好當卡片內縮
    let stat = match branch.diff_stat {
        Some(stat) => Line::from(diff_stat_spans(stat).to_vec()),
        None => Line::styled(
            format!("  {}", t!("github.branches.stat_unavailable")),
            Style::default().fg(TEXT_COLOR),
        ),
    };
    vec![meta, stat]
}

#[cfg(test)]
mod tests {
    use ratatui::{backend::TestBackend, Terminal};

    use super::*;
    use crate::github::DiffStat;

    fn branch(name: &str) -> GhBranch {
        GhBranch {
            name: name.to_string(),
            head_oid: "0123456789abcdef".to_string(),
            committer_date: "2026-01-02".to_string(),
            author: "alice".to_string(),
            diff_stat: Some(DiffStat {
                additions: 12,
                deletions: 3,
            }),
            url: String::new(),
        }
    }

    fn render_to_rows(branch: &GhBranch, width: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, CARD_HEIGHT)).unwrap();
        terminal
            .draw(|f| render_card(f, f.area(), branch, true))
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..CARD_HEIGHT)
            .map(|y| {
                (0..width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn card_shows_name_meta_and_line_stat() {
        let rows = render_to_rows(&branch("feat/x"), 40);

        assert!(rows[0].contains(" feat/x "), "{rows:?}");
        assert!(
            rows[1].contains("0123456 · 2026-01-02 · @alice"),
            "{rows:?}"
        );
        assert!(rows[2].contains("+12 -3 =15"), "{rows:?}");
    }

    /// 本機沒有 commit（沒 fetch 到）時不能秀 `+0 -0`，要明講算不出來。
    #[test]
    fn card_without_local_commit_explains_missing_line_stat() {
        let mut b = branch("feat/x");
        b.diff_stat = None;

        let rows = render_to_rows(&b, 60);

        // CJK 在 TestBackend 的 buffer 裡每個字後面會跟一格空白，所以不比對整句翻譯
        assert!(!rows[2].contains('+'), "{rows:?}");
        assert!(rows[2].contains("commit"), "{rows:?}");
    }

    #[test]
    fn long_name_is_truncated_inside_the_border() {
        let rows = render_to_rows(&branch(&"a".repeat(80)), 20);

        assert!(rows[0].contains('…'), "{rows:?}");
        assert!(rows[0].ends_with('┐'), "{rows:?}");
    }

    #[test]
    fn card_columns_follow_width_and_never_drop_below_one() {
        assert_eq!(columns(0), 1);
        assert_eq!(columns(MIN_CARD_WIDTH - 1), 1);
        assert_eq!(columns(MIN_CARD_WIDTH), 1);
        assert_eq!(columns(MIN_CARD_WIDTH * 2), 2);
        assert_eq!(columns(MIN_CARD_WIDTH * 3 + 5), 3);
    }

    #[test]
    fn text_outside_the_border_is_bright_gray() {
        let mut terminal = Terminal::new(TestBackend::new(40, CARD_HEIGHT)).unwrap();
        terminal
            .draw(|f| render_card(f, f.area(), &branch("feat/x"), false))
            .unwrap();
        let buf = terminal.backend().buffer().clone();

        // 非選取：title（第 0 列）與 meta（第 1 列）文字是亮灰，邊框維持 DarkGray
        assert_eq!(buf[(2, 0)].fg, Color::Gray);
        assert_eq!(buf[(2, 1)].fg, Color::Gray);
        assert_eq!(buf[(0, 0)].fg, Color::DarkGray);
    }

    #[test]
    fn tiny_width_does_not_panic() {
        render_to_rows(&branch("feat/x"), 4);
        render_to_rows(&branch("feat/x"), 1);
    }
}

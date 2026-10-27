use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use rust_i18n::t;

use crate::{
    github::{CheckState, CiEntry, CiState},
    widget::truncate_line,
};

use super::branches::{card_block, TEXT_COLOR};

/// 卡片左右各縮 1 格（邊框 2 + 內縮 1）；色點列可用寬度 = 卡片寬 - 3。
const BODY_CHROME: usize = 3;

/// check 色點的顏色：灰＝運作中、綠＝成功、紅＝失敗。運作中用 `Gray` 而非
/// `DarkGray`，才不會跟邊框糊在一起（同 branches.rs 的慣例）。
fn dot_color(state: CheckState) -> Color {
    match state {
        CheckState::Failed => Color::Red,
        CheckState::Pending => Color::Gray,
        CheckState::Passed => Color::Green,
    }
}

pub(super) fn render_card(f: &mut Frame, area: Rect, entry: &CiEntry, selected: bool) {
    let block = card_block(
        &format!("#{} {}", entry.number, entry.title),
        area.width,
        selected,
    );
    f.render_widget(
        Paragraph::new(card_body(
            entry,
            (area.width as usize).saturating_sub(BODY_CHROME),
        ))
        .block(block),
        area,
    );
}

fn card_body(entry: &CiEntry, width: usize) -> Vec<Line<'static>> {
    let passed = entry
        .checks
        .iter()
        .filter(|c| c.state == CheckState::Passed)
        .count();
    let meta = if entry.state() == CiState::Unknown {
        format!(" {}", entry.branch)
    } else {
        format!(" {} · {passed}/{}", entry.branch, entry.checks.len())
    };
    let meta = truncate_line(
        Line::styled(meta, Style::default().fg(TEXT_COLOR)),
        width + 1,
    );
    vec![meta, dots_line(entry, width)]
}

/// 每個 check 一個 `●`（`checks` 已依 fail → pending → pass 排序）；寬度不夠時
/// 截掉尾端並補 ` +N`，優先丟掉的是綠色。
fn dots_line(entry: &CiEntry, width: usize) -> Line<'static> {
    let gray = Style::default().fg(TEXT_COLOR);
    if entry.checks.is_empty() {
        return Line::from(vec![
            Span::styled(" …", gray),
            Span::styled(format!(" {}", t!("github.ci.no_checks")), gray),
        ]);
    }

    let total = entry.checks.len();
    let shown = if total <= width {
        total
    } else {
        // 預留 ` +N` 的位置，N 的位數隨顯示數量變，所以由大往小找第一個放得下的
        (0..width)
            .rev()
            .find(|&k| k + 1 + format!("+{}", total - k).len() <= width)
            .unwrap_or(0)
    };

    let mut spans = vec![Span::raw(" ")];
    spans.extend(
        entry.checks[..shown]
            .iter()
            .map(|c| Span::styled("●", Style::default().fg(dot_color(c.state)))),
    );
    if shown < total {
        spans.push(Span::styled(format!(" +{}", total - shown), gray));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use ratatui::{backend::TestBackend, Terminal};

    use super::*;
    use crate::github::GhCheck;
    use crate::view::github::branches::CARD_HEIGHT;

    fn entry(checks: &[CheckState]) -> CiEntry {
        CiEntry {
            number: 12,
            title: "修正 pty 斷線".to_string(),
            url: String::new(),
            branch: "feat/pty-fix".to_string(),
            head_sha: "abc".to_string(),
            checks: checks
                .iter()
                .enumerate()
                .map(|(i, &state)| GhCheck {
                    name: format!("c{i}"),
                    state,
                    run_id: None,
                })
                .collect(),
        }
    }

    fn render(entry: &CiEntry, width: u16) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, CARD_HEIGHT)).unwrap();
        terminal
            .draw(|f| render_card(f, f.area(), entry, false))
            .unwrap();
        terminal
    }

    fn row(terminal: &Terminal<TestBackend>, y: u16) -> String {
        let buf = terminal.backend().buffer();
        (0..buf.area.width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect()
    }

    #[test]
    fn dots_are_red_gray_green_in_order_with_count() {
        use CheckState::*;
        let terminal = render(&entry(&[Failed, Pending, Passed, Passed]), 40);

        assert!(row(&terminal, 1).contains("feat/pty-fix · 2/4"));
        let buf = terminal.backend().buffer();
        // 第 2 列：x=0 是邊框，x=1 是內縮空白，色點從 x=2 開始
        assert_eq!(buf[(2, 2)].symbol(), "●");
        assert_eq!(buf[(2, 2)].fg, Color::Red);
        assert_eq!(buf[(3, 2)].fg, Color::Gray);
        assert_eq!(buf[(4, 2)].fg, Color::Green);
        assert_eq!(buf[(5, 2)].fg, Color::Green);
    }

    #[test]
    fn too_many_dots_are_truncated_with_remaining_count() {
        let terminal = render(&entry(&[CheckState::Passed; 30]), 20);

        let line = row(&terminal, 2);
        assert!(line.contains(" +"), "{line}");
        assert!(!line.contains("●●●●●●●●●●●●●●●●●●●●"), "{line}");
        let dots = line.matches('●').count();
        let rest: usize = line
            .split('+')
            .nth(1)
            .unwrap()
            .trim_end_matches(|c: char| !c.is_ascii_digit())
            .parse()
            .unwrap();
        assert_eq!(dots + rest, 30, "{line}");
    }

    #[test]
    fn entry_without_checks_says_no_ci_yet() {
        let terminal = render(&entry(&[]), 40);

        let line = row(&terminal, 2);
        assert!(line.contains('…'), "{line}");
        assert!(!line.contains('●'), "{line}");
        assert!(
            !row(&terminal, 1).contains("0/0"),
            "unknown 不顯示 passed/total"
        );
    }

    #[test]
    fn tiny_width_does_not_panic() {
        render(&entry(&[CheckState::Failed; 5]), 4);
        render(&entry(&[CheckState::Failed; 5]), 1);
    }
}

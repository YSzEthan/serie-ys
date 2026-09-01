use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Padding, Paragraph, Widget},
};

use crate::{
    color::ColorTheme,
    event::UserEvent,
    keybind::KeyBind,
    widget::{h, keybind_hint_line},
};

pub struct PendingOverlay<'a> {
    title: &'static str,
    message: &'a str,
    color_theme: &'a ColorTheme,
    /// 底下那行鍵位提示，建構時就算好——兩個建構子手上都已經有
    /// `color_theme`，不必延後到 `render()` 才決定內容。
    hint: Line<'static>,
}

impl<'a> PendingOverlay<'a> {
    pub fn working(message: &'a str, color_theme: &'a ColorTheme, keybind: &KeyBind) -> Self {
        Self {
            title: " Working... ",
            message,
            color_theme,
            // 關掉它的是 `UserEvent::Cancel`（見 `App::handle_key`），不是
            // 寫死的 Esc。
            hint: keybind_hint_line(color_theme, keybind, &[h(&[UserEvent::Cancel], "hide")]),
        }
    }

    /// `AppEvent::ExeReplacedCheck` 自動重啟後的一次性通知——沒有背景
    /// 操作，任何鍵都能關（見 `App::handle_key` 對 `notice_message` 的
    /// 處理），不像 `working()` 只認 Cancel，所以底下是靜態文字，不透過
    /// `keybind_hint_line`。
    pub fn notice(message: &'a str, color_theme: &'a ColorTheme) -> Self {
        Self {
            title: " Restarted ",
            message,
            color_theme,
            hint: Line::styled(
                "any key: close",
                Style::default().fg(color_theme.help_key_fg),
            ),
        }
    }
}

impl Widget for PendingOverlay<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let dialog_width = 40u16.min(area.width.saturating_sub(4));
        let max_text_width = dialog_width.saturating_sub(4) as usize; // 邊框 + padding

        // 把訊息換行成多行
        let message_lines: Vec<Line> = wrap_text(self.message, max_text_width)
            .into_iter()
            .map(|s| Line::from(Span::raw(s).add_modifier(Modifier::BOLD)))
            .collect();

        let dialog_height = (4 + message_lines.len() as u16).min(area.height.saturating_sub(2));

        let dialog_x = (area.width.saturating_sub(dialog_width)) / 2;
        let dialog_y = (area.height.saturating_sub(dialog_height)) / 2;

        let dialog_area = Rect::new(
            area.x + dialog_x,
            area.y + dialog_y,
            dialog_width,
            dialog_height,
        );

        Clear.render(dialog_area, buf);

        let block = Block::default()
            .title(self.title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.color_theme.divider_fg))
            .style(
                Style::default()
                    .bg(self.color_theme.bg)
                    .fg(self.color_theme.fg),
            )
            .padding(Padding::horizontal(1));

        let inner_area = block.inner(dialog_area);
        block.render(dialog_area, buf);

        let mut lines = vec![Line::raw("")];
        lines.extend(message_lines);
        lines.push(Line::raw(""));
        lines.push(self.hint);

        Paragraph::new(lines).centered().render(inner_area, buf);
    }
}

fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return vec![String::new()];
    }

    let mut lines = Vec::new();
    let mut current_line = String::new();

    for word in text.split_whitespace() {
        // 超過 max_width 的單字要拆開處理
        if word.chars().count() > max_width {
            // 先把目前這行輸出
            if !current_line.is_empty() {
                lines.push(current_line);
                current_line = String::new();
            }
            // 把長單字拆成多個區塊
            let mut chars = word.chars().peekable();
            while chars.peek().is_some() {
                let chunk: String = chars.by_ref().take(max_width).collect();
                lines.push(chunk);
            }
        } else if current_line.is_empty() {
            current_line = word.to_string();
        } else if current_line.chars().count() + 1 + word.chars().count() <= max_width {
            current_line.push(' ');
            current_line.push_str(word);
        } else {
            lines.push(current_line);
            current_line = word.to_string();
        }
    }

    if !current_line.is_empty() {
        lines.push(current_line);
    }

    if lines.is_empty() {
        lines.push(String::new());
    }

    lines
}

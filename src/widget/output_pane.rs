use std::rc::Rc;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::Line,
    widgets::{Block, Borders, Padding, Paragraph, StatefulWidget, Widget},
};

use crate::app::AppContext;

#[derive(Debug, Default)]
pub struct OutputPaneState {
    height: usize,
    offset: usize,
}

impl OutputPaneState {
    pub fn scroll_down(&mut self) {
        self.offset = self.offset.saturating_add(1);
    }

    pub fn scroll_up(&mut self) {
        self.offset = self.offset.saturating_sub(1);
    }

    pub fn scroll_page_down(&mut self) {
        self.offset = self.offset.saturating_add(self.height);
    }

    pub fn scroll_page_up(&mut self) {
        self.offset = self.offset.saturating_sub(self.height);
    }

    pub fn scroll_half_page_down(&mut self) {
        self.offset = self.offset.saturating_add(self.height / 2);
    }

    pub fn scroll_half_page_up(&mut self) {
        self.offset = self.offset.saturating_sub(self.height / 2);
    }

    pub fn select_first(&mut self) {
        self.offset = 0;
    }

    pub fn select_last(&mut self) {
        self.offset = usize::MAX;
    }
}

pub struct OutputPane<'a> {
    lines: &'a Vec<Line<'a>>,
    ctx: Rc<AppContext>,
    title: Option<&'a str>,
}

impl<'a> OutputPane<'a> {
    pub fn new(lines: &'a Vec<Line<'a>>, ctx: Rc<AppContext>) -> Self {
        Self {
            lines,
            ctx,
            title: None,
        }
    }

    pub fn title(mut self, title: &'a str) -> Self {
        self.title = Some(title);
        self
    }
}

impl StatefulWidget for OutputPane<'_> {
    type State = OutputPaneState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let content_area_height = (area.height as usize).saturating_sub(1); // 扣掉上方邊框
        self.update_state(state, self.lines.len(), content_area_height);

        self.render_output_lines(area, buf, state);
    }
}

impl OutputPane<'_> {
    fn render_output_lines(&self, area: Rect, buf: &mut Buffer, state: &mut OutputPaneState) {
        let lines = self
            .lines
            .iter()
            .skip(state.offset)
            .take((area.height as usize).saturating_sub(1))
            .cloned()
            .collect::<Vec<_>>();
        let mut block = Block::default()
            .borders(Borders::TOP)
            .style(Style::default().fg(self.ctx.color_theme.divider_fg))
            .padding(Padding::horizontal(2));
        if let Some(title) = self.title {
            block = block.title(title);
        }
        let paragraph = Paragraph::new(lines)
            .style(Style::default().fg(self.ctx.color_theme.fg))
            .block(block);
        paragraph.render(area, buf);
    }

    fn update_state(&self, state: &mut OutputPaneState, line_count: usize, area_height: usize) {
        state.height = area_height;
        state.offset = state.offset.min(line_count.saturating_sub(area_height));
    }
}

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
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// 跳到指定 offset（hunk 導航用）並當下 clamp，不等下一次 render。
    /// `render` 也會 clamp，但那是在畫面畫出來**之後**；呼叫端如果在
    /// render 之前就要用新 offset 算東西（例如 title 的 `hunk n/m`），
    /// 必須在這裡先拿到夾好的值，否則跳到最後一個 hunk 剛好觸發 clamp
    /// 時，title 用的還是上一幀的舊 offset。
    pub fn scroll_to(&mut self, offset: usize, line_count: usize) {
        self.offset = offset;
        self.clamp(line_count);
    }

    /// 「最大 offset 不超過 `line_count - height`」這條規則只寫在這一處——
    /// `render` 與 `scroll_to` 各自要在不同時機套用同一條夾值公式，
    /// 分開寫兩份的話，改規則（例如保留一行重疊）容易漏改一邊。
    fn clamp(&mut self, line_count: usize) {
        self.offset = self.offset.min(line_count.saturating_sub(self.height));
    }

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
    title: Option<Line<'a>>,
}

impl<'a> OutputPane<'a> {
    pub fn new(lines: &'a Vec<Line<'a>>, ctx: Rc<AppContext>) -> Self {
        Self {
            lines,
            ctx,
            title: None,
        }
    }

    /// 樣式由呼叫端決定，widget 不替呼叫端猜 title 的顏色。
    pub fn title(mut self, title: Line<'a>) -> Self {
        self.title = Some(title);
        self
    }
}

impl StatefulWidget for OutputPane<'_> {
    type State = OutputPaneState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        state.height = (area.height as usize).saturating_sub(1); // 扣掉上方邊框
        state.clamp(self.lines.len());

        let lines = self
            .lines
            .iter()
            .skip(state.offset)
            .take(state.height)
            .cloned()
            .collect::<Vec<_>>();
        let mut block = Block::default()
            .borders(Borders::TOP)
            .style(Style::default().fg(self.ctx.color_theme.divider_fg))
            .padding(Padding::horizontal(2));
        if let Some(title) = self.title {
            block = block.title_top(title);
        }
        let paragraph = Paragraph::new(lines)
            .style(Style::default().fg(self.ctx.color_theme.fg))
            .block(block);
        paragraph.render(area, buf);
    }
}

use std::rc::Rc;

use ratatui::{
    crossterm::event::KeyEvent,
    layout::Rect,
    style::Style,
    widgets::{Block, Borders, Padding, Paragraph, Wrap},
    Frame,
};

use crate::{
    app::AppContext,
    event::{AppEvent, Sender, UserEvent, UserEventWithCount},
    view::View,
};

/// 內文的最大閱讀寬度，超出的部分留白置中——寬螢幕下一行拉到 200 字元
/// 會很難讀，CHANGELOG 條目又是短句而非需要利用寬螢幕排版的長文。
const READING_WIDTH: u16 = 100;

/// 更新後（或全新安裝）第一次啟動時跳出的 overlay，內容是這一版
/// `CHANGELOG.md` 的區塊（`update::extract_release_notes` 抽出來的）。
/// 結構照 `HelpView`：持有 `before` 供 Esc/q 關閉時展開回去。
///
/// 不重用 `widget::output_pane::OutputPane`：那個 widget 的 `Paragraph`
/// 沒開 `.wrap()`，`skip(offset).take(height)` 假設一個原始行對應一個視覺
/// 行——這對它現有的兩個用途（diff、user command 的 ANSI 輸出）是對的，
/// 但 CHANGELOG 條目常常是一整行很長的中文描述，不折行會被硬截斷在畫面
/// 右緣。這裡改成跟 `view/github/render.rs::render_preview` 一樣的模式：
/// 自己管一個 `offset`／`height`，`Paragraph` 開 `.wrap(Wrap { trim: false })`
/// 做詞界折行，`.scroll()` 的單位是折行後的視覺行、不是原始行。
#[derive(Debug)]
pub struct ReleaseNotesView<'a> {
    before: View<'a>,
    body: &'static str,
    offset: usize,
    /// 上一次 render 量到的可視高度（折行後的列數），PageDown／
    /// HalfPageDown 換算頁距要用；下一幀 render 會重新量測。
    height: usize,
    ctx: Rc<AppContext>,
    tx: Sender,
}

impl<'a> ReleaseNotesView<'a> {
    pub fn new(
        before: View<'a>,
        body: &'static str,
        ctx: Rc<AppContext>,
        tx: Sender,
    ) -> ReleaseNotesView<'a> {
        ReleaseNotesView {
            before,
            body,
            offset: 0,
            height: 0,
            ctx,
            tx,
        }
    }

    pub fn take_before_view(&mut self) -> View<'a> {
        std::mem::take(&mut self.before)
    }

    pub fn handle_event(&mut self, event_with_count: UserEventWithCount, _: KeyEvent) {
        let event = event_with_count.event;
        let count = event_with_count.count;

        match event {
            UserEvent::Quit => {
                self.tx.send(AppEvent::Quit);
            }
            UserEvent::Cancel | UserEvent::Close | UserEvent::NavigateLeft => {
                self.tx.send(AppEvent::CloseReleaseNotes);
            }
            UserEvent::NavigateDown | UserEvent::SelectDown => self.scroll_down(count),
            UserEvent::NavigateUp | UserEvent::SelectUp => self.scroll_up(count),
            UserEvent::PageDown => self.scroll_down(self.page().saturating_mul(count)),
            UserEvent::PageUp => self.scroll_up(self.page().saturating_mul(count)),
            UserEvent::HalfPageDown => self.scroll_down((self.page() / 2).saturating_mul(count)),
            UserEvent::HalfPageUp => self.scroll_up((self.page() / 2).saturating_mul(count)),
            _ => {}
        }
    }

    /// `height` 是「上一幀量到的可視高度」，Page／HalfPage 換算頁距共用
    /// 這個下限——高度還沒量過（第一幀）或量到 0 時，一頁至少要能挪動
    /// 一行，不能卡死在原地。
    fn page(&self) -> usize {
        self.height.max(1)
    }

    fn scroll_down(&mut self, n: usize) {
        self.offset = self.offset.saturating_add(n);
    }

    fn scroll_up(&mut self, n: usize) {
        self.offset = self.offset.saturating_sub(n);
    }

    /// 沒有內容快取：CHANGELOG 一節通常只有 10–60 行，`markdown::render`
    /// 是逐行純字串處理，這個 view 也沒有 marquee（只有按鍵才會重畫），
    /// 不值得為它另外維護一個依 width 判斷的快取（對比
    /// `github/preview.rs::PreviewCache`——那裡是幾百則留言的 timeline，
    /// 量級完全不同）。
    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::TOP)
            .style(Style::default().fg(self.ctx.color_theme.divider_fg))
            .padding(Padding::horizontal(2))
            .title_top(" Release Notes ");
        let inner = block.inner(area);
        self.height = inner.height as usize;
        f.render_widget(block, area);

        // 邊框／標題吃滿整個寬度，內文限制在一個閱讀寬度內水平置中——寬
        // 螢幕下整行拉到底邊緣會很難讀。窄螢幕（inner.width 本身就小於
        // READING_WIDTH）自然退化成滿版，不必特判。
        let content_width = inner.width.min(READING_WIDTH);
        let margin = (inner.width - content_width) / 2;
        let content_area = Rect {
            x: inner.x + margin,
            y: inner.y,
            width: content_width,
            height: inner.height,
        };

        let lines = super::markdown::render(self.body, content_width as usize);
        let paragraph = Paragraph::new(lines)
            .style(Style::default().fg(self.ctx.color_theme.fg))
            .wrap(Wrap { trim: false });

        let max_offset = paragraph
            .line_count(content_width)
            .saturating_sub(self.height)
            .min(u16::MAX as usize);
        self.offset = self.offset.min(max_offset);

        f.render_widget(paragraph.scroll((self.offset as u16, 0)), content_area);
    }
}

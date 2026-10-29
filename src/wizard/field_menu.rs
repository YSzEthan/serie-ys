//! 一組數字欄位收成一頁的子選單：主選單只留一列，點進來才逐項設定。
//! 欄位的編輯、存檔與主選單的數字欄位完全共用（`edit_number`／`NumberField`），
//! 這裡只負責瀏覽與進出。

use std::borrow::Cow;

use ratatui::{
    crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind},
    layout::{Constraint, Layout, Rect},
    widgets::{ListItem, ListState, Paragraph},
    DefaultTerminal, Frame,
};
use rust_i18n::t;

use super::{
    clamped_move, edit_number, editor_row_label, is_abort_key, styled_list, Dialog, Draft, Editor,
    NumberField, ResolvedDefaults,
};
use crate::color::ColorTheme;

pub(super) enum Flow {
    Continue,
    Back,
    Edit(NumberField),
}

pub(super) struct FieldMenuState {
    fields: &'static [NumberField],
    list: ListState,
}

impl FieldMenuState {
    pub(super) fn new(fields: &'static [NumberField]) -> Self {
        let mut list = ListState::default();
        list.select(Some(0));
        Self { fields, list }
    }

    /// 零 I/O 的純狀態轉移，可以直接餵 `KeyEvent` 測試。Esc／←／h 與
    /// Ctrl-C 都只是回上一層，不會放棄整個精靈。
    pub(super) fn on_key(&mut self, key: KeyEvent) -> Flow {
        if key.kind != KeyEventKind::Press {
            return Flow::Continue;
        }
        if is_abort_key(&key) {
            return Flow::Back;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => Flow::Back,
            KeyCode::Up | KeyCode::Char('k') => {
                clamped_move(&mut self.list, -1, self.fields.len());
                Flow::Continue
            }
            KeyCode::Down | KeyCode::Char('j') => {
                clamped_move(&mut self.list, 1, self.fields.len());
                Flow::Continue
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self
                .list
                .selected()
                .map_or(Flow::Continue, |i| Flow::Edit(self.fields[i])),
            _ => Flow::Continue,
        }
    }

    fn render(
        &mut self,
        f: &mut Frame,
        area: Rect,
        draft: &Draft,
        defaults: &ResolvedDefaults,
        theme: &ColorTheme,
    ) {
        let [list_area, hint_area] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);

        let items: Vec<ListItem> = self
            .fields
            .iter()
            .map(|&field| {
                let editor = Editor::Dialog(Dialog::Number(field));
                ListItem::new(editor_row_label(editor, draft, defaults, Cow::Borrowed("")))
            })
            .collect();
        f.render_stateful_widget(styled_list(items, theme), list_area, &mut self.list);

        let hint = crate::widget::hint_line(
            theme,
            &[
                ("↑↓/kj".into(), &*t!("wizard.hint.select")),
                ("Enter/→/l".into(), &*t!("wizard.hint.edit")),
                ("Esc/←/h".into(), &*t!("common.hint.back")),
            ],
            theme.help_key_fg,
        );
        f.render_widget(Paragraph::new(hint), hint_area);
    }
}

pub(super) fn run(
    terminal: &mut DefaultTerminal,
    draft: &mut Draft,
    defaults: &ResolvedDefaults,
    fields: &'static [NumberField],
    theme: &ColorTheme,
) -> crate::Result<()> {
    let mut state = FieldMenuState::new(fields);
    loop {
        terminal.draw(|f| state.render(f, f.area(), draft, defaults, theme))?;
        let Event::Key(key) = ratatui::crossterm::event::read()? else {
            continue;
        };
        match state.on_key(key) {
            Flow::Continue => {}
            Flow::Back => return Ok(()),
            Flow::Edit(field) => {
                edit_number(terminal, draft, defaults, field, theme)?;
                terminal.clear()?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::KeyModifiers;

    use super::*;
    use crate::wizard::DIFF_STAT_FIELDS;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn enter_edits_the_selected_field() {
        let mut s = FieldMenuState::new(&DIFF_STAT_FIELDS);
        assert!(matches!(
            s.on_key(key(KeyCode::Enter)),
            Flow::Edit(f) if f == DIFF_STAT_FIELDS[0]
        ));
        assert!(matches!(s.on_key(key(KeyCode::Down)), Flow::Continue));
        assert!(matches!(
            s.on_key(key(KeyCode::Char('l'))),
            Flow::Edit(f) if f == DIFF_STAT_FIELDS[1]
        ));
    }

    #[test]
    fn selection_is_clamped_to_the_field_list() {
        let mut s = FieldMenuState::new(&DIFF_STAT_FIELDS);
        s.on_key(key(KeyCode::Up));
        assert_eq!(s.list.selected(), Some(0));
        for _ in 0..DIFF_STAT_FIELDS.len() + 3 {
            s.on_key(key(KeyCode::Char('j')));
        }
        assert_eq!(s.list.selected(), Some(DIFF_STAT_FIELDS.len() - 1));
    }

    #[test]
    fn esc_left_h_and_ctrl_c_go_back() {
        let mut s = FieldMenuState::new(&DIFF_STAT_FIELDS);
        for code in [KeyCode::Esc, KeyCode::Left, KeyCode::Char('h')] {
            assert!(matches!(s.on_key(key(code)), Flow::Back));
        }
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(matches!(s.on_key(ctrl_c), Flow::Back));
    }
}

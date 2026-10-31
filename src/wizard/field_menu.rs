//! 一組欄位收成一頁的子選單：主選單只留一列入口，點進來才逐項設定。
//! 欄位的編輯、存檔與主選單完全共用（`CycleField::cycle`／`open_editor`），
//! 這裡只負責瀏覽與進出。
//!
//! 按鍵語意與主選單對齊：→／l 切換循環欄位（或開子畫面），Enter 只開子畫面、
//! 對循環欄位不做事。與主選單不同的是 ←／h 永遠回上一層（主選單的 ←／h
//! 會反向切換），所以子選單裡循環欄位只能正向輪迴。

use ratatui::{
    crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind},
    layout::{Constraint, Layout, Rect},
    widgets::{ListItem, ListState, Paragraph},
    DefaultTerminal, Frame,
};
use rust_i18n::t;

use super::{
    clamped_move, editor_row_label, is_abort_key, open_editor, styled_list, Dialog, Draft, Editor,
    Group, ResolvedDefaults,
};
use crate::color::ColorTheme;

pub(super) enum Flow {
    Continue,
    Back,
    Open(Dialog),
}

pub(super) struct FieldMenuState {
    items: &'static [Editor],
    list: ListState,
}

impl FieldMenuState {
    pub(super) fn new(group: Group) -> Self {
        let mut list = ListState::default();
        list.select(Some(0));
        Self {
            items: group.items(),
            list,
        }
    }

    /// 游標直接停在第 `index` 列。給測試用，省得為了定位模擬一串方向鍵。
    #[cfg(test)]
    pub(super) fn at(group: Group, index: usize) -> Self {
        let mut s = Self::new(group);
        s.list.select(Some(index));
        s
    }

    /// 零 I/O（循環欄位的切換只改 `draft`）的狀態轉移，可以直接餵 `KeyEvent`
    /// 測試。Esc／←／h 與 Ctrl-C 都只是回上一層，不會放棄整個精靈。
    pub(super) fn on_key(
        &mut self,
        key: KeyEvent,
        draft: &mut Draft,
        defaults: &ResolvedDefaults,
    ) -> Flow {
        if key.kind != KeyEventKind::Press {
            return Flow::Continue;
        }
        if is_abort_key(&key) {
            return Flow::Back;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => Flow::Back,
            KeyCode::Up | KeyCode::Char('k') => {
                clamped_move(&mut self.list, -1, self.items.len());
                Flow::Continue
            }
            KeyCode::Down | KeyCode::Char('j') => {
                clamped_move(&mut self.list, 1, self.items.len());
                Flow::Continue
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if let Some(Editor::Cycle(field)) = self.selected() {
                    field.cycle(draft, defaults, 1);
                }
                self.activate()
            }
            KeyCode::Enter => self.activate(),
            _ => Flow::Continue,
        }
    }

    fn selected(&self) -> Option<Editor> {
        self.list.selected().map(|i| self.items[i])
    }

    /// 循環欄位在這裡是 no-op（切換已在 →／l 那一步做完）；子畫面則開起來。
    fn activate(&self) -> Flow {
        match self.selected() {
            Some(Editor::Dialog(d)) => Flow::Open(d),
            Some(Editor::Cycle(_)) | None => Flow::Continue,
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
            .items
            .iter()
            .map(|&editor| ListItem::new(editor_row_label(editor, draft, defaults)))
            .collect();
        f.render_stateful_widget(styled_list(items, theme), list_area, &mut self.list);

        let hint = crate::widget::hint_line(
            theme,
            &[
                ("↑↓/kj".into(), &*t!("wizard.hint.select")),
                ("→/l".into(), &*t!("wizard.hint.switch_option")),
                ("Enter".into(), &*t!("wizard.hint.edit")),
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
    group: Group,
    theme: &ColorTheme,
) -> crate::Result<()> {
    let mut state = FieldMenuState::new(group);
    loop {
        terminal.draw(|f| state.render(f, f.area(), draft, defaults, theme))?;
        let Event::Key(key) = ratatui::crossterm::event::read()? else {
            continue;
        };
        match state.on_key(key, draft, defaults) {
            Flow::Continue => {}
            Flow::Back => return Ok(()),
            Flow::Open(dialog) => open_editor(terminal, draft, defaults, dialog, theme)?,
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::KeyModifiers;

    use super::*;
    use crate::{
        config,
        wizard::{CycleField, NumberField},
    };

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn setup() -> (Draft, ResolvedDefaults) {
        (
            Draft::new(),
            ResolvedDefaults::from_core(&config::CoreConfig::default()),
        )
    }

    /// 開 `group` 子選單並把游標移到 `editor` 那一列。
    fn menu_at(group: Group, editor: Editor) -> FieldMenuState {
        let idx = group
            .items()
            .iter()
            .position(|e| *e == editor)
            .expect("該組裡找不到這個項目");
        FieldMenuState::at(group, idx)
    }

    #[test]
    fn enter_and_right_open_the_selected_dialog() {
        let (mut d, df) = setup();
        let mut s = FieldMenuState::new(Group::DiffStat);
        assert!(matches!(
            s.on_key(key(KeyCode::Enter), &mut d, &df),
            Flow::Open(Dialog::Number(f)) if f == NumberField::GithubDiffStatTarget
        ));
        assert!(matches!(
            s.on_key(key(KeyCode::Down), &mut d, &df),
            Flow::Continue
        ));
        assert!(matches!(
            s.on_key(key(KeyCode::Char('l')), &mut d, &df),
            Flow::Open(Dialog::Number(f)) if f == NumberField::GithubDiffStatGreen
        ));
    }

    #[test]
    fn github_group_opens_the_diff_stat_menu_as_a_third_level() {
        let (mut d, df) = setup();
        let mut s = FieldMenuState::new(Group::GitHub);
        assert!(matches!(
            s.on_key(key(KeyCode::Enter), &mut d, &df),
            Flow::Open(Dialog::Group(Group::DiffStat))
        ));
    }

    #[test]
    fn right_cycles_a_cycle_field_but_enter_does_not() {
        let (mut d, df) = setup();
        let order = Editor::Cycle(CycleField::Order);
        let mut s = menu_at(Group::Graph, order);

        assert!(matches!(
            s.on_key(key(KeyCode::Enter), &mut d, &df),
            Flow::Continue
        ));
        assert!(!d.is_touched(order), "Enter 對循環欄位是 no-op");

        assert!(matches!(
            s.on_key(key(KeyCode::Right), &mut d, &df),
            Flow::Continue
        ));
        assert!(d.is_touched(order), "→ 切換並標記為已改");
        assert!(editor_row_label(order, &d, &df).contains("< topo >"));
    }

    #[test]
    fn left_goes_back_even_on_a_cycle_row() {
        let (mut d, df) = setup();
        let order = Editor::Cycle(CycleField::Order);
        let mut s = menu_at(Group::Graph, order);
        assert!(matches!(
            s.on_key(key(KeyCode::Left), &mut d, &df),
            Flow::Back
        ));
        assert!(!d.is_touched(order), "← 不該動到值");
    }

    #[test]
    fn selection_is_clamped_to_the_item_list() {
        let (mut d, df) = setup();
        let mut s = FieldMenuState::new(Group::DiffStat);
        s.on_key(key(KeyCode::Up), &mut d, &df);
        assert_eq!(s.list.selected(), Some(0));
        let len = Group::DiffStat.items().len();
        for _ in 0..len + 3 {
            s.on_key(key(KeyCode::Char('j')), &mut d, &df);
        }
        assert_eq!(s.list.selected(), Some(len - 1));
    }

    #[test]
    fn esc_left_h_and_ctrl_c_go_back() {
        let (mut d, df) = setup();
        let mut s = FieldMenuState::new(Group::DiffStat);
        for code in [KeyCode::Esc, KeyCode::Left, KeyCode::Char('h')] {
            assert!(matches!(s.on_key(key(code), &mut d, &df), Flow::Back));
        }
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(matches!(s.on_key(ctrl_c, &mut d, &df), Flow::Back));
    }

    /// `YSGIT_NO_UPDATE_CHECK` 的提示掛在 `UpdateMode` 那一列本身，搬進子選單
    /// 後仍要顯示。
    #[test]
    fn update_mode_row_keeps_the_env_note_inside_the_group() {
        let (d, df) = setup();
        let row = Editor::Cycle(CycleField::UpdateMode);
        // 只讀環境變數，不寫：避免平行測試互相污染。
        let expected = std::env::var_os("YSGIT_NO_UPDATE_CHECK").is_some();
        assert_eq!(
            editor_row_label(row, &d, &df).contains("YSGIT_NO_UPDATE_CHECK"),
            expected
        );
    }
}

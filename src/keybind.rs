use std::ops::{Deref, DerefMut};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rustc_hash::FxHashMap;
use serde::{de::Deserializer, Deserialize};

use crate::event::UserEvent;

const DEFAULT_KEY_BIND: &str = include_str!("../assets/default-keybind.toml");

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct KeyBind {
    map: FxHashMap<KeyEvent, UserEvent>,
    /// 每個 event 在設定檔裡宣告的**第一個**鍵。狀態列提示只放得下一個鍵，
    /// 而 `keys_for_event` 的排序是 `KeyEvent` 的 derive `PartialOrd`（struct
    /// 欄位順序），跟「作者心中的主鍵」無關 —— 它會把 `navigate_down` 排成
    /// `Down` 在前、`page_down` 排成 `PageDown` 在前。設定檔的宣告順序才是
    /// 答案，而且使用者自己的 config.toml 順序也會自動生效。
    primary: FxHashMap<UserEvent, KeyEvent>,
}

impl Deref for KeyBind {
    type Target = FxHashMap<KeyEvent, UserEvent>;

    fn deref(&self) -> &Self::Target {
        &self.map
    }
}

impl DerefMut for KeyBind {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.map
    }
}

impl KeyBind {
    pub fn new(custom_keybind_patch: Option<KeyBind>) -> Self {
        let mut keybind: KeyBind =
            toml::from_str(DEFAULT_KEY_BIND).expect("default key bind should be correct");

        if let Some(custom_keybind_patch) = custom_keybind_patch {
            keybind.merge(custom_keybind_patch);
        }

        keybind
    }

    /// 套用使用者設定檔的覆寫。兩張表存在的理由不同（`map` 答「按下去是什麼
    /// 事件」、`primary` 答「這個事件的主鍵是哪個」），但合併演算法一樣：
    /// 同 key 後者覆寫前者，就是 `Extend` 的標準語意。
    fn merge(&mut self, patch: KeyBind) {
        self.map.extend(patch.map);
        self.primary.extend(patch.primary);
    }

    /// 狀態列提示要顯示的那一個鍵。未綁定、或字串化後為空（`key_event_to_string`
    /// 對 `Null`／`Media` 這類會回空字串）都回 `None`，呼叫端據此整組略過。
    pub fn display_key(&self, user_event: UserEvent) -> Option<String> {
        let s = match self.primary.get(&user_event) {
            Some(key) => key_event_to_string(*key),
            // primary 只在 deserialize 時填。走 `insert()` 直接塞進來的綁定
            // （測試、未來的動態綁定）沒有宣告順序可言，退回 `keys_for_event`
            // 既有的排序——不要在這裡重抄一份同樣的排序邏輯。
            None => self.keys_for_event(user_event).into_iter().next()?,
        };
        (!s.is_empty()).then_some(s)
    }

    pub fn keys_for_event(&self, user_event: UserEvent) -> Vec<String> {
        let mut key_events: Vec<KeyEvent> = self
            .iter()
            .filter(|(_, ue)| **ue == user_event)
            .map(|(ke, _)| *ke)
            .collect();
        key_events.sort_by(|a, b| a.partial_cmp(b).unwrap()); // 至少用在按鍵綁定上看起來沒問題……
        key_events.into_iter().map(key_event_to_string).collect()
    }

    pub fn user_command_event_numbers(&self) -> Vec<usize> {
        let mut numbers: Vec<usize> = self
            .values()
            .filter_map(|ue| {
                if let UserEvent::UserCommand(n) = ue {
                    Some(*n)
                } else {
                    None
                }
            })
            .collect();
        numbers.sort_unstable();
        numbers
    }
}

impl<'de> Deserialize<'de> for KeyBind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let parsed_map = FxHashMap::<UserEvent, Vec<String>>::deserialize(deserializer)?;
        let mut key_map = FxHashMap::<KeyEvent, UserEvent>::default();
        let mut primary = FxHashMap::<UserEvent, KeyEvent>::default();
        for (user_event, key_events) in parsed_map {
            for (i, key_event_str) in key_events.into_iter().enumerate() {
                let key_event = match parse_key_event(&key_event_str) {
                    Ok(e) => e,
                    Err(s) => {
                        let msg = format!("{key_event_str:?} is not a valid key event: {s:}");
                        return Err(serde::de::Error::custom(msg));
                    }
                };
                // 宣告順序的第一個就是主鍵，狀態列提示顯示它
                if i == 0 {
                    primary.insert(user_event, key_event);
                }
                if let Some(conflict_user_event) = key_map.insert(key_event, user_event) {
                    let msg = format!(
                        "{key_event:?} map to multiple events: {user_event:?}, {conflict_user_event:?}"
                    );
                    return Err(serde::de::Error::custom(msg));
                }
            }
        }

        Ok(KeyBind {
            map: key_map,
            primary,
        })
    }
}

fn parse_key_event(raw: &str) -> Result<KeyEvent, String> {
    let raw_lower = raw.to_ascii_lowercase().replace(' ', "");
    let (remaining, modifiers) = extract_modifiers(&raw_lower);
    parse_key_code_with_modifiers(remaining, modifiers)
}

fn extract_modifiers(raw: &str) -> (&str, KeyModifiers) {
    let mut modifiers = KeyModifiers::empty();
    let mut current = raw;

    loop {
        match current {
            rest if rest.starts_with("ctrl-") => {
                modifiers.insert(KeyModifiers::CONTROL);
                current = &rest[5..];
            }
            rest if rest.starts_with("alt-") => {
                modifiers.insert(KeyModifiers::ALT);
                current = &rest[4..];
            }
            rest if rest.starts_with("shift-") => {
                modifiers.insert(KeyModifiers::SHIFT);
                current = &rest[6..];
            }
            _ => break, // 沒有偵測到已知前綴時就跳出迴圈
        };
    }

    (current, modifiers)
}

fn parse_key_code_with_modifiers(
    raw: &str,
    mut modifiers: KeyModifiers,
) -> Result<KeyEvent, String> {
    let c = match raw {
        "esc" => KeyCode::Esc,
        "enter" => KeyCode::Enter,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "backtab" => {
            modifiers.insert(KeyModifiers::SHIFT);
            KeyCode::BackTab
        }
        "backspace" => KeyCode::Backspace,
        "delete" => KeyCode::Delete,
        "insert" => KeyCode::Insert,
        "f1" => KeyCode::F(1),
        "f2" => KeyCode::F(2),
        "f3" => KeyCode::F(3),
        "f4" => KeyCode::F(4),
        "f5" => KeyCode::F(5),
        "f6" => KeyCode::F(6),
        "f7" => KeyCode::F(7),
        "f8" => KeyCode::F(8),
        "f9" => KeyCode::F(9),
        "f10" => KeyCode::F(10),
        "f11" => KeyCode::F(11),
        "f12" => KeyCode::F(12),
        "space" => KeyCode::Char(' '),
        "hyphen" => KeyCode::Char('-'),
        "minus" => KeyCode::Char('-'),
        "tab" => KeyCode::Tab,
        // 用 `chars().count()`，不是 `len()`：後者是 UTF-8 位元組長度，導致每個
        // 非 ASCII 按鍵（注音 `ㄅ` 是 3 bytes、西里爾字母 `й` 是 2 bytes）都會
        // 落到錯誤分支，使整份設定檔載入失敗。終端機本來就是把這些字元當成
        // 一般的 `KeyCode::Char` 送出，從來就沒有理由要拒絕它們。
        c if c.chars().count() == 1 => {
            let mut c = c.chars().next().unwrap();
            if modifiers.contains(KeyModifiers::SHIFT) {
                c = c.to_ascii_uppercase();
            }
            KeyCode::Char(c)
        }
        _ => return Err(format!("Unable to parse {raw}")),
    };
    Ok(KeyEvent::new(c, modifiers))
}

fn key_event_to_string(key_event: KeyEvent) -> String {
    if let KeyCode::Char(c) = key_event.code {
        if key_event.modifiers == KeyModifiers::SHIFT {
            return c.to_ascii_uppercase().into();
        }
    }

    let char;
    let key_code = match key_event.code {
        KeyCode::Backspace => "Backspace",
        KeyCode::Enter => "Enter",
        KeyCode::Left => "Left",
        KeyCode::Right => "Right",
        KeyCode::Up => "Up",
        KeyCode::Down => "Down",
        KeyCode::Home => "Home",
        KeyCode::End => "End",
        KeyCode::PageUp => "PageUp",
        KeyCode::PageDown => "PageDown",
        KeyCode::Tab => "Tab",
        KeyCode::BackTab => "BackTab",
        KeyCode::Delete => "Delete",
        KeyCode::Insert => "Insert",
        KeyCode::F(n) => {
            char = format!("F{n}");
            &char
        }
        KeyCode::Char(' ') => "Space",
        KeyCode::Char(c) => {
            char = c.to_string();
            &char
        }
        KeyCode::Esc => "Esc",
        KeyCode::Null => "",
        KeyCode::CapsLock => "",
        KeyCode::Menu => "",
        KeyCode::ScrollLock => "",
        KeyCode::Media(_) => "",
        KeyCode::NumLock => "",
        KeyCode::PrintScreen => "",
        KeyCode::Pause => "",
        KeyCode::KeypadBegin => "",
        KeyCode::Modifier(_) => "",
    };

    let mut modifiers = Vec::with_capacity(3);

    if key_event.modifiers.intersects(KeyModifiers::CONTROL) {
        modifiers.push("Ctrl");
    }

    if key_event.modifiers.intersects(KeyModifiers::SHIFT) {
        modifiers.push("Shift");
    }

    if key_event.modifiers.intersects(KeyModifiers::ALT) {
        modifiers.push("Alt");
    }

    let mut key = modifiers.join("-");

    if !key.is_empty() {
        key.push('-');
    }
    key.push_str(key_code);

    key
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 預設 TOML 由 `KeyBind::new()` 以 `.expect()` 解析 — 未知的 event 名稱或
    /// 同一按鍵綁到兩個 event，都只會在啟動時 panic。這裡把它拉進 CI。
    #[test]
    fn default_keybind_parses() {
        let keybind = KeyBind::new(None);
        assert_eq!(
            keybind.get(&KeyEvent::new(KeyCode::Char('P'), KeyModifiers::SHIFT)),
            Some(&UserEvent::TogglePrDraft),
        );
    }

    #[rustfmt::skip]
    #[test]
    fn test_deserialize_keybind() {
        let toml = r#"
            navigate_up = ["k"]
            navigate_down = ["j", "down"]
            navigate_left = ["ctrl-h", "shift-h", "alt-h"]
            navigate_right = ["ctrl-shift-l", "alt-shift-ctrl-l"]
            quit = ["esc", "f12"]
            user_command_1 = ["d"]
            user_command_view_toggle_10 = ["e"]
        "#;

        // 這個測試驗的是「按鍵字串怎麼解析成 KeyEvent」，所以只比對 key→event
        // 這張表；宣告順序推出來的 `primary` 由下面的 display_key 測試涵蓋。
        let expected: FxHashMap<KeyEvent, UserEvent> = [
                (
                    KeyEvent::new(KeyCode::Char('k'), KeyModifiers::empty()),
                    UserEvent::NavigateUp,
                ),
                (
                    KeyEvent::new(KeyCode::Char('j'), KeyModifiers::empty()),
                    UserEvent::NavigateDown,
                ),
                (
                    KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
                    UserEvent::NavigateDown,
                ),
                (
                    KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL),
                    UserEvent::NavigateLeft,
                ),
                (
                    KeyEvent::new(KeyCode::Char('h'), KeyModifiers::SHIFT),
                    UserEvent::NavigateLeft,
                ),
                (
                    KeyEvent::new(KeyCode::Char('h'), KeyModifiers::ALT),
                    UserEvent::NavigateLeft,
                ),
                (
                    KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL | KeyModifiers::SHIFT),
                    UserEvent::NavigateRight,
                ),
                (
                    KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL | KeyModifiers::SHIFT | KeyModifiers::ALT),
                    UserEvent::NavigateRight,
                ),
                (
                    KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
                    UserEvent::Quit,
                ),
                (
                    KeyEvent::new(KeyCode::F(12), KeyModifiers::empty()),
                    UserEvent::Quit,
                ),
                (
                    KeyEvent::new(KeyCode::Char('d'), KeyModifiers::empty()),
                    UserEvent::UserCommand(1),
                ),
                (
                    KeyEvent::new(KeyCode::Char('e'), KeyModifiers::empty()),
                    UserEvent::UserCommand(10),
                ),
            ]
            .into_iter()
            .collect();

        let actual: KeyBind = toml::from_str(toml).unwrap();

        assert_eq!(*actual, expected);
    }

    /// `display_key` 取的是**設定檔宣告順序的第一個鍵**，不是 `keys_for_event`
    /// 的排序結果。這四個案例正是兩者會分岔的地方 —— `KeyEvent` 的 derive
    /// `PartialOrd` 會把 `Down` 排在 `j` 前、`PageDown` 排在 `Ctrl-f` 前。
    #[test]
    fn display_key_follows_declaration_order_not_sort_order() {
        let keybind = KeyBind::new(None);

        assert_eq!(
            keybind.display_key(UserEvent::NavigateDown).as_deref(),
            Some("j")
        );
        assert_eq!(
            keybind.display_key(UserEvent::Cancel).as_deref(),
            Some("Esc")
        );
        assert_eq!(
            keybind.display_key(UserEvent::PageDown).as_deref(),
            Some("Ctrl-f")
        );
        assert_eq!(
            keybind.display_key(UserEvent::Confirm).as_deref(),
            Some("Enter")
        );

        // 對照組：同樣的 event 用 keys_for_event 拿到的第一個是不一樣的東西
        assert_eq!(
            keybind
                .keys_for_event(UserEvent::NavigateDown)
                .first()
                .unwrap(),
            "Down"
        );
        assert_eq!(
            keybind.keys_for_event(UserEvent::PageDown).first().unwrap(),
            "PageDown"
        );
    }

    #[test]
    fn display_key_returns_none_for_unbound_event() {
        // e 鍵已移除，預設沒有任何 user_command 綁定
        let keybind = KeyBind::new(None);
        assert_eq!(keybind.display_key(UserEvent::UserCommand(1)), None);
    }

    #[test]
    fn display_key_uses_user_declaration_order_after_patch() {
        let patch: KeyBind = toml::from_str(r#"cancel = ["q", "esc"]"#).unwrap();
        let keybind = KeyBind::new(Some(patch));

        // 使用者把 q 宣告在前，提示就顯示 q（不是預設的 Esc）
        assert_eq!(keybind.display_key(UserEvent::Cancel).as_deref(), Some("q"));
    }

    /// 非 ASCII 按鍵過去會在啟動時炸掉整份設定檔，因為單字元那條分支測的是
    /// `str::len()`（UTF-8 位元組數）而不是字元數。注音是促成這項修正的
    /// 案例：不支援 IMKit 組字區的注音輸入法會直接把 `ㄜ` 而不是 `k` 送進
    /// 終端機，所以要在不先切換輸入模式的情況下操作這個 app，唯一辦法就是
    /// 綁定注音鍵。
    #[test]
    fn parse_key_event_accepts_non_ascii_chars() {
        for raw in ["ㄜ", "й", "é"] {
            let expected = KeyEvent::new(
                KeyCode::Char(raw.chars().next().unwrap()),
                KeyModifiers::empty(),
            );
            assert_eq!(parse_key_event(raw), Ok(expected), "{raw}");
        }

        assert_eq!(
            parse_key_event("alt-ㄜ"),
            Ok(KeyEvent::new(KeyCode::Char('ㄜ'), KeyModifiers::ALT)),
        );

        // 真正的多字元亂碼仍然會被拒絕。
        assert!(parse_key_event("ㄜㄨ").is_err());
    }

    #[rustfmt::skip]
    #[test]
    fn test_key_event_to_string() {
        let key_event = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::empty());
        assert_eq!(key_event_to_string(key_event), "k");

        let key_event = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::empty());
        assert_eq!(key_event_to_string(key_event), "j");

        let key_event = KeyEvent::new(KeyCode::Down, KeyModifiers::empty());
        assert_eq!(key_event_to_string(key_event), "Down");

        let key_event = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL);
        assert_eq!(key_event_to_string(key_event), "Ctrl-h");

        let key_event = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::SHIFT);
        assert_eq!(key_event_to_string(key_event), "H");

        let key_event = KeyEvent::new(KeyCode::Char('H'), KeyModifiers::SHIFT);
        assert_eq!(key_event_to_string(key_event), "H");

        let key_event = KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT);
        assert_eq!(key_event_to_string(key_event), "Shift-Left");

        let key_event = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::ALT);
        assert_eq!(key_event_to_string(key_event), "Alt-h");

        let key_event = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        assert_eq!(key_event_to_string(key_event), "Ctrl-Shift-l");

        let key_event = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL | KeyModifiers::SHIFT | KeyModifiers::ALT);
        assert_eq!(key_event_to_string(key_event), "Ctrl-Shift-Alt-l");

        let key_event = KeyEvent::new(KeyCode::Esc, KeyModifiers::empty());
        assert_eq!(key_event_to_string(key_event), "Esc");

        let key_event = KeyEvent::new(KeyCode::F(12), KeyModifiers::empty());
        assert_eq!(key_event_to_string(key_event), "F12");
    }
}

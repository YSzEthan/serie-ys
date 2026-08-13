use std::{fmt, ops::Deref};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rustc_hash::FxHashMap;
use serde::{
    de::{Deserializer, MapAccess, Visitor},
    Deserialize,
};

use crate::event::UserEvent;

const DEFAULT_KEY_BIND: &str = include_str!("../assets/default-keybind.toml");

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct KeyBind {
    /// 唯一真值。宣告順序的 (event, keys)；`keys[0]` 是主鍵（狀態列提示用），
    /// 空 Vec = 設定檔寫了 `[]`，明確停用。`PartialEq` 因此是順序敏感的——
    /// 目前沒有呼叫端真的比較兩個 `KeyBind`，只是提醒未來的人。
    bindings: Vec<(UserEvent, Vec<KeyEvent>)>,
    /// 由 `bindings` 導出的查表快取，只在 `rebuild()` 一處寫入。熱路徑
    /// （每一次按鍵）走這裡，維持 O(1)；`bindings` 只在 wizard、help、docs
    /// 產生器這些非熱路徑被線性掃描。
    map: FxHashMap<KeyEvent, UserEvent>,
}

impl Deref for KeyBind {
    type Target = FxHashMap<KeyEvent, UserEvent>;

    fn deref(&self) -> &Self::Target {
        &self.map
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

    /// 套用使用者設定檔的覆寫：patch 裡宣告的每個 action，用 [`assign`] 逐一
    /// 指派——語意是取代，不是追加。這不是新規則，是今天 `map.extend()` 就
    /// 已經有的行為（`map` 以 `KeyEvent` 為鍵，patch 蓋過預設）搬到新結構
    /// 上，讓它對「一個鍵從此不再屬於原本的 action」這件事誠實。
    ///
    /// [`assign`]: Self::assign
    fn merge(&mut self, patch: KeyBind) {
        for (event, keys) in patch.bindings {
            self.assign(event, keys);
        }
    }

    /// 唯一的寫入點：把 `keys` 指派給 `event`，取代它原本的全部綁定。
    ///
    /// 先把 `keys` 從其他任何 action 身上拿掉，再指派——不做這一步，
    /// `bindings` 內部會自相矛盾（同一顆鍵同時屬於兩個 action），
    /// `rebuild()` 的結果就要看 Vec 順序決定，而不是「最後指派的人算數」。
    /// 做了這一步之後，`map[k] == e ⟺ k ∈ bindings 裡 e 那條 Vec` 這條
    /// 不變式恆成立，無法表示違反它的狀態。
    pub fn assign(&mut self, event: UserEvent, keys: Vec<KeyEvent>) {
        for (e, existing) in self.bindings.iter_mut() {
            if *e != event {
                existing.retain(|k| !keys.contains(k));
            }
        }
        match self.bindings.iter_mut().find(|(e, _)| *e == event) {
            Some(entry) => entry.1 = keys,
            None => self.bindings.push((event, keys)),
        }
        self.rebuild();
    }

    /// 把 `event` 的宣告整條移除——回到「這個 action 沒有被使用者設定過」
    /// 的狀態，跟 `assign(event, vec![])`（明確停用）是兩件不同的事。
    pub fn unset(&mut self, event: UserEvent) {
        self.bindings.retain(|(e, _)| *e != event);
        self.rebuild();
    }

    fn rebuild(&mut self) {
        self.map = self
            .bindings
            .iter()
            .flat_map(|(e, keys)| keys.iter().map(move |k| (*k, *e)))
            .collect();
    }

    /// 狀態列提示要顯示的那一個鍵：宣告順序的第一個。未綁定、或字串化後為
    /// 空（`key_event_to_string` 對 `Null`／`Media` 這類會回空字串）都回
    /// `None`，呼叫端據此整組略過。
    pub fn display_key(&self, user_event: UserEvent) -> Option<String> {
        let key = *self.key_events(user_event).first()?;
        let s = key_event_to_string(key);
        (!s.is_empty()).then_some(s)
    }

    /// `event` 目前綁定的全部鍵，宣告順序（`[0]` 是主鍵）。未綁定回空 slice。
    pub fn key_events(&self, user_event: UserEvent) -> &[KeyEvent] {
        self.bindings
            .iter()
            .find(|(e, _)| *e == user_event)
            .map_or(&[], |(_, keys)| keys.as_slice())
    }

    pub fn keys_for_event(&self, user_event: UserEvent) -> Vec<String> {
        let mut key_events: Vec<KeyEvent> = self.key_events(user_event).to_vec();
        key_events.sort_by(|a, b| a.partial_cmp(b).unwrap()); // 至少用在按鍵綁定上看起來沒問題……
        key_events.into_iter().map(key_event_to_string).collect()
    }

    /// 全部 action，宣告順序（＝ `assets/default-keybind.toml` 的檔案順序，
    /// 該檔案照 `UserEvent` 的宣告順序排列）。wizard 的動作清單直接用這個
    /// 順序，不必另外手抄一份 `UserEvent` 清單。
    ///
    /// 這個順序仰賴 `Cargo.toml` 的 `toml = { features = ["preserve_order"] }`
    /// ——`toml` crate 預設把整份文件的表格鍵收進 `BTreeMap`，反序列化時會
    /// 照字母排序把它們餵給 `Deserialize`，跟檔案本身的行順序無關（TOML
    /// **陣列**不受影響，`bindings` 裡每個 event 自己的 `Vec<KeyEvent>`
    /// 一路都是照檔案排的）。拿掉這個 feature，這個函式的回傳順序會悄悄
    /// 變成字母序，wizard 的動作清單也會跟著變，且不會有任何編譯期或
    /// 執行期錯誤——這正是 `every_user_event_has_a_default_binding_entry`
    /// 之外還需要一條「順序穩定」測試的原因。
    pub fn bindings(&self) -> &[(UserEvent, Vec<KeyEvent>)] {
        &self.bindings
    }

    pub fn user_command_event_numbers(&self) -> Vec<usize> {
        let mut numbers: Vec<usize> = self
            .bindings
            .iter()
            .filter(|(_, keys)| !keys.is_empty())
            .filter_map(|(ue, _)| match ue {
                UserEvent::UserCommand(n) => Some(*n),
                _ => None,
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
        struct KeyBindVisitor;

        impl<'de> Visitor<'de> for KeyBindVisitor {
            type Value = KeyBind;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a map of user event to an array of key strings")
            }

            fn visit_map<A>(self, mut map: A) -> Result<KeyBind, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut bindings: Vec<(UserEvent, Vec<KeyEvent>)> = Vec::new();
                // 只用來偵測衝突，真值是 `bindings`。
                let mut owner: FxHashMap<KeyEvent, UserEvent> = FxHashMap::default();

                while let Some((user_event, key_strings)) =
                    map.next_entry::<UserEvent, Vec<String>>()?
                {
                    // 同一份檔案裡兩個別名（`ref_list`／`ref_list_toggle`、
                    // `user_command_3`／`user_command_view_toggle_3`）指到
                    // 同一個 event：允許的話 `bindings` 就有重複 entry，
                    // `assign()` 只會找到第一筆、`keys_for_event` 會漏掉
                    // 第二筆的鍵，不變式當場破功，所以在這裡直接拒絕。
                    if bindings.iter().any(|(e, _)| *e == user_event) {
                        let msg = format!(
                            "{user_event:?} is declared more than once in this file (check for alias action names that map to the same event)"
                        );
                        return Err(serde::de::Error::custom(msg));
                    }

                    let mut key_events = Vec::with_capacity(key_strings.len());
                    for key_event_str in key_strings {
                        let key_event = match parse_key_event(&key_event_str) {
                            Ok(e) => e,
                            Err(s) => {
                                let msg =
                                    format!("{key_event_str:?} is not a valid key event: {s:}");
                                return Err(serde::de::Error::custom(msg));
                            }
                        };
                        if let Some(conflict_user_event) = owner.insert(key_event, user_event) {
                            let msg = format!(
                                "{key_event:?} map to multiple events: {user_event:?}, {conflict_user_event:?}"
                            );
                            return Err(serde::de::Error::custom(msg));
                        }
                        key_events.push(key_event);
                    }
                    bindings.push((user_event, key_events));
                }

                let mut keybind = KeyBind {
                    bindings,
                    map: FxHashMap::default(),
                };
                keybind.rebuild();
                Ok(keybind)
            }
        }

        deserializer.deserialize_map(KeyBindVisitor)
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

/// `key_event_to_string` 的反向，但寫的是**設定檔格式**（小寫 `ctrl-n`），
/// 不是顯示格式（`Ctrl-n`、`G`）。這個函式唯一的規格是：產生的字串能被
/// `parse_key_event` 讀回同一個 `KeyEvent`，否則回 `None`——與其為
/// `Media`、`Modifier`、無 SHIFT 的 `BackTab`、`SUPER`／`HYPER` 修飾鍵這些
/// 每一種不可表示的情況各寫一條特例，不如讓函式在最後自己驗一次；驗不過
/// 的鍵本來就是「綁了也不會生效」的鍵，回 `None` 才是誠實的。
pub fn key_event_to_config_string(key_event: KeyEvent) -> Option<String> {
    if key_event.code == KeyCode::Char(' ') {
        // `parse_key_event` 第一行 `.replace(' ', "")` 會把字面空白吃成空
        // 字串，而 `checkout = ["space"]` 是預設綁定，這是硬失敗，必須有
        // 專用名稱。
        return round_trip_checked(key_event, "space".to_string());
    }

    if let KeyCode::Char(c) = key_event.code {
        if c.is_ascii_uppercase() || key_event.modifiers.contains(KeyModifiers::SHIFT) {
            // `parse_key_event` 用 `to_ascii_lowercase()` 剝大小寫，這裡跟它
            // 對稱；`c.to_ascii_lowercase()` 對非 ASCII 是 no-op，所以非
            // ASCII 大寫字元（例如 `Ä`）不會被這條分支誤殺，交給最後的
            // round-trip 檢查裁決。
            let s = format!("shift-{}", c.to_ascii_lowercase());
            return round_trip_checked(key_event, s);
        }
    }

    let char_buf;
    let name = match key_event.code {
        KeyCode::Esc => "esc",
        KeyCode::Enter => "enter",
        KeyCode::Left => "left",
        KeyCode::Right => "right",
        KeyCode::Up => "up",
        KeyCode::Down => "down",
        KeyCode::Home => "home",
        KeyCode::End => "end",
        KeyCode::PageUp => "pageup",
        KeyCode::PageDown => "pagedown",
        KeyCode::Backspace => "backspace",
        KeyCode::Delete => "delete",
        KeyCode::Insert => "insert",
        KeyCode::Tab => "tab",
        KeyCode::BackTab => "backtab",
        KeyCode::F(n) => {
            char_buf = format!("f{n}");
            &char_buf
        }
        KeyCode::Char('-') => "hyphen",
        KeyCode::Char(c) => {
            char_buf = c.to_string();
            &char_buf
        }
        _ => return None,
    };

    let mut modifiers = Vec::with_capacity(3);
    if key_event.modifiers.contains(KeyModifiers::CONTROL) {
        modifiers.push("ctrl");
    }
    if key_event.modifiers.contains(KeyModifiers::ALT) {
        modifiers.push("alt");
    }
    if key_event.modifiers.contains(KeyModifiers::SHIFT) {
        modifiers.push("shift");
    }
    modifiers.push(name);

    round_trip_checked(key_event, modifiers.join("-"))
}

/// `KeyEvent` 的 `PartialEq` 本身就是這裡要的判準：它在比較前先各自跑一次
/// `normalize_case`（大寫字元 ⟺ 有 SHIFT），跟執行期 `map.get(&key)` 查表
/// 用的是同一套等價關係——不必在這裡另外重新推理一次「這兩個算不算同一顆
/// 鍵」。
fn round_trip_checked(key_event: KeyEvent, s: String) -> Option<String> {
    let parsed = parse_key_event(&s).ok()?;
    (parsed == key_event).then_some(s)
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
        // 這張表；宣告順序推出來的主鍵順序由下面的 display_key 測試涵蓋。
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

    // -----------------------------------------------------------------
    // 取代語意 / `[]` 停用 / 搶鍵
    // -----------------------------------------------------------------

    #[test]
    fn patch_replaces_instead_of_appending() {
        let patch: KeyBind = toml::from_str(r#"navigate_down = ["ctrl-n"]"#).unwrap();
        let keybind = KeyBind::new(Some(patch));

        assert_eq!(
            keybind.get(&KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)),
            Some(&UserEvent::NavigateDown),
        );
        // 預設的 j／down 不再生效
        assert_eq!(
            keybind.get(&KeyEvent::new(KeyCode::Char('j'), KeyModifiers::empty())),
            None,
        );
        assert_eq!(
            keybind.get(&KeyEvent::new(KeyCode::Down, KeyModifiers::empty())),
            None
        );
    }

    #[test]
    fn empty_array_disables_the_action() {
        let patch: KeyBind = toml::from_str(r#"refresh = []"#).unwrap();
        let keybind = KeyBind::new(Some(patch));

        assert_eq!(
            keybind.keys_for_event(UserEvent::Refresh),
            Vec::<String>::new()
        );
        assert_eq!(
            keybind.get(&KeyEvent::new(KeyCode::Char('r'), KeyModifiers::empty())),
            None,
        );
    }

    /// `r` 是 `refresh` 的預設鍵。使用者把 `r` 搶去給 `quit`，`refresh` 的
    /// 宣告要跟著失去這顆鍵——不能停在「`map[r]` 換人了，但 `refresh` 自己
    /// 的 `bindings` entry 沒被通知」這種兩份狀態互相矛盾的半殘狀態。
    #[test]
    fn patch_steals_a_key_from_its_previous_owner() {
        let patch: KeyBind = toml::from_str(r#"quit = ["r"]"#).unwrap();
        let keybind = KeyBind::new(Some(patch));

        assert_eq!(
            keybind.get(&KeyEvent::new(KeyCode::Char('r'), KeyModifiers::empty())),
            Some(&UserEvent::Quit),
        );
        assert!(!keybind
            .keys_for_event(UserEvent::Refresh)
            .contains(&"r".to_string()));
        assert_eq!(keybind.display_key(UserEvent::Refresh), None);
    }

    /// 任意一連串 `assign`／`unset` 之後，從 `bindings` 重算的 `map` 必須
    /// 等於當前的 `map`——`rebuild()` 是唯一寫入點這件事本身不是型別保證的，
    /// 這條測試補上。
    #[test]
    fn rebuild_invariant_holds_after_arbitrary_assignments() {
        let mut keybind = KeyBind::new(None);
        keybind.assign(
            UserEvent::Quit,
            vec![KeyEvent::new(KeyCode::Char('r'), KeyModifiers::empty())],
        );
        keybind.assign(UserEvent::HelpToggle, vec![]);
        keybind.unset(UserEvent::Cancel);
        keybind.assign(
            UserEvent::Cancel,
            vec![KeyEvent::new(KeyCode::Esc, KeyModifiers::empty())],
        );

        let mut recomputed = keybind.clone();
        recomputed.rebuild();
        assert_eq!(recomputed.map, keybind.map);
    }

    #[test]
    fn duplicate_event_aliases_in_one_file_are_rejected() {
        let toml = r#"
            ref_list = ["tab"]
            ref_list_toggle = ["r"]
        "#;
        assert!(toml::from_str::<KeyBind>(toml).is_err());
    }

    #[test]
    fn every_user_event_has_a_default_binding_entry() {
        let keybind = KeyBind::new(None);
        let event_names = declared_user_event_names();
        assert!(!event_names.is_empty());

        for name in &event_names {
            let found = keybind
                .bindings()
                .iter()
                .any(|(e, _)| format!("{e:?}") == *name);
            assert!(found, "UserEvent::{name} 沒有出現在 default-keybind.toml");
        }
    }

    /// `bindings()` 的順序必須是 `UserEvent` 的宣告順序——這條測試釘住的
    /// 不是邏輯，是 `Cargo.toml` 裡 `toml = { features = ["preserve_order"] }`
    /// 這個容易被無感拿掉的開關：拿掉它，`toml::from_str` 反序列化整份
    /// 表格時會照字母排序餵給 `Deserialize`，這個函式的回傳順序會悄悄
    /// 變成字母序，wizard 的動作清單會跟著錯，但不會有任何編譯期或執行期
    /// 錯誤——只有這條測試會紅。
    #[test]
    fn bindings_order_matches_user_event_declaration_order() {
        let keybind = KeyBind::new(None);
        let expected = declared_user_event_names();
        let actual: Vec<String> = keybind
            .bindings()
            .iter()
            .map(|(e, _)| format!("{e:?}"))
            .collect();
        assert_eq!(actual, expected);
    }

    /// 掃 `src/event.rs` 的 `UserEvent` enum 原始碼，取宣告順序的變體名稱
    /// （不含 `UserCommand`／`Unknown`，兩者不在 `assets/default-keybind.toml`
    /// 裡）。跟 `src/view/help.rs` 的一致性測試同一個手法：這是近似，不是
    /// 型別保證，但比另外手抄一份清單更不容易漂移。
    fn declared_user_event_names() -> Vec<String> {
        include_str!("event.rs")
            .lines()
            .skip_while(|l| !l.trim_start().starts_with("pub enum UserEvent"))
            .skip(1)
            .take_while(|l| !l.trim_start().starts_with('}'))
            .filter_map(|l| {
                let name = l.trim().trim_end_matches(',').trim_end_matches("(usize)");
                (!name.is_empty() && name != "Unknown" && name != "UserCommand")
                    .then(|| name.to_string())
            })
            .collect()
    }

    #[test]
    fn key_event_to_config_string_round_trips_every_default_key() {
        let keybind = KeyBind::new(None);
        for (event, keys) in keybind.bindings() {
            for key in keys {
                let s = key_event_to_config_string(*key)
                    .unwrap_or_else(|| panic!("{event:?} 的鍵 {key:?} 無法字串化"));
                let parsed =
                    parse_key_event(&s).unwrap_or_else(|e| panic!("{s:?} round-trip 失敗：{e}"));
                assert_eq!(parsed.code, key.code, "{s:?}");
                assert_eq!(parsed.modifiers, key.modifiers, "{s:?}");
            }
        }
    }

    #[rustfmt::skip]
    #[test]
    fn key_event_to_config_string_matches_expected_format() {
        let cases = [
            (KeyEvent::new(KeyCode::Char(' '), KeyModifiers::empty()), Some("space")),
            (KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL), Some("ctrl-n")),
            (KeyEvent::new(KeyCode::Char('J'), KeyModifiers::empty()), Some("shift-j")),
            (KeyEvent::new(KeyCode::Char('j'), KeyModifiers::SHIFT), Some("shift-j")),
            (KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()), Some("esc")),
            (KeyEvent::new(KeyCode::F(5), KeyModifiers::empty()), Some("f5")),
            (KeyEvent::new(KeyCode::Char('-'), KeyModifiers::empty()), Some("hyphen")),
            (KeyEvent::new(KeyCode::Null, KeyModifiers::empty()), None),
            (KeyEvent::new(KeyCode::Modifier(ratatui::crossterm::event::ModifierKeyCode::LeftSuper), KeyModifiers::SUPER), None),
        ];
        for (key, expected) in cases {
            assert_eq!(key_event_to_config_string(key).as_deref(), expected, "{key:?}");
        }
    }
}

use garde::Validate;
use ratatui::style::Color as RatatuiColor;
use serde::{Deserialize, Serialize};
use smart_default::SmartDefault;
use umbra::optional;

#[optional(derives = [Deserialize], visibility = pub)]
#[derive(Debug, Clone, Serialize, PartialEq, Eq, SmartDefault, Validate)]
#[garde(allow_unvalidated)]
pub struct ColorTheme {
    #[default(RatatuiColor::Reset)]
    pub fg: RatatuiColor,
    #[default(RatatuiColor::Reset)]
    pub bg: RatatuiColor,

    #[default(RatatuiColor::White)]
    pub list_selected_fg: RatatuiColor,
    #[default(RatatuiColor::DarkGray)]
    pub list_selected_bg: RatatuiColor,
    #[default(RatatuiColor::Yellow)]
    pub list_ref_paren_fg: RatatuiColor,
    #[default(RatatuiColor::Green)]
    pub list_ref_branch_fg: RatatuiColor,
    #[default(RatatuiColor::Red)]
    pub list_ref_remote_branch_fg: RatatuiColor,
    #[default(RatatuiColor::Yellow)]
    pub list_ref_tag_fg: RatatuiColor,
    #[default(RatatuiColor::Magenta)]
    pub list_ref_stash_fg: RatatuiColor,
    #[default(RatatuiColor::Cyan)]
    pub list_head_fg: RatatuiColor,
    #[default(RatatuiColor::Reset)]
    pub list_subject_fg: RatatuiColor,
    #[default(RatatuiColor::Cyan)]
    pub list_name_fg: RatatuiColor,
    #[default(RatatuiColor::Yellow)]
    pub list_hash_fg: RatatuiColor,
    #[default(RatatuiColor::Magenta)]
    pub list_date_fg: RatatuiColor,
    #[default(RatatuiColor::Black)]
    pub list_match_fg: RatatuiColor,
    #[default(RatatuiColor::Yellow)]
    pub list_match_bg: RatatuiColor,

    #[default(RatatuiColor::Reset)]
    pub detail_label_fg: RatatuiColor,
    #[default(RatatuiColor::Reset)]
    pub detail_name_fg: RatatuiColor,
    #[default(RatatuiColor::Reset)]
    pub detail_date_fg: RatatuiColor,
    #[default(RatatuiColor::Blue)]
    pub detail_email_fg: RatatuiColor,
    #[default(RatatuiColor::Reset)]
    pub detail_hash_fg: RatatuiColor,
    #[default(RatatuiColor::Green)]
    pub detail_ref_branch_fg: RatatuiColor,
    #[default(RatatuiColor::Red)]
    pub detail_ref_remote_branch_fg: RatatuiColor,
    #[default(RatatuiColor::Yellow)]
    pub detail_ref_tag_fg: RatatuiColor,
    #[default(RatatuiColor::Green)]
    pub detail_file_change_add_fg: RatatuiColor,
    #[default(RatatuiColor::Yellow)]
    pub detail_file_change_modify_fg: RatatuiColor,
    #[default(RatatuiColor::Red)]
    pub detail_file_change_delete_fg: RatatuiColor,
    #[default(RatatuiColor::Magenta)]
    pub detail_file_change_move_fg: RatatuiColor,

    /// ratatui 沒有具名的 orange，208 是 xterm 256 色盤裡的 DarkOrange——
    /// 用索引值而不是絕對 RGB，任何 256 色終端都能正確顯示，不需要
    /// truecolor 支援。
    #[default(RatatuiColor::Indexed(208))]
    pub diff_title_path_fg: RatatuiColor,
    #[default(RatatuiColor::Cyan)]
    pub diff_title_hunk_fg: RatatuiColor,

    #[default(RatatuiColor::White)]
    pub ref_selected_fg: RatatuiColor,
    #[default(RatatuiColor::DarkGray)]
    pub ref_selected_bg: RatatuiColor,

    #[default(RatatuiColor::Green)]
    pub help_block_title_fg: RatatuiColor,
    #[default(RatatuiColor::Yellow)]
    pub help_key_fg: RatatuiColor,

    #[default(RatatuiColor::Reset)]
    pub virtual_cursor_fg: RatatuiColor,
    #[default(RatatuiColor::Reset)]
    pub status_input_fg: RatatuiColor,
    #[default(RatatuiColor::DarkGray)]
    pub status_input_transient_fg: RatatuiColor,
    #[default(RatatuiColor::Yellow)]
    pub status_interactive_fg: RatatuiColor,
    #[default(RatatuiColor::Cyan)]
    pub status_info_fg: RatatuiColor,
    #[default(RatatuiColor::Green)]
    pub status_success_fg: RatatuiColor,
    #[default(RatatuiColor::Yellow)]
    pub status_warn_fg: RatatuiColor,
    #[default(RatatuiColor::Red)]
    pub status_error_fg: RatatuiColor,

    #[default(RatatuiColor::DarkGray)]
    pub divider_fg: RatatuiColor,

    #[garde(dive)]
    #[nested]
    pub graph: GraphColors,
}

/// Commit 圖形依序輪流套用的顏色。原本是獨立的 `[graph.color]` 區塊，
/// 跟 `[color]` 分家；搬進 `ColorTheme` 讓它們共用同一份驗證與遷移邏輯，
/// `[color.graph]` 在設定檔裡也就跟其他介面顏色放在一起。
#[optional(derives = [Deserialize])]
#[derive(Debug, Clone, Serialize, PartialEq, Eq, SmartDefault, Validate)]
pub struct GraphColors {
    #[garde(length(min = 1), inner(pattern(r"^#([0-9a-fA-F]{6}|[0-9a-fA-F]{8})$")))]
    #[default(vec![
        "#E06C76".into(),
        "#98C379".into(),
        "#E5C07B".into(),
        "#61AFEF".into(),
        "#C678DD".into(),
        "#56B6C2".into(),
    ])]
    pub branches: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphColor {
    r: u8,
    g: u8,
    b: u8,
}

impl GraphColor {
    pub fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    pub fn to_ratatui_color(self) -> RatatuiColor {
        RatatuiColor::Rgb(self.r, self.g, self.b)
    }
}

#[derive(Debug, Clone)]
pub struct GraphColorSet {
    pub colors: Vec<GraphColor>,
}

impl GraphColorSet {
    pub fn new(config: &GraphColors) -> Self {
        let colors = config
            .branches
            .iter()
            .filter_map(|s| parse_rgba_color(s))
            .collect();

        Self { colors }
    }

    pub fn get(&self, index: usize) -> GraphColor {
        self.colors[index % self.colors.len()]
    }
}

pub(crate) fn parse_rgba_color(s: &str) -> Option<GraphColor> {
    if !s.starts_with('#') {
        return None;
    }

    let s = &s[1..];
    let l = s.len();
    if l != 6 && l != 8 {
        return None;
    }

    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    if l == 8 {
        // alpha 仍會做十六進位驗證，所以格式錯誤的 `#RRGGBBZZ` 照樣會被拒絕，
        // 但值本身會被丟棄 —— 自從唯一會用到 alpha 的 PNG 渲染器被移除後，
        // GraphColor 就只剩 RGB。
        u8::from_str_radix(&s[6..8], 16).ok()?;
    }
    Some(GraphColor::from_rgb(r, g, b))
}

/// 43 個平面色鍵，順序＝範本 `[color]` 區塊的出現順序。使用者在 wizard 裡
/// 看到的排序，就是他打開設定檔看到的排序（`color_keys_match_the_asset_in_order`
/// 釘住）。
pub const COLOR_KEYS: &[&str] = &[
    "fg",
    "bg",
    "list_selected_fg",
    "list_selected_bg",
    "list_ref_paren_fg",
    "list_ref_branch_fg",
    "list_ref_remote_branch_fg",
    "list_ref_tag_fg",
    "list_ref_stash_fg",
    "list_head_fg",
    "list_subject_fg",
    "list_name_fg",
    "list_hash_fg",
    "list_date_fg",
    "list_match_fg",
    "list_match_bg",
    "detail_label_fg",
    "detail_name_fg",
    "detail_date_fg",
    "detail_email_fg",
    "detail_hash_fg",
    "detail_ref_branch_fg",
    "detail_ref_remote_branch_fg",
    "detail_ref_tag_fg",
    "detail_file_change_add_fg",
    "detail_file_change_modify_fg",
    "detail_file_change_delete_fg",
    "detail_file_change_move_fg",
    "diff_title_path_fg",
    "diff_title_hunk_fg",
    "ref_selected_fg",
    "ref_selected_bg",
    "help_block_title_fg",
    "help_key_fg",
    "virtual_cursor_fg",
    "status_input_fg",
    "status_input_transient_fg",
    "status_interactive_fg",
    "status_info_fg",
    "status_success_fg",
    "status_warn_fg",
    "status_error_fg",
    "divider_fg",
];

/// `ColorTheme` 攤平成有序陣列，索引與 `COLOR_KEYS` 對齊。這對轉換是彼此的
/// 反函式，各自跑一次完整的 serde 來回 —— wizard 的顏色編輯器全程用索引
/// 存取，不會每畫一格色塊就轉一次（43 列清單每幀都要顯示現值，逐鍵轉換
/// 就是 43 次/幀）。
///
/// `graph` 一起帶著走，即使顏色編輯器不編輯它：`From<&FlatColors>` 少了它
/// 就會把使用者的分支色盤打回預設。
pub struct FlatColors {
    /// 與 `COLOR_KEYS` 同序同長。
    pub values: Vec<RatatuiColor>,
    pub graph: GraphColors,
}

impl From<&ColorTheme> for FlatColors {
    fn from(theme: &ColorTheme) -> Self {
        let table =
            toml::Value::try_from(theme).expect("ColorTheme 的每個欄位都能序列化成 toml::Value");
        let values = COLOR_KEYS
            .iter()
            .map(|key| {
                table
                    .get(*key)
                    .cloned()
                    .unwrap_or_else(|| panic!("COLOR_KEYS 與 ColorTheme 欄位一一對應，缺了 {key}"))
                    .try_into::<RatatuiColor>()
                    .expect("Color::Deserialize 對自己 Display 出來的字串一定成功")
            })
            .collect();
        FlatColors {
            values,
            graph: theme.graph.clone(),
        }
    }
}

impl From<&FlatColors> for ColorTheme {
    fn from(flat: &FlatColors) -> Self {
        // 內部走 `Color` 自己的 `Serialize`（Display，"DarkGray"），不是
        // `color_to_config_string`——兩條都能 round-trip，但用後者的話
        // `NAMED_COLORS` 一旦寫錯，連預覽都跟著錯。預覽要獨立於拼法表，
        // 拼法表只負責寫檔。
        let mut table = toml::value::Table::new();
        for (key, color) in COLOR_KEYS.iter().zip(&flat.values) {
            table.insert(
                (*key).to_string(),
                toml::Value::try_from(color).expect("Color 一定能序列化"),
            );
        }
        table.insert(
            "graph".to_string(),
            toml::Value::try_from(&flat.graph).expect("GraphColors 一定能序列化"),
        );
        let optional: OptionalColorTheme = toml::Value::Table(table)
            .try_into()
            .expect("剛從 FlatColors 組出來的 table，鍵與型別都對得上 OptionalColorTheme");
        optional.into()
    }
}

/// 預覽區的三個小塊。前綴就是答案 —— 逐欄位標註會漂移，這 5 行不會。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewBlock {
    List,
    Detail,
    Status,
}

pub fn preview_block(key: &str) -> PreviewBlock {
    if key.starts_with("list_") {
        PreviewBlock::List
    } else if key.starts_with("detail_") || key.starts_with("diff_") {
        PreviewBlock::Detail
    } else {
        PreviewBlock::Status
    }
}

/// `Color` → 設定檔拼法。刻意跟 `assets/default-config.toml` 同一種寫法
/// （全小寫、多字詞用連字號），而不是 ratatui `Display` 的 PascalCase：
/// `Display` 給 `"DarkGray"`，範本寫 `"dark-gray"`。每一筆都保證
/// `RatatuiColor::from_str(s) == color`。
///
/// 反向不要用這張表——config string → `Color` 繼續交給 `FromStr`，它還吃
/// `grey` / `silver` / `bright-*` / 底線 / 空白一堆別名，這張表只認一種拼法。
pub(crate) const NAMED_COLORS: &[(&str, RatatuiColor)] = &[
    ("reset", RatatuiColor::Reset),
    ("black", RatatuiColor::Black),
    ("red", RatatuiColor::Red),
    ("green", RatatuiColor::Green),
    ("yellow", RatatuiColor::Yellow),
    ("blue", RatatuiColor::Blue),
    ("magenta", RatatuiColor::Magenta),
    ("cyan", RatatuiColor::Cyan),
    // "light-gray" 絕對不能出現在這裡：ratatui 的 `FromStr` 正規化鏈有
    // `.replace("lightgray", "white")`，會靜默把它讀成 `Color::White`。
    ("gray", RatatuiColor::Gray),
    ("dark-gray", RatatuiColor::DarkGray),
    ("light-red", RatatuiColor::LightRed),
    ("light-green", RatatuiColor::LightGreen),
    ("light-yellow", RatatuiColor::LightYellow),
    ("light-blue", RatatuiColor::LightBlue),
    ("light-magenta", RatatuiColor::LightMagenta),
    ("light-cyan", RatatuiColor::LightCyan),
    ("white", RatatuiColor::White),
];

pub(crate) fn color_to_config_string(color: RatatuiColor) -> String {
    NAMED_COLORS
        .iter()
        .find(|(_, c)| *c == color)
        .map(|(name, _)| (*name).to_string())
        // Rgb 與 Indexed 的 Display 輸出跟範本一字不差（"#E06C76" 大寫
        // hex、"208" 純數字），直接沿用。
        .unwrap_or_else(|| color.to_string())
}

/// 把 ANSI 色彩名稱轉成 `Color::Rgb(r,g,b)`，讓呼叫端可以拿去跟已經是 RGB
/// 形式的顏色比較或合併（例如 `commit_list.rs` 裡套用在 graph 儲存格上的
/// 選取列背景色）。
pub fn ratatui_color_to_rgb(color: RatatuiColor) -> RatatuiColor {
    match color {
        RatatuiColor::Rgb(r, g, b) => RatatuiColor::Rgb(r, g, b),
        RatatuiColor::Black => RatatuiColor::Rgb(0, 0, 0),
        RatatuiColor::DarkGray => RatatuiColor::Rgb(80, 80, 80),
        RatatuiColor::Gray => RatatuiColor::Rgb(128, 128, 128),
        RatatuiColor::White => RatatuiColor::Rgb(255, 255, 255),
        RatatuiColor::Red => RatatuiColor::Rgb(255, 0, 0),
        RatatuiColor::Green => RatatuiColor::Rgb(0, 128, 0),
        RatatuiColor::Blue => RatatuiColor::Rgb(0, 0, 255),
        RatatuiColor::Yellow => RatatuiColor::Rgb(255, 255, 0),
        RatatuiColor::Cyan => RatatuiColor::Rgb(0, 255, 255),
        RatatuiColor::Magenta => RatatuiColor::Rgb(255, 0, 255),
        _ => RatatuiColor::Rgb(0, 0, 0),
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case("#ff0000", Some(GraphColor { r: 255, g: 0, b: 0 }))]
    #[case("#AABBCCDD", Some(GraphColor { r: 170, g: 187, b: 204 }))]
    #[case("#AABBCCZZ", None)] // alpha byte 仍會做十六進位驗證，只是值被丟棄
    #[case("#ff000", None)]
    #[case("#fff", None)]
    #[case("000000", None)]
    #[case("##123456", None)]
    fn test_parse_rgba_color(#[case] input: &str, #[case] expected: Option<GraphColor>) {
        assert_eq!(parse_rgba_color(input), expected);
    }

    /// `COLOR_KEYS` 的集合必須跟 `ColorTheme` 的頂層鍵一致——漏一個新欄位
    /// 或多一個已刪除的欄位都要紅燈。順便擋住未來有人加 `#[serde(skip)]`。
    #[test]
    fn color_keys_covers_every_color_theme_field() {
        let value = toml::Value::try_from(ColorTheme::default()).unwrap();
        let table = value.as_table().unwrap();
        let mut expected: Vec<&str> = table
            .keys()
            .map(String::as_str)
            .filter(|k| *k != "graph")
            .collect();
        expected.sort_unstable();

        let mut actual: Vec<&str> = COLOR_KEYS.to_vec();
        actual.sort_unstable();

        assert_eq!(actual, expected);
    }

    /// 集合相等對重複鍵是盲的，對順序也是盲的——而順序是使用者唯一看得到
    /// 的東西。用 `toml_edit`（保留文件順序）讀 asset，一條測試同時釘住
    /// 集合、順序、無重複，而且用型別（`is_value()`）而不是白名單排掉
    /// `graph`。
    #[test]
    fn color_keys_match_the_asset_in_order() {
        let doc: toml_edit::DocumentMut = include_str!("../assets/default-config.toml")
            .parse()
            .unwrap();
        let asset_keys: Vec<&str> = doc["color"]
            .as_table()
            .unwrap()
            .iter()
            .filter(|(_, item)| item.is_value())
            .map(|(k, _)| k)
            .collect();

        assert_eq!(COLOR_KEYS, asset_keys.as_slice());
    }

    #[test]
    fn color_to_config_string_round_trips_through_ratatui_from_str() {
        for (name, color) in NAMED_COLORS {
            assert_eq!(RatatuiColor::from_str(name).unwrap(), *color, "{name}");
        }
        for i in 0..=255u8 {
            let color = RatatuiColor::Indexed(i);
            let s = color_to_config_string(color);
            assert_eq!(RatatuiColor::from_str(&s).unwrap(), color, "{s}");
        }
        for (r, g, b) in [(224, 108, 118), (0, 0, 0), (255, 255, 255), (18, 52, 86)] {
            let color = RatatuiColor::Rgb(r, g, b);
            let s = color_to_config_string(color);
            assert_eq!(RatatuiColor::from_str(&s).unwrap(), color, "{s}");
        }
    }

    /// 範本裡每個顏色值的拼法，必須就是 wizard 寫回去會用的拼法——不然
    /// 使用者改一個顏色，檔案裡就出現兩種寫法。
    #[test]
    fn asset_color_values_use_the_wizard_spelling() {
        let asset: toml::Table =
            toml::from_str(include_str!("../assets/default-config.toml")).unwrap();
        for (key, v) in asset["color"].as_table().unwrap() {
            let Some(s) = v.as_str() else { continue }; // graph 是 table，跳過
            let color = RatatuiColor::from_str(s).unwrap();
            assert_eq!(color_to_config_string(color), s, "{key}");
        }
    }

    /// `ColorTheme` → `FlatColors` → `ColorTheme` 逐欄位相等，含
    /// `graph.branches`。這是顏色編輯器的地基：如果這條不成立，wizard
    /// 存進去的值跟顯示出來的值就不是同一份資料。
    #[test]
    fn flat_colors_round_trips_through_color_theme() {
        let mut theme = ColorTheme::default();
        // 43 個互不相同的值，任何一個索引搞錯位置都會被抓到。
        for (i, key) in COLOR_KEYS.iter().enumerate() {
            let _ = key;
            let flat = FlatColors::from(&theme);
            let mut values = flat.values;
            values[i] = RatatuiColor::Indexed(i as u8);
            theme = ColorTheme::from(&FlatColors {
                values,
                graph: flat.graph,
            });
        }

        let flat = FlatColors::from(&theme);
        let roundtrip = ColorTheme::from(&flat);
        assert_eq!(roundtrip, theme);

        for (i, expected) in flat.values.iter().enumerate() {
            assert_eq!(*expected, RatatuiColor::Indexed(i as u8), "index {i}");
        }
    }
}

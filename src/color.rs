use ratatui::style::Color as RatatuiColor;
use serde::Deserialize;
#[cfg(test)]
use serde::Serialize;
use smart_default::SmartDefault;
use umbra::optional;

use crate::config::GraphColorConfig;

#[optional(derives = [Deserialize], visibility = pub)]
#[cfg_attr(test, derive(Serialize))]
#[derive(Debug, Clone, PartialEq, Eq, SmartDefault)]
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
    pub fn new(config: &GraphColorConfig) -> Self {
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

fn parse_rgba_color(s: &str) -> Option<GraphColor> {
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
}

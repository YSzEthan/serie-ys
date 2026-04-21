pub mod commit_detail;
pub mod commit_list;
pub mod commit_user_command;
pub mod marquee;
pub mod pending_overlay;
pub mod ref_list;

use ratatui::{
    style::Stylize,
    text::{Line, Span},
};

use crate::color::ColorTheme;

/// `key  desc  key  desc…` — key 用 `help_key_fg`，desc 用 `status_input_transient_fg`。
pub fn build_hint_line(theme: &ColorTheme, pairs: &[(&str, &str)]) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(pairs.len() * 2);
    for (i, (key, desc)) in pairs.iter().enumerate() {
        let sep = if i + 1 < pairs.len() { "  " } else { "" };
        spans.push(Span::raw((*key).to_string()).fg(theme.help_key_fg));
        spans.push(Span::raw(format!(" {desc}{sep}")).fg(theme.status_input_transient_fg));
    }
    Line::from(spans)
}

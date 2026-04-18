use std::{
    cell::RefCell,
    env,
    io::{self, Write},
    process::Command,
};

use arboard::Clipboard;
use base64::{engine::general_purpose::STANDARD, Engine};

use crate::config::ClipboardConfig;

const USER_COMMAND_MARKER_PREFIX: &str = "{{";
const USER_COMMAND_TARGET_HASH_MARKER: &str = "{{target_hash}}";
const USER_COMMAND_FIRST_PARENT_HASH_MARKER: &str = "{{first_parent_hash}}";
const USER_COMMAND_PARENT_HASHES_MARKER: &str = "{{parent_hashes}}";
const USER_COMMAND_REFS_MARKER: &str = "{{refs}}";
const USER_COMMAND_BRANCHES_MARKER: &str = "{{branches}}";
const USER_COMMAND_REMOTE_BRANCHES_MARKER: &str = "{{remote_branches}}";
const USER_COMMAND_TAGS_MARKER: &str = "{{tags}}";
const USER_COMMAND_AREA_WIDTH_MARKER: &str = "{{area_width}}";
const USER_COMMAND_AREA_HEIGHT_MARKER: &str = "{{area_height}}";

thread_local! {
    static CLIPBOARD: RefCell<Option<Clipboard>> = const { RefCell::new(None) };
}

pub fn copy_to_clipboard(value: String, config: &ClipboardConfig) -> Result<(), String> {
    match config {
        ClipboardConfig::Auto => {
            if is_ssh_session() {
                copy_to_clipboard_osc52(&value)
            } else {
                copy_to_clipboard_auto(value)
            }
        }
        ClipboardConfig::Osc52 => copy_to_clipboard_osc52(&value),
        ClipboardConfig::Custom { commands } => copy_to_clipboard_custom(value, commands),
    }
}

fn is_ssh_session() -> bool {
    env::var_os("SSH_CONNECTION").is_some() || env::var_os("SSH_TTY").is_some()
}

// tmux DCS passthrough：把 inner 所有 \x1b 替換成 \x1b\x1b，包在 \x1bPtmux;...\x1b\\ 裡。
// 用通用 escape 處理而不是 hardcode 單一 \x1b 位置，未來若把終止符從 \x07 換成 \x1b\\ 不會漏掉。
fn wrap_for_tmux(inner: &str) -> String {
    let escaped = inner.replace('\x1b', "\x1b\x1b");
    format!("\x1bPtmux;{escaped}\x1b\\")
}

fn format_osc52_raw(value: &str) -> String {
    let encoded = STANDARD.encode(value.as_bytes());
    format!("\x1b]52;c;{encoded}\x07")
}

fn copy_to_clipboard_osc52(value: &str) -> Result<(), String> {
    let raw = format_osc52_raw(value);
    let sequence = if env::var_os("TMUX").is_some() {
        wrap_for_tmux(&raw)
    } else {
        raw
    };
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(sequence.as_bytes())
        .map_err(|e| format!("Failed to write OSC 52 sequence: {e}"))?;
    stdout
        .flush()
        .map_err(|e| format!("Failed to flush stdout: {e}"))?;
    Ok(())
}

fn copy_to_clipboard_custom(value: String, commands: &[String]) -> Result<(), String> {
    use std::io::Write;
    use std::process::Stdio;

    if commands.is_empty() {
        return Err("No clipboard command specified".to_string());
    }

    let mut child = Command::new(&commands[0])
        .args(&commands[1..])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to run {}: {e}", commands[0]))?;

    child
        .stdin
        .take()
        .expect("stdin should be available")
        .write_all(value.as_bytes())
        .map_err(|e| format!("Failed to write to {}: {e}", commands[0]))?;

    child
        .wait()
        .map_err(|e| format!("{} failed: {e}", commands[0]))?;

    Ok(())
}

fn copy_to_clipboard_auto(value: String) -> Result<(), String> {
    CLIPBOARD.with_borrow_mut(|clipboard| {
        if clipboard.is_none() {
            *clipboard = Clipboard::new()
                .map(Some)
                .map_err(|e| format!("Failed to create clipboard: {e:?}"))?;
        }

        clipboard
            .as_mut()
            .expect("The clipboard should have been initialized above")
            .set_text(value)
            .map_err(|e| format!("Failed to copy to clipboard: {e:?}"))
    })
}

/// Outcome of `open_url`. `Hyperlinked` means we couldn't (or shouldn't) spawn
/// a browser locally — the caller should instead surface the URL as an OSC 8
/// clickable button so the user's local terminal can hand it off to their
/// own browser.
pub enum OpenUrlOutcome {
    Spawned,
    Hyperlinked(String),
}

pub fn open_url(url: &str) -> Result<OpenUrlOutcome, String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(format!("Refusing to open non-http URL: {url}"));
    }

    if is_ssh_session() {
        return Ok(OpenUrlOutcome::Hyperlinked(url.to_string()));
    }

    #[cfg(target_os = "macos")]
    let (prog, args): (&str, &[&str]) = ("open", &[url]);
    #[cfg(target_os = "linux")]
    let (prog, args): (&str, &[&str]) = ("xdg-open", &[url]);
    // 用 rundll32 取代 `cmd /C start ""`，避開 cmd.exe 對 URL 內 `&`/`^`/`%` 的 shell 解釋。
    #[cfg(target_os = "windows")]
    let (prog, args): (&str, &[&str]) = ("rundll32", &["url.dll,FileProtocolHandler", url]);

    Command::new(prog)
        .args(args)
        .spawn()
        .map(|_| OpenUrlOutcome::Spawned)
        .map_err(|e| format!("Failed to open URL: {e}"))
}

/// Format a URL + label into an OSC 8 hyperlink escape sequence. Terminals
/// that support OSC 8 (ghostty, iTerm2, Kitty, WezTerm) render the label as
/// a clickable link. tmux gets DCS-passthrough wrapping.
pub fn format_osc8_hyperlink(url: &str, label: &str) -> String {
    let raw = format!("\x1b]8;;{url}\x1b\\{label}\x1b]8;;\x1b\\");
    if env::var_os("TMUX").is_some() {
        wrap_for_tmux(&raw)
    } else {
        raw
    }
}

pub struct ExternalCommandParameters<'a> {
    pub command: &'a [String],
    pub target_hash: &'a str,
    pub parent_hashes: Vec<&'a str>,
    pub all_refs: Vec<&'a str>,
    pub branches: Vec<&'a str>,
    pub remote_branches: Vec<&'a str>,
    pub tags: Vec<&'a str>,
    pub area_width: u16,
    pub area_height: u16,
}

pub fn exec_user_command(params: ExternalCommandParameters) -> Result<String, String> {
    let command = build_user_command(&params);

    let output = Command::new(&command[0])
        .args(&command[1..])
        .output()
        .map_err(|e| format!("Failed to execute command: {e:?}"))?;

    if !output.status.success() {
        let msg = format!(
            "Command exited with non-zero status: {}, stderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        return Err(msg);
    }

    Ok(String::from_utf8_lossy(&output.stdout).into())
}

pub fn exec_user_command_suspend(params: ExternalCommandParameters) -> Result<(), String> {
    let command = build_user_command(&params);

    let output = Command::new(&command[0])
        .args(&command[1..])
        .status()
        .map_err(|e| format!("Failed to execute command: {e:?}"))?;

    if !output.success() {
        let msg = format!("Command exited with non-zero status: {output}");
        return Err(msg);
    }

    Ok(())
}

fn build_user_command(params: &ExternalCommandParameters) -> Vec<String> {
    fn to_vec(ss: &[&str]) -> Vec<String> {
        ss.iter().map(|s| s.to_string()).collect()
    }
    let mut command = Vec::new();
    for arg in params.command {
        if !arg.contains(USER_COMMAND_MARKER_PREFIX) {
            command.push(arg.clone());
            continue;
        }
        match arg.as_str() {
            // If the marker is used as a standalone argument, expand it into multiple arguments.
            // This allows the command to receive each item as a separate argument and correctly handle items that contain spaces.
            USER_COMMAND_BRANCHES_MARKER => command.extend(to_vec(&params.branches)),
            USER_COMMAND_REMOTE_BRANCHES_MARKER => command.extend(to_vec(&params.remote_branches)),
            USER_COMMAND_TAGS_MARKER => command.extend(to_vec(&params.tags)),
            USER_COMMAND_REFS_MARKER => command.extend(to_vec(&params.all_refs)),
            USER_COMMAND_PARENT_HASHES_MARKER => command.extend(to_vec(&params.parent_hashes)),
            // Otherwise, replace the marker within the single argument string.
            _ => command.push(replace_command_arg(arg, params)),
        }
    }
    command
}

fn replace_command_arg(s: &str, params: &ExternalCommandParameters) -> String {
    let sep = " ";
    let target_hash = params.target_hash;
    let first_parent_hash = &params.parent_hashes.first().cloned().unwrap_or_default();
    let parent_hashes = &params.parent_hashes.join(sep);
    let all_refs = &params.all_refs.join(sep);
    let branches = &params.branches.join(sep);
    let remote_branches = &params.remote_branches.join(sep);
    let tags = &params.tags.join(sep);
    let area_width = &params.area_width.to_string();
    let area_height = &params.area_height.to_string();

    s.replace(USER_COMMAND_TARGET_HASH_MARKER, target_hash)
        .replace(USER_COMMAND_FIRST_PARENT_HASH_MARKER, first_parent_hash)
        .replace(USER_COMMAND_PARENT_HASHES_MARKER, parent_hashes)
        .replace(USER_COMMAND_REFS_MARKER, all_refs)
        .replace(USER_COMMAND_BRANCHES_MARKER, branches)
        .replace(USER_COMMAND_REMOTE_BRANCHES_MARKER, remote_branches)
        .replace(USER_COMMAND_TAGS_MARKER, tags)
        .replace(USER_COMMAND_AREA_WIDTH_MARKER, area_width)
        .replace(USER_COMMAND_AREA_HEIGHT_MARKER, area_height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_osc52_raw_encodes_value() {
        // "ABC" → base64 "QUJD"
        assert_eq!(format_osc52_raw("ABC"), "\x1b]52;c;QUJD\x07");
    }

    #[test]
    fn wrap_for_tmux_escapes_single_esc() {
        // 實際 pipeline：tmux passthrough 包住 BEL-terminated OSC 52，只有開頭一個 \x1b
        let inner = format_osc52_raw("ABC");
        assert_eq!(
            wrap_for_tmux(&inner),
            "\x1bPtmux;\x1b\x1b]52;c;QUJD\x07\x1b\\"
        );
    }

    #[test]
    fn wrap_for_tmux_escapes_multiple_escs() {
        // 若未來終止符改為 ST (\x1b\\)，inner 會有兩個 \x1b，都要 escape
        let inner = "\x1b]52;c;QUJD\x1b\\";
        assert_eq!(
            wrap_for_tmux(inner),
            "\x1bPtmux;\x1b\x1b]52;c;QUJD\x1b\x1b\\\x1b\\"
        );
    }

    #[test]
    fn format_osc8_hyperlink_plain_no_tmux() {
        // 跑 test 通常不在 tmux 內；若在 tmux 下請跳過此檢查。
        if env::var_os("TMUX").is_some() {
            return;
        }
        assert_eq!(
            format_osc8_hyperlink("https://x.com", "[#1]"),
            "\x1b]8;;https://x.com\x1b\\[#1]\x1b]8;;\x1b\\"
        );
    }
}

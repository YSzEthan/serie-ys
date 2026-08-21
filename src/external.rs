use std::{
    cell::RefCell,
    env,
    fs::OpenOptions,
    io::{self, Read, Write},
    process::Command,
    sync::{Arc, Mutex},
    thread,
};

use arboard::Clipboard;
use base64::{engine::general_purpose::STANDARD, Engine};

use crate::config::ClipboardConfig;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

// 手動宣告，不為了一個 syscall 拉整個 `libc` crate 進來——`exec_shell_command`
// 用它讓 spawn 出來的 shell 跟 serie 的控制終端機斷開，見那裡的說明。
#[cfg(unix)]
extern "C" {
    fn setsid() -> i32;
}

#[cfg(unix)]
fn libc_setsid() -> i32 {
    // SAFETY：`setsid()` 是無參數、不觸碰記憶體的簡單 syscall wrapper，
    // 只在 `pre_exec` 的 fork 後子行程情境呼叫，符合它 async-signal-safe
    // 的要求。
    unsafe { setsid() }
}

const USER_COMMAND_MARKER_PREFIX: &str = "{{";
const USER_COMMAND_TARGET_HASH_MARKER: &str = "{{target_hash}}";
const USER_COMMAND_FIRST_PARENT_HASH_MARKER: &str = "{{first_parent_hash}}";
const USER_COMMAND_PARENT_HASHES_MARKER: &str = "{{parent_hashes}}";
const USER_COMMAND_REFS_MARKER: &str = "{{refs}}";
const USER_COMMAND_BRANCHES_MARKER: &str = "{{branches}}";
const USER_COMMAND_REMOTE_BRANCHES_MARKER: &str = "{{remote_branches}}";
const USER_COMMAND_TAGS_MARKER: &str = "{{tags}}";
const USER_COMMAND_STASH_MARKER: &str = "{{stash}}";
const USER_COMMAND_AREA_WIDTH_MARKER: &str = "{{area_width}}";
const USER_COMMAND_AREA_HEIGHT_MARKER: &str = "{{area_height}}";

thread_local! {
    static CLIPBOARD: RefCell<Option<Clipboard>> = const { RefCell::new(None) };
}

pub fn copy_to_clipboard(value: String, config: &ClipboardConfig) -> Result<(), String> {
    match config {
        ClipboardConfig::Auto => {
            if should_use_osc52() {
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

pub fn is_tmux() -> bool {
    env::var_os("TMUX").is_some()
}

// tmux session 可能在 SSH 前就存在、看不到 SSH_* env，導致 arboard 寫不到 host
// 剪貼簿；只要在 tmux 內，一律改走 OSC52 讓外層終端處理。
fn should_use_osc52() -> bool {
    is_ssh_session() || is_tmux()
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
    let in_tmux = is_tmux();

    // /dev/tty 永遠先寫一次：在純 SSH 或非 tmux 環境下這就是終端，bytes 直達外層解析剪貼簿。
    // tmux 內的 /dev/tty 是 pane pty，bytes 會被 set-clipboard 攔截 — 由下面 list-clients 路徑兜底。
    let mut wrote_any = write_to_tty("/dev/tty", &raw).is_ok();

    // tmux 場景（含 nested popup / floax）：tmux 只把 OSC52 轉發給 attached client，popup chain
    // 會把字節當視覺內容吞掉。直接 enumerate 所有 client tty，把 bytes 灌到每個 client 的 pty slave，
    // 完全繞過 set-clipboard 與 allow-passthrough 邏輯。任一 tty 寫失敗都靜默忽略。
    if in_tmux {
        for tty in tmux_client_ttys() {
            if write_to_tty(&tty, &raw).is_ok() {
                wrote_any = true;
            }
        }
    }

    if wrote_any {
        return Ok(());
    }

    // 兩條路徑都沒成功：退回 stdout（外加 tmux 下的 DCS-wrapped fallback）。
    let sequence = if in_tmux {
        format!("{raw}{}", wrap_for_tmux(&raw))
    } else {
        raw
    };
    write_to_stdout(&sequence)
}

fn write_to_tty(path: &str, s: &str) -> io::Result<()> {
    let mut f = OpenOptions::new().write(true).open(path)?;
    f.write_all(s.as_bytes())?;
    f.flush()
}

fn write_to_stdout(s: &str) -> Result<(), String> {
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(s.as_bytes())
        .map_err(|e| format!("Failed to write OSC 52 sequence: {e}"))?;
    stdout
        .flush()
        .map_err(|e| format!("Failed to flush stdout: {e}"))?;
    Ok(())
}

fn tmux_client_ttys() -> Vec<String> {
    let Ok(out) = Command::new("tmux")
        .args(["list-clients", "-F", "#{client_tty}"])
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
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

/// `open_url` 的結果。`NotSpawned`：沒有本機瀏覽器可開（SSH／mosh），
/// 呼叫端負責把 URL 顯示出來、讓終端自己偵測——理由見它的呼叫端
/// `App::open_url`。
pub enum OpenUrlOutcome {
    Spawned,
    NotSpawned,
}

pub fn open_url(url: &str) -> Result<OpenUrlOutcome, String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(format!("Refusing to open non-http URL: {url}"));
    }

    if is_ssh_session() {
        return Ok(OpenUrlOutcome::NotSpawned);
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

/// 把 URL 加標籤格式化成 OSC 8 超連結跳脫序列。支援 OSC 8 的終端機（ghostty、
/// iTerm2、Kitty、WezTerm）會把標籤渲染成可點擊連結。
///
/// **呼叫端負責在 tmux 下跳過**，不要指望這裡處理：tmux 的 DCS passthrough
/// 會把 inner sequence 直接轉發給外層終端、不保留游標定位，疊在 cell 上的
/// label 會被印到任意座標。這在「疊加在同寬度純文字上」的場景（GitHub
/// 列表／預覽的 `#N`）能被 `is_tmux()` early-return 安全跳過——跳過後看到
/// 的仍是正確的純文字；換成別的場景前，先確認一樣有純文字可以退回。
pub fn format_osc8_hyperlink(url: &str, label: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\{label}\x1b]8;;\x1b\\")
}

pub struct ExternalCommandParameters<'a> {
    pub command: &'a [String],
    pub target_hash: &'a str,
    pub parent_hashes: Vec<&'a str>,
    pub all_refs: Vec<&'a str>,
    pub branches: Vec<&'a str>,
    pub remote_branches: Vec<&'a str>,
    pub tags: Vec<&'a str>,
    pub stash: Option<&'a str>,
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
            // 若標記是單獨一個引數，就把它展開成多個引數。
            // 這樣指令才能把每個項目當成獨立引數接收，正確處理內含空白的項目。
            USER_COMMAND_BRANCHES_MARKER => command.extend(to_vec(&params.branches)),
            USER_COMMAND_REMOTE_BRANCHES_MARKER => command.extend(to_vec(&params.remote_branches)),
            USER_COMMAND_TAGS_MARKER => command.extend(to_vec(&params.tags)),
            USER_COMMAND_REFS_MARKER => command.extend(to_vec(&params.all_refs)),
            USER_COMMAND_PARENT_HASHES_MARKER => command.extend(to_vec(&params.parent_hashes)),
            // 否則就在該單一引數字串內取代標記。
            _ => command.push(replace_command_arg(arg, params)),
        }
    }
    command
}

/// 需要選到真正 commit 才有值的 marker——Working changes（virtual row）
/// 沒有這些，字串裡含其中之一而 `params` 是 `None` 就該報錯，不能靜默展開
/// 成空字串（`git show {{target_hash}}` 會變成 `git show `，代入了使用者
/// 沒選的內容）。`{{area_width}}`/`{{area_height}}` 不算，來自面板尺寸，
/// 跟有沒有選到 commit 無關，見 `replace_markers` 最後兩行。
const COMMIT_MARKERS: &[&str] = &[
    USER_COMMAND_TARGET_HASH_MARKER,
    USER_COMMAND_FIRST_PARENT_HASH_MARKER,
    USER_COMMAND_PARENT_HASHES_MARKER,
    USER_COMMAND_REFS_MARKER,
    USER_COMMAND_BRANCHES_MARKER,
    USER_COMMAND_REMOTE_BRANCHES_MARKER,
    USER_COMMAND_TAGS_MARKER,
    USER_COMMAND_STASH_MARKER,
];

/// `marker` 必須是 `COMMIT_MARKERS` 裡的一個，否則 panic——呼叫端已經用
/// `COMMIT_MARKERS` 篩過，這裡拿不到值代表兩份表對不上，是程式錯誤。
/// `Single(v)` 等價於單元素的 `join`，不需要一個 enum 描述這兩種形狀。
fn commit_marker_value(
    marker: &str,
    params: &ExternalCommandParameters,
    esc: &impl Fn(&str) -> Result<String, String>,
) -> Result<String, String> {
    let join = |vs: &[&str]| -> Result<String, String> {
        Ok(vs
            .iter()
            .copied()
            .map(esc)
            .collect::<Result<Vec<_>, _>>()?
            .join(" "))
    };
    match marker {
        USER_COMMAND_TARGET_HASH_MARKER => esc(params.target_hash),
        USER_COMMAND_FIRST_PARENT_HASH_MARKER => {
            esc(params.parent_hashes.first().copied().unwrap_or(""))
        }
        USER_COMMAND_PARENT_HASHES_MARKER => join(&params.parent_hashes),
        USER_COMMAND_REFS_MARKER => join(&params.all_refs),
        USER_COMMAND_BRANCHES_MARKER => join(&params.branches),
        USER_COMMAND_REMOTE_BRANCHES_MARKER => join(&params.remote_branches),
        USER_COMMAND_TAGS_MARKER => join(&params.tags),
        USER_COMMAND_STASH_MARKER => esc(params.stash.unwrap_or("")),
        _ => unreachable!("{marker} 不在 COMMIT_MARKERS 裡"),
    }
}

/// 把 `s` 裡的 `{{marker}}` 換成值——`replace_command_arg`（逐字元代入，給
/// argv 元素用）與 `expand_shell_command_line`（shell quote 後代入，給整段
/// shell 指令字串用）共用同一份 marker 表，差異只在 `esc`。
///
/// 只在字串真的含某個 marker 時才求值／轉義：`{{stash}}` 不出現在指令裡
/// 就不用管有沒有選到 stash，`params` 是 `None`（Working changes 列）時
/// 也一樣——真正該報錯的只有「字串含 commit marker 但沒有 commit 可代」。
///
/// `esc` 回傳 `Result` 是為了 Windows shell quoting：某些字元在 `cmd /C`
/// 底下沒有安全的轉義方式，那條路要能回傳 `Err` 而不是悄悄產出壞字串。
/// `replace_command_arg` 用的 identity esc 永遠不會走到 `Err` 分支。
fn replace_markers(
    s: &str,
    params: Option<&ExternalCommandParameters>,
    area_width: u16,
    area_height: u16,
    esc: impl Fn(&str) -> Result<String, String>,
) -> Result<String, String> {
    let mut result = s.to_string();

    for &marker in COMMIT_MARKERS {
        if !result.contains(marker) {
            continue;
        }
        let Some(params) = params else {
            return Err(format!("沒有選到 commit，無法代入 {marker}"));
        };
        let value = commit_marker_value(marker, params, &esc)?;
        result = result.replace(marker, &value);
    }

    if result.contains(USER_COMMAND_AREA_WIDTH_MARKER) {
        result = result.replace(USER_COMMAND_AREA_WIDTH_MARKER, &area_width.to_string());
    }
    if result.contains(USER_COMMAND_AREA_HEIGHT_MARKER) {
        result = result.replace(USER_COMMAND_AREA_HEIGHT_MARKER, &area_height.to_string());
    }

    Ok(result)
}

fn replace_command_arg(s: &str, params: &ExternalCommandParameters) -> String {
    replace_markers(
        s,
        Some(params),
        params.area_width,
        params.area_height,
        |v| Ok(v.to_string()),
    )
    .expect("完整 params + identity esc 永遠不會回 Err")
}

/// POSIX：任何值都能安全地用單引號包起來，內部單引號轉成 `'\''`
/// （先結束引號、插入一個跳脫過的單引號、再開新引號）。
#[cfg(not(target_os = "windows"))]
fn shell_quote(v: &str) -> Result<String, String> {
    Ok(format!("'{}'", v.replace('\'', r"'\''")))
}

/// Windows：`cmd /C` 沒有跟 POSIX 單引號等價的萬用轉義規則，含這些字元
/// 的值直接報錯，不要靜靜產出可能被 cmd 另行解釋的字串。沒有特殊字元時
/// 才需要引號的話就加雙引號；純字母數字原樣返回。
#[cfg(target_os = "windows")]
fn shell_quote(v: &str) -> Result<String, String> {
    const DANGEROUS: &[char] = &['"', '%', '^', '&', '|', '<', '>', '(', ')', '!'];
    if v.chars().any(|c| DANGEROUS.contains(&c)) {
        return Err(format!(
            "無法在 Windows 上安全代入含 {DANGEROUS:?} 的值：{v:?}"
        ));
    }
    if v.contains(char::is_whitespace) {
        Ok(format!("\"{v}\""))
    } else {
        Ok(v.to_string())
    }
}

/// 把使用者在命令列輸入的整段字串裡的 marker 換成目前選取 commit 的值，
/// 每個值各自 shell-quote 後再代入——`{{branches}}` 展開成 `'main' 'dev'`
/// 仍是兩個獨立參數，且 ref 名稱裡的單引號不會逃逸出去執行別的指令。
///
/// `params` 是 `None`：目前選取的是 Working changes（virtual row），沒有
/// 真正的 commit。`{{area_width}}`/`{{area_height}}` 這兩個不吃 `params`，
/// 一樣可以代入；含其餘 commit marker 就回 `Err`。
///
/// **巢狀 quote 無解**：marker 自己已經帶引號，使用者不該再手動包一層
/// （`--grep="{{target_hash}}"` 會展開成 `--grep="'abc'"`，單引號變字面值）。
pub fn expand_shell_command_line(
    command_line: &str,
    params: Option<&ExternalCommandParameters>,
    area_width: u16,
    area_height: u16,
) -> Result<String, String> {
    replace_markers(command_line, params, area_width, area_height, shell_quote)
}

/// POSIX shell 的 basename allowlist——決定 `exec_shell_command` 要不要加
/// `-i`（讀 rc 檔的 alias／function）、要不要用 `exec 2>&1; ` 把 rc 初始化
/// 噪音濾掉。用 basename 比對，不管呼叫端給的是完整路徑（`/bin/zsh`）還是
/// 裸命令（`zsh`）；不在表內的（fish、nushell、Windows 的 `cmd`、使用者
/// 自訂的怪 shell）一律當非 POSIX 處理，不冒然加只有這幾種 shell 才認得
/// 的旗標／語法。
pub(crate) fn is_posix_shell(prog: &str) -> bool {
    const POSIX_SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "mksh", "ash"];
    let basename = std::path::Path::new(prog)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(prog);
    POSIX_SHELLS.contains(&basename)
}

/// 讀取階段的記憶體上限——`yes`、`find /` 這類指令的輸出可以無限長，
/// 這裡是真正接住的防線（不是事後對已經讀進記憶體的字串 truncate）。
const MAX_OUTPUT_BYTES: usize = 5 * 1024 * 1024;

/// 讀 `reader` 直到 EOF 或 `cap` bytes，回傳 `(內容, 是否被截斷)`。多讀
/// 一個 byte 來判斷「剛好等於 cap」跟「超過 cap」的差別，超過時把多讀的
/// 那個 byte 丟掉。
fn read_capped(reader: &mut impl Read, cap: usize) -> (Vec<u8>, bool) {
    let mut buf = Vec::new();
    let _ = reader.take(cap as u64 + 1).read_to_end(&mut buf);
    let truncated = buf.len() > cap;
    if truncated {
        buf.truncate(cap);
    }
    (buf, truncated)
}

/// 在 `repo_path` 底下經由 `shell` 執行 `command_line`，回傳合併後的
/// stdout+stderr——非零 exit 一樣回傳輸出（附一行狀態），不當成 `Err`：
/// 命令本身的錯誤訊息就是使用者要看的東西。`Err` 只保留給「連 shell 都
/// 沒能啟動」這種真正例外的情況。
///
/// `shell` 是 `[程式, 旗標...]`（例如 `["zsh", "-i", "-c"]`），`command_line`
/// 會當成最後一個引數接在後面——來源見 `app::resolve_shell_command` 或
/// `core.shell.command` 設定。
///
/// POSIX shell（`is_posix_shell`）額外做兩件事：
/// - 指令前面接 `exec 2>&1; `，把 shell 自己 rc 初始化階段的 stderr 噪音
///   （例如 oh-my-zsh／powerlevel10k 的警告）留在 `exec` 之前、隨
///   `stderr(Stdio::null())` 丟棄；`exec` 之後使用者指令的 stderr 併入
///   stdout pipe，兩者交錯順序也因此是對的（不會像「stdout 全部接在前面 +
///   stderr 全部接在後面」那樣把 `git fetch` 的進度整團排到最後）。
/// - 帶 `SERIE_SHELL=1` 環境變數，讓使用者可以在 rc 檔開頭偵測它、
///   early return 跳過主題／外掛，把每次執行都要付的 shell 初始化成本
///   從約 1 秒降到接近 0（見 `docs/src/configurations/config-file-format.md`
///   的 `[core.shell]` 說明）。
///
/// `child_slot`：spawn 出來的 child 會先存進這裡再開始讀——讓
/// `ShellView` 能在使用者中途關閉（Esc）或整個 app 結束時砍掉還在跑的
/// 指令（見 `view::shell::ShellView` 的 `Drop`），不留孤兒程序握著 pipe。
pub fn exec_shell_command(
    command_line: &str,
    repo_path: &std::path::Path,
    shell: &[String],
    child_slot: &Arc<Mutex<Option<std::process::Child>>>,
) -> Result<String, String> {
    let Some((prog, flags)) = shell.split_first() else {
        return Err("core.shell.command 不能是空陣列".to_string());
    };
    let posix = is_posix_shell(prog);
    let full_command = if posix {
        format!("exec 2>&1; {command_line}")
    } else {
        command_line.to_string()
    };

    let mut cmd = Command::new(prog);
    cmd.args(flags)
        .arg(&full_command)
        .current_dir(repo_path)
        .env("SERIE_SHELL", "1")
        // 指令不該能讀鍵盤——`Stdio::null()` 是承重牆，不要為了互動指令拿掉。
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(if posix {
            std::process::Stdio::null()
        } else {
            std::process::Stdio::piped()
        });

    // zsh 帶 `-i` 的 job control 初始化會經由 `/dev/tty`（不是繼承的 fd 0，
    // 那個是 `/dev/tty` 永遠解析到的「目前這個 session 的控制終端機」，跟
    // fd 0 有沒有被 redirect 無關）去搶 serie 自己的控制終端機。實測：不
    // detach 的話，即使 zsh 印出「can't change option: monitor」放棄
    // monitor mode，它中途仍可能已經 `tcsetpgrp` 過一次——這裡把它
    // SIGKILL 掉（`ShellView::Drop`）時它來不及還原，serie 自己讀鍵盤的
    // thread 就此讀不到終端輸入，`event::EventController` 的 watchdog 偵測
    // 到停滯後會在約 2 秒內把整個 app 關掉（`AppEvent::Quit` 的
    // watchdog fallback）。`setsid()` 讓子 process 進到一個全新、沒有控制
    // 終端機的 session——`/dev/tty` 直接打不開，job control 初始化在
    // *更早* 的地方就會安分失敗，不會碰到 serie 的終端。
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            if libc_setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to run {prog}: {e}"))?;
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take();

    *child_slot.lock().unwrap() = Some(child);

    // 非 POSIX shell 路徑（Windows 的 `cmd /C`、fish、nushell……）stdout／
    // stderr 各自獨立 pipe，依序讀會死鎖：子行程寫滿其中一邊的 OS pipe
    // buffer 後卡住等讀者，這條 thread 卻還卡在讀另一邊——兩邊互等，直到
    // 使用者手動關閉 shell 才會被砍掉。開一條 thread 併行讀 stderr，跟
    // stdout 在目前這條 thread 同時進行，就不會有誰等誰的問題。
    let stderr_thread =
        stderr.map(|mut stderr| thread::spawn(move || read_capped(&mut stderr, MAX_OUTPUT_BYTES)));

    let (out_buf, out_truncated) = read_capped(&mut stdout, MAX_OUTPUT_BYTES);
    let mut text = String::from_utf8_lossy(&out_buf).into_owned();

    // stdout 讀滿上限代表不會再讀了——child 若還在寫 stdout（pipe 滿）會
    // 卡住，不再往 stderr 寫也不會結束，下面 join stderr thread 就永遠
    // 等不到 EOF。這裡先砍掉 child 讓它解套，join 才回得來。
    if out_truncated {
        if let Some(child) = child_slot.lock().unwrap().as_mut() {
            let _ = child.kill();
        }
    }

    let (err_buf, err_truncated) = stderr_thread
        .and_then(|t| t.join().ok())
        .unwrap_or_default();
    text.push_str(&String::from_utf8_lossy(&err_buf));

    let truncated = out_truncated || err_truncated;
    if truncated {
        // child 可能還沒被上面那次 kill 砍到（例如只有 stderr 端截斷）——
        // 對已經死掉的 child 再 kill 一次是無害的 no-op。
        if let Some(child) = child_slot.lock().unwrap().as_mut() {
            let _ = child.kill();
        }
        text.push_str(&format!("\n… 輸出過長，已在 {MAX_OUTPUT_BYTES} bytes 截斷"));
    }

    // 先把 child 從鎖裡取出來再 wait——`child_slot.lock().unwrap().as_mut()`
    // 這種寫法的暫存 `MutexGuard` 活到整個語句結束，會讓 `wait()` 全程握著
    // 鎖：使用者這時按 Esc，主執行緒 `ShellView::Drop` 的 `self.child.lock()`
    // 就會被卡住（指令背景化、關掉 stdout 但自己還沒結束時最容易踩到）。
    // 拆成兩個語句，鎖在 `.take()` 那行結束就放掉，`wait()` 操作的是已經
    // 拿出來的本地變數，不再需要鎖。
    let child = child_slot.lock().unwrap().take();
    let status = child.and_then(|mut c| c.wait().ok());
    if let Some(status) = status {
        if !status.success() {
            text.push_str(&format!("\n[exit status: {status}]"));
        }
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_osc52_raw_encodes_value() {
        // "ABC" 編碼成 base64 就是 "QUJD"
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
    fn format_osc8_hyperlink_wraps_url_and_label() {
        assert_eq!(
            format_osc8_hyperlink("https://x.com", "[#1]"),
            "\x1b]8;;https://x.com\x1b\\[#1]\x1b]8;;\x1b\\"
        );
    }

    fn params_with_stash<'a>(
        command: &'a [String],
        stash: Option<&'a str>,
    ) -> ExternalCommandParameters<'a> {
        ExternalCommandParameters {
            command,
            target_hash: "abc123",
            parent_hashes: vec![],
            all_refs: vec![],
            branches: vec![],
            remote_branches: vec![],
            tags: vec![],
            stash,
            area_width: 80,
            area_height: 30,
        }
    }

    #[test]
    fn build_user_command_expands_stash_marker_when_selected() {
        // 選取 stash commit：{{stash}} 替換為 stash 名稱
        let command = [
            "git".to_string(),
            "stash".to_string(),
            "show".to_string(),
            "{{stash}}".to_string(),
        ];
        let params = params_with_stash(&command, Some("stash@{0}"));
        assert_eq!(
            build_user_command(&params),
            vec!["git", "stash", "show", "stash@{0}"]
        );
    }

    #[test]
    fn build_user_command_stash_marker_empty_when_not_stash() {
        // 非 stash commit：{{stash}} 替換為空字串
        let command = ["echo".to_string(), "[{{stash}}]".to_string()];
        let params = params_with_stash(&command, None);
        assert_eq!(build_user_command(&params), vec!["echo", "[]"]);
    }

    /// `replace_command_arg` 的 identity esc 不該轉義任何東西——argv 元素
    /// 直接傳給 `Command::args()`，不經過 shell，加引號反而是錯的。
    #[test]
    fn replace_command_arg_identity_esc_does_not_quote() {
        let command: [String; 0] = [];
        let params = ExternalCommandParameters {
            command: &command,
            target_hash: "abc123",
            parent_hashes: vec![],
            all_refs: vec![],
            branches: vec!["it's-a-branch"],
            remote_branches: vec![],
            tags: vec![],
            stash: None,
            area_width: 80,
            area_height: 30,
        };
        assert_eq!(
            replace_command_arg("{{target_hash}} {{branches}}", &params),
            "abc123 it's-a-branch"
        );
    }

    #[test]
    fn expand_shell_command_line_quotes_each_branch_separately() {
        let command: [String; 0] = [];
        let params = ExternalCommandParameters {
            command: &command,
            target_hash: "abc123",
            parent_hashes: vec![],
            all_refs: vec![],
            branches: vec!["main", "dev"],
            remote_branches: vec![],
            tags: vec![],
            stash: None,
            area_width: 80,
            area_height: 30,
        };
        let expanded =
            expand_shell_command_line("git log {{branches}}", Some(&params), 80, 30).unwrap();
        assert_eq!(expanded, "git log 'main' 'dev'");
    }

    #[test]
    fn expand_shell_command_line_escapes_single_quote_in_value() {
        let command: [String; 0] = [];
        let params = ExternalCommandParameters {
            command: &command,
            target_hash: "abc123",
            parent_hashes: vec![],
            all_refs: vec![],
            branches: vec!["it's-a-branch"],
            remote_branches: vec![],
            tags: vec![],
            stash: None,
            area_width: 80,
            area_height: 30,
        };
        // ref 名稱含單引號時，展開後仍是安全的單一 shell token——
        // 不會逃逸出去變成「執行別的指令」。
        let expanded =
            expand_shell_command_line("git log {{branches}}", Some(&params), 80, 30).unwrap();
        assert_eq!(expanded, r"git log 'it'\''s-a-branch'");
    }

    #[test]
    fn expand_shell_command_line_interpolates_target_hash_mid_string() {
        let command: [String; 0] = [];
        let params = ExternalCommandParameters {
            command: &command,
            target_hash: "deadbeef",
            parent_hashes: vec![],
            all_refs: vec![],
            branches: vec![],
            remote_branches: vec![],
            tags: vec![],
            stash: None,
            area_width: 80,
            area_height: 30,
        };
        let expanded =
            expand_shell_command_line("git log --grep={{target_hash}} -1", Some(&params), 80, 30)
                .unwrap();
        assert_eq!(expanded, "git log --grep='deadbeef' -1");
    }

    #[test]
    fn expand_shell_command_line_without_commit_errors_on_commit_marker() {
        // Working changes 列（virtual row）沒有真正的 commit——`params`
        // 傳 `None`，錯誤訊息要點名是哪一個 marker，不能靜默展開成空字串。
        let err = expand_shell_command_line("git show {{target_hash}}", None, 80, 30).unwrap_err();
        assert!(
            err.contains("{{target_hash}}"),
            "錯誤訊息要點名 marker: {err}"
        );
    }

    #[test]
    fn expand_shell_command_line_without_commit_allows_area_markers() {
        // {{area_width}}/{{area_height}} 來自面板尺寸，跟有沒有選到 commit
        // 無關，Working changes 列一樣要能用。
        let expanded =
            expand_shell_command_line("echo {{area_width}}x{{area_height}}", None, 120, 40)
                .unwrap();
        assert_eq!(expanded, "echo 120x40");
    }

    #[test]
    fn expand_shell_command_line_without_commit_passes_through_when_no_marker() {
        // 沒有 marker 的指令（`git status` 這類）在 Working changes 列
        // 要能原樣執行，不該因為沒有 commit 就被擋下。
        let expanded = expand_shell_command_line("git status", None, 80, 30).unwrap();
        assert_eq!(expanded, "git status");
    }

    #[test]
    fn is_posix_shell_matches_by_basename() {
        assert!(is_posix_shell("zsh"));
        assert!(is_posix_shell("/bin/zsh"));
        assert!(is_posix_shell("/usr/local/bin/bash"));
        assert!(!is_posix_shell("fish"));
        assert!(!is_posix_shell("cmd"));
        assert!(!is_posix_shell(""));
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn exec_shell_command_merges_stdout_and_stderr_in_order() {
        // POSIX 路徑：`exec 2>&1; ` 讓兩個串流併成一個 pipe，順序要跟
        // 指令本身寫的順序一致，不是「stdout 全部 + stderr 全部」。
        let shell = vec!["sh".to_string(), "-c".to_string()];
        let child_slot = Arc::new(Mutex::new(None));
        let output = exec_shell_command(
            "echo out1; echo err1 >&2; echo out2",
            &env::temp_dir(),
            &shell,
            &child_slot,
        )
        .unwrap();
        assert_eq!(output.trim_end(), "out1\nerr1\nout2");
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn exec_shell_command_appends_exit_status_on_failure() {
        let shell = vec!["sh".to_string(), "-c".to_string()];
        let child_slot = Arc::new(Mutex::new(None));
        let output = exec_shell_command("exit 3", &env::temp_dir(), &shell, &child_slot).unwrap();
        assert!(
            output.contains("[exit status:"),
            "非零 exit 要附上狀態: {output}"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn shell_quote_rejects_dangerous_characters_on_windows() {
        assert!(shell_quote("safe-value").is_ok());
        assert!(shell_quote("has space").unwrap().starts_with('"'));
        assert!(shell_quote("dangerous & value").is_err());
        assert!(shell_quote("quoted\"value").is_err());
    }
}

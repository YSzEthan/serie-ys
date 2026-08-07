//! `-h` 在真人終端機下會變成互動選單，但這裡的執行環境（`.output()` 天然管線化
//! stdout）等同非 TTY，驗證的正是「非 TTY 下 `-h` 的行為完全不變」這條基準線 ——
//! `run()` 攔截 `ErrorKind::DisplayHelp` 時特意只在 `stdout().is_terminal()` 才
//! 進 wizard，這裡就是在確認那個判斷式擋得住。

use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ysgit"))
        .args(args)
        .output()
        .expect("failed to execute ysgit")
}

#[test]
fn short_and_long_help_are_byte_identical_and_exit_zero() {
    let short = run(&["-h"]);
    let long = run(&["--help"]);

    assert!(short.status.success(), "-h 應該 exit 0");
    assert!(long.status.success(), "--help 應該 exit 0");
    assert_eq!(
        short.stdout, long.stdout,
        "-h 跟 --help 在非 TTY 下應該印出完全一樣的內容"
    );
}

#[test]
fn help_output_lists_every_flag_including_the_new_path_browser() {
    let out = run(&["--help"]);
    let stdout = String::from_utf8(out.stdout).unwrap();

    for needle in [
        "ysgit",
        "-p, --path-browser",
        "-n, --max-count",
        "-o, --order",
        "-g, --graph-width",
        "-c, --compact",
        "-s, --graph-style",
        "-i, --initial-selection",
        "-h, --help",
        "-V, --version",
    ] {
        assert!(
            stdout.contains(needle),
            "--help 輸出缺少 {needle:?}:\n{stdout}"
        );
    }
}

#[test]
fn invalid_flag_still_hints_at_help_and_exits_nonzero() {
    // 這一條專門釘住「-h 改用 try_parse() 攔截後，其餘錯誤路徑一個字都沒動」——
    // 之前的方案（把 help 欄位改成 ArgAction::SetTrue）會讓這個提示消失。
    let out = run(&["--bogus"]);
    let stderr = String::from_utf8(out.stderr).unwrap();

    assert!(!out.status.success());
    assert!(
        stderr.contains("try '--help'"),
        "錯誤訊息應該保留 clap 原生的 --help 提示:\n{stderr}"
    );
}

#[test]
fn help_combined_with_an_invalid_value_still_displays_help_not_the_value_error() {
    // clap 掃到 -h 立刻用 DisplayHelp 中止，不會繼續 parse 到後面的無效值 ——
    // 這是 ArgAction::Help 的原生語意，用 try_parse() 攔截不能改變這個順序。
    let out = run(&["-h", "-o", "not-a-real-order"]);
    assert!(
        out.status.success(),
        "應該顯示 help 並 exit 0，不是回報無效值錯誤"
    );
    assert_eq!(
        out.stdout,
        run(&["--help"]).stdout,
        "印的必須是完整 help 內容，不是靜默 exit 0 什麼都沒印"
    );
    assert!(out.stderr.is_empty(), "不該有任何錯誤輸出");
}

#[test]
fn version_flag_is_untouched() {
    let out = run(&["-V"]);
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.starts_with("ysgit "));
}

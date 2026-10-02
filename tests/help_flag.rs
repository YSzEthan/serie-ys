//! `-h` 在真人終端機下會變成互動選單，但這裡的執行環境（`.output()` 天然管線化
//! stdout）等同非 TTY，驗證的正是「非 TTY 下 `-h` 的行為完全不變」這條基準線 ——
//! `run()` 攔截 `ErrorKind::DisplayHelp` 時特意只在 `stdout().is_terminal()` 才
//! 進 wizard，這裡就是在確認那個判斷式擋得住。

use std::process::{Command, Output};

/// 一律指定 `SERIE_CONFIG_FILE`：設定檔預設跟著執行檔放在 `target/debug/.ysgit.toml`，
/// 開發者用 debug 版精靈選過 English 之後，「預設輸出是繁體中文」這類斷言就會失敗。
fn run_with_config(args: &[&str], config: &str) -> Output {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, config).unwrap();
    Command::new(env!("CARGO_BIN_EXE_ysgit"))
        .args(args)
        .env("SERIE_CONFIG_FILE", &path)
        .output()
        .expect("failed to execute ysgit")
}

fn run(args: &[&str]) -> Output {
    run_with_config(args, "")
}

fn stdout_of(out: Output) -> String {
    assert!(out.status.success(), "應該 exit 0：{out:?}");
    String::from_utf8(out.stdout).unwrap()
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
        "--update-mode",
        "--update-interval",
        "--auto-restart",
        "--release-notes",
        "--auto-fetch",
        "--auto-fetch-interval",
        "--fetch-prune",
        "--locale",
        "--whats-new",
        "-h, --help",
        "-V, --version",
        "-U, --update",
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

#[test]
fn help_is_traditional_chinese_by_default() {
    let stdout = stdout_of(run(&["--help"]));
    assert!(stdout.contains("顯示說明"), "{stdout}");
    assert!(!stdout.contains("Show help"), "{stdout}");
}

#[test]
fn locale_flag_switches_the_help_text() {
    let en = stdout_of(run(&["--locale", "en", "--help"]));
    assert!(en.contains("Show help"), "{en}");
    assert!(en.contains("Interface language"), "{en}");
    assert!(!en.contains("顯示說明"), "{en}");

    let zh = stdout_of(run(&["--locale", "zh-tw", "--help"]));
    assert!(zh.contains("顯示說明"), "{zh}");

    // `--locale=en` 寫法與旗標出現在 `--help` 之後也要生效
    let en2 = stdout_of(run(&["--help", "--locale=en"]));
    assert_eq!(en, en2);
}

#[test]
fn config_locale_switches_help_and_the_flag_overrides_it() {
    let config = "[core.option]\nlocale = \"en\"\n";
    let from_config = stdout_of(run_with_config(&["--help"], config));
    assert!(from_config.contains("Show help"), "{from_config}");

    let overridden = stdout_of(run_with_config(&["--locale", "zh-tw", "--help"], config));
    assert!(overridden.contains("顯示說明"), "{overridden}");
}

#[test]
fn short_and_long_help_stay_identical_in_english() {
    let short = run(&["--locale", "en", "-h"]);
    let long = run(&["--locale", "en", "--help"]);
    assert_eq!(short.stdout, long.stdout);
}

#[test]
fn unknown_locale_is_a_clap_error() {
    let out = run(&["--locale", "xx"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("invalid value"), "{stderr}");
}

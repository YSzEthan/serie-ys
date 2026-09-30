//! i18n 的完整性測試。
//!
//! rust-i18n 的 key 打錯只會在執行期回傳 key 本身，編譯器抓不到，所以用測試補：
//!
//! - `t!` 的第一個參數只能是字面字串（否則掃不到、也無從檢查）
//! - 程式碼裡用到的每個 key 都要有 zh-TW 翻譯
//! - `locales/` 裡的每個 key 都要被程式碼用到（抓死 key）
//! - 每個 key 在 [`crate::Locale`] 的每個語系都要有翻譯
//! - 每個語系的 `%{name}` 佔位符要與呼叫處給的參數同名
//! - 英文翻譯不得含 CJK 字元（抓漏翻與殘留的全形標點）
//!
//! 「每個語系都有翻譯」必須直接解析 TOML 檢查，不能用 `_rust_i18n_try_translate`：
//! `i18n!` 設了 `fallback = "zh-TW"`，缺英文時它會靜默退回中文、永遠回 `Some`。

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use clap::ValueEnum;

use crate::Locale;

const LOCALE: &str = "zh-TW";

/// 所有語系的 rust-i18n tag，來源是 [`Locale`]——新增語系時這裡自動跟著走。
fn locale_tags() -> Vec<&'static str> {
    Locale::value_variants().iter().map(|l| l.code()).collect()
}

/// CJK 字元與全形標點：`U+3000–303F`（CJK 標點）、`U+4E00–9FFF`（漢字）、
/// `U+FF00–FFEF`（全形／半形形式，如 `：，（）／`）。
fn is_cjk(c: char) -> bool {
    matches!(c, '\u{3000}'..='\u{303F}' | '\u{4E00}'..='\u{9FFF}' | '\u{FF00}'..='\u{FFEF}')
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// 去掉整行 `//` 註解，行號照樣保留。行尾註解不處理：字串裡的 `//`（例如 URL）
/// 與 `'"'` 這類 char literal 會讓「是否在字串內」的判斷不可靠，而目前 `src/`
/// 沒有行尾註解含 `t!(`。
fn strip_comment_lines(src: &str) -> String {
    src.lines()
        .map(|l| {
            if l.trim_start().starts_with("//") {
                ""
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 一次 `t!(...)` 呼叫：key 與具名參數。
struct Call {
    key: String,
    args: BTreeSet<String>,
    at: String,
}

/// 掃 `t!("key", name = ..., ...)`。`t!(` 後不是字面字串就直接 panic。
fn collect_calls() -> Vec<Call> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rs(&root, &mut files);
    files.sort();

    let mut calls = Vec::new();
    for file in files {
        if file.file_name().is_some_and(|n| n == "i18n.rs") {
            continue;
        }
        let rel = file.strip_prefix(&root).unwrap().display().to_string();
        let src = strip_comment_lines(&fs::read_to_string(&file).unwrap());
        let bytes = src.as_bytes();
        let mut from = 0;
        while let Some(i) = src[from..].find("t!(") {
            let pos = from + i;
            from = pos + 3;
            // 前一個字元是識別字元 → 是 format!( / assert!( 之類，不是 t!
            if pos > 0 && (bytes[pos - 1].is_ascii_alphanumeric() || bytes[pos - 1] == b'_') {
                continue;
            }
            let line = src[..pos].matches('\n').count() + 1;
            let at = format!("{rel}:{line}");
            let rest = src[from..].trim_start();
            assert!(rest.starts_with('"'), "{at}: t! 的第一個參數必須是字面字串");
            // key 是 `a.b.c` 形式的識別字，不會含跳脫字元，找下一個 `"` 即可
            let (key, after_key) = rest[1..]
                .split_once('"')
                .unwrap_or_else(|| panic!("{at}: 字串沒有結尾"));
            let key = key.to_string();

            // 具名參數：到這個 t!( 的對應右括號為止，找 `ident =`（排除 `==`；`name => v` 也是合法寫法，由 rust-i18n 接受）
            let args_src = matching_paren_body(after_key);
            calls.push(Call {
                key,
                args: named_args(args_src),
                at,
            });
        }
    }
    calls
}

/// `lit_rest` 是 key 字串之後的內容，回傳到對應 `)` 之前的部分。
fn matching_paren_body(lit_rest: &str) -> &str {
    let mut depth = 1;
    let mut in_str = false;
    let mut escaped = false;
    for (i, c) in lit_rest.char_indices() {
        if in_str {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    return &lit_rest[..i];
                }
            }
            _ => {}
        }
    }
    lit_rest
}

/// 只看最上層（括號深度 0）逗號後的 `ident =`。
fn named_args(body: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut depth = 0;
    let mut in_str = false;
    let mut escaped = false;
    let mut seg_start = 0;
    let mut segs = Vec::new();
    for (i, c) in body.char_indices() {
        if in_str {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                segs.push(&body[seg_start..i]);
                seg_start = i + 1;
            }
            _ => {}
        }
    }
    segs.push(&body[seg_start..]);
    for seg in segs {
        let seg = seg.trim();
        let ident: String = seg
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        let rest = seg[ident.len()..].trim_start();
        let is_named_arg = !ident.is_empty() && rest.starts_with('=') && !rest.starts_with("==");
        if is_named_arg && ident != "locale" {
            out.insert(ident);
        }
    }
    out
}

/// 把 `locales/*.toml` 攤平成 key → (語系 tag → 翻譯值)。
///
/// 葉節點 = 值全是字串的表；它的鍵必須全是已知的語系 tag，否則直接 panic——
/// 抓 `en = "…"`、`zh-tw = "…"` 這類打錯的 tag（打錯的話那個語系會整個靜默缺漏）。
fn locale_entries() -> BTreeMap<String, BTreeMap<String, String>> {
    fn walk(
        prefix: &str,
        table: &toml::Table,
        tags: &[&str],
        out: &mut BTreeMap<String, BTreeMap<String, String>>,
    ) {
        for (k, v) in table {
            let toml::Value::Table(t) = v else { continue };
            let key = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{prefix}.{k}")
            };
            if t.values().all(|v| v.is_str()) {
                let mut values = BTreeMap::new();
                for (tag, v) in t {
                    assert!(
                        tags.contains(&tag.as_str()),
                        "{key}: 未知的語系 tag {tag:?}（已知：{tags:?}）"
                    );
                    values.insert(tag.clone(), v.as_str().unwrap().to_string());
                }
                out.insert(key, values);
            } else {
                walk(&key, t, tags, out);
            }
        }
    }

    let tags = locale_tags();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
    let mut out = BTreeMap::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "toml") {
            let table: toml::Table = fs::read_to_string(&path).unwrap().parse().unwrap();
            walk("", &table, &tags, &mut out);
        }
    }
    out
}

/// 翻譯值裡的 `%{name}` 佔位符名稱。
fn placeholders(value: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = value;
    while let Some(i) = rest.find("%{") {
        rest = &rest[i + 2..];
        if let Some(j) = rest.find('}') {
            out.insert(rest[..j].trim().to_string());
            rest = &rest[j + 1..];
        }
    }
    out
}

#[test]
fn every_used_key_has_a_translation() {
    for call in collect_calls() {
        assert!(
            crate::_rust_i18n_try_translate(LOCALE, &call.key).is_some(),
            "{}: 缺少 {LOCALE} 翻譯：{}",
            call.at,
            call.key
        );
    }
}

#[test]
fn every_locale_key_is_used() {
    let used: BTreeSet<String> = collect_calls().into_iter().map(|c| c.key).collect();
    let dead: Vec<String> = locale_entries()
        .into_keys()
        .filter(|k| !used.contains(k))
        .collect();
    assert!(dead.is_empty(), "locales/ 裡沒被使用的 key：{dead:#?}");
}

#[test]
fn every_key_is_translated_in_every_locale() {
    let tags = locale_tags();
    let mut missing = Vec::new();
    for (key, values) in locale_entries() {
        for tag in &tags {
            if !values.contains_key(*tag) {
                missing.push(format!("{key}（缺 {tag}）"));
            }
        }
    }
    assert!(missing.is_empty(), "缺少翻譯：{missing:#?}");
}

#[test]
fn placeholders_match_call_arguments_in_every_locale() {
    let entries = locale_entries();
    for call in collect_calls() {
        let Some(values) = entries.get(&call.key) else {
            continue;
        };
        for (tag, value) in values {
            assert_eq!(
                placeholders(value),
                call.args,
                "{}: {} 的 {tag} 佔位符與呼叫參數不一致",
                call.at,
                call.key
            );
        }
    }
}

#[test]
fn english_values_contain_no_cjk() {
    let en = Locale::En.code();
    let mut bad = Vec::new();
    for (key, values) in locale_entries() {
        if let Some(v) = values.get(en) {
            if v.chars().any(is_cjk) {
                bad.push(format!("{key}: {v:?}"));
            }
        }
    }
    assert!(bad.is_empty(), "英文翻譯含 CJK 字元或全形標點：{bad:#?}");
}

/// 各語系之間的前後空白要一致：標題的前後空格、子句樣板的前導分隔符，版面與組句
/// 都靠它。唯一的例外是「全形冒號／問號後面的空格」：中文的 `：`、`？` 自帶字距，
/// 後面可以直接接內容；英文的 `:`、`?` 沒有，所以英文值可以（且通常必須）多一個尾端
/// 空格，例如 `選擇 branch：` ↔ `Select branch: `。
#[test]
fn surrounding_whitespace_is_consistent_across_locales() {
    let lead = |s: &str| s.len() - s.trim_start().len();
    let trail = |s: &str| s.len() - s.trim_end().len();
    let zh_tag = Locale::ZhTw.code();
    let mut bad = Vec::new();
    for (key, values) in locale_entries() {
        let Some(zh) = values.get(zh_tag) else {
            continue;
        };
        let zh_ends_fullwidth = zh.trim_end().ends_with(['：', '？']);
        for (tag, v) in &values {
            if tag == zh_tag {
                continue;
            }
            let trailing_ok = trail(v) == trail(zh)
                || (zh_ends_fullwidth
                    && trail(zh) == 0
                    && trail(v) == 1
                    && v.trim_end().ends_with([':', '?']));
            if lead(v) != lead(zh) || !trailing_ok {
                bad.push(format!("{key}: {zh_tag}={zh:?} {tag}={v:?}"));
            }
        }
    }
    assert!(bad.is_empty(), "各語系前後空白不一致：{bad:#?}");
}

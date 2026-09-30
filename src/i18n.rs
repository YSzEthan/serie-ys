//! i18n 的完整性測試。
//!
//! rust-i18n 的 key 打錯只會在執行期回傳 key 本身，編譯器抓不到，所以用測試補：
//!
//! - `t!` 的第一個參數只能是字面字串（否則掃不到、也無從檢查）
//! - 程式碼裡用到的每個 key 都要有 zh-TW 翻譯
//! - `locales/` 裡的每個 key 都要被程式碼用到（抓死 key）
//! - 翻譯值裡的 `%{name}` 佔位符要與呼叫處給的參數同名

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

const LOCALE: &str = "zh-TW";

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

/// 把 `locales/*.toml` 攤平成 key → zh-TW 值。
fn locale_entries() -> BTreeMap<String, String> {
    fn walk(prefix: &str, table: &toml::Table, out: &mut BTreeMap<String, String>) {
        for (k, v) in table {
            let toml::Value::Table(t) = v else { continue };
            let key = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{prefix}.{k}")
            };
            if let Some(toml::Value::String(s)) = t.get(LOCALE) {
                out.insert(key, s.clone());
            } else {
                walk(&key, t, out);
            }
        }
    }

    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
    let mut out = BTreeMap::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "toml") {
            let table: toml::Table = fs::read_to_string(&path).unwrap().parse().unwrap();
            walk("", &table, &mut out);
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
fn placeholders_match_call_arguments() {
    let entries = locale_entries();
    for call in collect_calls() {
        let Some(value) = entries.get(&call.key) else {
            continue;
        };
        assert_eq!(
            placeholders(value),
            call.args,
            "{}: {} 的佔位符與呼叫參數不一致",
            call.at,
            call.key
        );
    }
}

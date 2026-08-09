//! 把 `git diff --color=never` 的純文字輸出解析成可渲染的 `Line`，自己決定行號、
//! header 與行內差異高亮，不再假手 git 的 `--color=always`。
//!
//! `git --word-diff` 這條捷徑被排除過：它以空白切 word，本專案註解全是中文
//! （沒有空白），整段會被當成一個 word 全標；而且它把 `-`/`+` 行合併成一團，
//! 行號無從標起。所以走 delta／tig／lazygit 的路——保留 `-`/`+` 兩行，行內只
//! 反白真正改掉的字元。

use std::ops::Range;

use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use crate::color::ColorTheme;

/// 單行超過這個字元數就不做行內高亮（整行維持整行標色）——避免單一超長行的
/// LCS backtrack（最壞 O(n²)）拖慢同步跑在按鍵路徑上的 `sync_diff`。
const MAX_EMPHASIS_LINE_CHARS: usize = 1000;
/// 一個檔案的行內高亮總預算（LCS DP 的 cell 數，`old.len() * new.len()` 累加）。
/// 超過之後不是砍掉最後一對的計算結果，而是整檔退回整行標色——用完就用完，
/// 不需要每對重新判斷「還夠不夠」。
const PER_FILE_CHAR_DIFF_BUDGET: usize = 2_000_000;
/// 相似度低於這個門檻就不做行內高亮，整行標色——錯配的 `-`/`+` 對子相似度
/// 自然低，「不猜」的目的用這個數字就達成，不需要另外判斷「像不像同一行」。
const SIMILARITY_THRESHOLD: f64 = 0.5;
/// 兩段高亮區間之間相隔在這麼多個未變字元以內就合併成一段——避免逐字元
/// LCS 在兩行不相干的程式碼上，因為空白或單一字母巧合配對，標出一堆
/// 「彩色紙屑」般的細碎區間。
const MERGE_GAP: usize = 3;

/// 一份已解析並上色的 diff。`hunk_starts` 是每個 hunk header 在 `lines` 裡的
/// 索引，供 `]`/`[` 跳轉與 title 的 `hunk n/m` 共用同一份資料。
#[derive(Debug, Default)]
pub struct RenderedDiff {
    pub lines: Vec<Line<'static>>,
    pub hunk_starts: Vec<usize>,
    pub notes: DiffNotes,
}

/// 裝的是「異常時才值得在 title 上出現一次」的東西。變更類型、路徑、增刪
/// 統計都交給呼叫端從 `FileChange`／`DiffTarget` 取——那邊才是真相來源，
/// 解析 `--- a/`／`+++ b/` 不可靠（`diff.noprefix`／`diff.mnemonicPrefix`
/// 會改那兩行的形狀），數 `+`/`-` 行數也會跟被截斷的輸出對不上。
///
/// `binary`／`mode` 由 `parse` 從文字本身判斷；`truncated` 不是文字判斷得
/// 出來的（`parse` 只看得到已經被砍過的內容，分不出「本來就短」跟「被砍
/// 過」），`parse` 一律留 `false`，由呼叫端拿到 `file_diff` 回傳的截斷旗標
/// 後自行補上——放進同一個 struct 只是讓 render 端一次讀完所有旗標。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffNotes {
    pub binary: bool,
    pub mode: Option<ModeNote>,
    pub truncated: bool,
}

/// mode 變更只在「異常」時才值得顯示：一般檔案的 100644 → 100644 沒有新聞。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeNote {
    Symlink,
    Executable,
    ModeChangedToExecutable,
    ModeChangedFromExecutable,
}

impl ModeNote {
    pub fn label(self) -> &'static str {
        match self {
            ModeNote::Symlink => "symlink",
            ModeNote::Executable => "executable",
            ModeNote::ModeChangedToExecutable => "mode → executable",
            ModeNote::ModeChangedFromExecutable => "mode → not executable",
        }
    }
}

/// 解析並上色。`tab_width` 沿用 `core.user_command.tab_width`（跟
/// `ansi_output_to_lines` 借同一顆設定），因為 ratatui 畫不出 tab，必須在算
/// 行內高亮區間**之前**展開，否則區間會跟展開後的內容對不上。
pub fn parse(text: &str, tab_width: u16, theme: &ColorTheme) -> RenderedDiff {
    let (mut rows, notes) = parse_rows(text, tab_width);
    apply_emphasis(&mut rows);
    let gutter_width = gutter_width(&rows);
    let (lines, hunk_starts) = render_rows(&rows, gutter_width, theme);
    RenderedDiff {
        lines,
        hunk_starts,
        notes,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowKind {
    HunkHeader,
    Context,
    Added,
    Removed,
    /// `\ No newline at end of file`：不是一行內容，是對上一行的附註，
    /// 不佔行號、不參與行內高亮配對。
    NoNewline,
}

#[derive(Debug, Clone)]
struct DiffRow {
    kind: RowKind,
    line_no: Option<usize>,
    content: String,
    /// 字元級（不是位元組）區間——中文等多位元組字元一個字算一格，
    /// 渲染時逐字元分組成 span，不需要處理位元組邊界。
    emphasis: Vec<Range<usize>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileMode {
    Regular,
    Executable,
    Symlink,
}

/// 這份 diff 用哪一側的行號當 gutter。由 `+++ ` header 決定：新側不存在
/// （整檔刪除，`+++ /dev/null`）才切到舊側，其餘一律新側——這是唯一需要
/// 檢查的訊號，不需要另外掃一遍 row 猜「這份 diff 有沒有 `+`」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NumberSide {
    Old,
    New,
}

fn parse_rows(text: &str, tab_width: u16) -> (Vec<DiffRow>, DiffNotes) {
    let mut rows = Vec::new();
    let mut notes = DiffNotes::default();
    let mut old_mode: Option<FileMode> = None;
    let mut new_mode: Option<FileMode> = None;
    let mut number_side = NumberSide::New;
    let mut display_line = 0usize;
    let mut in_hunks = false;
    let tab_spaces = " ".repeat(tab_width as usize);

    for raw_line in text.lines() {
        if raw_line.starts_with("@@ ") {
            in_hunks = true;
            if let Some((old_start, new_start)) = parse_hunk_header(raw_line) {
                display_line = match number_side {
                    NumberSide::New => new_start,
                    NumberSide::Old => old_start,
                };
            }
            rows.push(DiffRow {
                kind: RowKind::HunkHeader,
                line_no: None,
                content: raw_line.to_string(),
                emphasis: Vec::new(),
            });
            continue;
        }

        if !in_hunks {
            if let Some(rest) = raw_line.strip_prefix("old mode ") {
                old_mode = parse_mode(rest);
            } else if let Some(rest) = raw_line.strip_prefix("new mode ") {
                new_mode = parse_mode(rest);
            } else if let Some(rest) = raw_line.strip_prefix("new file mode ") {
                new_mode = parse_mode(rest);
            } else if let Some(rest) = raw_line.strip_prefix("deleted file mode ") {
                old_mode = parse_mode(rest);
            } else if raw_line == "+++ /dev/null" {
                number_side = NumberSide::Old;
            } else if raw_line.starts_with("Binary files ") && raw_line.ends_with(" differ") {
                notes.binary = true;
            }
            // 其餘 header 行（diff --git／index／--- a/／+++ b/ 非 /dev/null）
            // 不含這裡用得到的資訊，略過。
            continue;
        }

        if raw_line == r"\ No newline at end of file" {
            rows.push(DiffRow {
                kind: RowKind::NoNewline,
                line_no: None,
                content: raw_line.to_string(),
                emphasis: Vec::new(),
            });
            continue;
        }

        let mut chars = raw_line.chars();
        let Some(marker) = chars.next() else {
            continue;
        };
        let kind = match marker {
            ' ' => RowKind::Context,
            '+' => RowKind::Added,
            '-' => RowKind::Removed,
            // 理論上不會出現的行（不認得的前綴），忽略而非 panic。
            _ => continue,
        };
        let body = &raw_line[marker.len_utf8()..];
        let content = body.replace('\t', &tab_spaces);
        let line_no = match (number_side, kind) {
            (NumberSide::New, RowKind::Removed) | (NumberSide::Old, RowKind::Added) => None,
            _ => {
                let n = display_line;
                display_line += 1;
                Some(n)
            }
        };
        rows.push(DiffRow {
            kind,
            line_no,
            content,
            emphasis: Vec::new(),
        });
    }

    notes.mode = mode_note(old_mode, new_mode);
    (rows, notes)
}

/// 解析 `@@ -a[,b] +c[,d] @@ 選配的 context`，只取兩個起始行號——後面的
/// 計數欄位在這裡用不到（行號是逐行累加出來的，不是靠宣告的計數驗證）。
fn parse_hunk_header(line: &str) -> Option<(usize, usize)> {
    let rest = line.strip_prefix("@@ -")?;
    let (old_part, rest) = rest.split_once(' ')?;
    let new_part = rest.strip_prefix('+')?;
    let new_part = new_part.split(' ').next()?;
    let old_start: usize = old_part.split(',').next()?.parse().ok()?;
    let new_start: usize = new_part.split(',').next()?.parse().ok()?;
    Some((old_start, new_start))
}

fn parse_mode(raw: &str) -> Option<FileMode> {
    match raw.trim() {
        "120000" => Some(FileMode::Symlink),
        m if m.ends_with("755") => Some(FileMode::Executable),
        m if m.ends_with("644") => Some(FileMode::Regular),
        _ => None,
    }
}

fn mode_note(old: Option<FileMode>, new: Option<FileMode>) -> Option<ModeNote> {
    use FileMode::{Executable, Regular, Symlink};
    match (old, new) {
        (_, Some(Symlink)) => Some(ModeNote::Symlink),
        (None, Some(Executable)) => Some(ModeNote::Executable),
        (Some(Regular) | Some(Symlink), Some(Executable)) => {
            Some(ModeNote::ModeChangedToExecutable)
        }
        (Some(Executable), Some(Regular)) => Some(ModeNote::ModeChangedFromExecutable),
        _ => None,
    }
}

/// 找連續的 `-` 群組與緊接的 `+` 群組，i 對 i 配到 `min(n, m)`。不是「行數
/// 相等才配」——改 3 行順手加 1 行（3 減 4 加）是最常見的編輯形狀，那條
/// 規則會讓它整段沒有高亮。相似度太低的對子（見 `SIMILARITY_THRESHOLD`）
/// 自然放棄行內高亮，「不猜」的目的一樣達成。
fn apply_emphasis(rows: &mut [DiffRow]) {
    let mut budget = PER_FILE_CHAR_DIFF_BUDGET;
    let mut i = 0;
    while i < rows.len() {
        if rows[i].kind != RowKind::Removed {
            i += 1;
            continue;
        }
        let removed_start = i;
        let mut j = i;
        while j < rows.len() && rows[j].kind == RowKind::Removed {
            j += 1;
        }
        let added_start = j;
        let mut k = added_start;
        while k < rows.len() && rows[k].kind == RowKind::Added {
            k += 1;
        }
        let pair_count = (j - removed_start).min(k - added_start);

        for p in 0..pair_count {
            if budget == 0 {
                break;
            }
            let old_chars: Vec<char> = rows[removed_start + p].content.chars().collect();
            let new_chars: Vec<char> = rows[added_start + p].content.chars().collect();
            if old_chars.len() > MAX_EMPHASIS_LINE_CHARS
                || new_chars.len() > MAX_EMPHASIS_LINE_CHARS
            {
                continue;
            }
            let cost = old_chars.len() * new_chars.len();
            budget = budget.saturating_sub(cost);

            let (old_unmatched, new_unmatched) = char_lcs_diff(&old_chars, &new_chars);
            let max_len = old_chars.len().max(new_chars.len()).max(1);
            let matched = old_chars.len() - old_unmatched.len();
            let similarity = matched as f64 / max_len as f64;
            if similarity < SIMILARITY_THRESHOLD {
                continue;
            }
            rows[removed_start + p].emphasis = merge_ranges(&old_unmatched, MERGE_GAP);
            rows[added_start + p].emphasis = merge_ranges(&new_unmatched, MERGE_GAP);
        }

        i = k;
    }
}

/// 對兩個字元序列做字元級 LCS，回傳兩側「不在 LCS 裡」的字元索引（皆為
/// 遞增排序）——這些就是真正改動的部分。`old`／`new` 各自最多
/// `MAX_EMPHASIS_LINE_CHARS` 字元，DP table 開好開滿（最壞 1000×1000）
/// 換取一次 backtrack 就拿到實際配對位置；真正需要控管的是總運算量，
/// 由 `PER_FILE_CHAR_DIFF_BUDGET` 在呼叫端把關，這裡不重複判斷。
fn char_lcs_diff(old: &[char], new: &[char]) -> (Vec<usize>, Vec<usize>) {
    let (n, m) = (old.len(), new.len());
    let mut dp = vec![vec![0u16; m + 1]; n + 1];
    for i in 1..=n {
        for j in 1..=m {
            dp[i][j] = if old[i - 1] == new[j - 1] {
                dp[i - 1][j - 1] + 1
            } else {
                dp[i - 1][j].max(dp[i][j - 1])
            };
        }
    }

    let mut old_unmatched = Vec::new();
    let mut new_unmatched = Vec::new();
    let (mut i, mut j) = (n, m);
    while i > 0 && j > 0 {
        if old[i - 1] == new[j - 1] {
            i -= 1;
            j -= 1;
        } else if dp[i - 1][j] >= dp[i][j - 1] {
            i -= 1;
            old_unmatched.push(i);
        } else {
            j -= 1;
            new_unmatched.push(j);
        }
    }
    while i > 0 {
        i -= 1;
        old_unmatched.push(i);
    }
    while j > 0 {
        j -= 1;
        new_unmatched.push(j);
    }
    old_unmatched.reverse();
    new_unmatched.reverse();
    (old_unmatched, new_unmatched)
}

/// 把遞增排序的字元索引合併成區間，相隔 `max_gap` 個字元以內的合併成一段。
fn merge_ranges(indices: &[usize], max_gap: usize) -> Vec<Range<usize>> {
    let mut ranges: Vec<Range<usize>> = Vec::new();
    for &idx in indices {
        match ranges.last_mut() {
            Some(last) if idx <= last.end + max_gap => last.end = idx + 1,
            _ => ranges.push(idx..idx + 1),
        }
    }
    ranges
}

/// gutter 寬度跟著這份 diff 出現過的最大行號動態算，不寫死——小檔省空間，
/// 破萬行的檔案也不會把版面擠爆。
fn gutter_width(rows: &[DiffRow]) -> usize {
    let max_no = rows.iter().filter_map(|r| r.line_no).max().unwrap_or(0);
    max_no.to_string().len().max(3)
}

fn render_rows(
    rows: &[DiffRow],
    gutter_width: usize,
    theme: &ColorTheme,
) -> (Vec<Line<'static>>, Vec<usize>) {
    let mut lines = Vec::with_capacity(rows.len());
    let mut hunk_starts = Vec::new();
    let gutter_blank = " ".repeat(gutter_width);

    for row in rows {
        match row.kind {
            RowKind::HunkHeader => {
                hunk_starts.push(lines.len());
                lines.push(Line::from(Span::styled(
                    row.content.clone(),
                    Style::default()
                        .fg(theme.divider_fg)
                        .add_modifier(Modifier::BOLD),
                )));
            }
            RowKind::NoNewline => {
                lines.push(Line::from(Span::styled(
                    row.content.clone(),
                    Style::default().fg(theme.divider_fg),
                )));
            }
            RowKind::Context | RowKind::Added | RowKind::Removed => {
                let gutter = match row.line_no {
                    Some(n) => format!("{n:>gutter_width$}"),
                    None => gutter_blank.clone(),
                };
                let (marker, style) = match row.kind {
                    RowKind::Added => ("+", Style::default().fg(theme.detail_file_change_add_fg)),
                    RowKind::Removed => {
                        ("-", Style::default().fg(theme.detail_file_change_delete_fg))
                    }
                    _ => (" ", Style::default()),
                };
                let mut spans = vec![
                    Span::styled(
                        format!("{gutter} │ "),
                        Style::default().fg(theme.divider_fg),
                    ),
                    Span::styled(marker, style),
                ];
                spans.extend(emphasized_spans(&row.content, &row.emphasis, style));
                lines.push(Line::from(spans));
            }
        }
    }

    (lines, hunk_starts)
}

/// 依 `emphasis` 區間把整行內容切成多個 span：命中區間內的字元疊
/// `REVERSED`，維持傳入的既有紅/綠 style（跟著使用者調色盤走，不新增任何
/// theme 欄位）。`emphasis` 保證遞增排序且不重疊（`merge_ranges` 的輸出），
/// 直接沿區間切片就好，不需要逐字元判斷「這格屬不屬於某個區間」。
fn emphasized_spans(
    content: &str,
    emphasis: &[Range<usize>],
    base_style: Style,
) -> Vec<Span<'static>> {
    if emphasis.is_empty() {
        return vec![Span::styled(content.to_string(), base_style)];
    }
    let emph_style = base_style.add_modifier(Modifier::REVERSED);
    let chars: Vec<char> = content.chars().collect();

    let mut spans = Vec::new();
    let mut pos = 0;
    for r in emphasis {
        let end = r.end.min(chars.len());
        if pos < r.start {
            spans.push(Span::styled(
                chars[pos..r.start].iter().collect::<String>(),
                base_style,
            ));
        }
        spans.push(Span::styled(
            chars[r.start..end].iter().collect::<String>(),
            emph_style,
        ));
        pos = end;
    }
    if pos < chars.len() {
        spans.push(Span::styled(
            chars[pos..].iter().collect::<String>(),
            base_style,
        ));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> ColorTheme {
        ColorTheme::default()
    }

    fn row_kinds(diff: &str) -> Vec<(RowKind, Option<usize>, String)> {
        let (rows, _) = parse_rows(diff, 4);
        rows.into_iter()
            .map(|r| (r.kind, r.line_no, r.content))
            .collect()
    }

    #[test]
    fn line_numbers_increment_and_removed_lines_are_blank() {
        let diff = "@@ -5,3 +5,3 @@\n context\n-old\n+new\n context2\n";
        let rows = row_kinds(diff);
        assert_eq!(
            rows,
            vec![
                (RowKind::HunkHeader, None, "@@ -5,3 +5,3 @@".into()),
                (RowKind::Context, Some(5), "context".into()),
                (RowKind::Removed, None, "old".into()),
                (RowKind::Added, Some(6), "new".into()),
                (RowKind::Context, Some(7), "context2".into()),
            ]
        );
    }

    #[test]
    fn no_newline_marker_does_not_shift_following_line_numbers() {
        // `\ No newline at end of file` 是獨立一行（對上一行的附註），
        // 不能被誤當成內容行，否則它之後的行號會整段錯位。
        let diff = "@@ -1,2 +1,2 @@\n-old\n\\ No newline at end of file\n+new\n";
        let rows = row_kinds(diff);
        assert_eq!(
            rows,
            vec![
                (RowKind::HunkHeader, None, "@@ -1,2 +1,2 @@".into()),
                (RowKind::Removed, None, "old".into()),
                (
                    RowKind::NoNewline,
                    None,
                    "\\ No newline at end of file".into()
                ),
                (RowKind::Added, Some(1), "new".into()),
            ]
        );
    }

    #[test]
    fn whole_file_deletion_numbers_by_old_side() {
        let diff = "--- a/gone.txt\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-line1\n-line2\n";
        let rows = row_kinds(diff);
        assert_eq!(
            rows,
            vec![
                (RowKind::HunkHeader, None, "@@ -1,2 +0,0 @@".into()),
                (RowKind::Removed, Some(1), "line1".into()),
                (RowKind::Removed, Some(2), "line2".into()),
            ]
        );
    }

    #[test]
    fn tab_is_expanded_before_line_becomes_content() {
        let diff = "@@ -1,1 +1,1 @@\n+\tindented\n";
        let (rows, _) = parse_rows(diff, 4);
        assert_eq!(rows[1].content, "    indented");
    }

    #[test]
    fn gutter_width_grows_with_the_largest_line_number() {
        let diff = "@@ -1,1 +12000,1 @@\n context\n";
        let (rows, _) = parse_rows(diff, 4);
        assert_eq!(gutter_width(&rows), 5);

        let small = "@@ -1,1 +1,1 @@\n context\n";
        let (rows, _) = parse_rows(small, 4);
        assert_eq!(gutter_width(&rows), 3);
    }

    #[test]
    fn binary_and_mode_notes_are_captured() {
        let diff = "diff --git a/logo.png b/logo.png\nindex abc..def 100644\nBinary files a/logo.png and b/logo.png differ\n";
        let (_, notes) = parse_rows(diff, 4);
        assert!(notes.binary);
        assert_eq!(notes.mode, None);

        let diff = "diff --git a/run.sh b/run.sh\nold mode 100644\nnew mode 100755\nindex abc..def 100755\n";
        let (_, notes) = parse_rows(diff, 4);
        assert_eq!(notes.mode, Some(ModeNote::ModeChangedToExecutable));

        let diff = "diff --git a/run.sh b/run.sh\nnew file mode 100755\nindex 000..abc\n--- /dev/null\n+++ b/run.sh\n@@ -0,0 +1,1 @@\n+echo hi\n";
        let (_, notes) = parse_rows(diff, 4);
        assert_eq!(notes.mode, Some(ModeNote::Executable));

        let diff = "diff --git a/link b/link\nnew file mode 120000\nindex 000..abc\n--- /dev/null\n+++ b/link\n@@ -0,0 +1,1 @@\n+target\n";
        let (_, notes) = parse_rows(diff, 4);
        assert_eq!(notes.mode, Some(ModeNote::Symlink));
    }

    #[test]
    fn three_minus_four_plus_still_gets_inline_emphasis_for_paired_lines() {
        // 「行數相等才配」這條規則被否決過：改 3 行順手加 1 行是最常見的
        // 編輯形狀，前三對照樣要有行內高亮，第四個新增行沒有配對對象。
        // 每對故意只換一個字元（而非純插入）——純插入時舊側本來就該是
        // 空高亮（沒有任何字元被「改掉」，只是新側多了東西），那不是 bug，
        // 用「換一個字元」才測得出兩側都該有高亮。
        let mut rows = vec![
            DiffRow {
                kind: RowKind::Removed,
                line_no: None,
                content: "let a = 1;".into(),
                emphasis: Vec::new(),
            },
            DiffRow {
                kind: RowKind::Removed,
                line_no: None,
                content: "let b = 2;".into(),
                emphasis: Vec::new(),
            },
            DiffRow {
                kind: RowKind::Removed,
                line_no: None,
                content: "let c = 3;".into(),
                emphasis: Vec::new(),
            },
            DiffRow {
                kind: RowKind::Added,
                line_no: Some(1),
                content: "let a = 9;".into(),
                emphasis: Vec::new(),
            },
            DiffRow {
                kind: RowKind::Added,
                line_no: Some(2),
                content: "let b = 8;".into(),
                emphasis: Vec::new(),
            },
            DiffRow {
                kind: RowKind::Added,
                line_no: Some(3),
                content: "let c = 7;".into(),
                emphasis: Vec::new(),
            },
            DiffRow {
                kind: RowKind::Added,
                line_no: Some(4),
                content: "let d = 4;".into(),
                emphasis: Vec::new(),
            },
        ];
        apply_emphasis(&mut rows);
        assert!(!rows[0].emphasis.is_empty(), "第一對舊側應該有行內高亮");
        assert!(!rows[1].emphasis.is_empty(), "第二對舊側應該有行內高亮");
        assert!(!rows[2].emphasis.is_empty(), "第三對舊側應該有行內高亮");
        assert!(!rows[3].emphasis.is_empty(), "第一對新側應該有行內高亮");
        assert!(!rows[4].emphasis.is_empty(), "第二對新側應該有行內高亮");
        assert!(!rows[5].emphasis.is_empty(), "第三對新側應該有行內高亮");
        assert!(
            rows[6].emphasis.is_empty(),
            "多出來沒有配對對象的新增行不該有高亮"
        );
    }

    #[test]
    fn unrelated_lines_are_not_forced_into_emphasis() {
        let mut rows = vec![
            DiffRow {
                kind: RowKind::Removed,
                line_no: None,
                content: "fn foo() -> Result<(), Error> { Ok(()) }".into(),
                emphasis: Vec::new(),
            },
            DiffRow {
                kind: RowKind::Added,
                line_no: Some(1),
                content: "struct Bar { x: u32, y: u32 }".into(),
                emphasis: Vec::new(),
            },
        ];
        apply_emphasis(&mut rows);
        assert!(
            rows[0].emphasis.is_empty() && rows[1].emphasis.is_empty(),
            "兩行完全不相干時不該猜出行內高亮"
        );
    }

    #[test]
    fn overly_long_lines_skip_emphasis() {
        let long_old = "a".repeat(MAX_EMPHASIS_LINE_CHARS + 1);
        let long_new = "b".repeat(MAX_EMPHASIS_LINE_CHARS + 1);
        let mut rows = vec![
            DiffRow {
                kind: RowKind::Removed,
                line_no: None,
                content: long_old,
                emphasis: Vec::new(),
            },
            DiffRow {
                kind: RowKind::Added,
                line_no: Some(1),
                content: long_new,
                emphasis: Vec::new(),
            },
        ];
        apply_emphasis(&mut rows);
        assert!(rows[0].emphasis.is_empty());
        assert!(rows[1].emphasis.is_empty());
    }

    #[test]
    fn merge_ranges_joins_nearby_gaps_but_not_far_ones() {
        // 0,1 相鄰；4 跟前段相隔 2 個字元（<=3）合併；10 跟前段相隔 5 個字元不合併。
        let ranges = merge_ranges(&[0, 1, 4, 10], 3);
        assert_eq!(ranges, vec![0..5, 10..11]);
    }

    #[test]
    fn chinese_comment_only_highlights_the_changed_character() {
        let old: Vec<char> = "不再帶 PR 專屬片段".chars().collect();
        let new: Vec<char> = "不再帶 PR 專用片段".chars().collect();
        let (old_unmatched, new_unmatched) = char_lcs_diff(&old, &new);
        assert_eq!(old_unmatched.len(), 1);
        assert_eq!(new_unmatched.len(), 1);
        assert_eq!(old[old_unmatched[0]], '屬');
        assert_eq!(new[new_unmatched[0]], '用');
    }

    #[test]
    fn parse_renders_lines_and_hunk_starts() {
        let diff = "@@ -1,1 +1,1 @@\n-old\n+new\n@@ -10,1 +10,1 @@\n context\n";
        let rendered = parse(diff, 4, &theme());
        assert_eq!(rendered.hunk_starts.len(), 2);
        assert_eq!(rendered.hunk_starts[0], 0);
        assert_eq!(rendered.lines.len(), 5);
    }
}

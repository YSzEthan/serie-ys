use std::{
    borrow::Cow,
    env,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};

use garde::Validate;
use rustc_hash::FxHashMap;
use serde::Deserialize;
use smart_default::SmartDefault;
use umbra::optional;

use crate::{
    color::{ColorTheme, OptionalColorTheme},
    keybind::KeyBind,
    update::{AutoRestart, UpdateMode, MAX_INTERVAL_HOURS, MIN_INTERVAL_HOURS},
    CommitOrderType, CompactType, GraphStyle, GraphWidthType, InitialSelection, Result,
};

const CONFIG_FILE_NAME: &str = ".ysgit.toml";
const CONFIG_FILE_ENV_NAME: &str = "SERIE_CONFIG_FILE";

pub fn load() -> Result<(CoreConfig, UiConfig, ColorTheme, Option<KeyBind>)> {
    let config = match config_file_path_from_env() {
        Some(user_path) => {
            if !user_path.exists() {
                let msg = format!(
                    "Config file specified by ${CONFIG_FILE_ENV_NAME} environment variable not found: {}",
                    user_path.display()
                );
                return Err(msg.into());
            }
            read_config_from_path(&user_path)
        }
        None => match default_config_file_path() {
            Some(default_path) if default_path.exists() => read_config_from_path(&default_path),
            _ => Ok(Config::default()),
        },
    }?;

    config.validate()?;

    Ok((config.core, config.ui, config.color, config.keybind))
}

fn config_file_path_from_env() -> Option<PathBuf> {
    env::var(CONFIG_FILE_ENV_NAME).ok().map(PathBuf::from)
}

/// 設定檔實際會用到的路徑：`$SERIE_CONFIG_FILE` 優先，否則跟著執行檔走。
/// `load()`／精靈的寫回都要看同一個檔案——`load()` 自己的分支邏輯還要
/// 額外分辨「env 指定但檔案不存在就報錯」，跟這裡「單純告訴呼叫端寫去
/// 哪」是不同需求，所以沒有讓 `load()` 直接呼叫這個函式，兩者各自維護，
/// 但公式必須逐字一致。
pub(crate) fn effective_path() -> Option<PathBuf> {
    config_file_path_from_env().or_else(default_config_file_path)
}

/// 首次啟動生成一份含所有旋鈕與中文說明的預設設定檔——沒有這一步，「調整
/// 設定」就只能影響那一次啟動：根本沒有檔案可以寫回。要在 `Args::try_parse()`
/// 之前呼叫（`run()` 裡的順序），精靈才讀得到剛生成的檔。
///
/// 三個邊界，都刻意不出聲失敗（成功也不出聲——這是背景動作，不是使用者
/// 主動要求的操作，吵反而不對）：
/// - `$SERIE_CONFIG_FILE` 那條不自動建：使用者明確指定了路徑，維持
///   `load()` 既有的「檔案不存在就報錯」行為，不能在這裡搶先生成一份
///   放在別的位置。
/// - 用 `create_new` 而非「`exists()` 再 `write`」：後者是 TOCTOU，兩次
///   系統呼叫間檔案可能被別的 ysgit process 建立。`create_new` 是單一
///   atomic 的 open，`AlreadyExists` 直接當作正常結束。
/// - 目錄唯讀（例如裝在 `/usr/local/bin`）時寫入會失敗：印一句提示，
///   不能擋住啟動——這只是「幫使用者建一份範本」，不是必要條件，
///   `load()` 找不到檔案本來就會退回內建預設值繼續跑。
pub fn ensure_config_file() {
    if config_file_path_from_env().is_some() {
        return;
    }
    let Some(path) = default_config_file_path() else {
        return;
    };
    match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut file) => {
            const DEFAULT_CONFIG: &str = include_str!("../assets/default-config.toml");
            if let Err(e) = file.write_all(DEFAULT_CONFIG.as_bytes()) {
                eprintln!("寫入預設設定檔失敗（{}）：{e}", path.display());
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => {
            eprintln!("無法建立預設設定檔（{}）：{e}", path.display());
        }
    }
}

/// 設定檔跟著執行檔走的位置：`<exe 所在目錄>/.ysgit.toml`。自我更新只
/// `fs::rename` 執行檔本身（見 `update::download_and_replace`），同目錄的
/// 設定檔不會被動到——這是選這個位置而不是 `~/.config` 的核心理由。
/// `exe_dir()` 已經處理過 symlink／`(deleted)` 這些自我更新特有的坑
/// （見該函式註解），這裡不重算一次。
fn default_config_file_path() -> Option<PathBuf> {
    crate::update::exe_dir().map(|dir| dir.join(CONFIG_FILE_NAME))
}

fn read_config_from_path(path: &Path) -> Result<Config> {
    let content = std::fs::read_to_string(path)?;
    let content = migrate_legacy_toml(&content);
    let config: OptionalConfig = toml::from_str(&content)?;
    Ok(config.into())
}

#[optional(derives = [Deserialize])]
#[derive(Debug, Default, Clone, PartialEq, Eq, Validate)]
struct Config {
    #[garde(dive)]
    #[nested]
    core: CoreConfig,
    #[garde(dive)]
    #[nested]
    ui: UiConfig,
    #[garde(dive)]
    #[nested]
    color: ColorTheme,
    // 使用者自訂的按鍵綁定，格式請參考 `assets/default-keybind.toml`
    #[garde(skip)]
    keybind: Option<KeyBind>,
}

#[optional(derives = [Deserialize])]
#[derive(Debug, Default, Clone, PartialEq, Eq, Validate)]
pub struct CoreConfig {
    #[garde(skip)]
    #[nested]
    pub option: CoreOptionConfig,
    #[garde(skip)]
    #[nested]
    pub search: CoreSearchConfig,
    #[garde(dive)]
    #[nested]
    pub user_command: CoreUserCommandConfig,
    #[garde(dive)]
    #[nested]
    pub external: CoreExternalConfig,
    #[garde(dive)]
    #[nested]
    pub update: CoreUpdateConfig,
}

#[optional(derives = [Deserialize])]
#[derive(Debug, Clone, PartialEq, Eq, SmartDefault)]
pub struct CoreOptionConfig {
    pub order: Option<CommitOrderType>,
    pub graph_width: Option<GraphWidthType>,
    pub compact: Option<CompactType>,
    pub graph_style: Option<GraphStyle>,
    pub initial_selection: Option<InitialSelection>,
    pub max_count: Option<usize>,
}

/// 自動更新設定，三個欄位對應 `-U`／背景檢查／重啟提示。`interval_hours`
/// 要驗證範圍：設 0 會讓週期檢查退化成無限打網路，沒有上限則
/// `hours * 3600` 在 release build 會 wrapping、繞回極小值，回到同一個熱
/// 迴圈——兩者都要在載入時就擋掉，不能靠程式裡默默 clamp（那會讓使用者
/// 以為設定生效了）。
#[optional(derives = [Deserialize])]
#[derive(Debug, Clone, PartialEq, Eq, SmartDefault, Validate)]
pub struct CoreUpdateConfig {
    #[garde(skip)]
    pub mode: Option<UpdateMode>,
    #[garde(range(min = MIN_INTERVAL_HOURS, max = MAX_INTERVAL_HOURS))]
    pub interval_hours: Option<u64>,
    #[garde(skip)]
    pub auto_restart: Option<AutoRestart>,
}

#[optional(derives = [Deserialize])]
#[derive(Debug, Clone, PartialEq, Eq, SmartDefault)]
pub struct CoreSearchConfig {
    #[default = false]
    pub ignore_case: bool,
    #[default = false]
    pub fuzzy: bool,
}

#[optional]
#[derive(Debug, Clone, PartialEq, Eq, SmartDefault, Validate)]
pub struct CoreUserCommandConfig {
    #[garde(dive)]
    #[default(FxHashMap::default())]
    pub commands: FxHashMap<String, UserCommand>,
    #[garde(range(min = 0))]
    #[default = 4]
    pub tab_width: u16,
}

impl<'de> Deserialize<'de> for OptionalCoreUserCommandConfig {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::{Error, MapAccess, Visitor};
        use std::fmt;

        struct OptionalCoreUserCommandConfigVisitor;

        impl<'de> Visitor<'de> for OptionalCoreUserCommandConfigVisitor {
            type Value = OptionalCoreUserCommandConfig;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a user command configuration")
            }

            fn visit_map<V>(self, mut map: V) -> std::result::Result<Self::Value, V::Error>
            where
                V: MapAccess<'de>,
            {
                let mut commands = FxHashMap::default();
                let mut tab_width = None;

                while let Some(key) = map.next_key::<String>()? {
                    if let Some(suffix) = key.strip_prefix("commands_") {
                        let command_key = suffix.to_string();
                        if command_key.is_empty() {
                            return Err(V::Error::custom(
                                "command key cannot be empty, like `commands_`",
                            ));
                        }
                        let command_value: UserCommand = map.next_value()?;
                        commands.insert(command_key, command_value);
                    } else if key == "tab_width" {
                        tab_width = Some(map.next_value()?);
                    } else if key == "commands" {
                        return Err(V::Error::custom(
                            "invalid key `commands`, use `commands_n` format instead",
                        ));
                    } else {
                        let _: serde::de::IgnoredAny = map.next_value()?;
                    }
                }

                let commands = if commands.is_empty() {
                    None
                } else {
                    Some(commands)
                };

                Ok(OptionalCoreUserCommandConfig {
                    commands,
                    tab_width,
                })
            }
        }

        deserializer.deserialize_map(OptionalCoreUserCommandConfigVisitor)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Validate)]
pub struct UserCommand {
    #[garde(length(min = 1))]
    pub name: String,
    #[serde(default)]
    #[garde(skip)]
    pub r#type: UserCommandType,
    #[garde(length(min = 1), inner(length(min = 1)))]
    pub commands: Vec<String>,
    #[serde(default)]
    #[garde(custom(validate_user_command_refresh(&self.r#type)))]
    pub refresh: bool,
}

fn validate_user_command_refresh(
    command_type: &UserCommandType,
) -> impl FnOnce(&bool, &()) -> garde::Result + '_ {
    move |refresh, _| {
        if matches!(command_type, UserCommandType::Inline) && *refresh {
            return Err(garde::Error::new(
                "refresh cannot be true for inline command",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UserCommandType {
    #[default]
    Inline,
    Silent,
    Suspend,
}

#[optional(derives = [Deserialize])]
#[derive(Debug, Clone, PartialEq, Eq, SmartDefault, Validate)]
pub struct UiConfig {
    #[garde(skip)]
    #[default(CursorType::Native)]
    pub cursor_type: CursorType,
    #[garde(range(min = 1))]
    #[default = 26]
    pub refs_width: u16,
    #[garde(dive)]
    #[nested]
    pub pane_height: UiPaneHeightConfig,
    #[garde(dive)]
    #[nested]
    pub list: UiListConfig,
    #[garde(dive)]
    #[nested]
    pub detail: UiDetailConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub enum CursorType {
    Native,
    Virtual(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default, Validate)]
pub enum ClipboardConfig {
    #[default]
    Auto,
    Osc52,
    Custom {
        #[garde(length(min = 1), inner(length(min = 1)))]
        commands: Vec<String>,
    },
}

#[optional(derives = [Deserialize])]
#[derive(Debug, Clone, PartialEq, Eq, SmartDefault, Validate)]
pub struct CoreExternalConfig {
    #[garde(dive)]
    #[default(ClipboardConfig::Auto)]
    pub clipboard: ClipboardConfig,
}

#[optional(derives = [Deserialize])]
#[derive(Debug, Clone, PartialEq, Eq, SmartDefault, Validate)]
pub struct UiListConfig {
    #[garde(length(min = 1))]
    #[default(vec![
        UserListColumnType::Graph,
        UserListColumnType::Marker,
        UserListColumnType::Subject,
        UserListColumnType::Date,
        UserListColumnType::Name,
        UserListColumnType::Hash,
    ])]
    pub columns: Vec<UserListColumnType>,
    #[garde(range(min = 1))]
    #[default = 20]
    pub subject_min_width: u16,
    #[garde(length(min = 1))]
    #[default = "%Y-%m-%d"]
    pub date_format: String,
    #[garde(range(min = 0))]
    #[default = 10]
    pub date_width: u16,
    #[garde(skip)]
    #[default = true]
    pub date_local: bool,
    #[garde(range(min = 0))]
    #[default = 20]
    pub name_width: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UserListColumnType {
    Graph,
    Marker,
    Subject,
    Name,
    Hash,
    Date,
}

#[optional(derives = [Deserialize])]
#[derive(Debug, Clone, PartialEq, Eq, SmartDefault, Validate)]
pub struct UiDetailConfig {
    #[garde(length(min = 1))]
    #[default = "%Y-%m-%d %H:%M:%S %z"]
    pub date_format: String,
    #[garde(skip)]
    #[default = true]
    pub date_local: bool,
}

/// Detail／Diff／使用者自訂指令三個 pane 各自的高度。
///
/// `diff` 是 Detail view 選檔案時，底部單一檔案 diff pane 的高度。tab 展開
/// 沿用 `core.user_command.tab_width`——同一個程式裡不該有兩個「一個 tab
/// 展開成幾格」的答案。
#[optional(derives = [Deserialize])]
#[derive(Debug, Clone, PartialEq, Eq, SmartDefault, Validate)]
pub struct UiPaneHeightConfig {
    #[garde(range(min = 1))]
    #[default = 20]
    pub detail: u16,
    #[garde(range(min = 1))]
    #[default = 20]
    pub diff: u16,
    #[garde(range(min = 1))]
    #[default = 20]
    pub user_command: u16,
}

// ---------------------------------------------------------------------------
// 舊版 TOML 結構 → 新結構的遷移。純函式，不寫檔——`read_config_from_path()`
// 讀進來的內容在記憶體裡先轉換再 parse，`load()` 因此永遠能拿到值；檔案
// 本身要等使用者進精靈存檔（`wizard::write_touched_settings()`）才會真的
// 落地變成新格式，理由與取捨見該函式呼叫處的說明。
// ---------------------------------------------------------------------------

/// 舊路徑 → 新路徑的搬移表。`graph.color.branches` 那條的目的地鍵名跟
/// 來源相同（`branches`），其餘四條連鍵名都變了（`width` → `refs_width`
/// 之類）——`move_value` 兩種都處理，搬移表不用分兩張。
const LEGACY_KEY_MOVES: &[(&[&str], &[&str])] = &[
    (
        &["graph", "color", "branches"],
        &["color", "graph", "branches"],
    ),
    (&["ui", "common", "cursor_type"], &["ui", "cursor_type"]),
    (&["ui", "refs", "width"], &["ui", "refs_width"]),
    (
        &["ui", "detail", "height"],
        &["ui", "pane_height", "detail"],
    ),
    (&["ui", "diff", "height"], &["ui", "pane_height", "diff"]),
    (
        &["ui", "user_command", "height"],
        &["ui", "pane_height", "user_command"],
    ),
];

/// 把舊格式 `.ysgit.toml`（`[ui.common]`／`[ui.detail]`／`[ui.diff]`／
/// `[ui.user_command]`／`[ui.refs]` 的 `height`／`width`／`cursor_type`，
/// 以及獨立的 `[graph.color]`）轉成新結構。
///
/// 沒有任何舊鍵、或 `existing` 本身 parse 不動（`toml_edit` 對語法錯誤沒
/// 辦法），一律原樣借回去——語法錯誤留給 `toml::from_str` 產生它自己
/// 唯一、正規的錯誤訊息，這裡不用再開一條錯誤路徑。
pub(crate) fn migrate_legacy_toml(existing: &str) -> Cow<'_, str> {
    let Ok(mut doc) = existing.parse::<toml_edit::DocumentMut>() else {
        return Cow::Borrowed(existing);
    };

    // 不能用 `.any()` short-circuit：每一條都要真的執行過 `move_value`，
    // 不是只求「有沒有任何一條命中」。
    let mut touched = false;
    for (from, to) in LEGACY_KEY_MOVES {
        touched |= move_value(&mut doc, from, to);
    }

    // 搬空的舊區塊要清掉，否則會印出一段沒有內容的 `[ui.diff]`——父層直接
    // 從 `LEGACY_KEY_MOVES` 的來源路徑推導，不手寫第二張表：那張表曾經
    // 手抄漏過 `ui.detail`（`height` 搬空之後，只寫過 `height` 沒寫
    // `date_format`／`date_local` 的舊檔案會留下一段空的 `[ui.detail]`），
    // 兩份清單本來就該是同一份。
    for (from, _) in LEGACY_KEY_MOVES {
        if let Some((_, parents)) = from.split_last() {
            remove_if_empty(&mut doc, parents);
        }
    }

    if touched {
        Cow::Owned(doc.to_string())
    } else {
        Cow::Borrowed(existing)
    }
}

/// 把 `from` 路徑的鍵搬到 `to` 路徑，保留鍵本身的 decor（含鍵前面的行內
/// 註解）。`from` 不存在時是 no-op（代表這條已經是新格式）；`to` 已經有
/// 值時新格式優先，`from` 直接丟棄不覆寫——半升級檔案（使用者手動改過
/// 一部分）不該被舊值蓋掉。回傳是否真的搬動了什麼。
fn move_value(doc: &mut toml_edit::DocumentMut, from: &[&str], to: &[&str]) -> bool {
    let (Some((from_key, from_parents)), Some((to_key, to_parents))) =
        (from.split_last(), to.split_last())
    else {
        return false;
    };

    let Some((key, item)) =
        navigate(doc.as_table_mut(), from_parents).and_then(|src| src.remove_entry(from_key))
    else {
        return false;
    };

    let Some(dest_table) = ensure_table(doc.as_table_mut(), to_parents) else {
        // 目的路徑上有非表格值（使用者自己寫壞的）——放回原地，不能讓
        // 值憑空消失。
        if let Some(src_table) = navigate(doc.as_table_mut(), from_parents) {
            src_table.insert_formatted(&key, item);
        }
        return false;
    };
    if dest_table.contains_key(to_key) {
        return true;
    }
    dest_table.insert_formatted(&rename_key(key, to_key), item);
    true
}

/// 沿著 `path` 逐層找子表，中途任何一段不是表格就回 `None`。
fn navigate<'a>(
    mut table: &'a mut toml_edit::Table,
    path: &[&str],
) -> Option<&'a mut toml_edit::Table> {
    for segment in path {
        table = table.get_mut(segment)?.as_table_mut()?;
    }
    Some(table)
}

/// 沿著 `path` 逐層找子表，不存在就建立（跟 `wizard::table_entry` 同一套
/// `Table::entry().or_insert(toml_edit::table())` 寫法，不用 `doc["a"]["b"]`
/// 鏈式索引，理由見該函式的註解——那種寫法在中間層不存在時會生成
/// inline table）。中途任何一段已經是非表格值就回 `None`。
fn ensure_table<'a>(
    mut table: &'a mut toml_edit::Table,
    path: &[&str],
) -> Option<&'a mut toml_edit::Table> {
    for segment in path {
        table = table
            .entry(segment)
            .or_insert(toml_edit::table())
            .as_table_mut()?;
    }
    Some(table)
}

/// 鍵名不變就原樣傳回；改名時保留原本的 decor（鍵前面的空白與行內
/// 註解），只是換掉印出來的名字——這正是要用 `insert_formatted` 而不是
/// `insert(&str, ..)` 的理由：後者的 `Key::new` 是全新的空 decor。
fn rename_key(key: toml_edit::Key, new_name: &str) -> toml_edit::Key {
    if key.get() == new_name {
        return key;
    }
    let mut renamed = toml_edit::Key::new(new_name);
    *renamed.leaf_decor_mut() = key.leaf_decor().clone();
    renamed
}

/// 只在真的空的時候才刪——使用者若在裡面寫了別的（含已經失效的舊鍵），
/// 原封不動留著，不能連著使用者自己的東西一起清掉。
fn remove_if_empty(doc: &mut toml_edit::DocumentMut, path: &[&str]) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let Some(table) = navigate(doc.as_table_mut(), parents) else {
        return;
    };
    let is_empty = table
        .get(last)
        .and_then(toml_edit::Item::as_table)
        .is_some_and(toml_edit::Table::is_empty);
    if is_empty {
        table.remove(last);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::color::GraphColors;

    /// 取出設定檔格式文件裡第一段 ```toml 範例。三條測試都要拿它跟
    /// `assets/default-config.toml` 或 `ColorTheme::default()` 比對。
    fn documented_example() -> &'static str {
        include_str!("../docs/src/configurations/config-file-format.md")
            .split("```toml\n")
            .nth(1)
            .and_then(|s| s.split("```").next())
            .expect("設定檔格式文件裡找不到 ```toml 範例區塊")
    }

    #[test]
    fn migrate_legacy_toml_moves_every_known_path_to_its_new_location() {
        let old = r##"
            [ui.common]
            cursor_type = { Virtual = "|" }
            [ui.detail]
            height = 30
            date_format = "%Y/%m/%d %H:%M:%S"
            [ui.diff]
            height = 15
            [ui.user_command]
            height = 25
            [ui.refs]
            width = 40
            [graph.color]
            branches = ["#ff0000", "#00ff00"]
        "##;
        let migrated = migrate_legacy_toml(old);
        assert!(matches!(migrated, Cow::Owned(_)));

        let config: Config = toml::from_str::<OptionalConfig>(&migrated).unwrap().into();
        assert_eq!(config.ui.cursor_type, CursorType::Virtual("|".into()));
        assert_eq!(config.ui.refs_width, 40);
        assert_eq!(config.ui.pane_height.detail, 30);
        assert_eq!(config.ui.pane_height.diff, 15);
        assert_eq!(config.ui.pane_height.user_command, 25);
        assert_eq!(config.ui.detail.date_format, "%Y/%m/%d %H:%M:%S");
        assert_eq!(
            config.color.graph.branches,
            vec!["#ff0000".to_string(), "#00ff00".to_string()]
        );

        // 舊區塊搬空了就該消失，不能留下一段沒有內容的 `[ui.diff]`。
        let doc: toml::Table = toml::from_str(&migrated).unwrap();
        assert!(!doc.contains_key("graph"));
        let ui = doc["ui"].as_table().unwrap();
        for legacy in ["common", "diff", "user_command", "refs"] {
            assert!(!ui.contains_key(legacy), "`ui.{legacy}` 應該已經搬空移除");
        }
    }

    #[test]
    fn migrate_legacy_toml_is_a_noop_on_already_new_format() {
        let new = documented_example();
        let migrated = migrate_legacy_toml(new);
        assert!(matches!(migrated, Cow::Borrowed(_)));
        assert_eq!(migrated.as_ref(), new);
    }

    #[test]
    fn migrate_legacy_toml_keeps_the_comment_above_a_moved_key() {
        let old = "[graph.color]\n# 各分支依序輪流套用的顏色。\nbranches = [\"#ff0000\"]\n";
        let migrated = migrate_legacy_toml(old);
        // 光是「字串還在」不能證明真的搬了——原封不動借回去一樣會過。
        // 要連著確認舊區塊消失、註解緊貼著搬到新鍵前面，才是真的釘住
        // `insert_formatted` 保留 decor 這件事。
        assert!(!migrated.contains("[graph.color]"), "{migrated}");
        assert!(
            migrated.contains("# 各分支依序輪流套用的顏色。\nbranches ="),
            "{migrated}"
        );
    }

    #[test]
    fn migrate_legacy_toml_leaves_syntactically_broken_files_untouched() {
        let broken = "[ui.common\ncursor_type = \"Native\"";
        let migrated = migrate_legacy_toml(broken);
        assert!(matches!(migrated, Cow::Borrowed(_)));
        assert_eq!(migrated.as_ref(), broken);
    }

    #[test]
    fn migrate_legacy_toml_prefers_the_new_key_when_both_exist() {
        let old = r#"
            [ui]
            refs_width = 99
            [ui.refs]
            width = 40
        "#;
        let migrated = migrate_legacy_toml(old);
        let config: Config = toml::from_str::<OptionalConfig>(&migrated).unwrap().into();
        assert_eq!(config.ui.refs_width, 99);
    }

    #[test]
    fn test_config_default() {
        let actual = Config::default();
        let expected = Config {
            core: CoreConfig {
                option: CoreOptionConfig {
                    order: None,
                    graph_width: None,
                    compact: None,
                    graph_style: None,
                    initial_selection: None,
                    max_count: None,
                },
                update: CoreUpdateConfig {
                    mode: None,
                    interval_hours: None,
                    auto_restart: None,
                },
                search: CoreSearchConfig {
                    ignore_case: false,
                    fuzzy: false,
                },
                user_command: CoreUserCommandConfig {
                    commands: FxHashMap::default(),
                    tab_width: 4,
                },
                external: CoreExternalConfig {
                    clipboard: ClipboardConfig::Auto,
                },
            },
            ui: UiConfig {
                cursor_type: CursorType::Native,
                refs_width: 26,
                pane_height: UiPaneHeightConfig {
                    detail: 20,
                    diff: 20,
                    user_command: 20,
                },
                list: UiListConfig {
                    columns: vec![
                        UserListColumnType::Graph,
                        UserListColumnType::Marker,
                        UserListColumnType::Subject,
                        UserListColumnType::Date,
                        UserListColumnType::Name,
                        UserListColumnType::Hash,
                    ],
                    subject_min_width: 20,
                    date_format: "%Y-%m-%d".into(),
                    date_width: 10,
                    date_local: true,
                    name_width: 20,
                },
                detail: UiDetailConfig {
                    date_format: "%Y-%m-%d %H:%M:%S %z".into(),
                    date_local: true,
                },
            },
            color: ColorTheme::default(),
            keybind: None,
        };
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_config_complete_toml() {
        let toml = r##"
            [core.option]
            order = "topo"
            graph_width = "single"
            graph_style = "angular"
            initial_selection = "head"
            [core.search]
            ignore_case = true
            fuzzy = true
            [core.user_command]
            commands_1 = { name = "git diff no color", commands = ["git", "diff", "{{first_parent_hash}}", "{{target_hash}}"] }
            commands_2 = { name = "echo hello", type = "silent", commands = ["echo", "hello"], refresh = true }
            commands_10 = { name = "echo world", type = "inline", commands = ["echo", "world"], refresh = false }
            commands_3 = { name = "open vim", type = "suspend", commands = ["vim"] }
            tab_width = 2
            [ui]
            cursor_type = { Virtual = "|" }
            refs_width = 40
            [ui.pane_height]
            detail = 30
            user_command = 30
            [ui.list]
            columns = ["date", "subject", "hash", "graph"]
            subject_min_width = 40
            date_format = "%Y/%m/%d"
            date_width = 20
            date_local = false
            name_width = 30
            [ui.detail]
            date_format = "%Y/%m/%d %H:%M:%S"
            date_local = false
            [color.graph]
            branches = ["#ff0000", "#00ff00", "#0000ff"]
        "##;
        let actual: Config = toml::from_str::<OptionalConfig>(toml).unwrap().into();
        let expected = Config {
            core: CoreConfig {
                option: CoreOptionConfig {
                    order: Some(CommitOrderType::Topo),
                    graph_width: Some(GraphWidthType::Single),
                    compact: None,
                    graph_style: Some(GraphStyle::Angular),
                    initial_selection: Some(InitialSelection::Head),
                    max_count: None,
                },
                update: CoreUpdateConfig {
                    mode: None,
                    interval_hours: None,
                    auto_restart: None,
                },
                search: CoreSearchConfig {
                    ignore_case: true,
                    fuzzy: true,
                },
                user_command: CoreUserCommandConfig {
                    commands: FxHashMap::from_iter([
                        (
                            "1".into(),
                            UserCommand {
                                name: "git diff no color".into(),
                                r#type: UserCommandType::Inline,
                                commands: vec![
                                    "git".into(),
                                    "diff".into(),
                                    "{{first_parent_hash}}".into(),
                                    "{{target_hash}}".into(),
                                ],
                                refresh: false,
                            },
                        ),
                        (
                            "2".into(),
                            UserCommand {
                                name: "echo hello".into(),
                                r#type: UserCommandType::Silent,
                                commands: vec!["echo".into(), "hello".into()],
                                refresh: true,
                            },
                        ),
                        (
                            "3".into(),
                            UserCommand {
                                name: "open vim".into(),
                                r#type: UserCommandType::Suspend,
                                commands: vec!["vim".into()],
                                refresh: false,
                            },
                        ),
                        (
                            "10".into(),
                            UserCommand {
                                name: "echo world".into(),
                                r#type: UserCommandType::Inline,
                                commands: vec!["echo".into(), "world".into()],
                                refresh: false,
                            },
                        ),
                    ]),
                    tab_width: 2,
                },
                external: CoreExternalConfig {
                    clipboard: ClipboardConfig::Auto,
                },
            },
            ui: UiConfig {
                cursor_type: CursorType::Virtual("|".into()),
                refs_width: 40,
                pane_height: UiPaneHeightConfig {
                    detail: 30,
                    diff: 20,
                    user_command: 30,
                },
                list: UiListConfig {
                    columns: vec![
                        UserListColumnType::Date,
                        UserListColumnType::Subject,
                        UserListColumnType::Hash,
                        UserListColumnType::Graph,
                    ],
                    subject_min_width: 40,
                    date_format: "%Y/%m/%d".into(),
                    date_width: 20,
                    date_local: false,
                    name_width: 30,
                },
                detail: UiDetailConfig {
                    date_format: "%Y/%m/%d %H:%M:%S".into(),
                    date_local: false,
                },
            },
            color: ColorTheme {
                graph: GraphColors {
                    branches: vec!["#ff0000".into(), "#00ff00".into(), "#0000ff".into()],
                },
                ..ColorTheme::default()
            },
            keybind: None,
        };
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_config_partial_toml() {
        let toml = r#"
            [ui.list]
            date_format = "%Y/%m/%d"
        "#;
        let actual: Config = toml::from_str::<OptionalConfig>(toml).unwrap().into();
        // 只設了 `ui.list.date_format`，其餘每一項都該維持預設——這正是這條
        // 測試要證明的事，所以只明寫那一個欄位，其餘用 `..Default::default()`
        // 帶過。`test_config_default` 已經逐欄位釘死過完整的預設值一次，
        // 這裡不用再抄一份（`test_config_complete_toml` 不適用這招：那條
        // 測試每個欄位都該明寫，才驗得出「解析出來的值」而非「預設值」）。
        let expected = Config {
            ui: UiConfig {
                list: UiListConfig {
                    date_format: "%Y/%m/%d".into(),
                    ..UiListConfig::default()
                },
                ..UiConfig::default()
            },
            ..Config::default()
        };
        assert_eq!(actual, expected);
    }

    /// schema 只用 `^<字面前綴><數字>$` 這一種 pattern（`commands_1`、
    /// `user_command_1`）。碰到其他形式直接 panic —— 默默放行等於沒有這道檢查。
    fn schema_pattern_property<'a>(
        schema: &'a serde_json::Value,
        key: &str,
    ) -> Option<&'a serde_json::Value> {
        let patterns = schema.get("patternProperties")?.as_object()?;
        patterns.iter().find_map(|(pattern, sub)| {
            let prefix = pattern
                .strip_prefix('^')
                .and_then(|p| p.strip_suffix("[0-9]+$"))
                .unwrap_or_else(|| panic!("看不懂的 patternProperties: {pattern}"));
            let digits = key.strip_prefix(prefix)?;
            let matches = !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit());
            matches.then_some(sub)
        })
    }

    /// 檢查 TOML 表裡每個鍵在 `config.schema.json` 都有宣告。
    ///
    /// serde 這邊沒有 `deny_unknown_fields`，未知欄位一律靜默忽略，所以
    /// 「範例能 parse」完全擋不住死鍵 —— `graph.color.edge` / `background`
    /// 就是這樣在文件裡活過好幾個版本。真正會擋的是 schema 的
    /// `additionalProperties: false`，這裡就照著它走一遍。
    fn assert_keys_declared_in_schema(table: &toml::Table, schema: &serde_json::Value, path: &str) {
        for (key, value) in table {
            let child = schema
                .get("properties")
                .and_then(|p| p.get(key))
                .or_else(|| schema_pattern_property(schema, key))
                .unwrap_or_else(|| panic!("文件範例的 `{path}{key}` 不在 config.schema.json 裡"));
            if let Some(sub_table) = value.as_table() {
                if child.get("properties").is_some() {
                    assert_keys_declared_in_schema(sub_table, child, &format!("{path}{key}."));
                }
            }
        }
    }

    /// mdBook 的設定檔範例必須是真的能貼進 `config.toml` 直接用的東西。
    ///
    /// 兩件事各自抓到過真實錯誤：schema 比對抓死鍵（`graph.color.edge` /
    /// `background` 隨圖片渲染路徑移除，文件卻還留著），預設值比對抓值漂移
    /// （`ui.list.columns` 的順序在文件與 schema 都寫成 name/hash/date，
    /// 實際是 date/name/hash）。
    #[test]
    fn documented_example_config_is_valid_and_shows_real_defaults() {
        let example = documented_example();

        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../config.schema.json")).unwrap();
        let table: toml::Table = toml::from_str(example)
            .unwrap_or_else(|e| panic!("文件範例不是合法 TOML: {e}\n---\n{example}"));
        assert_keys_declared_in_schema(&table, &schema, "");

        let parsed: OptionalConfig = toml::from_str(example).unwrap();
        let mut actual = Config::from(parsed);
        // `core.option`／`core.update` 的欄位與 `keybind` 是 Option，「未設定」
        // 與「設定成預設值」在型別上不同（命令列參數要能覆蓋，所以預設留到更
        // 後面才解析）。範例把它們明寫出來正是它的用途，比對前歸零，其餘欄位
        // 照比。
        actual.core.option = CoreOptionConfig::default();
        actual.core.update = CoreUpdateConfig::default();
        actual.keybind = None;
        assert_eq!(actual, Config::default());
    }

    /// `assets/default-config.toml` 是首次啟動寫給使用者的那份檔案，跟
    /// 上面那個文件範例是同一件事的兩份拷貝（一份給人讀文件、一份給程式
    /// 內嵌），值必須同步——這條測試就是防漂移的機制：兩者各自 parse
    /// 成 `Config` 後逐欄位比對，不比原始文字（註解、排版本來就不同）。
    #[test]
    fn default_config_asset_matches_documented_example() {
        let example = documented_example();
        let doc_config: Config = toml::from_str::<OptionalConfig>(example).unwrap().into();

        let asset = include_str!("../assets/default-config.toml");
        let asset_config: Config = toml::from_str::<OptionalConfig>(asset).unwrap().into();

        assert_eq!(asset_config, doc_config);
    }

    /// `assets/default-config.toml` 本身也要通過 schema 檢查——它是首次
    /// 啟動就會寫到使用者磁碟上的檔案，死鍵在這裡比在文件範例裡更嚴重
    /// （文件錯了只是誤導讀者，這個錯了是實際寫進使用者設定檔）。
    #[test]
    fn default_config_asset_keys_are_declared_in_schema() {
        let asset = include_str!("../assets/default-config.toml");
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../config.schema.json")).unwrap();
        let table: toml::Table = toml::from_str(asset)
            .unwrap_or_else(|e| panic!("assets/default-config.toml 不是合法 TOML: {e}"));
        assert_keys_declared_in_schema(&table, &schema, "");
    }

    /// `assets/default-config.toml` 裡明寫出來的值（`core.option`／
    /// `core.update` 除外，理由同 `documented_example_config_is_valid_...`）
    /// 必須真的是 `Config::default()`——這是它作為「首次啟動範本」的存在
    /// 意義：使用者看到的第一份設定檔，內容要跟沒有這份檔案時的行為一致。
    #[test]
    fn default_config_asset_shows_real_defaults() {
        let asset = include_str!("../assets/default-config.toml");
        let parsed: OptionalConfig = toml::from_str(asset).unwrap();
        let mut actual = Config::from(parsed);
        actual.core.option = CoreOptionConfig::default();
        actual.core.update = CoreUpdateConfig::default();
        actual.keybind = None;
        assert_eq!(actual, Config::default());
    }

    /// 取出 `[color]` 表的欄位路徑，只認鍵不看值；巢狀表（目前只有
    /// `graph`）用 `.` 串接成 `graph.branches` 這種路徑，不然只比頂層鍵
    /// 的話，`[color.graph]` 底下漏寫 `branches` 也會被放行——跟
    /// `status_interactive_fg` 消失好幾個版本沒人發現是同一種盲點。
    fn color_keys(value: &toml::Value) -> BTreeSet<String> {
        fn walk(value: &toml::Value, prefix: &str, out: &mut BTreeSet<String>) {
            match value.as_table() {
                Some(table) => {
                    for (key, v) in table {
                        let path = if prefix.is_empty() {
                            key.clone()
                        } else {
                            format!("{prefix}.{key}")
                        };
                        walk(v, &path, out);
                    }
                }
                None => {
                    out.insert(prefix.to_string());
                }
            }
        }
        let mut out = BTreeSet::new();
        walk(value, "", &mut out);
        out
    }

    /// `ColorTheme` 有的欄位，範本與文件範例都一定要寫出來。
    ///
    /// 上面兩條測試只守單向：`assert_keys_declared_in_schema` 驗「範例的鍵
    /// 在 schema 裡有宣告」，`default_config_asset_shows_real_defaults` 驗
    /// 「範本寫出來的值等於預設值」。兩者都建立在「範本裡沒寫的鍵就當作
    /// 沒有」這個前提上——少寫一個鍵，比對照樣兩邊都是預設值，全部綠燈，
    /// `status_interactive_fg` 就是這樣消失了好幾個版本沒人發現。
    ///
    /// 這裡反過來，從 `ColorTheme::default()` 本身出發：struct 有的欄位，
    /// 範本／文件範例都要出現。只比鍵、不比值——`ratatui::Color` 的
    /// `Serialize` 走 `Display`（`Color::Reset` 序列化成 `"Reset"`），跟
    /// 範本手寫的 `"reset"` 本來就對不上，值的漂移已經有上面那條測試守著。
    #[test]
    fn every_color_field_appears_in_asset_and_doc_example() {
        let expected = toml::Value::try_from(ColorTheme::default()).unwrap();
        let expected_keys = color_keys(&expected);

        let asset: toml::Table =
            toml::from_str(include_str!("../assets/default-config.toml")).unwrap();
        let asset_keys = color_keys(asset.get("color").unwrap());
        assert_eq!(
            asset_keys, expected_keys,
            "assets/default-config.toml 的 [color] 跟 ColorTheme 欄位對不上"
        );

        let doc_table: toml::Table = toml::from_str(documented_example()).unwrap();
        let doc_keys = color_keys(doc_table.get("color").unwrap());
        assert_eq!(
            doc_keys, expected_keys,
            "文件範例的 [color] 跟 ColorTheme 欄位對不上"
        );
    }

    /// `graph_width` 的可選值散在四個地方：`GraphWidthType` 的 derive、
    /// `config.schema.json` 的 enum、還有兩份文件的清單。上面那個測試只比
    /// 「鍵」有沒有宣告，值漂移它一律放行 —— 可選值增減時這道檢查是唯一
    /// 會響的。
    ///
    /// 比的是 clap 認得的全部字串（canonical 加別名），所以 schema 少列
    /// 別名、或留著已經拿掉的值，兩種方向都會被抓到。
    #[test]
    fn graph_width_schema_enum_matches_every_accepted_cli_value() {
        use clap::ValueEnum;

        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../config.schema.json")).unwrap();
        let mut declared: Vec<String> = schema["properties"]["core"]["properties"]["option"]
            ["properties"]["graph_width"]["enum"]
            .as_array()
            .expect("config.schema.json 裡的 graph_width 沒有 enum")
            .iter()
            .map(|v| v.as_str().expect("enum 值不是字串").to_string())
            .collect();

        let mut accepted: Vec<String> = GraphWidthType::value_variants()
            .iter()
            .flat_map(|variant| {
                variant
                    .to_possible_value()
                    .expect("每個變體都該有對應的命令列值")
                    .get_name_and_aliases()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .collect();

        declared.sort();
        accepted.sort();
        assert_eq!(declared, accepted);
    }

    #[test]
    fn compact_schema_enum_matches_every_accepted_cli_value() {
        use clap::ValueEnum;

        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../config.schema.json")).unwrap();
        let mut declared: Vec<String> = schema["properties"]["core"]["properties"]["option"]
            ["properties"]["compact"]["enum"]
            .as_array()
            .expect("config.schema.json 裡的 compact 沒有 enum")
            .iter()
            .map(|v| v.as_str().expect("enum 值不是字串").to_string())
            .collect();

        let mut accepted: Vec<String> = CompactType::value_variants()
            .iter()
            .flat_map(|variant| {
                variant
                    .to_possible_value()
                    .expect("每個變體都該有對應的命令列值")
                    .get_name_and_aliases()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .collect();

        declared.sort();
        accepted.sort();
        assert_eq!(declared, accepted);
    }

    #[test]
    fn update_mode_schema_enum_matches_every_accepted_cli_value() {
        use clap::ValueEnum;

        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../config.schema.json")).unwrap();
        let mut declared: Vec<String> = schema["properties"]["core"]["properties"]["update"]
            ["properties"]["mode"]["enum"]
            .as_array()
            .expect("config.schema.json 裡的 core.update.mode 沒有 enum")
            .iter()
            .map(|v| v.as_str().expect("enum 值不是字串").to_string())
            .collect();

        let mut accepted: Vec<String> = UpdateMode::value_variants()
            .iter()
            .flat_map(|variant| {
                variant
                    .to_possible_value()
                    .expect("每個變體都該有對應的命令列值")
                    .get_name_and_aliases()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .collect();

        declared.sort();
        accepted.sort();
        assert_eq!(declared, accepted);
    }

    #[test]
    fn auto_restart_schema_enum_matches_every_accepted_cli_value() {
        use clap::ValueEnum;

        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../config.schema.json")).unwrap();
        let mut declared: Vec<String> = schema["properties"]["core"]["properties"]["update"]
            ["properties"]["auto_restart"]["enum"]
            .as_array()
            .expect("config.schema.json 裡的 core.update.auto_restart 沒有 enum")
            .iter()
            .map(|v| v.as_str().expect("enum 值不是字串").to_string())
            .collect();

        let mut accepted: Vec<String> = AutoRestart::value_variants()
            .iter()
            .flat_map(|variant| {
                variant
                    .to_possible_value()
                    .expect("每個變體都該有對應的命令列值")
                    .get_name_and_aliases()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .collect();

        declared.sort();
        accepted.sort();
        assert_eq!(declared, accepted);
    }

    #[test]
    fn removed_config_field_does_not_break_sibling_fields() {
        let toml = r#"
            [core.option]
            protocol = "kitty"
            order = "topo"
        "#;
        let config: Config = toml::from_str::<OptionalConfig>(toml).unwrap().into();
        assert_eq!(config.core.option.order, Some(CommitOrderType::Topo));
    }

    #[test]
    fn test_config_graph_style_ascii() {
        let toml = r#"
            [core.option]
            graph_style = "ascii"
        "#;
        let config: Config = toml::from_str::<OptionalConfig>(toml).unwrap().into();
        assert_eq!(config.core.option.graph_style, Some(GraphStyle::Ascii));
    }

    #[test]
    fn test_config_clipboard_auto() {
        let toml = r#"
            [core.external]
            clipboard = "Auto"
        "#;
        let config: Config = toml::from_str::<OptionalConfig>(toml).unwrap().into();
        assert_eq!(config.core.external.clipboard, ClipboardConfig::Auto);
    }

    #[test]
    fn test_config_clipboard_osc52() {
        let toml = r#"
            [core.external]
            clipboard = "Osc52"
        "#;
        let config: Config = toml::from_str::<OptionalConfig>(toml).unwrap().into();
        assert_eq!(config.core.external.clipboard, ClipboardConfig::Osc52);
    }

    #[test]
    fn test_config_clipboard_custom_single_command() {
        let toml = r#"
            [core.external]
            clipboard = { Custom = { commands = ["wl-copy"] } }
        "#;
        let config: Config = toml::from_str::<OptionalConfig>(toml).unwrap().into();
        assert_eq!(
            config.core.external.clipboard,
            ClipboardConfig::Custom {
                commands: vec!["wl-copy".into()]
            }
        );
    }

    #[test]
    fn test_config_clipboard_custom_command_with_args() {
        let toml = r#"
            [core.external]
            clipboard = { Custom = { commands = ["xclip", "-selection", "clipboard"] } }
        "#;
        let config: Config = toml::from_str::<OptionalConfig>(toml).unwrap().into();
        assert_eq!(
            config.core.external.clipboard,
            ClipboardConfig::Custom {
                commands: vec!["xclip".into(), "-selection".into(), "clipboard".into()]
            }
        );
    }

    #[test]
    fn update_interval_hours_zero_fails_validation() {
        let update = CoreUpdateConfig {
            mode: None,
            interval_hours: Some(0),
            auto_restart: None,
        };
        assert!(update.validate().is_err());
    }

    #[test]
    fn update_interval_hours_above_max_fails_validation() {
        let update = CoreUpdateConfig {
            mode: None,
            interval_hours: Some(MAX_INTERVAL_HOURS + 1),
            auto_restart: None,
        };
        assert!(update.validate().is_err());
    }

    #[test]
    fn update_interval_hours_within_range_passes_validation() {
        let update = CoreUpdateConfig {
            mode: None,
            interval_hours: Some(MIN_INTERVAL_HOURS),
            auto_restart: None,
        };
        assert!(update.validate().is_ok());
    }

    #[test]
    fn update_interval_hours_unset_passes_validation() {
        // None＝沒設定，garde 對 Option<T> 的 range 是 None 放行、Some 才驗。
        let update = CoreUpdateConfig::default();
        assert!(update.validate().is_ok());
    }
}

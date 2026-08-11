use std::{
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

pub fn load() -> Result<(
    CoreConfig,
    UiConfig,
    GraphConfig,
    ColorTheme,
    Option<KeyBind>,
)> {
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

    Ok((
        config.core,
        config.ui,
        config.graph,
        config.color,
        config.keybind,
    ))
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
    graph: GraphConfig,
    #[garde(skip)]
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
#[derive(Debug, Default, Clone, PartialEq, Eq, Validate)]
pub struct UiConfig {
    #[garde(skip)]
    #[nested]
    pub common: UiCommonConfig,
    #[garde(dive)]
    #[nested]
    pub list: UiListConfig,
    #[garde(dive)]
    #[nested]
    pub detail: UiDetailConfig,
    #[garde(dive)]
    #[nested]
    pub diff: UiDiffConfig,
    #[garde(dive)]
    #[nested]
    pub user_command: UiUserCommandConfig,
    #[garde(dive)]
    #[nested]
    pub refs: UiRefsConfig,
}

#[optional(derives = [Deserialize])]
#[derive(Debug, Clone, PartialEq, Eq, SmartDefault)]
pub struct UiCommonConfig {
    #[default(CursorType::Native)]
    pub cursor_type: CursorType,
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
    #[garde(range(min = 1))]
    #[default = 20]
    pub height: u16,
    #[garde(length(min = 1))]
    #[default = "%Y-%m-%d %H:%M:%S %z"]
    pub date_format: String,
    #[garde(skip)]
    #[default = true]
    pub date_local: bool,
}

/// Detail view 選檔案時，底部單一檔案 diff pane 的高度。tab 展開沿用
/// `core.user_command.tab_width`——同一個程式裡不該有兩個「一個 tab 展開成
/// 幾格」的答案。
#[optional(derives = [Deserialize])]
#[derive(Debug, Clone, PartialEq, Eq, SmartDefault, Validate)]
pub struct UiDiffConfig {
    #[garde(range(min = 1))]
    #[default = 20]
    pub height: u16,
}

#[optional(derives = [Deserialize])]
#[derive(Debug, Clone, PartialEq, Eq, SmartDefault, Validate)]
pub struct UiUserCommandConfig {
    #[garde(range(min = 1))]
    #[default = 20]
    pub height: u16,
}

#[optional(derives = [Deserialize])]
#[derive(Debug, Clone, PartialEq, Eq, SmartDefault, Validate)]
pub struct UiRefsConfig {
    #[garde(range(min = 1))]
    #[default = 26]
    pub width: u16,
}

#[optional(derives = [Deserialize])]
#[derive(Debug, Default, Clone, PartialEq, Eq, Validate)]
pub struct GraphConfig {
    #[garde(dive)]
    #[nested]
    pub color: GraphColorConfig,
}

#[optional(derives = [Deserialize])]
#[derive(Debug, Clone, PartialEq, Eq, SmartDefault, Validate)]
pub struct GraphColorConfig {
    #[garde(length(min = 1), inner(pattern(r"^#([0-9a-fA-F]{6}|[0-9a-fA-F]{8})$")))]
    #[default(vec![
        "#E06C76".into(),
        "#98C379".into(),
        "#E5C07B".into(),
        "#61AFEF".into(),
        "#C678DD".into(),
        "#56B6C2".into(),
    ])]
    pub branches: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

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
                common: UiCommonConfig {
                    cursor_type: CursorType::Native,
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
                    height: 20,
                    date_format: "%Y-%m-%d %H:%M:%S %z".into(),
                    date_local: true,
                },
                diff: UiDiffConfig { height: 20 },
                user_command: UiUserCommandConfig { height: 20 },
                refs: UiRefsConfig { width: 26 },
            },
            graph: GraphConfig {
                color: GraphColorConfig {
                    branches: vec![
                        "#E06C76".into(),
                        "#98C379".into(),
                        "#E5C07B".into(),
                        "#61AFEF".into(),
                        "#C678DD".into(),
                        "#56B6C2".into(),
                    ],
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
            [ui.common]
            cursor_type = { Virtual = "|" }
            [ui.list]
            columns = ["date", "subject", "hash", "graph"]
            subject_min_width = 40
            date_format = "%Y/%m/%d"
            date_width = 20
            date_local = false
            name_width = 30
            [ui.detail]
            height = 30
            date_format = "%Y/%m/%d %H:%M:%S"
            date_local = false
            [ui.user_command]
            height = 30
            [ui.refs]
            width = 40
            [graph.color]
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
                common: UiCommonConfig {
                    cursor_type: CursorType::Virtual("|".into()),
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
                    height: 30,
                    date_format: "%Y/%m/%d %H:%M:%S".into(),
                    date_local: false,
                },
                diff: UiDiffConfig { height: 20 },
                user_command: UiUserCommandConfig { height: 30 },
                refs: UiRefsConfig { width: 40 },
            },
            graph: GraphConfig {
                color: GraphColorConfig {
                    branches: vec!["#ff0000".into(), "#00ff00".into(), "#0000ff".into()],
                },
            },
            color: ColorTheme::default(),
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
                common: UiCommonConfig {
                    cursor_type: CursorType::Native,
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
                    date_format: "%Y/%m/%d".into(),
                    date_width: 10,
                    date_local: true,
                    name_width: 20,
                },
                detail: UiDetailConfig {
                    height: 20,
                    date_format: "%Y-%m-%d %H:%M:%S %z".into(),
                    date_local: true,
                },
                diff: UiDiffConfig { height: 20 },
                user_command: UiUserCommandConfig { height: 20 },
                refs: UiRefsConfig { width: 26 },
            },
            graph: GraphConfig {
                color: GraphColorConfig {
                    branches: vec![
                        "#E06C76".into(),
                        "#98C379".into(),
                        "#E5C07B".into(),
                        "#61AFEF".into(),
                        "#C678DD".into(),
                        "#56B6C2".into(),
                    ],
                },
            },
            color: ColorTheme::default(),
            keybind: None,
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
        let doc = include_str!("../docs/src/configurations/config-file-format.md");
        let example = doc
            .split("```toml\n")
            .nth(1)
            .and_then(|s| s.split("```").next())
            .expect("設定檔格式文件裡找不到 ```toml 範例區塊");

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
        let doc = include_str!("../docs/src/configurations/config-file-format.md");
        let example = doc
            .split("```toml\n")
            .nth(1)
            .and_then(|s| s.split("```").next())
            .expect("設定檔格式文件裡找不到 ```toml 範例區塊");
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

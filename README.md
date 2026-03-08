# Serie-ys

> 這是 [lusingander/serie](https://github.com/lusingander/serie) 的 fork，新增了額外功能。

[![Built With Ratatui](https://img.shields.io/badge/Built_With-Ratatui-000?logo=ratatui&logoColor=fff&labelColor=000&color=fff)](https://ratatui.rs)

在終端機中呈現豐富的 git commit 圖，如同魔法般 📚

<img src="./img/demo.gif">

（此 demo 展示的是 [Ratatui](https://github.com/ratatui/ratatui) 儲存庫！）

## 關於

Serie（[`/zéːriə/`](https://lusingander.github.io/serie/faq/index.html#how-do-i-pronounce-serie)）是一個 TUI 應用程式，利用終端模擬器的圖片顯示協議來渲染 commit 圖，效果類似 `git log --graph --all`。

## Fork 新增功能

以下功能為此 fork 新增，原版 serie 不包含：

- **Tag 管理** — 按 `t` 建立 tag，`Ctrl-t` 刪除 tag，支援推送到 remote
- **Remote refs 切換** — 按 `o` 顯示/隱藏 remote-only 的 commit，使用 BFS filtered graph 重新計算佈局
- **Ref 刪除** — 在 refs 列表中刪除 branch（local/remote）或 tag
- **篩選 (Filter)** — 按 `f` 篩選 commit 列表
- **狀態列快捷鍵提示** — 狀態列顯示當前視圖可用的快捷鍵
- **等待覆蓋層** — 長時間 git 操作（push/delete remote）時顯示等待提示

### 為什麼？

雖然有些使用者偏好透過 CLI 使用 Git，但他們在查看 commit 記錄時往往需要依賴 GUI 或功能豐富的 TUI。也有些人覺得 `git log --graph` 就已足夠。

就我個人而言，即使加上額外選項，`git log --graph` 的輸出仍然難以閱讀。僅僅為了查看記錄就去學習複雜的工具，似乎太過繁瑣。

### 目標

- 在終端機中提供豐富的 `git log --graph` 體驗。
- 提供以 commit 圖為核心的 Git 儲存庫瀏覽方式。

### 非目標

- 實作功能完整的 Git 客戶端。
- 建立具有複雜 UI 的 TUI 應用程式。
- 在任何終端環境中都能運作。

## 文件

如需詳細的使用方式、設定和進階功能，請參閱[完整文件](https://lusingander.github.io/serie/)。

## 系統需求

- Git
- 支援的終端模擬器
  - 詳情請參閱[相容性](https://lusingander.github.io/serie/getting-started/compatibility.html)。

## 安裝

從此 fork 安裝（需要 Rust toolchain）：

```
$ cargo install --git https://github.com/YSzEthan/serie-ys.git
```

或從原版 crates.io 安裝（不含 fork 新增功能）：

```
$ cargo install --locked serie
```

其他下載方式請參閱[安裝說明](https://lusingander.github.io/serie/getting-started/installation.html)。

## 使用方式

### 基本用法

在你的 git 儲存庫目錄中執行 `serie`：

```
$ cd <你的 git 儲存庫>
$ serie
```

### 選項

```
Serie - 在終端機中呈現豐富的 git commit 圖，如同魔法般 📚

用法：serie [OPTIONS]

選項：
  -n, --max-count <NUMBER>        渲染的最大 commit 數量
  -p, --protocol <TYPE>           渲染圖形的圖片協議 [預設: auto] [可選值: auto, iterm, kitty]
  -o, --order <TYPE>              Commit 排序演算法 [預設: chrono] [可選值: chrono, topo]
  -g, --graph-width <TYPE>        Commit 圖形的儲存格寬度 [預設: auto] [可選值: auto, double, single]
  -s, --graph-style <TYPE>        Commit 圖形的邊線風格 [預設: rounded] [可選值: rounded, angular]
  -i, --initial-selection <TYPE>  初始選取的 commit [預設: latest] [可選值: latest, head]
      --preload                   預先載入所有圖形圖片
  -h, --help                      顯示說明
  -V, --version                   顯示版本
```

各選項的詳細說明請參閱[命令列選項](https://lusingander.github.io/serie/getting-started/command-line-options.html)。

### 快捷鍵

按 `?` 鍵即可查看快捷鍵列表。

[預設快捷鍵](https://lusingander.github.io/serie/keybindings/index.html)可以自訂覆蓋。詳情請參閱[自訂快捷鍵](https://lusingander.github.io/serie/keybindings/custom-keybindings.html)。

### 設定

設定檔按以下優先順序載入：

- `$SERIE_CONFIG_FILE`
  - 若已設定 `$SERIE_CONFIG_FILE` 但檔案不存在，將會產生錯誤。
- `$XDG_CONFIG_HOME/serie/config.toml`
  - 若未設定 `$XDG_CONFIG_HOME`，則使用 `~/.config/`。

若設定檔不存在，所有項目將使用預設值。
若設定檔存在但部分項目未設定，未設定的項目將使用預設值。

設定檔格式的詳細資訊請參閱[設定檔格式](https://lusingander.github.io/serie/configurations/config-file-format.html)。

### 使用者自訂指令

使用者自訂指令功能可讓你執行自訂的外部指令。
你可以在專用視圖中顯示像 `git diff` 這樣的指令輸出，或在背景執行像刪除分支這樣的指令。

指令設定方式詳見[使用者自訂指令](https://lusingander.github.io/serie/features/user-command.html)。

## 相容性

### 支援的終端機

支援以下圖片協議：

- [Inline Images Protocol (iTerm2)](https://iterm2.com/documentation-images.html)
- [Terminal graphics protocol (kitty)](https://sw.kovidgoyal.net/kitty/graphics-protocol/)

更多資訊請參閱[相容性](https://lusingander.github.io/serie/getting-started/compatibility.html)。

### 不支援的環境

- 不支援 Sixel 圖形。
- 不支援終端多工器（screen、tmux、Zellij 等）。

## 截圖

<img src="./img/list.png" width=600>
<img src="./img/detail.png" width=600>
<img src="./img/refs.png" width=600>
<img src="./img/searching.png" width=600>
<img src="./img/applied.png" width=600>
<img src="./img/diff_git.png" width=600>
<img src="./img/diff_difft.png" width=600>

以下儲存庫用於上述範例：

- [ratatui/ratatui](https://github.com/ratatui/ratatui)
- [charmbracelet/vhs](https://github.com/charmbracelet/vhs)
- [lusingander/stu](https://github.com/lusingander/stu)

## 貢獻

如需開始貢獻，請先閱讀 [CONTRIBUTING.md](CONTRIBUTING.md)。

未遵循這些指引的貢獻可能不會被接受。

## 授權條款

MIT

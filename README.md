# Serie-ys

> 這是 [lusingander/serie](https://github.com/lusingander/serie) 的 fork，新增了額外功能。

[![Built With Ratatui](https://img.shields.io/badge/Built_With-Ratatui-000?logo=ratatui&logoColor=fff&labelColor=000&color=fff)](https://ratatui.rs)

Git Graph in Terminal

<img src="./docs/src/img/list.svg" width="100%">

（畫面由 `scripts/capture_screenshots.py` 從實際執行中的 `ysgit` 擷取）

## 關於

Serie（[`/zéːriə/`](docs/src/faq/index.md)）是一個 TUI 應用程式，用 Unicode 製表字元渲染 commit 圖，效果類似 `git log --graph --all`。

## Fork 新增功能

以下功能為此 fork 新增，原版 serie 不包含：

- **GitHub Issue/PR 瀏覽器** — 按 `g` 開啟，瀏覽與篩選 issue／PR、切換 checkbox、三階段確認 merge PR、開關 issue/PR 狀態、複製連結或在瀏覽器開啟
- **Tag 管理** — 按 `t` 建立 tag，`Ctrl-t` 刪除 tag，支援推送到 remote
- **Remote refs 切換** — 按 `o` 顯示/隱藏 remote-only 的 commit，使用 BFS filtered graph 重新計算佈局
- **Ref 刪除** — 在 refs 列表中刪除 branch（local/remote）或 tag
- **篩選 (Filter)** — 按 `'` 篩選 commit 列表（`f` 是 fetch）
- **緊湊模式 (Compact)** — `-c` 讓 commit 文字貼齊該列圖形實際延伸的位置，依終端機寬度自動判斷要不要開
- **互動式啟動精靈** — 在真人終端機下按 `-h` 不是印說明就結束，而是跳出選單讓你逐項調好再直接啟動（見下方[選項](#選項)）
- **互動式目錄瀏覽器** — `-p` 用 ranger 式單欄介面挑 `[PATH]`，含 `.git` 的目錄會標記出來
- **狀態列快捷鍵提示** — 狀態列顯示當前視圖可用的快捷鍵
- **等待覆蓋層** — 長時間 git 操作（push/delete remote）時顯示等待提示

### 為什麼？

有些人平常用 CLI 操作 git，但要看 commit 記錄時還是得開 GUI 或功能齊全的 TUI。也有人覺得 `git log --graph` 就夠了。

我自己是覺得 `git log --graph` 就算加了選項還是很難讀。但為了看個記錄去學一套複雜的工具，又太麻煩。

### 目標

- 在終端機中提供豐富的 `git log --graph` 體驗。
- 提供以 commit 圖為核心的 Git 儲存庫瀏覽方式。

### 非目標

- 實作功能完整的 Git 客戶端。
- 建立具有複雜 UI 的 TUI 應用程式。

## 文件

完整的使用方式與設定說明在 [docs/](docs/src/SUMMARY.md)。

## 系統需求

- Git
- 能顯示 Unicode 製表字元（`● ◯ │ ─ ╭ ╮ ╯ ╰`）的終端機
  - 字型缺字時改用 `-s angular`（直角，字型涵蓋率較高）或 `-s ascii`（純 ASCII）。
  - 詳情請參閱[相容性](docs/src/getting-started/compatibility.md)。

## 安裝

從 [releases](https://github.com/YSzEthan/serie-ys/releases) 下載預先編譯好的執行檔（macOS arm64／x64、Linux、Windows，附 `checksum.txt`），或用 Rust toolchain 自行安裝：

```
$ cargo install --git https://github.com/YSzEthan/serie-ys.git
```

也可以從原版 crates.io 安裝（不含 fork 新增功能，執行檔名為 `serie`）：

```
$ cargo install --locked serie
```

各平台的詳細步驟請參閱 [docs/src/getting-started/installation.md](docs/src/getting-started/installation.md)。

## 使用方式

### 基本用法

在你的 git 儲存庫目錄中執行 `ysgit`，或直接把路徑當參數傳進去：

```
$ cd <你的 git 儲存庫>
$ ysgit

$ ysgit <你的 git 儲存庫>
```

### 選項

```
ysgit - Git Graph in Terminal

Usage: ysgit [OPTIONS] [PATH]

Arguments:
  [PATH]  git 倉庫路徑 [default: current directory]

Options:
  -p, --path-browser              以互動式目錄瀏覽器選擇 [PATH]（類似 ranger；可搭配上面的路徑引數指定起始目錄）
  -n, --max-count <NUMBER>        要渲染的最大 commit 數量
  -o, --order <TYPE>              Commit 排序演算法 [default: chrono] [possible values: chrono, topo]
  -g, --graph-width <TYPE>        Commit 圖形格子寬度 [default: auto] [possible values: auto, double, single]
  -c, --compact <TYPE>            緊湊模式：commit 文字貼齊該列 graph 實際畫到的最右邊，不保留固定留白 [default: auto] [possible values: auto, on, off]
  -s, --graph-style <TYPE>        Commit 圖形邊線風格 [default: rounded] [possible values: rounded, angular, ascii]
  -i, --initial-selection <TYPE>  初始選取的 commit [default: latest] [possible values: latest, head]
  -h, --help                      顯示說明
  -V, --version                   顯示版本
  -U, --update                    檢查 GitHub Release 並更新執行檔本身
```

> **`-h` 上面寫的「顯示說明」只是非 TTY（管線、CI）下的行為。** 在真人終端機直接執行
> `ysgit -h`／`--help` 會跳出互動式啟動精靈：上下鍵（或 `k`／`j`）在選項間移動，左右鍵
> （或 `h`／`l`）原地輪迴切換該選項的值，`Enter` 直接用選好的組合啟動，也可以先請它印出
> 等效的指令字串。`Esc`／`Ctrl-C`／`Ctrl-D` 放棄，跟原本 `--help` 一樣 exit 0。
>
> 精靈裡的 `[PATH]` 那一列用的就是 `-p` 的目錄瀏覽器，兩者共用同一份實作。

> 此 fork 的執行檔名為 `ysgit`（見 `Cargo.toml` 的 `[[bin]]`），不是上游的 `serie`。

各選項的詳細說明請參閱[命令列選項](docs/src/getting-started/command-line-options.md)。

### 快捷鍵

按 `?` 鍵即可查看快捷鍵列表。

[預設快捷鍵](docs/src/keybindings/index.md)可以自訂覆蓋。詳情請參閱[自訂快捷鍵](docs/src/keybindings/custom-keybindings.md)。

### 設定

設定檔按以下優先順序載入：

- `$SERIE_CONFIG_FILE`
  - 若已設定 `$SERIE_CONFIG_FILE` 但檔案不存在，將會產生錯誤。
- `$XDG_CONFIG_HOME/serie/config.toml`
  - 若未設定 `$XDG_CONFIG_HOME`，則使用 `~/.config/`。

沒設到的項目一律用預設值，不管是整份設定檔不存在，還是檔案裡漏了某幾項。

設定檔格式的詳細資訊請參閱[設定檔格式](docs/src/configurations/config-file-format.md)。

舊設定檔若還留著 `core.option.protocol`、`graph.row_image_width` 這類已隨圖片渲染路徑移除的鍵，會是「schema 擋、runtime 不擋」—— 編輯器標紅，但程式照常啟動、該鍵不生效。詳見[舊設定檔裡的死鍵](docs/src/configurations/index.md)。

### 使用者自訂指令

綁一個按鍵去跑你自己的外部指令。`git diff` 這種要看輸出的會開專用視圖顯示，刪 branch 這種不用看的在背景跑，`vim` 這種要搶終端機的則先把應用程式暫停。

指令設定方式詳見[使用者自訂指令](docs/src/features/user-command.md)。

## 相容性

commit 圖是用一般文字繪製的，不送任何圖片跳脫序列。因此**沒有終端機白名單**：只要能畫出 Unicode 製表字元的終端機都能用。

- 字型缺 `● ◯ │ ─ ╭ ╮ ╯ ╰` 這些字時，用 `-s angular`（直角）或 `-s ascii`（`* o | - +`）。
- 終端多工器（tmux、screen、Zellij 等）**可以正常使用**。上游因為圖片協議無法穿透多工器而排除它們，這個限制在此 fork 已經不存在。
- Sixel、iTerm2 inline images、kitty graphics protocol 都不再需要，有沒有支援都不影響。

> 這是此 fork 與上游最大的行為差異，上游文件的[相容性](https://lusingander.github.io/serie/getting-started/compatibility.html)一節不適用於這裡。

## 截圖

Commit 詳情（`Enter`）：

<img src="./docs/src/img/detail.svg" width="100%">

Refs 清單（`Tab`）—— 此 fork 可以在這裡刪除 branch 與 tag：

<img src="./docs/src/img/refs.svg" width="100%">

篩選（`'`）—— 此 fork 新增，會用 BFS 重算 filtered graph 的佈局：

<img src="./docs/src/img/filter.svg" width="100%">

這些不是螢幕截圖，是從執行中的 `ysgit` 擷取出來的 SVG。作法與重跑方式見[截圖](docs/src/features/screenshots.md)。

## 貢獻

動手之前請先看過 [CONTRIBUTING.md](CONTRIBUTING.md)。

未遵循這些指引的貢獻可能不會被接受。

## 授權條款

MIT

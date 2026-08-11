# 設定檔格式

## 範例

```toml
[core.option]
order = "chrono"
graph_width = "auto"
compact = "auto"
graph_style = "rounded"
initial_selection = "latest"

[core.update]
mode = "check"
interval_hours = 6
auto_restart = "off"

[core.search]
ignore_case = false
fuzzy = false

[core.user_command]
tab_width = 4

[core.external]
clipboard = "Auto"

[ui]
cursor_type = "Native"
refs_width = 26

[ui.pane_height]
detail = 20
diff = 20
user_command = 20

[ui.list]
columns = ["graph", "marker", "subject", "date", "name", "hash"]
subject_min_width = 20
date_format = "%Y-%m-%d"
date_width = 10
date_local = true
name_width = 20

[ui.detail]
date_format = "%Y-%m-%d %H:%M:%S %z"
date_local = true

[color]
fg = "reset"
bg = "reset"
list_selected_fg = "white"
list_selected_bg = "dark-gray"
list_ref_paren_fg = "yellow"
list_ref_branch_fg = "green"
list_ref_remote_branch_fg = "red"
list_ref_tag_fg = "yellow"
list_ref_stash_fg = "magenta"
list_head_fg = "cyan"
list_subject_fg = "reset"
list_name_fg = "cyan"
list_hash_fg = "yellow"
list_date_fg = "magenta"
list_match_fg = "black"
list_match_bg = "yellow"
detail_label_fg = "reset"
detail_name_fg = "reset"
detail_date_fg = "reset"
detail_email_fg = "blue"
detail_hash_fg = "reset"
detail_ref_branch_fg = "green"
detail_ref_remote_branch_fg = "red"
detail_ref_tag_fg = "yellow"
detail_file_change_add_fg = "green"
detail_file_change_modify_fg = "yellow"
detail_file_change_delete_fg = "red"
detail_file_change_move_fg = "magenta"
diff_title_path_fg = "208"
diff_title_hunk_fg = "cyan"
ref_selected_fg = "white"
ref_selected_bg = "dark-gray"
help_block_title_fg = "green"
help_key_fg = "yellow"
virtual_cursor_fg = "reset"
status_input_fg = "reset"
status_input_transient_fg = "dark-gray"
status_interactive_fg = "yellow"
status_info_fg = "cyan"
status_success_fg = "green"
status_warn_fg = "yellow"
status_error_fg = "red"
divider_fg = "dark-gray"

[color.graph]
branches = [
  "#E06C76",
  "#98C379",
  "#E5C07B",
  "#61AFEF",
  "#C678DD",
  "#56B6C2",
]

[keybind]
# 詳見另一節「自訂快捷鍵」。
# ...
```

## 設定項目

### `core.option.order`

Commit 排序演算法。

- 型別：`string`（enum）
- 預設值：`chrono`
- 可選值：
  - `chrono`
  - `topo`

命令列參數指定的值優先。

### `core.option.graph_width`

圖形的每一欄佔用幾個終端字元格。

- 型別：`string`（enum）
- 預設值：`auto`
- 可選值：
  - `auto`
  - `double`
  - `single`

命令列參數指定的值優先。

### `core.option.compact`

緊湊模式：commit 的說明文字貼齊該列 graph 實際畫到的最右邊，不保留固定
留白，marker 欄也一併拿掉。

- 型別：`string`（enum）
- 預設值：`auto`
- 可選值：
  - `auto`：依終端機寬度決定
  - `on`
  - `off`

命令列參數指定的值優先。

### `core.option.graph_style`

Commit 圖形的邊線風格。

- 型別：`string`（enum）
- 預設值：`rounded`
- 可選值：
  - `rounded`
  - `angular`
  - `ascii`

命令列參數指定的值優先。

### `core.option.initial_selection`

啟動時初始選取的 commit。

- 型別：`string`（enum）
- 預設值：`latest`
- 可選值：
  - `latest`
  - `head`

命令列參數指定的值優先。

### `core.option.max_count`

要渲染的最大 commit 數量。

- 型別：`integer`
- 預設值：無（不限制，渲染全部 commit）

命令列參數指定的值優先。

### `core.update.mode`

自動更新檢查模式。

- 型別：`string`（enum）
- 預設值：`check`
- 可選值：
  - `off`：完全不自動檢查——手動的 <kbd>U</kbd> 鍵／`-U` 不受影響
  - `check`：查到新版問 y/n
  - `auto`：查到新版就直接下載替換，不問

命令列參數指定的值優先。

### `core.update.interval_hours`

自動更新的檢查間隔（小時），啟動時查一次、之後持續運作期間每隔這個時間再查一次。

- 型別：`integer`
- 預設值：`6`
- 可選範圍：`1`–`48`

命令列參數指定的值優先。

### `core.update.auto_restart`

更新完成後是否自動重啟（TUI）／開啟新版（CLI），不再詢問。

- 型別：`string`（enum）
- 預設值：`off`
- 可選值：
  - `off`
  - `on`

命令列參數指定的值優先。

### `core.search.ignore_case`

是否預設啟用忽略大小寫。

- 型別：`boolean`
- 預設值：`false`

### `core.search.fuzzy`

是否預設啟用模糊比對。

- 型別：`boolean`
- 預設值：`false`

### `core.user_command.commands_{n}`

執行外部指令的指令定義。

可以用 `commands_{n}` 的格式指定多組。詳情請參閱另一節[使用者自訂指令](../features/user-command.md)。

- 型別：`object`
- 欄位：
  - `name`：`string` —— 指令名稱。
  - `type`：`string`（enum）—— 指令型別。
    - 預設值：`inline`
    - 可選值：
      - `inline`：在使用者自訂指令視圖中顯示輸出。
      - `silent`：在背景執行，不開啟視圖。
      - `suspend`：暫停應用程式後執行，適合互動式指令。
  - `commands`：`array of strings` —— 指令與其引數。
  - `refresh`：`boolean` —— 執行後是否重新載入儲存庫並更新畫面。僅 `silent` 與 `suspend` 可用。
    - 預設值：`false`
- 範例：
  - `commands_1 = { name = "git diff", commands = ["git", "--no-pager", "diff", "--color=always", "{{first_parent_hash}}", "{{target_hash}}"]}`
  - `commands_2 = { name = "delete branch", type = "silent", commands = ["git", "branch", "-D", "{{branches}}"], refresh = true }`
  - `commands_3 = { name = "amend commit", type = "suspend", commands = ["git", "commit", "--amend"], refresh = true }`

### `core.user_command.tab_width`

使用者自訂指令輸出中，tab 展開成幾個空白。

- 型別：`u16`
- 預設值：`4`

### `core.external.clipboard`

複製操作使用的剪貼簿方式。

- 型別：`object`（enum）
- 預設值：`Auto`
- 可選值：
  - `Auto`：使用預設剪貼簿函式庫（`arboard`）。當 `$SSH_CONNECTION` 或 `$SSH_TTY` 有設定時，改用 `Osc52`，讓複製結果進到**本機**剪貼簿，而不是遠端主機的 X11／Wayland 剪貼簿。
  - `Osc52`：一律對 stdout 送出 OSC 52 終端跳脫序列；支援的終端機（iTerm2、Kitty、WezTerm、foot、開了 `set-clipboard on` 的 tmux 等）會把文字寫入本機剪貼簿。不需要 X11 forwarding 就能透過 SSH 運作。
  - `{ Custom = { commands = ["..."] } }`：使用自訂指令，文字透過 stdin 傳入。
    - `commands`：`array of strings` —— 指令與其引數。
- 範例：
  - `clipboard = "Auto"`
  - `clipboard = "Osc52"`
  - `clipboard = { Custom = { commands = ["wl-copy"] } }`
  - `clipboard = { Custom = { commands = ["xclip", "-selection", "clipboard"] } }`

`Osc52` 的注意事項：

- tmux：3.3 以後 `allow-passthrough` 預設關閉，需要在 `~/.tmux.conf` 加上 `set -g allow-passthrough on`。若你已經設了 `set -g set-clipboard on`，tmux 會自行處理 OSC 52，不需要 passthrough。
- 不支援的終端機會靜默忽略這個序列（不會有錯誤訊息），但這仍然好過以往那種複製到錯誤主機剪貼簿的行為。

### `ui.cursor_type`

輸入欄位中游標的顯示方式。

- 型別：`object`（enum）
- 預設值：`Native`
- 可選值：
  - `Native`：使用終端機原生游標。
  - `{ Virtual = "|" }`：使用指定字串當作虛擬游標。
    - 值：`string` —— 用來顯示虛擬游標的字串。

### `ui.refs_width`

Refs 清單區域的寬度。

- 型別：`u16`
- 預設值：`26`

### `ui.pane_height.detail`

Commit 詳情區域的高度。

- 型別：`u16`
- 預設值：`20`

### `ui.pane_height.diff`

Commit 詳情中，選取檔案後底部單一檔案 diff 區域的高度。詳情請參閱另一節
[在 Detail view 中檢視檔案 diff](../features/file-diff.md)。

- 型別：`u16`
- 預設值：`20`

### `ui.pane_height.user_command`

使用者自訂指令區域的高度。

- 型別：`u16`
- 預設值：`20`

### `ui.list.columns`

Commit 清單中各欄的順序與顯示與否。

- 型別：`array of strings`（enum）
- 預設值：`["graph", "marker", "subject", "date", "name", "hash"]`
- 可選值：
  - `graph`
  - `marker`
  - `subject`
  - `name`
  - `hash`
  - `date`

### `ui.list.subject_min_width`

Commit 清單中 subject 的最小寬度。

- 型別：`u16`
- 預設值：`20`

### `ui.list.date_format`

Commit 清單中 author date 的日期格式。

- 型別：`string`
- 預設值：`"%Y-%m-%d"`

格式須使用 strftime 格式：
https://docs.rs/chrono/latest/chrono/format/strftime/index.html

### `ui.list.date_width`

Commit 清單中 author date 的寬度。

- 型別：`u16`
- 預設值：`10`

### `ui.list.date_local`

Commit 清單中的 author date 是否以本地時區顯示。

- 型別：`boolean`
- 預設值：`true`

### `ui.list.name_width`

Commit 清單中 author name 的寬度。

- 型別：`u16`
- 預設值：`20`

### `ui.detail.date_format`

Commit 詳情中 author／committer date 的日期格式。

- 型別：`string`
- 預設值：`"%Y-%m-%d %H:%M:%S %z"`

格式須使用 strftime 格式：
https://docs.rs/chrono/latest/chrono/format/strftime/index.html

### `ui.detail.date_local`

Commit 詳情中的 author／committer date 是否以本地時區顯示。

- 型別：`boolean`
- 預設值：`true`

### `color`

應用程式各元素的顏色。

註：圖形的顏色是用 `[color.graph]` 指定的（見下一節）。

- 型別：`string`
- 預設值：見上方範例

顏色可以用以下任一種格式指定：

- ANSI 顏色名稱
  - `"red"`、`"bright-blue"`、`"light-red"`、`"reset"` 等
- 8-bit（256 色）索引值
  - `"34"`、`"128"`、`"255"` 等
- 24-bit true color 十六進位碼
  - `"#abcdef"` 等

### `color.graph.branches`

Commit 圖形使用的顏色陣列。

- 型別：`array of strings`
- 預設值：
  - `"#E06C76"`
  - `"#98C379"`
  - `"#E5C07B"`
  - `"#61AFEF"`
  - `"#C678DD"`
  - `"#56B6C2"`

顏色須以 `#RRGGBB` 或 `#RRGGBBAA` 格式指定。`AA`（alpha）仍會做 hex 格式檢查，但數值會被丟棄 —— 圖形改用文字繪製後就沒有任何消費者需要 alpha 了。

> `color.graph` 底下**只有** `branches` 一個鍵。舊版的 `edge` 與 `background` 已隨圖片渲染路徑一起移除，留在設定檔裡會被 schema 判定為非法。

### `keybind`

應用程式中各個動作的快捷鍵。

詳情請參閱另一節[自訂快捷鍵](../keybindings/custom-keybindings.md)。

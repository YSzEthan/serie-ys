# 使用者自訂指令

綁一個按鍵去跑你自己的外部指令。共有三種型別：`inline`、`silent` 與 `suspend`。

- `inline`（預設）
  - 在 TUI 的專用視圖中顯示指令的輸出（stdout）。
  - 例如可以用你慣用的工具檢視 commit diff。
- `silent`
  - 在背景執行指令，不開啟視圖。
  - 適合不需要看輸出的操作，例如刪除 branch 或加 tag。
- `suspend`
  - 暫停應用程式後執行指令。
  - 適合需要控制終端機的互動式指令，例如會開編輯器的 `git commit --amend`，或搭配 pager 的 `git diff`。

定義一個使用者自訂指令需要設定兩項：

- 快捷鍵定義。指定執行各個使用者自訂指令的按鍵。
  - 設定鍵名：`keybind.user_command_{n}`
- 指令定義。指定實際要執行的指令。
  - 設定鍵名：`core.user_command.commands_{n}`

`config.toml` 設定範例：

```toml
[keybind]
user_command_1 = ["d"]
user_command_2 = ["shift-d"]
user_command_3 = ["b"]
user_command_4 = ["a"]

[core.user_command]
# Inline 指令（預設）
commands_1 = { "name" = "git diff", commands = ["git", "--no-pager", "diff", "--color=always", "{{first_parent_hash}}", "{{target_hash}}"] }
# Inline 指令，並指定顯示區域大小
commands_2 = { "name" = "xxx", commands = ["xxx", "{{first_parent_hash}}", "{{target_hash}}", "--width", "{{area_width}}", "--height", "{{area_height}}"] }
# Silent 指令，執行後重新載入
commands_3 = { "name" = "delete branch", type = "silent", commands = ["git", "branch", "-D", "{{branches}}"], refresh = true }
# Suspend 指令，執行後重新載入
commands_4 = { "name" = "amend commit", type = "suspend", commands = ["git", "commit", "--amend"], refresh = true }
```

## Refresh

`silent` 與 `suspend` 型別可以設定 `refresh = true`，指令執行完畢後自動重新載入儲存庫並更新畫面（例如 commit 清單）。指令會改動儲存庫狀態時很有用。

注意 `refresh = true` 不能用在 `inline` 指令上。

## 變數

指令定義中可以使用以下變數，執行時會替換成對應的值。

### 變數清單

- `{{target_hash}}`
  - 選取的 commit 的 hash。
  - 範例：`b0ce4cb9c798576af9b4accc9f26ddce5e72063d`
- `{{first_parent_hash}}`
  - 選取的 commit 的第一個 parent 的 hash。
  - 範例：`c103d9744df8ebf100773a11345f011152ec5581`
- `{{parent_hashes}}`
  - 選取的 commit 的所有 parent 的 hash，以空白分隔。
  - 範例：`c103d9744df8ebf100773a11345f011152ec5581 a1b2c3d4e5f67890123456789abcdef0123456789`
- `{{refs}}`
  - 指向選取 commit 的所有 ref（branch、remote branch、tag）名稱，以空白分隔。
  - 範例：`master v1.0.0`
- `{{branches}}`
  - 指向選取 commit 的所有 branch 名稱，以空白分隔。
  - 範例：`master feature-branch`
- `{{remote_branches}}`
  - 指向選取 commit 的所有 remote branch 名稱，以空白分隔。
  - 範例：`origin/master origin/feature-branch`
- `{{tags}}`
  - 指向選取 commit 的所有 tag 名稱，以空白分隔。
  - 範例：`v1.0.0 v1.0.1`
- `{{stash}}`
  - 選取的 commit 是 stash commit 時，該 stash 的名稱；否則為空字串。
  - 範例：`stash@{0}`
- `{{area_width}}`
  - 使用者自訂指令顯示區域的寬度（字元格數）。
  - 範例：`80`
- `{{area_height}}`
  - 使用者自訂指令顯示區域的高度（字元格數）。
  - 範例：`30`

### 清單型變數與引數展開

代表多個值的變數（上面標示「以空白分隔」的那些）有特殊處理：

- 獨立標記
  - 單獨作為一個引數時（例如 `["git", "branch", "-D", "{{branches}}"]`），會展開成多個獨立引數（例如 `["git", "branch", "-D", "br1", "br2"]`）。
- 混合標記
  - 與其他字元組合時（例如 `["echo", "refs: {{refs}}"]`），會替換成單一的空白分隔字串（例如 `["echo", "refs: ref1 ref2"]`）。
- 空清單
  - 清單為空且作為獨立標記時，該引數會被整個移除（例如 `["git", "branch", "-D", "{{branches}}"]` 變成 `["git", "branch", "-D"]`）。

要把多個值傳給預期分開引數的指令時，建議使用獨立標記，這樣含空白的名稱也能正確處理。

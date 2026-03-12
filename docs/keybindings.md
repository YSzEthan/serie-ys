# Serie 快捷鍵總覽

所有快捷鍵皆可透過 `~/.config/serie/config.toml` 自訂覆蓋。
預設設定檔：[`assets/default-keybind.toml`](../assets/default-keybind.toml)

---

## Common（全域通用）

| 快捷鍵 | 動作 |
|---|---|
| `ctrl-c` | 強制退出 |
| `q` | 退出 |
| `?` | 開啟 Help |

---

## List（Commit 列表）

預設啟動畫面。

### 一般模式

| 快捷鍵 | 動作 |
|---|---|
| `j` / `down` / `shift-j` | 向下移動 |
| `k` / `up` / `shift-k` | 向上移動 |
| `alt-j` / `alt-down` | 跳到父 commit |
| `g` | 跳到頂 |
| `shift-g` | 跳到底 |
| `ctrl-f` / `pagedown` | 整頁向下翻動 |
| `ctrl-b` / `pageup` | 整頁向上翻動 |
| `ctrl-d` | 半頁向下翻動 |
| `ctrl-u` | 半頁向上翻動 |
| `ctrl-e` | 逐行向下捲動 |
| `ctrl-y` | 逐行向上捲動 |
| `shift-h` | 選取畫面頂部 |
| `shift-m` | 選取畫面中間 |
| `shift-l` | 選取畫面底部 |
| `enter` / `l` / `right` | 開啟 Detail 視圖 |
| `tab` | 開啟 Refs 列表 |
| `/` | 開始搜尋 |
| `f` | 開始過濾 |
| `n` | 下一筆搜尋結果 |
| `shift-n` | 上一筆搜尋結果 |
| `esc` | 取消搜尋/過濾 |
| `c` | 複製 short commit hash |
| `shift-c` | 複製完整 commit hash |
| `t` | 開啟 CreateTag 對話框 |
| `ctrl-t` | 開啟 DeleteTag 對話框 |
| `o` | 切換遠端 refs 顯示 |
| `space` | 開啟 GitHub Issues/PRs |
| `r` / `shift-r` | 重新整理 |
| `d` | 執行 User Command 1 |

### 搜尋模式（Searching）

按下 `/` 進入搜尋模式。

| 快捷鍵 | 動作 |
|---|---|
| 文字輸入 | 輸入搜尋關鍵字 |
| `enter` | 套用搜尋 |
| `esc` | 取消搜尋 |
| `alt-c` | 切換大小寫敏感 |
| `ctrl-x` | 切換模糊搜尋 |

### 過濾模式（Filtering）

按下 `f` 進入過濾模式。

| 快捷鍵 | 動作 |
|---|---|
| 文字輸入 | 輸入過濾關鍵字 |
| `enter` | 套用過濾 |
| `esc` | 取消過濾 |
| `alt-c` | 切換大小寫敏感 |
| `ctrl-x` | 切換模糊搜尋 |

---

## Detail（Commit 詳細資訊）

從 List 視圖按下 `enter` 進入。

| 快捷鍵 | 動作 |
|---|---|
| `enter` / `esc` / `backspace` | 關閉詳細頁 |
| `y`（硬編碼） | 關閉詳細頁 |
| `u` | 切換 pane（diff / 檔案清單） |
| `j` / `down` | 向下捲動 |
| `k` / `up` | 向上捲動 |
| `ctrl-f` / `pagedown` | 整頁向下翻動 |
| `ctrl-b` / `pageup` | 整頁向上翻動 |
| `ctrl-d` | 半頁向下翻動 |
| `ctrl-u` | 半頁向上翻動 |
| `g` | 跳到頂 |
| `shift-g` | 跳到底 |
| `l` / `right` | 選取更舊的 commit |
| `h` / `left` | 選取更新的 commit |
| `shift-j` | 選取更舊的 commit |
| `shift-k` | 選取更新的 commit |
| `alt-j` / `alt-down` | 選取父 commit |
| `c` | 複製 short commit hash |
| `shift-c` | 複製完整 commit hash |
| `o` | 切換遠端 refs 顯示 |
| `?` | 開啟 Help |
| `r` / `shift-r` | 重新整理 |
| `d` | 執行 User Command |

---

## Refs（分支/標籤參照列表）

從 List 視圖按下 `tab` 進入。

| 快捷鍵 | 動作 |
|---|---|
| `esc` / `backspace` / `tab` | 關閉 Refs 列表 |
| `j` / `down` / `shift-j` | 向下移動 |
| `k` / `up` / `shift-k` | 向上移動 |
| `g` | 跳到頂 |
| `shift-g` | 跳到底 |
| `l` / `right` | 展開節點 |
| `h` / `left` | 收合節點 |
| `c` / `shift-c` | 複製 ref 名稱 |
| `d` / `ctrl-t` | 開啟 DeleteRef 對話框 |
| `?` | 開啟 Help |
| `r` / `shift-r` | 重新整理 |

---

## UserCommand（使用者自訂指令輸出）

從 List / Detail 視圖按下 `d` 進入。

| 快捷鍵 | 動作 |
|---|---|
| `esc` / `backspace` | 關閉 |
| `j` / `down` | 向下捲動 |
| `k` / `up` | 向上捲動 |
| `ctrl-f` / `pagedown` | 整頁向下翻動 |
| `ctrl-b` / `pageup` | 整頁向上翻動 |
| `ctrl-d` | 半頁向下翻動 |
| `ctrl-u` | 半頁向上翻動 |
| `g` | 跳到頂 |
| `shift-g` | 跳到底 |
| `shift-j` | 選取更舊的 commit |
| `shift-k` | 選取更新的 commit |
| `alt-j` / `alt-down` | 選取父 commit |
| `enter` | 開啟 Detail |
| `t` | 開啟 CreateTag |
| `d` | 切換關閉（同鍵再按關閉），其他 `user_command_N` 切換指令 |
| `?` | 開啟 Help |
| `r` / `shift-r` | 重新整理 |

---

## Help（幫助畫面）

從任意視圖按下 `?` 進入。

| 快捷鍵 | 動作 |
|---|---|
| `?` / `esc` / `backspace` | 關閉 Help |
| `j` / `down` / `shift-j` | 向下捲動 |
| `k` / `up` / `shift-k` | 向上捲動 |
| `ctrl-f` / `pagedown` | 整頁向下翻動 |
| `ctrl-b` / `pageup` | 整頁向上翻動 |
| `ctrl-d` | 半頁向下翻動 |
| `ctrl-u` | 半頁向上翻動 |
| `g` | 跳到頂 |
| `shift-g` | 跳到底 |

---

## CreateTag（建立標籤對話框）

從 List 視圖按下 `t` 進入。

三個焦點欄位：Tag name → Message → Push checkbox。

| 快捷鍵 | 動作 |
|---|---|
| `esc` | 取消 |
| `enter` | 送出建立 |
| `tab` / `backtab` | 切換焦點欄位 |
| `j` / `down` | 切換焦點到下一個欄位 |
| `k` / `up` | 切換焦點到上一個欄位 |
| `l` / `right` / `h` / `left` | checkbox 上切換勾選；文字欄位上移動游標 |
| `space` | 在 checkbox 上切換勾選 |
| 文字輸入 | 輸入 tag 名稱或 message |

---

## DeleteTag（刪除標籤對話框）

從 List 視圖按下 `ctrl-t` 進入。

| 快捷鍵 | 動作 |
|---|---|
| `esc` | 取消 |
| `enter` | 確認刪除 |
| `j` / `down` / `shift-j` | 選取下一個 tag |
| `k` / `up` / `shift-k` | 選取上一個 tag |
| `l` / `right` / `h` / `left` | 切換「Delete from origin」checkbox |

---

## DeleteRef（刪除分支/標籤對話框）

從 Refs 視圖按下 `d` 或 `ctrl-t` 進入。

| 快捷鍵 | 動作 |
|---|---|
| `esc` | 取消 |
| `enter` | 確認刪除 |
| `l` / `right` / `h` / `left` / `j` / `down` | 切換 checkbox（Tag: delete from remote / Branch: force delete） |

---

## GitHub（GitHub Issues/PRs 視圖）

從 List 視圖按下 `space` 進入。

### 列表模式

| 快捷鍵 | 動作 |
|---|---|
| `space` / `esc` / `backspace` | 關閉 GitHub 視圖 |
| `tab` | 切換 Issues ↔ PRs 頁籤 |
| `j` / `down` / `shift-j` | 向下移動 |
| `k` / `up` / `shift-k` | 向上移動 |
| `g` | 跳到頂 |
| `shift-g` | 跳到底 |
| `ctrl-f` / `pagedown` | 整頁向下翻動 |
| `ctrl-b` / `pageup` | 整頁向上翻動 |
| `ctrl-d` | 半頁向下翻動 |
| `ctrl-u` | 半頁向上翻動 |
| `enter` | 開啟選取項目的詳情 |
| `f` | 切換狀態過濾（open → closed → all） |
| `r` / `shift-r` | 重新整理 |

### 詳情模式

按下 `enter` 開啟詳情。

| 快捷鍵 | 動作 |
|---|---|
| `esc` / `backspace` | 返回列表 |
| `j` / `down` / `shift-j` | 向下捲動 |
| `k` / `up` / `shift-k` | 向上捲動 |
| `ctrl-f` / `pagedown` | 整頁向下翻動 |
| `ctrl-b` / `pageup` | 整頁向上翻動 |
| `ctrl-d` | 半頁向下翻動 |
| `ctrl-u` | 半頁向上翻動 |
| `g` | 跳到頂 |

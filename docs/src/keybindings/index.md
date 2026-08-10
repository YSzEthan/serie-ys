# 快捷鍵

<!-- 由 `cargo test` 從 `src/view/help.rs` 產生，請勿手動編輯。 -->
<!-- 重新產生：UPDATE_KEYBINDINGS_DOC=1 cargo test -->

在應用程式中按 <kbd>?</kbd> 可隨時查看這份清單，且已套用你自己的覆寫設定。

以下是預設值，修改方式請參閱[自訂快捷鍵](./custom-keybindings.md)。

## 預設快捷鍵

### 共通

| 按鍵 | 說明 | 設定鍵名 |
| --- | --- | --- |
| <kbd>Ctrl-c</kbd> | 強制離開 | `force_quit` |
| <kbd>q</kbd> | 離開（按兩下） | `quit` |
| <kbd>F1</kbd> <kbd>?</kbd> | 開啟說明 | `help_toggle` |
| <kbd>U</kbd> | 檢查更新 | `check_update` |

### 說明頁

| 按鍵 | 說明 | 設定鍵名 |
| --- | --- | --- |
| <kbd>F1</kbd> <kbd>?</kbd> <kbd>n</kbd> <kbd>Esc</kbd> <kbd>Backspace</kbd> <kbd>Left</kbd> <kbd>h</kbd> | 關閉說明 | `help_toggle` `cancel` `close` `navigate_left` |
| <kbd>Down</kbd> <kbd>j</kbd> <kbd>J</kbd> | 向下捲動 | `navigate_down` `select_down` |
| <kbd>Up</kbd> <kbd>k</kbd> <kbd>K</kbd> | 向上捲動 | `navigate_up` `select_up` |

### Commit 清單

| 按鍵 | 說明 | 設定鍵名 |
| --- | --- | --- |
| <kbd>Down</kbd> <kbd>j</kbd> | 向下移動 | `navigate_down` |
| <kbd>Up</kbd> <kbd>k</kbd> | 向上移動 | `navigate_up` |
| <kbd>i</kbd> | 跳到頂端 | `go_to_top` |
| <kbd>G</kbd> | 跳到底端 | `go_to_bottom` |
| <kbd>.</kbd> | 回到 HEAD | `go_to_head` |
| <kbd>J</kbd> | graph 向下捲動 | `select_down` |
| <kbd>K</kbd> | graph 向上捲動 | `select_up` |
| <kbd>m</kbd> | 選擇 parent commit | `go_to_parent` |
| <kbd>Enter</kbd> <kbd>y</kbd> <kbd>Right</kbd> <kbd>l</kbd> | 顯示 commit 詳情 | `confirm` `navigate_right` |
| <kbd>Tab</kbd> | 開啟 refs 清單 | `ref_list` |
| <kbd>:</kbd> | 開始搜尋 | `search` |
| <kbd>'</kbd> | 開始過濾 | `filter` |
| <kbd>n</kbd> <kbd>Esc</kbd> | 取消搜尋／過濾 | `cancel` |
| <kbd>]</kbd> | 下一個符合項 | `go_to_next` |
| <kbd>[</kbd> | 上一個符合項 | `go_to_previous` |
| <kbd>x</kbd> | 切換模糊比對 | `fuzzy_toggle` |
| <kbd>Alt-c</kbd> | 切換大小寫忽略 | `ignore_case_toggle` |
| <kbd>c</kbd> | 複製 commit short hash | `short_copy` |
| <kbd>C</kbd> | 複製 commit subject | `full_copy` |
| <kbd>b</kbd> | 複製 branch 名稱（優先 local） | `branch_copy` |
| <kbd>B</kbd> | 複製 remote branch 名稱 | `full_branch_copy` |
| <kbd>v</kbd> | 複製 tag 名稱 | `tag_copy` |
| <kbd>t</kbd> | 在 commit 上建立 tag | `create_tag` |
| <kbd>Ctrl-t</kbd> | 刪除 commit 上的 tag | `delete_tag` |
| <kbd>d</kbd> | 刪除 commit 上的 local branch | `delete_ref` |
| <kbd>o</kbd> | 切換 remote refs | `remote_refs_toggle` |
| <kbd>g</kbd> | 開啟 GitHub issues/PRs | `github_toggle` |
| <kbd>f</kbd> | fetch 所有 remote | `fetch` |
| <kbd>Space</kbd> | checkout 選取的 commit/ref | `checkout` |
| <kbd>r</kbd> | 重新整理 | `refresh` |

### Commit 詳情

| 按鍵 | 說明 | 設定鍵名 |
| --- | --- | --- |
| <kbd>n</kbd> <kbd>Esc</kbd> <kbd>Backspace</kbd> <kbd>Enter</kbd> <kbd>y</kbd> | 關閉 commit 詳情 | `cancel` `close` `confirm` |
| <kbd>u</kbd> | 切換詳情區塊 | `detail_pane_toggle` |
| <kbd>Down</kbd> <kbd>j</kbd> | 向下捲動／Files 區塊移動檔案游標 | `navigate_down` |
| <kbd>Up</kbd> <kbd>k</kbd> | 向上捲動／Files 區塊移動檔案游標 | `navigate_up` |
| <kbd>J</kbd> | Files 區塊：diff 逐行下捲 | `select_down` |
| <kbd>K</kbd> | Files 區塊：diff 逐行上捲 | `select_up` |
| <kbd>Ctrl-d</kbd> | Files 區塊：diff 半頁下捲 | `half_page_down` |
| <kbd>Ctrl-u</kbd> | Files 區塊：diff 半頁上捲 | `half_page_up` |
| <kbd>]</kbd> | Files 區塊：跳到下一個 hunk | `go_to_next` |
| <kbd>[</kbd> | Files 區塊：跳到上一個 hunk | `go_to_previous` |
| <kbd>PageDown</kbd> <kbd>Ctrl-f</kbd> | Files 區塊：diff 整頁下捲 | `page_down` |
| <kbd>PageUp</kbd> <kbd>Ctrl-b</kbd> | Files 區塊：diff 整頁上捲 | `page_up` |
| <kbd>Right</kbd> <kbd>l</kbd> | 選擇較舊 commit | `navigate_right` |
| <kbd>Left</kbd> <kbd>h</kbd> | 選擇較新 commit | `navigate_left` |
| <kbd>m</kbd> | 選擇 parent commit | `go_to_parent` |
| <kbd>c</kbd> | 複製 commit short hash | `short_copy` |
| <kbd>C</kbd> | 複製 commit subject | `full_copy` |
| <kbd>b</kbd> | 複製 branch 名稱（優先 local） | `branch_copy` |
| <kbd>B</kbd> | 複製 remote branch 名稱 | `full_branch_copy` |
| <kbd>v</kbd> | 複製 tag 名稱 | `tag_copy` |
| <kbd>o</kbd> | 切換 remote refs | `remote_refs_toggle` |
| <kbd>Tab</kbd> | 開啟 refs 清單 | `ref_list` |
| <kbd>g</kbd> | 開啟 GitHub issues/PRs | `github_toggle` |
| <kbd>F1</kbd> <kbd>?</kbd> | 開啟說明 | `help_toggle` |
| <kbd>r</kbd> | 重新整理 | `refresh` |

### Refs 清單

| 按鍵 | 說明 | 設定鍵名 |
| --- | --- | --- |
| <kbd>n</kbd> <kbd>Esc</kbd> | 關閉 refs 清單 | `cancel` |
| <kbd>Down</kbd> <kbd>j</kbd> <kbd>J</kbd> | 向下移動 | `navigate_down` `select_down` |
| <kbd>Up</kbd> <kbd>k</kbd> <kbd>K</kbd> | 向上移動 | `navigate_up` `select_up` |
| <kbd>Right</kbd> <kbd>l</kbd> | 展開節點 | `navigate_right` |
| <kbd>Left</kbd> <kbd>h</kbd> | 收合節點／關閉 | `navigate_left` |
| <kbd>Space</kbd> | checkout 選取的 branch | `checkout` |
| <kbd>d</kbd> <kbd>Ctrl-t</kbd> | 刪除 ref | `delete_ref` `delete_tag` |
| <kbd>r</kbd> | 重新整理 | `refresh` |

### GitHub View

| 按鍵 | 說明 | 設定鍵名 |
| --- | --- | --- |
| <kbd>g</kbd> <kbd>n</kbd> <kbd>Esc</kbd> <kbd>Backspace</kbd> | 關閉 GitHub view | `github_toggle` `cancel` `close` |
| <kbd>Tab</kbd> | 切換 Issue／PR 分頁 | `ref_list` |
| <kbd>Down</kbd> <kbd>j</kbd> <kbd>J</kbd> | 向下移動 | `navigate_down` `select_down` |
| <kbd>Up</kbd> <kbd>k</kbd> <kbd>K</kbd> | 向上移動 | `navigate_up` `select_up` |
| <kbd>PageDown</kbd> <kbd>Ctrl-f</kbd> | 向下一頁 | `page_down` |
| <kbd>PageUp</kbd> <kbd>Ctrl-b</kbd> | 向上一頁 | `page_up` |
| <kbd>Ctrl-d</kbd> | 向下半頁 | `half_page_down` |
| <kbd>Ctrl-u</kbd> | 向上半頁 | `half_page_up` |
| <kbd>i</kbd> | 跳到頂端 | `go_to_top` |
| <kbd>G</kbd> | 跳到底端 | `go_to_bottom` |
| <kbd>Enter</kbd> <kbd>y</kbd> <kbd>Right</kbd> <kbd>l</kbd> | 預覽內容／切換 checkbox | `confirm` `navigate_right` |
| <kbd>Left</kbd> <kbd>h</kbd> | 返回／取消 | `navigate_left` |
| <kbd>:</kbd> | 搜尋／輸入純數字跳到 #N | `search` |
| <kbd>'</kbd> | 過濾 | `filter` |
| <kbd>c</kbd> | 複製 issue/PR URL | `short_copy` |
| <kbd>C</kbd> | 在瀏覽器開啟 issue/PR | `full_copy` |
| <kbd>v</kbd> | 複製 issue/PR 編號 (#N) | `tag_copy` |
| <kbd>u</kbd> | 開啟相關 issue/PR 選單 | `detail_pane_toggle` |
| <kbd>r</kbd> | 重新整理 | `refresh` |
| <kbd>p</kbd> | 三階段 merge PR：選 method、刪 branch、確認 | `merge_pr` |
| <kbd>X</kbd> | 關閉／重開 issue 或 PR | `toggle_issue_state` |
| <kbd>P</kbd> | PR 定案／打回草稿 | `toggle_pr_draft` |
| <kbd>z</kbd> | 展開／摺疊 commit 記錄 | `toggle_commit_log` |

### Create Tag

| 按鍵 | 說明 | 設定鍵名 |
| --- | --- | --- |
| <kbd>Enter</kbd> <kbd>y</kbd> | 確定建立 | `confirm` |
| <kbd>n</kbd> <kbd>Esc</kbd> | 取消並關閉 | `cancel` |
| <kbd>Down</kbd> <kbd>j</kbd> <kbd>Up</kbd> <kbd>k</kbd> | 切換輸入欄位 | `navigate_down` `navigate_up` |
| <kbd>Right</kbd> <kbd>l</kbd> <kbd>Left</kbd> <kbd>h</kbd> | 切換 push 選項 | `navigate_right` `navigate_left` |

### Delete Tag

| 按鍵 | 說明 | 設定鍵名 |
| --- | --- | --- |
| <kbd>Enter</kbd> <kbd>y</kbd> | 確定刪除 | `confirm` |
| <kbd>n</kbd> <kbd>Esc</kbd> | 取消並關閉 | `cancel` |
| <kbd>Down</kbd> <kbd>j</kbd> <kbd>J</kbd> | 選擇下一個 tag | `navigate_down` `select_down` |
| <kbd>Up</kbd> <kbd>k</kbd> <kbd>K</kbd> | 選擇上一個 tag | `navigate_up` `select_up` |
| <kbd>Right</kbd> <kbd>l</kbd> <kbd>Left</kbd> <kbd>h</kbd> | 切換「從 remote 刪除」 | `navigate_right` `navigate_left` |

### Delete Ref

| 按鍵 | 說明 | 設定鍵名 |
| --- | --- | --- |
| <kbd>Enter</kbd> <kbd>y</kbd> | 確定刪除 ref | `confirm` |
| <kbd>n</kbd> <kbd>Esc</kbd> | 取消 | `cancel` |
| <kbd>Right</kbd> <kbd>l</kbd> <kbd>Left</kbd> <kbd>h</kbd> <kbd>Down</kbd> <kbd>j</kbd> | 切換 yes／no | `navigate_right` `navigate_left` `navigate_down` |

### User Command

| 按鍵 | 說明 | 設定鍵名 |
| --- | --- | --- |
| <kbd>n</kbd> <kbd>Esc</kbd> <kbd>Backspace</kbd> | 關閉 user command | `cancel` `close` |
| <kbd>Down</kbd> <kbd>j</kbd> <kbd>J</kbd> | 向下捲動 | `navigate_down` `select_down` |
| <kbd>Up</kbd> <kbd>k</kbd> <kbd>K</kbd> | 向上捲動 | `navigate_up` `select_up` |
| <kbd>PageDown</kbd> <kbd>Ctrl-f</kbd> | 向下一頁 | `page_down` |
| <kbd>PageUp</kbd> <kbd>Ctrl-b</kbd> | 向上一頁 | `page_up` |
| <kbd>Ctrl-d</kbd> | 向下半頁 | `half_page_down` |
| <kbd>Ctrl-u</kbd> | 向上半頁 | `half_page_up` |
| <kbd>i</kbd> | 跳到頂端 | `go_to_top` |
| <kbd>G</kbd> | 跳到底端 | `go_to_bottom` |
| <kbd>m</kbd> | 選擇 parent commit | `go_to_parent` |
| <kbd>r</kbd> | 重新整理 | `refresh` |
| <kbd>Enter</kbd> <kbd>y</kbd> | 顯示 commit 詳情 | `confirm` |
| <kbd>F1</kbd> <kbd>?</kbd> | 開啟說明 | `help_toggle` |

## 寫死的按鍵

以下按鍵無法透過設定檔變更，因為它們屬於一次性的提示互動，不歸任何 view 的 keymap 管。

| 按鍵 | 出現位置 | 動作 |
| --- | ----- | ------ |
| <kbd>1</kbd>–<kbd>9</kbd> | Ref／checkout／關聯／branch 選擇器 | 選第 n 項 |
| <kbd>m</kbd> <kbd>s</kbd> <kbd>r</kbd> | Merge PR 提示（第 1 步） | merge／squash／rebase |
| <kbd>y</kbd> <kbd>n</kbd> | Merge PR 提示（第 2 步） | merge 後是否刪除該 branch |
| <kbd>f</kbd> | 刪除 branch 確認 | 強制刪除 |
| <kbd>Tab</kbd> <kbd>Shift-Tab</kbd> | Create tag 對話框 | 在欄位間移動 |
| <kbd>Space</kbd> | Create tag 對話框（核取方塊） | 切換核取狀態 |

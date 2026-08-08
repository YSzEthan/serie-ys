# 在 Detail view 中檢視檔案 diff

在 commit 詳情（Detail view）的檔案樹中選取檔案，底部會即時顯示該檔案的 diff。

## 使用方式

1. 在 commit 清單上按 <kbd>Enter</kbd> 開啟 commit 詳情
2. 按 <kbd>u</kbd> 切換到右側的檔案樹（Files 區塊）
3. 用 <kbd>j</kbd> / <kbd>k</kbd> 移動游標到想看的檔案（目錄列會自動跳過），
   底部 diff 區域會即時換成該檔案的內容

diff 區域一次只顯示「游標所在那一個檔案」的內容，不是整包 commit 的 diff。

### 按鍵

Files 區塊啟用時：

| 按鍵 | 動作 |
| --- | --- |
| <kbd>j</kbd> / <kbd>k</kbd> | 移動檔案游標 |
| <kbd>J</kbd> / <kbd>K</kbd> | 捲動 diff（逐行） |
| <kbd>Ctrl-d</kbd> / <kbd>Ctrl-u</kbd> | 捲動 diff（半頁） |
| <kbd>Ctrl-f</kbd> / <kbd>Ctrl-b</kbd> | 捲動 diff（整頁） |
| <kbd>h</kbd> / <kbd>l</kbd> | 切換較新／較舊 commit（跟 Info 區塊一樣） |

按 <kbd>u</kbd> 切回左側 Info 區塊時，diff 區域會收起，畫面回到只顯示 commit 資訊。

## Working Changes

尚未提交的變更（list 最上方那一列）也適用同一套操作，staged、unstaged、
以及 untracked 的新檔案都能個別選取檢視 diff。

## 設定

diff 區域的高度可以透過 `ui.diff.height` 調整，詳見
[設定檔格式](../configurations/config-file-format.md#uidiffheight)。

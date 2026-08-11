# 設定

用 config.toml 調整應用程式的行為。

設定檔按以下優先順序載入：

- `$SERIE_CONFIG_FILE`
  - 若已設定 `$SERIE_CONFIG_FILE` 但檔案不存在，將會產生錯誤。
- `<執行檔所在目錄>/.ysgit.toml`
  - 跟著執行檔走的隱藏檔，首次啟動會自動生成一份含所有選項與說明的預設檔。
  - 自我更新（見[命令列選項](../getting-started/command-line-options.md#-u---update)）
    只會 `rename` 替換執行檔本身，不會動到這個檔案。

沒設到的項目一律用預設值，不管是整份設定檔不存在，還是檔案裡漏了某幾項。

舊版（v2.5.x 以前）設定檔放在 `~/.config/serie/config.toml`，不再讀取；有需要的話手動
把內容搬到新位置即可。

## 舊設定檔裡的死鍵

隨著圖片渲染路徑移除，`core.option.protocol` 與 `graph.row_image_width` 這類鍵已經不存在了。舊設定檔留著它們的話，編輯器會標紅但程式不會報錯。

標紅是因為 `config.schema.json` 全面設了 `additionalProperties: false`，吃這份 schema 的編輯器會把未知鍵判定為錯誤——這是刻意的，用來提醒你清掉。程式那邊則是 `src/config.rs` 沒有用 `deny_unknown_fields`，serde 對未知欄位預設靜默忽略，同一段落裡的其他鍵照常生效。

死鍵不會讓程式起不來，但也不會有任何效果，建議直接刪掉。

## 舊版 `[ui.*]`／`[graph.color]` 區塊自動升級

`[ui.common]`、`[ui.detail].height`、`[ui.diff].height`、`[ui.user_command].height`、
`[ui.refs].width`，以及獨立的 `[graph.color]` 區塊，併進了新結構
（見[設定檔格式](./config-file-format.md)）。這些不是死鍵——讀取設定檔時會在記憶體裡
自動轉換成新路徑，值不會遺失，也不需要手動改。

編輯器吃 `config.schema.json` 的話，這些舊路徑一樣會標紅（跟死鍵外觀相同，但性質不同：
死鍵改了也沒用，這些是「值還有效，只是路徑舊了」）。進一次互動式精靈（`-h`）存檔，
就會把整份設定檔寫回新結構，標紅一併清掉。

----

- [設定檔格式](./config-file-format.md)

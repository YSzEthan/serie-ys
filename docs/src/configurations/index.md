# 設定

用 config.toml 調整應用程式的行為。

設定檔按以下優先順序載入：

- `$SERIE_CONFIG_FILE`
  - 若已設定 `$SERIE_CONFIG_FILE` 但檔案不存在，將會產生錯誤。
- `$XDG_CONFIG_HOME/serie/config.toml`
  - 若未設定 `$XDG_CONFIG_HOME`，則使用 `~/.config/`。

沒設到的項目一律用預設值，不管是整份設定檔不存在，還是檔案裡漏了某幾項。

## 舊設定檔裡的死鍵

隨著圖片渲染路徑移除，`core.option.protocol` 與 `graph.row_image_width` 這類鍵已經不存在了。舊設定檔留著它們的話，編輯器會標紅但程式不會報錯。

標紅是因為 `config.schema.json` 全面設了 `additionalProperties: false`，吃這份 schema 的編輯器會把未知鍵判定為錯誤——這是刻意的，用來提醒你清掉。程式那邊則是 `src/config.rs` 沒有用 `deny_unknown_fields`，serde 對未知欄位預設靜默忽略，同一段落裡的其他鍵照常生效。

死鍵不會讓程式起不來，但也不會有任何效果，建議直接刪掉。

----

- [設定檔格式](./config-file-format.md)

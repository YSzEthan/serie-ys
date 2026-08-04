# 簡介

**Serie**（[`/zéːriə/`](../faq/index.md)）是一個 TUI 應用程式，用 Unicode 製表字元渲染 commit 圖，效果類似 `git log --graph --all`。

<img src="../img/list.svg" width="100%">

（畫面由 `scripts/capture_screenshots.py` 從實際執行中的 `ysgit` 擷取，更多請見[截圖](../features/screenshots.md)）

## 為什麼？

雖然有些使用者偏好透過 CLI 使用 Git，但他們在查看 commit 記錄時往往需要依賴 GUI 或功能豐富的 TUI。也有些人覺得 `git log --graph` 就已足夠。

就我個人而言，即使加上額外選項，`git log --graph` 的輸出仍然難以閱讀。僅僅為了查看記錄就去學習複雜的工具，似乎太過繁瑣。

## 目標

- 在終端機中提供豐富的 `git log --graph` 體驗。
- 提供以 commit 圖為核心的 Git 儲存庫瀏覽方式。

## 非目標

- 實作功能完整的 Git 客戶端。
- 建立具有複雜 UI 的 TUI 應用程式。

---

_以 Rust 與 [ratatui](https://github.com/ratatui/ratatui) 打造。_  
_Serie 以 MIT 授權條款發布於 [GitHub](https://github.com/lusingander/serie)。_

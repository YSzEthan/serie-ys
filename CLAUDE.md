# CLAUDE.md
**一律使用正體中文**
本文件為 Claude Code (claude.ai/code) 使用此儲存庫中的程式碼時提供指導。

## 專案概覽

Serie 是一個 Rust TUI 應用程式，用於在支援圖片協議的終端機（iTerm2、Kitty）中視覺化 git commit 圖。使用 Ratatui 和 crossterm 建構。MSRV: 1.87.0。

## 建置與開發指令

```bash
cargo build --verbose              # Debug 建置
cargo build --release              # Release 建置（lto=true, codegen-units=1）
cargo test --verbose               # 執行所有測試
cargo test <test_name> --verbose   # 執行單一測試
cargo fmt --all -- --check         # 檢查格式
cargo clippy --all-targets --all-features -- -D warnings  # Lint（warnings = errors）
```

## Skills

- `/ci-check` — 依序執行 fmt、build、clippy、test 完整 CI 流程
- `/graph-test` — 執行圖形渲染整合測試，輸出快照至 `./out/graph/`

## 架構

### 事件驅動狀態機
- **進入點：** `main.rs` → `lib.rs` → `app.rs`（`App::run()` 事件迴圈）
- **視圖：** 在 `view/views.rs` 中以 Enum 為基礎的狀態機 — List、Detail、Help、Refs、CreateTag、DeleteTag、DeleteRef、UserCommand
- **事件：** `event.rs` 中的 `AppEvent` enum，透過 mpsc channels 分發

### 核心資料流
1. `git.rs` — 包裝 git CLI 指令（非 libgit2），快取 commits/refs/parent-child maps
2. `graph/calc.rs` — 計算視覺圖形佈局（x,y 位置）
3. `graph/image.rs` — 將圖形渲染為 PNG 圖片，編碼為終端協議格式
4. `widget/` — Ratatui 有狀態 widget，負責 UI 渲染

### 關鍵設計決策
- `Arc<str>` 用於 `CommitHash` — 跨執行緒便宜複製
- `FxHashMap`（rustc-hash）用於內部 maps — 比預設 hasher 更快
- 延遲載入圖片，可選 `--preload` 旗標；使用 Rayon 平行產生
- 兩種終端圖片協議：iTerm2（inline images）和 Kitty（graphics protocol），透過環境變數自動偵測
- 包裝 Git CLI 而非使用 libgit2 binding

### 設定
- TOML 設定檔位於 `~/.config/serie/config.toml` 或 `$SERIE_CONFIG_FILE`
- Schema：`config.schema.json`
- 預設快捷鍵：`assets/default-keybind.toml`
- `src/config.rs` 中的設定結構使用 `umbra::optional` 巨集進行部分覆蓋

### 測試
- 整合測試位於 `tests/graph.rs` — 建立暫存 git 倉庫，產生圖形圖片，與 golden snapshots 比對
- 測試輸出儲存至 `./out/graph` 供手動檢查

## 程式碼風格
- 最大行寬：100 字元（`rustfmt.toml`）
- Clippy too-many-arguments 閾值：12
- Match arm leading pipes：Never
- Tab spaces：4，不使用 hard tabs

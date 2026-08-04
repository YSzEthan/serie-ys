# CLAUDE.md
本文件為 Claude Code (claude.ai/code) 使用此儲存庫中的程式碼時提供指導。

**一律使用正體中文**
## 專案概覽

Serie 是一個 Rust TUI 應用程式，用 Unicode 製表字元在終端機中視覺化 git commit 圖。使用 Ratatui 和 crossterm 建構。MSRV: 1.88.0（以 `Cargo.toml` 的 `rust-version` 為準）。執行檔名為 `ysgit`。

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
- `/graph-test` — 執行圖形渲染整合測試，輸出文字快照至 `./out/graph/`

## 架構

### 事件驅動狀態機
- **進入點：** `main.rs` → `lib.rs` → `app.rs`（`App::run()` 事件迴圈）
- **視圖：** 在 `view/views.rs` 中以 Enum 為基礎的狀態機 — List、Detail、Help、Refs、CreateTag、DeleteTag、DeleteRef、UserCommand
- **事件：** `event.rs` 中的 `AppEvent` enum，透過 mpsc channels 分發

### 核心資料流
1. `git.rs` — 包裝 git CLI 指令（非 libgit2），快取 commits/refs/parent-child maps
2. `graph/calc.rs` — 計算視覺圖形佈局（x,y 位置）
3. `graph/text.rs` — 把佈局轉成 `TextCell` 序列。`Glyph`（語義角色）與 `GlyphSet`（風格 → 字元對照表）分離，`CellWidthType::cells_per_column()` 是「一個 graph 欄佔幾格」的唯一真相
4. `widget/` — Ratatui 有狀態 widget，負責 UI 渲染

### 關鍵設計決策
- `Arc<str>` 用於 `CommitHash` — 跨執行緒便宜複製
- `FxHashMap`（rustc-hash）用於內部 maps — 比預設 hasher 更快
- 純文字繪圖，不送任何圖片跳脫序列 — 因此沒有終端機白名單，tmux 等多工器可用
- `-s` 三種風格（rounded／angular／ascii）只是換一張 `GlyphSet`，渲染邏輯不分岔
- 包裝 Git CLI 而非使用 libgit2 binding

### 設定
- TOML 設定檔位於 `~/.config/serie/config.toml` 或 `$SERIE_CONFIG_FILE`
- Schema：`config.schema.json`
- 預設快捷鍵：`assets/default-keybind.toml`
- `src/config.rs` 中的設定結構使用 `umbra::optional` 巨集進行部分覆蓋

### 測試
- 整合測試位於 `tests/graph.rs` — 建立暫存 git 倉庫，產生文字圖形，與 `tests/graph/*.txt` golden snapshot 逐字元比對
- 測試輸出儲存至 `./out/graph` 供手動檢查（`.txt`，用 `git diff` 就看得出哪個字元移動了）
- 文件本身也有測試釘著：`docs/src/keybindings/index.md` 由 `src/view/help.rs` 產生（`UPDATE_KEYBINDINGS_DOC=1 cargo test` 重新產生），`docs/src/configurations/config-file-format.md` 的範例 TOML 會比對 `config.schema.json` 與 `Config::default()`

## 程式碼風格
- 最大行寬：100 字元（`rustfmt.toml`）
- Clippy too-many-arguments 閾值：12
- Match arm leading pipes：Never
- Tab spaces：4，不使用 hard tabs

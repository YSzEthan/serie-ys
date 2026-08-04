# 貢獻指南

感謝你考慮貢獻。動手之前請先看過以下指引。

未遵循這些指引的貢獻可能不會被接受。

## 回報 issue

回報前請先確認是否已有相同內容的 issue。

也請先參閱[常見問題](docs/src/faq/index.md)。

### 回報 bug

回報 bug 時請附上以下資訊：

- 應用程式版本
  - `ysgit --version`
- 終端機版本與其執行的作業系統
- 重現問題所需的 git 儲存庫資訊
  - 可以的話請提供最小的重現儲存庫（要在十萬筆 commit 的儲存庫上除錯很困難）

### 提議新功能

提議新功能前，請先看過[目標與非目標](docs/src/introduction/index.md)。

### 終端機相容性

commit 圖是用一般文字繪製的，沒有終端協議需要支援。顯示不正常時，請先確認你的字型是否有[相容性](docs/src/getting-started/compatibility.md)一節列出的製表字元，以及 `-s ascii` 是否能正常顯示。

## Pull request

歡迎 pull request，但不保證一定會被接受。遵循以下指引可以提高被接受的機率。

### 建立 pull request

- 建立 pull request 時，請比照[回報 issue](#回報-issue) 的指引。
- 不是每個 pull request 都需要先開 issue。小幅或直接了當的修改（例如文件修正、明顯的 bug 修復）可以直接開 pull request。
- 較複雜或會改變行為的修改，強烈建議先開 issue 討論做法，避免白做工。
- 不要夾帶與該 pull request 主題無關的修改。

### 持續整合

使用 [GitHub Actions](.github/workflows/build.yml) 執行基本檢查：

- stable 與 MSRV 兩個 Rust 版本都跑。
- 執行 build、test、format、lint。

### 改善 commit 圖

歡迎改善 commit 圖。

commit 圖的測試放在 [./tests/graph.rs](./tests/graph.rs)。

執行測試會把渲染結果（`.txt` 快照）與測試用儲存庫輸出到 `./out/graph`。
新增測試案例時，請把對應的快照放到 `./tests/graph/` 底下。
既有圖形有變動時，覆蓋快照並確認沒有非預期的改動 —— 文字快照的 `git diff` 會直接顯示哪些字元移動了。

### 更新文件裡的畫面

UI 有變動時，`docs/src/img/*.svg` 要重新擷取。作法見[截圖](docs/src/features/screenshots.md)。

## 授權條款

本專案採用 [MIT 授權條款](LICENSE)。貢獻者提交貢獻即表示同意遵守該授權條款。

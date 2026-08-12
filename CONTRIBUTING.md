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

### Commit 訊息

採用 [Conventional Commits](https://www.conventionalcommits.org/)，格式是 `type: 描述`，
描述用正體中文，尾端帶 issue 與 PR 編號（PR 那組由 squash merge 自動附上）：

```text
fix: gh CLI 呼叫加 timeout，並優化 GitHub 資料載入效率 (#57) (#58)
```

版號由 [.github/scripts/prepare_release.py](.github/scripts/prepare_release.py) 從
merge 進 `main` 的 commit type 推算。判準是**這個 commit 有沒有改到使用者下載的執行檔**：

| type | 版號 | CHANGELOG 區塊 |
| --- | --- | --- |
| `feat` | minor | Features |
| `fix` | patch | Bug Fixes |
| `perf` | patch | Performance |
| `refactor` | patch | Refactors |
| `revert` | patch | Reverts |
| `build` | patch | Build System |
| `style` | patch | Styles |
| `docs` | 不動 | Documentation |
| `test` | 不動 | Tests |
| `ci` | 不動 | CI |
| `chore` | 不動 | Chores |

標記 `!`（例如 `feat!:`）或在 body 寫 `BREAKING CHANGE:` footer 一律升 major，
不分 type。

「不動」的 type 仍會列進 CHANGELOG，只是自己不觸發發版；跟其他有升版的 commit
一起 merge 時會一併寫進該次版本。

格式檢查有兩道，都在 merge 之前——落到 `main` 上才發現就只能改寫已推送的歷史了：

- **本機 commit**：[lefthook](https://lefthook.dev/) 的 `commit-msg` hook。
  先裝 lefthook（`brew install lefthook` 等），再在 repo 根目錄跑一次：

  ```sh
  lefthook install
  ```

  `git revert` / `git commit --fixup` / merge 這些 git 自己產生的訊息會放行，
  不用為了它們加 `--no-verify`。但要注意 `Revert "…"` 這種預設標題**不會升版號**
  ——要發版請把 subject 改成 `revert: 描述 (#issue)`。

- **PR 標題**：[pr-title.yml](.github/workflows/pr-title.yml)。squash merge 時，
  PR 有多個 commit 的話 subject 取自 PR 標題而不是 commit 訊息，本機 hook 看不到，
  所以這道獨立檢查不能少。

`main` 上的 release workflow 不會因為格式不合而中止，只留 warning annotation；
「這次 push 沒有升版號」同樣只留 warning。**CI 綠燈不等於有發版**，看 annotation。

merge 前想自己確認會發出什麼版本：

```sh
python3 .github/scripts/prepare_release.py --from "$(git describe --tags --abbrev=0)" --dry-run
```

`pre-push` hook 會自動跑這一行，只提示不擋推。

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

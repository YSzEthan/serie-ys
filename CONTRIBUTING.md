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

### 效能量測

百萬 commit 等級的大 repo 效能優化（見 epic #115）需要可重現的量測工具，都在
`scripts/gen-large-repo.py`（合成 repo 產生器）與 `examples/perf.rs`（照
app 啟動順序量各階段耗時／記憶體／graph 規模）。

**準備兩個基準 repo**（放在 `~/perf-repos/`，不要放 `/tmp`——macOS 會定期
清掉太久沒碰的檔案）：

```sh
scripts/gen-large-repo.py ~/perf-repos/big1m
```

`big1m` 是 100 萬個 commit 的合成 repo，固定 seed，任何時間重新產生都是
逐 byte 相同的內容（腳本自己會用 known-answer 檢查這件事）。量測之前
`git -C ~/perf-repos/big1m status --porcelain` 必須是空的——Phase 3
（#120）之後會在裡面測存檔，一旦有 working changes，載入與 full graph
的數字都會跟著變。

linux kernel（148 萬 commit、merge 密集）是唯一開得起來的真實大 repo。
下面的指令把 commit 集合釘死在基準那一份，不管什麼時候 clone，數字都能
互相比較：

```sh
git clone --bare --filter=tree:0 https://github.com/torvalds/linux.git ~/perf-repos/linux.git
cd ~/perf-repos/linux.git
git config maintenance.auto false
git config gc.auto 0
git config remote.origin.tagOpt --no-tags
git update-ref refs/heads/master 165768bb70265b5c38cf0b73fafd75be235f8b14
git for-each-ref --format='delete %(refname)' --no-merged master refs/tags | git update-ref --stdin
git rev-list --count --branches --tags HEAD   # 必須印出 1483832
git commit-graph write --reachable
```

- `--filter=tree:0` 只抓 commit，約 830 MB
- 第一次跑 `perf` 會多從網路抓約 300 MB：bare repo 載入時會跑一次
  `git diff --cached --numstat`，Phase 3 之後這個呼叫就會消失。在 ysgit
  裡打開 detail 也會觸發網路抓取
- clone 之後不要 fetch，commit 數一變就沒辦法跟其他人的量測結果比較
- 新 clone 的 pack 佈局跟基準那份不同：結構數字（`cells`／`edges`／
  `width`）保證完全相同，時間與記憶體要在同一台機器上跟自己重新量的基準比

**執行**：

```sh
cargo build --release --example perf
target/release/examples/perf ~/perf-repos/big1m
target/release/examples/perf ~/perf-repos/linux.git
```

- `big1m`：先跑一次暖機（讓 pack 進到系統的檔案快取），再跑 3 次取中位數
- `linux.git`：在 Phase 2a（#118，lane 引擎）之前，`calc_graph` 加
  `stats` 要跑約 8～9 分鐘、heap 峰值超過 60 GB（這台機器 16 GB，會大量
  swap）。暖機**只跑到
  `load` 那一列印出就中止**，不要讓它跑完整趟——`calc_graph` 會把 pack
  的檔案快取全部擠出去，讓正式那次的 `load` 反而變成冷的。之後只正式跑
  1 次，不用取中位數

**每個欄位的意思**：

- 單位一律是 MiB（`1024 × 1024` bytes）
- `rss`：`ps -o rss=`，使用者實際感受到的記憶體。系統記憶體吃緊、開始
  swap 時會失真——linux 的 `calc_graph` 曾經印出 RSS 2 GB，實際 edge
  資料超過 32 GB
- `heap`／`peak`：計數 `#[global_allocator]` 量到的目前 heap 用量與該
  階段內的峰值，不受 swap 影響。實測過計數本身的額外開銷：`calc_graph`
  與 `filtered` 兩個配置最密集的階段，10 次交錯量測後中位數的差距在
  ±1% 內，遠低於原本設的 3% 門檻，所以一律計數，沒有另外做開關
- `stats` 這個階段是統計兩張 graph（edge 總數、每列寬），不算進前面任何
  一個階段的時間

**怎麼對照**：

- 同一個 repo 的 `cells`／`edges`／`width p50`／`p99`／`max` 只要 repo
  內容沒變就該完全相同——這幾個數字不受機器、pack 佈局、系統負載影響
- 時間與 RSS 有雜訊，落在 ±10% 通常算正常；`heap` 的雜訊小很多
- linux 在 #118 之前，`calc_graph`／`stats`／`drop` 的時間與記憶體是
  swap 主導的，只記錄、不用來比較

**PR 要貼什麼**：兩個 repo 的完整輸出，各用一個 `text` 區塊。

### 更新文件裡的畫面

UI 有變動時，`docs/src/img/*.svg` 要重新擷取。作法見[截圖](docs/src/features/screenshots.md)。

## 授權條款

本專案採用 [MIT 授權條款](LICENSE)。貢獻者提交貢獻即表示同意遵守該授權條款。

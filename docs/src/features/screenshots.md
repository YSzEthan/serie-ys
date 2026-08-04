# 截圖

Commit 清單：

<img src="../img/list.svg" width="100%">

Commit 詳情（`Enter`）：

<img src="../img/detail.svg" width="100%">

Refs 清單（`Tab`）—— 此 fork 可以在這裡刪除 branch 與 tag：

<img src="../img/refs.svg" width="100%">

篩選（`'`）—— 此 fork 新增，會用 BFS 重算 filtered graph 的佈局：

<img src="../img/filter.svg" width="100%">

## 這些圖是怎麼來的

不是螢幕截圖。整個畫面本來就是文字，所以 `scripts/capture_screenshots.py` 直接在 pty 裡跑起 `ysgit`、送出按鍵、把終端輸出的 ANSI 逐格解析成 SVG 的 `<text>`。

這樣做的好處是圖不會默默過期 —— 重跑一次就是最新的畫面：

```
$ cargo build --release
$ scripts/generate_test_repo.sh /tmp/demo-repo 120
$ scripts/capture_screenshots.py /tmp/demo-repo docs/src/img/
```

示範倉庫是產生出來的，不是任何人的實際歷史，所以有分支、merge 與 tag 可看，也不會把工作內容拍進文件裡。

同一個示範倉庫重跑擷取會得到逐位元組相同的 SVG；但 `generate_test_repo.sh` 用了亂數與當下時間，重新產生倉庫會換一批 hash 與日期。

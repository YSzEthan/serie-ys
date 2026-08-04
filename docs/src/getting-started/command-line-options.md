# 命令列選項

## \[PATH\]

git 儲存庫路徑。

省略時使用當前目錄。

## -n, --max-count \<NUMBER\>

渲染的最大 commit 數量。

未指定時渲染全部 commit。行為類似 `git log` 的 `--max-count` 選項。

## -o, --order \<TYPE\>

Commit 排序演算法。

_可選值：_ `chrono`、`topo`

`chrono` 盡可能依 commit 日期排序。
`topo` 盡可能把同一條 branch 上的 commit 排在一起。

```
chrono                topo
●        022          ●        022
│ ●      031          ●        021
● │      021          │ ●      031
│ │ ●    012          │ │ ●    012
│ │ ●    011          │ │ ●    011
│ │ │ ●  002          │ │ │ ●  002
●─╯─╯─╯  001          ●─╯─╯─╯  001
```

## -g, --graph-width \<TYPE\>

圖形的每一欄佔用幾個終端字元格。

_可選值：_ `auto`、`double`、`single`

`double` 畫出符號加一格連接線；`single` 只畫符號，圖形區域寬度減半：

```
double        single
●─╮           ●╮
●─│─╮         ●│╮
●─│─│─╮       ●││╮
●─│─│─│─╮     ●│││╮
●─│─│─│─│─╮   ●││││╮
│ ● │ │ │ │   │●││││
●─╯─╯─╯─╯─╯   ●╯╯╯╯╯
●             ●
```

未指定或指定 `auto` 時，寬度夠就用 `double`，不夠則自動改用 `single`。
終端機窄到連 `single` 都放不下時不會拒絕啟動，圖形區域直接在右緣截斷。

> **`single` 的已知限制。** 兩格折成一格後，橫向連接線會輸給該欄既有的任何內容，所以長的橫線整段消失：`●─│─│─│─│─╮` 會變成 `●││││╮`。還看得出 commit 落在哪一欄、哪些欄有線經過，但看不出誰跟誰相連。

## -s, --graph-style \<TYPE\>

Commit 圖形的邊線風格。

_可選值：_ `rounded`、`angular`、`ascii`

`rounded` 用圓角，`angular` 用直角，`ascii` 只用純 ASCII 字元（給畫不出製表字元的終端機或字型用）：

```
rounded      angular      ascii
●─╮          ●─┐          *-+
│ ●          │ ●          | *
●─│          ●─│          *-|
●─│          ●─│          *-|
│ ●          │ ●          | *
● │          ● │          * |
●─╯          ●─┘          *-+
```

## -i, --initial-selection \<TYPE\>

啟動時初始選取的 commit。

_可選值：_ `latest`、`head`

`latest` 選取最新的 commit。

`head` 選取 HEAD 所在的 commit。

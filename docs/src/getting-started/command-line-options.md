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

_可選值：_ `auto`、`double-f`、`double-l`、`single`（`double` 是 `double-f` 的別名）

`double-f` 與 `double-l` 都畫出符號加一格連接線，`single` 只畫符號、圖形區域寬度減半。

三者的差別在同一欄有多條線交會的時候：`double-f` 與 `single` 把它們合併成對應的接點字元（`┼┬┴├┤`），`double-l` 讓優先序高的那條蓋掉其餘。三種寬度下 commit 自己那一欄都看不出穿過去的線——該格由 `●` 佔住。

```
double-l       double-f        single
●─╮            ●─╮             ●╮
●─│─╮          ●─┼─╮           ●┼╮
●─│─│─╮        ●─┼─┼─╮         ●┼┼╮
●─│─│─│─╮      ●─┼─┼─┼─╮       ●┼┼┼╮
●─│─│─│─│─╮    ●─┼─┼─┼─┼─╮     ●┼┼┼┼╮
│ ● │ │ │ │    │ ● │ │ │ │     │●││││
●─╯─╯─╯─╯─╯    ●─┴─┴─┴─┴─╯     ●┴┴┴┴╯
●              ●               ●
```

`double-l` 是 `double-f` 出現以前唯一的 double 行為，留著給習慣舊畫面的人用。它會漏線：兩條線交會、而且兩條都不往右延伸的時候（例如一條直線碰上 `╮`），輸的那條會整條消失，右半格也不會留下任何線索。`double-f` 沒有這個問題。

未指定或指定 `auto` 時，寬度夠就用 `double-f`，不夠則自動改用 `single`。
終端機窄到連 `single` 都放不下時不會拒絕啟動，圖形區域直接在右緣截斷。

## -s, --graph-style \<TYPE\>

Commit 圖形的邊線風格。

_可選值：_ `rounded`、`angular`、`ascii`

`rounded` 用圓角，`angular` 用直角，`ascii` 只用純 ASCII 字元（給畫不出製表字元的終端機或字型用）：

```
rounded      angular      ascii
●─╮          ●─┐          *-+
│ ●          │ ●          | *
●─┤          ●─┤          *-+
●─┤          ●─┤          *-+
│ ●          │ ●          | *
● │          ● │          * |
●─╯          ●─┘          *-+
```

## -i, --initial-selection \<TYPE\>

啟動時初始選取的 commit。

_可選值：_ `latest`、`head`

`latest` 選取最新的 commit。

`head` 選取 HEAD 所在的 commit。

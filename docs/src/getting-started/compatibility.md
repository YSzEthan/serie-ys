# 相容性

commit 圖是用一般文字繪製的，不送任何圖片跳脫序列，因此**沒有終端機白名單** —— 只要畫得出 Unicode 製表字元的終端機都能用。

## 需要哪些字元

預設的 `rounded` 風格會用到這些字元：

```
● ◯ │ ─ ╭ ╮ ╯ ╰
```

字型缺字（或寬度算錯）時，改用其他 `--graph-style`（見[命令列選項](./command-line-options.md)）：

- `angular` 把圓角換成直角（`┌ ┐ ┘ └`），一般字型的涵蓋率較高。
- `ascii` 完全不用製表字元，只用 `* o | - +`。

## 終端多工器

tmux、screen、Zellij 等都可以正常使用。早期版本把 commit 圖當圖片渲染，而圖片協議無法穿透多工器；這個限制已經不存在了。

## 不再相關的項目

- **Sixel 圖形** —— 用不到。
- **Inline Images Protocol (iTerm2)** —— 用不到。
- **Terminal graphics protocol (kitty)** —— 用不到。

終端機支不支援這些協議，對 commit 圖的顯示沒有任何影響。

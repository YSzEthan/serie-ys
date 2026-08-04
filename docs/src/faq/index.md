# 常見問題

## 圖形顯示成方框、問號或字元錯位

commit 圖是用 Unicode 製表字元（`● ◯ │ ─ ╭ ╮ ╯ ╰`）繪製的。字型沒有涵蓋全部字元時，改用 `-s angular`（直角，字型涵蓋率較高）或 `-s ascii`（只用純 ASCII）。

過程中不涉及任何終端協議，所以這一定是字型問題，不會是終端機支援度的問題。詳見[相容性](../getting-started/compatibility.md)。

## 跟其他 git TUI 客戶端相比有什麼優勢？

- 分支一多仍然讀得懂的 commit 圖
- 簡潔乾淨的介面

反過來說，以下情況可能不適合你：

- 你已經滿意 `git log --graph` 或現有 TUI 客戶端的圖形顯示
- 你需要在 TUI 客戶端裡做複雜的 git 操作

## Serie 怎麼念？

念法同德文的 Serie（**/ˈzeːriə/**），大致是 **「ZAY-ree-eh」**，不是英文的 "series"。

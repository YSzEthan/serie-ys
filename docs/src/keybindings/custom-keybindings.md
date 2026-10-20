# 自訂快捷鍵

你可以設定自己的快捷鍵。

在[設定檔](../configurations/config-file-format.md)的 `[keybind]` 段落中撰寫即可套用。

預設快捷鍵設定寫在 [`./assets/default-keybind.toml`](https://github.com/YSzEthan/serie-ys/blob/main/assets/default-keybind.toml)，你可以用相同格式為各個 action 設定快捷鍵。

- 一個 action 可以設定多組快捷鍵。
- 未設定快捷鍵的 action 會沿用預設值。
- 設定了快捷鍵的 action，會**完全取代**它的預設鍵位，不是疊加上去——例如
  `navigate_down = ["ctrl-n"]` 之後，預設的 `j`／`down` 就不再對
  `navigate_down` 生效了。
- 把快捷鍵設成 `[]` 即可停用該 action（是上一條規則的特例：取代成空陣列）。

## 同一顆鍵綁給多個 action

同一顆鍵可以綁給**分屬不同畫面**的 action。例如預設的 `t`，在 commit 清單是 `create_tag`（建立 tag），在 GitHub 介面是 `toggle_label_mode`（切換 label 顯示）；按下時依你目前所在的畫面決定觸發哪一個。

以下情況不行，因為按下去分不出要做哪一個：

- 兩個 action 在**同一個畫面**都有作用（例如 `fetch` 與 `toggle_pr_draft` 在 GitHub 介面都有作用）。
- 其中一個是**全域** action：`force_quit`、`quit`、`help_toggle`、`github_toggle`、`check_update`、`cancel`、`confirm`。

撞鍵時的處理依來源而定：

- 同一份設定檔裡撞鍵：啟動時報錯。
- 設定檔跟內建預設撞鍵：以你的設定為準，內建預設的 action 會失去那顆鍵。

每個 action 在哪些畫面有作用，請看[預設快捷鍵](./index.md)裡各畫面的分區。

## 按鍵格式

可以使用以下格式定義快捷鍵。

### 修飾鍵

- `ctrl-`
- `alt-`
- `shift-`

修飾鍵可以組合，例如：`ctrl-shift-a`。

### 特殊鍵

| 按鍵 | 說明 |
| --- | --- |
| `esc` | Escape |
| `enter` | Enter |
| `left` | 左方向鍵 |
| `right` | 右方向鍵 |
| `up` | 上方向鍵 |
| `down` | 下方向鍵 |
| `home` | Home |
| `end` | End |
| `pageup` | Page Up |
| `pagedown` | Page Down |
| `backtab` | Back Tab（Shift + Tab） |
| `backspace` | Backspace |
| `delete` | Delete |
| `insert` | Insert |
| `f1` - `f12` | 功能鍵 |
| `space` | 空白鍵 |
| `hyphen`、`minus` | 連字號（-） |
| `tab` | Tab |

### 字元鍵

上面沒列到的任何單一字元（例如 `a`、`b`、`1`、`!`）都可以當按鍵使用。

非 ASCII 字元同樣可以，例如注音的 `ㄜ`、西里爾字母的 `й`、帶重音的 `é`。判斷依據是字元數而非位元組數，所以一個多位元組字元算一個按鍵。

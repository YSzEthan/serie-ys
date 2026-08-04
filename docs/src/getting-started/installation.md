# 安裝

此 fork 的執行檔名為 `ysgit`。

## 下載執行檔

從 [releases](https://github.com/YSzEthan/serie-ys/releases) 下載預先編譯好的執行檔。每個版本都提供以下平台，另附 `checksum.txt` 可驗證：

- `ysgit_<版本>-macos-arm64`
- `ysgit_<版本>-macos-x64`
- `ysgit_<版本>-linux`
- `ysgit_<版本>-windows.exe`

下載後改名為 `ysgit`、加上執行權限，放進 `$PATH` 即可：

```
$ chmod +x ysgit_1.8.0-macos-arm64
$ mv ysgit_1.8.0-macos-arm64 ~/.local/bin/ysgit
```

macOS 上另有兩點：

- 從瀏覽器下載的檔案會被加上 quarantine 屬性，執行時會被 Gatekeeper 擋下。用 `xattr -d com.apple.quarantine ysgit` 移除。
- **更新版本時請先 `rm` 舊檔再放新檔**，不要直接覆蓋。直接覆蓋會讓新檔案繼承舊檔的 security metadata，啟動時被 SIGKILL（exit 137），而且沒有任何錯誤訊息可查。

## Cargo

```
$ cargo install --git https://github.com/YSzEthan/serie-ys.git
```

## 從原始碼建置

```
$ git clone https://github.com/YSzEthan/serie-ys.git
$ cd serie-ys
$ cargo build --release # 非 release 建置會非常慢
$ ./target/release/ysgit
```

## 上游版本

以下管道安裝的是上游的 `serie`，不含此 fork 新增的功能（tag 管理、remote refs 切換、ref 刪除、篩選等）。

### [Cargo](https://crates.io/crates/serie)

```
$ cargo install --locked serie
```

### [Arch Linux](https://archlinux.org/packages/extra/x86_64/serie/)

```
$ pacman -S serie
```

### [Homebrew](https://formulae.brew.sh/formula/serie)

```
$ brew install serie
```

或從 [tap](https://github.com/lusingander/homebrew-tap/blob/master/serie.rb) 安裝：

```
$ brew install lusingander/tap/serie
```

### [NetBSD](https://pkgsrc.se/devel/serie)

```
$ pkgin install serie
```

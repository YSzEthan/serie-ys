#!/usr/bin/env python3
"""把 ysgit 的真實畫面擷取成 SVG，供 README 與 docs/ 使用。

為什麼是 SVG 而不是 PNG 螢幕截圖：整個畫面本來就是文字，所以「截圖」可以是
一格一格的 <text>，而不是一張點陣圖。體積差 30 倍（~40K vs ~1.2M），
diff 看得懂，而且這支腳本可以重跑 —— 圖不會像先前那批一樣默默過期。

用法：
    scripts/generate_test_repo.sh /tmp/demo-repo 120
    scripts/capture_screenshots.py /tmp/demo-repo docs/src/img/

刻意用產生出來的測試倉庫而不是本專案自己的歷史：測試倉庫有分支、merge
與 tag，graph 才看得出東西，而且不會把實際工作內容拍進 README。
"""

import fcntl
import html
import os
import pty
import re
import select
import signal
import struct
import subprocess
import sys
import termios
import time
import unicodedata

COLS, ROWS = 108, 30

# 字型格寬。0.6 是等寬字型 advance/font-size 的常見比值（Menlo、SF Mono 都是），
# 每個 <text> 另外標了 textLength，所以就算讀者的字型比值不同也不會跑版。
FONT_SIZE = 14.0
CELL_W = FONT_SIZE * 0.6
CELL_H = 18.0
PAD = 12.0
DEFAULT_FG = "#c8c8c8"
DEFAULT_BG = "#1c1c1c"

CSI = re.compile(r"\x1b\[([0-9;?]*)([A-Za-z])")

# ANSI 16 色 -> RGB。ratatui 的具名顏色會走這條，真彩色走 38;2 / 48;2。
ANSI16 = {
    30: (0, 0, 0), 31: (205, 49, 49), 32: (13, 188, 121), 33: (229, 229, 16),
    34: (36, 114, 200), 35: (188, 63, 188), 36: (17, 168, 205), 37: (229, 229, 229),
    90: (102, 102, 102), 91: (241, 76, 76), 92: (35, 209, 139), 93: (245, 245, 67),
    94: (59, 142, 234), 95: (214, 112, 214), 96: (41, 184, 219), 97: (255, 255, 255),
}

# (檔名, 進入該畫面要送的按鍵, 說明)
VIEWS = [
    ("list", "", "commit 清單"),
    ("detail", "\r", "commit 詳情"),
    ("refs", "\t", "refs 清單"),
    # 關鍵字刻意含 g：輸入模式下的 g 曾經被 app 層攔去開 GitHub view
    # （見 src/app.rs 的 global_app_event）。這條要是又壞掉，重跑擷取就會
    # 拍到 GitHub view 而不是篩選結果，等於一道哨兵。
    ("filter", "'logging", "篩選"),
]


ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BINARY = f"{ROOT}/target/release/ysgit"


def capture(repo, keys, settle=2.5, per_key=0.6):
    """在 pty 裡跑 ysgit，送出按鍵，回傳整段 ANSI 輸出。

    per_key 給得寬鬆是有原因的：按鍵送太快時 TUI 還沒進入輸入模式，後續字元
    會被當成指令吃掉（`'billing` 的 `g` 就這樣把畫面切到 GitHub view）。
    """
    pid, fd = pty.fork()
    if pid == 0:
        os.environ.update(TERM="xterm-256color", COLORTERM="truecolor")
        # 用不存在的 XDG_CONFIG_HOME，畫面才不會被跑腳本的人的設定檔影響。
        os.environ["XDG_CONFIG_HOME"] = "/nonexistent-for-capture"
        os.environ.pop("SERIE_CONFIG_FILE", None)
        os.execvp(BINARY, ["ysgit", repo])

    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    out = b""

    def drain(seconds):
        nonlocal out
        end = time.time() + seconds
        while time.time() < end:
            r, _, _ = select.select([fd], [], [], 0.1)
            if not r:
                continue
            try:
                chunk = os.read(fd, 1 << 16)
            except OSError:
                return False
            if not chunk:
                return False
            out += chunk
        return True

    if not drain(settle):
        raise RuntimeError(f"ysgit 還沒畫出畫面就結束了（repo={repo}）")
    for key in keys:
        os.write(fd, key.encode())
        if not drain(per_key):
            raise RuntimeError(f"送出 {key!r} 後 ysgit 結束了")
    drain(0.6)

    os.kill(pid, signal.SIGKILL)
    os.waitpid(pid, 0)
    return out.decode("utf8", "replace")


def parse(data):
    """極簡 VT：把 ANSI 串流放進格子，每格記 (字元, fg, bg)。

    只實作 ratatui 實際會送的那幾個序列（CUP / ED / EL / SGR）。第二個 half
    的全形字記成 None，輸出時跳過 —— 少了這步後續字元會壓在全形字右半格上。
    """
    blank = lambda: [[(" ", None, None)] * COLS for _ in range(ROWS)]
    grid = blank()
    cx = cy = 0
    fg = bg = None
    i = 0
    while i < len(data):
        m = CSI.match(data, i)
        if m:
            ps, fin = m.group(1), m.group(2)
            nums = [int(x) for x in ps.split(";") if x.isdigit()]
            if fin == "H":
                cy = nums[0] - 1 if nums else 0
                cx = nums[1] - 1 if len(nums) > 1 else 0
            elif fin == "J":
                grid, cx, cy = blank(), 0, 0
            elif fin == "K":
                for x in range(cx, COLS):
                    grid[cy][x] = (" ", None, bg)
            elif fin == "m":
                if not nums:
                    fg = bg = None
                k = 0
                while k < len(nums):
                    n = nums[k]
                    if n == 0:
                        fg = bg = None
                    elif n == 39:
                        fg = None
                    elif n == 49:
                        bg = None
                    elif n in ANSI16:
                        fg = ANSI16[n]
                    elif n - 10 in ANSI16:
                        bg = ANSI16[n - 10]
                    elif n in (38, 48) and k + 4 < len(nums) and nums[k + 1] == 2:
                        rgb = (nums[k + 2], nums[k + 3], nums[k + 4])
                        if n == 38:
                            fg = rgb
                        else:
                            bg = rgb
                        k += 4
                    k += 1
            i = m.end()
            continue

        ch = data[i]
        if ch == "\x1b":
            i += 2
            continue
        if ch == "\r":
            cx = 0
        elif ch == "\n":
            cy, cx = cy + 1, 0
        elif ch >= " ":
            wide = unicodedata.east_asian_width(ch) in ("W", "F")
            if 0 <= cy < ROWS and 0 <= cx < COLS:
                grid[cy][cx] = (ch, fg, bg)
                if wide and cx + 1 < COLS:
                    grid[cy][cx + 1] = (None, fg, bg)
            cx += 2 if wide else 1
        i += 1
    return grid


def runs(row, key):
    """把一列切成同屬性的連續區段，回傳 (起始欄, 結束欄, 值)。"""
    x = 0
    while x < COLS:
        v = key(row[x])
        x2 = x + 1
        while x2 < COLS and key(row[x2]) == v:
            x2 += 1
        yield x, x2, v
        x = x2


def to_svg(grid):
    hexc = lambda c: "#%02x%02x%02x" % c
    w = COLS * CELL_W + 2 * PAD
    h = ROWS * CELL_H + 2 * PAD
    parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{w:.0f}" height="{h:.0f}" '
        f'viewBox="0 0 {w:.0f} {h:.0f}" '
        f'font-family="ui-monospace,SFMono-Regular,Menlo,Consolas,monospace" '
        f'font-size="{FONT_SIZE:g}">',
        f'<rect width="100%" height="100%" fill="{DEFAULT_BG}" rx="6"/>',
    ]
    for y, row in enumerate(grid):
        for x, x2, bg in runs(row, lambda c: c[2]):
            if bg is None:
                continue
            parts.append(
                f'<rect x="{PAD + x * CELL_W:.1f}" y="{PAD + y * CELL_H:.1f}" '
                f'width="{(x2 - x) * CELL_W:.1f}" height="{CELL_H:.1f}" fill="{hexc(bg)}"/>'
            )
    for y, row in enumerate(grid):
        for x, x2, fg in runs(row, lambda c: c[1]):
            text = "".join(c for c, _, _ in row[x:x2] if c is not None)
            if not text.strip():
                continue
            parts.append(
                f'<text x="{PAD + x * CELL_W:.1f}" y="{PAD + y * CELL_H + FONT_SIZE:.1f}" '
                f'fill="{hexc(fg) if fg else DEFAULT_FG}" '
                # textLength 釘住區段寬度，讀者字型的 advance 比值不同也不會跑版
                f'textLength="{(x2 - x) * CELL_W:.1f}" lengthAdjust="spacingAndGlyphs" '
                f'xml:space="preserve">{html.escape(text)}</text>'
            )
    parts.append("</svg>")
    return "\n".join(parts) + "\n"


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    repo, outdir = sys.argv[1], sys.argv[2].rstrip("/")

    if not os.path.exists(BINARY):
        sys.exit(f"找不到 {BINARY}，先跑 cargo build --release")
    subprocess.run(["git", "-C", repo, "rev-parse", "HEAD"],
                   check=True, capture_output=True)

    for name, keys, desc in VIEWS:
        grid = parse(capture(repo, keys))
        # 每張圖都該長得不一樣；抓錯畫面（送鍵太快被當成指令）時這裡會擋下來。
        status = "".join(c for c, _, _ in grid[-1] if c is not None).strip()
        if not status:
            sys.exit(f"{name}: 底部狀態列是空的，畫面可能沒畫完")
        path = f"{outdir}/{name}.svg"
        with open(path, "w") as f:
            f.write(to_svg(grid))
        print(f"{path:<24} {os.path.getsize(path) // 1024:>4} KB  {desc:<12} 狀態列: {status[:46]}")


if __name__ == "__main__":
    main()

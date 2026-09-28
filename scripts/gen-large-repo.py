#!/usr/bin/env python3
"""固定 seed 的合成大 repo 產生器，供 `examples/perf.rs` 量測用（見 #116）。

用法：
    scripts/gen-large-repo.py ~/perf-repos/big1m
    scripts/gen-large-repo.py /tmp/small --commits 20000 --branches 12 --seed 42

產生一條 main 線，加上最多 `--branches` 條同時開著的 feature branch，隨機
commit／開分支／merge 回 main。灌入方式是組出 `git fast-import` 的輸入串流
直接餵給它，不逐一呼叫 `git commit`——100 萬個 commit 跑 `git commit` 的
process 開銷會壓過產生器本身。

## 為什麼看起來比一般的 fast-import 腳本囉唆

第一版所有 feature branch 共用同一個 `refs/heads/tmp`，12 條分支輪流寫
這個 ref，於是幾乎每次要接續某條分支時，fast-import 發現 `from` 指到的
commit 不是這個 ref 目前的 tip（上一個寫入的是別條分支），就得從正在寫的
pack 回頭讀那個 commit 的 tree——1M commit 跑下來回讀了約 60 萬次，多花
39 秒的 system time。

這裡改成每條開著的分支各佔一個獨立的 slot ref（`refs/heads/tmp<N>`）：

- 接續同一條分支時，`from :<mark>` 就是這個 slot ref 目前的 tip，
  fast-import 直接比對相等、不用回讀
- 開新分支（或重用一個剛合併完、slot 空出來的舊 ref）時，`from` 指到
  `refs/heads/main` 這個具名分支，fast-import 直接從自己的內部 branch
  table 複製 oid 與 tree，同樣不用回讀 pack
- 這個產生器完全不呼叫 `filemodify`，所有 commit 的 tree 永遠是空的，
  所以連「slot 第一次使用」那筆 tree 也不必回讀

commit 物件本身不含 ref 名稱，所以這個改法不影響任何一個 commit 的 hash——
同樣的 `--seed` 一定產生逐 byte 相同的 commit，`main()` 裡的 known-answer
檢查就是拿這件事當驗收：預設參數 (1,000,000 個 commit、12 條分支、
seed 42) 產生的 `refs/heads/main` 必須永遠是同一個 hash。

## RNG 呼叫順序不能動

`random.Random(seed)` 實例方法與 module-level `random.seed(seed)` 之後呼叫
`random.xxx()` 產生的序列相同（已驗證），但 Python 只保證 `random()` 本身
跨版本穩定，`choice()`／`randint()` 內部實作方式不保證——這正是下面
known-answer 檢查存在的理由：一旦 Python 版本升級讓這兩個函式的呼叫方式
變了，或有人「順手」把某段迴圈改寫成看似等價但呼叫次數不同的寫法，這裡
會直接報錯而不是默默產生一個內容不同、卻沒人發現的 repo。

## 額外的 remote-only commit

結尾在 `refs/remotes/origin/main` 上放一個從 `main^1`（不是 main 的 tip）
分岔出來的 commit，模擬「fetch 之後 origin/main 領先本地一個 commit」。
放在 `main^1` 而不是 tip 上，是因為 `calc_commit_positions` 的保留欄規則
只在「HEAD 的 first-parent 後代」時才會把它跟 HEAD 疊在同一欄——接在
`main^1` 上可見集合就是完整的 999,956 個舊 commit，filtered graph 因此會
跟 full graph 逐 edge 相同，是比 ±10% 更精確的比對基準（見 CONTRIBUTING.md
「效能量測」一節）。

`--commits` 指的是 commit 物件總數（含 root），預設值下實際可達的 commit
數是 999,957 個（1,000,000 個 mark，其中一部分是後來被合併、但仍然存在的
feature commit；相減之後 `rev-list --count --all` 會印出這個數字）。
"""

from __future__ import annotations

import argparse
import heapq
import os
import random
import subprocess
import sys
import time
from pathlib import Path

WORDS = (
    "fix add refactor update remove improve handle support parser render "
    "cache graph list view config test docs"
).split()
NULL_OID = b"0" * 40

# known-answer：預設參數 (1_000_000, 12, 42) 必須永遠產生這兩個 hash。
BASELINE_ARGS = (1_000_000, 12, 42)
BASELINE_MAIN = "78e2147f874b0e4b9e7f4dc4317be54224087747"
BASELINE_ORIGIN_MAIN = "cb34267e545a2398df656a670b669ada6d412405"


def write_stream(out, n: int, w: int, seed: int) -> None:
    """把整個 fast-import 串流寫進 `out`（一個 binary 的 file-like 物件）。

    呼叫端負責在 `out` 關閉前把它接到 `git fast-import` 的 stdin。
    """
    rng = random.Random(seed)
    mark = 0
    t = 1_000_000_000

    def commit(ref: str, from_: bytes | None = None, merge: int | None = None) -> int:
        nonlocal mark, t
        mark += 1
        t += 30
        # `range(rng.randint(...))` 的上界在生成器建立當下就求值一次，
        # 之後每次 `rng.choice(WORDS)` 才逐一呼叫——呼叫順序、次數都跟
        # 第一版逐行對應，才能保證 RNG 序列不變。
        subj = " ".join(rng.choice(WORDS) for _ in range(rng.randint(4, 9)))
        body = " ".join(rng.choice(WORDS) for _ in range(rng.randint(0, 40)))
        msg = (subj + "\n\n" + body).encode()
        lines = [
            b"commit %s\n" % ref.encode(),
            b"mark :%d\n" % mark,
            b"author Dev%d <dev%d@example.com> %d +0800\n" % (mark % 300, mark % 300, t),
            b"committer Dev%d <dev%d@example.com> %d +0800\n" % (mark % 300, mark % 300, t),
            b"data %d\n" % len(msg),
            msg,
            b"\n",
        ]
        if from_ is not None:
            lines.append(b"from %s\n" % from_)
        if merge is not None:
            lines.append(b"merge :%d\n" % merge)
        lines.append(b"\n")
        out.write(b"".join(lines))
        return mark

    main_tip = commit("refs/heads/main")
    prev_main: int | None = None
    open_br: list[list[int]] = []  # [[slot, tip], ...]，跟原型的 open_br 對應
    free_slots: list[int] = []  # min-heap：釋放的 slot 優先重用，維持 slot 數貼近 W
    next_slot = 0

    while mark < n:
        r = rng.random()
        if r < 0.15 and len(open_br) < w:
            if free_slots:
                slot = heapq.heappop(free_slots)
            else:
                slot = next_slot
                next_slot += 1
            tip = commit("refs/heads/tmp%d" % slot, from_=b"refs/heads/main")
            open_br.append([slot, tip])
        elif r < 0.30 and open_br:
            i = rng.randrange(len(open_br))
            slot, tip = open_br.pop(i)
            heapq.heappush(free_slots, slot)
            prev_main, main_tip = main_tip, commit(
                "refs/heads/main", from_=b":%d" % main_tip, merge=tip
            )
        elif r < 0.85 and open_br:
            i = rng.randrange(len(open_br))
            slot, tip = open_br[i]
            open_br[i][1] = commit("refs/heads/tmp%d" % slot, from_=b":%d" % tip)
        else:
            prev_main, main_tip = main_tip, commit("refs/heads/main", from_=b":%d" % main_tip)

    if prev_main is None:
        raise SystemExit("--commits 太小：main 只有 root commit，沒有 main^1 可以當 remote-only 的分岔點")

    commit("refs/remotes/origin/main", from_=b":%d" % prev_main)

    # 每個 slot 只會依序分配到遞增的編號，中間不會跳號，所以 0..next_slot
    # 就是曾經用過的全部 slot——不需要另外把 open_br／free_slots 兜起來。
    for slot in range(next_slot):
        out.write(b"reset refs/heads/tmp%d\nfrom %s\n\n" % (slot, NULL_OID))

    out.write(b"done\n")


def run_git(repo: Path, env: dict[str, str], *args: str, capture: bool = False) -> str:
    result = subprocess.run(
        ["git", "-C", str(repo), *args],
        env=env,
        check=True,
        capture_output=capture,
        text=capture,
    )
    return result.stdout.strip() if capture else ""


def main() -> None:
    parser = argparse.ArgumentParser(description="固定 seed 的合成大 repo 產生器")
    parser.add_argument("dir", help="輸出目錄，必須不存在或是空目錄")
    parser.add_argument("--commits", type=int, default=BASELINE_ARGS[0], help="commit 物件總數")
    parser.add_argument("--branches", type=int, default=BASELINE_ARGS[1], help="同時開著的 branch 上限")
    parser.add_argument("--seed", type=int, default=BASELINE_ARGS[2])
    args = parser.parse_args()

    if args.commits < 2:
        raise SystemExit("--commits 必須 >= 2")
    if args.branches < 1:
        raise SystemExit("--branches 必須 >= 1")

    target = Path(args.dir).expanduser()
    if target.exists() and any(target.iterdir()):
        raise SystemExit(f"{target} 已存在且非空，拒絕覆寫")
    target.mkdir(parents=True, exist_ok=True)

    # 不受使用者的 ~/.gitconfig 影響（例如 fastimport.unpackLimit）。
    env = os.environ.copy()
    env["GIT_CONFIG_NOSYSTEM"] = "1"
    env["GIT_CONFIG_GLOBAL"] = os.devnull

    run_git(target, env, "init", "--quiet", "--object-format=sha1", "--initial-branch=main")
    run_git(target, env, "config", "gc.auto", "0")
    run_git(target, env, "config", "maintenance.auto", "false")

    t0 = time.time()
    proc = subprocess.Popen(
        [
            "git", "-C", str(target), "fast-import", "--quiet", "--done",
            f"--active-branches={args.branches + 2}",
        ],
        stdin=subprocess.PIPE,
        env=env,
        bufsize=1 << 20,
    )
    assert proc.stdin is not None
    write_stream(proc.stdin, args.commits, args.branches, args.seed)
    proc.stdin.close()
    ret = proc.wait()
    if ret != 0:
        raise SystemExit(f"git fast-import 失敗（exit code {ret}）")
    elapsed = time.time() - t0

    run_git(target, env, "commit-graph", "write", "--reachable", "--no-progress")

    refs = sorted(run_git(target, env, "for-each-ref", "--format=%(refname)", capture=True).split())
    expected = ["refs/heads/main", "refs/remotes/origin/main"]
    if refs != expected:
        raise SystemExit(f"產生完的 ref 集合不符預期：得到 {refs}，預期 {expected}")

    main_hash = run_git(target, env, "rev-parse", "refs/heads/main", capture=True)
    origin_hash = run_git(target, env, "rev-parse", "refs/remotes/origin/main", capture=True)
    commit_count = run_git(target, env, "rev-list", "--count", "--all", capture=True)

    if (args.commits, args.branches, args.seed) == BASELINE_ARGS:
        if main_hash != BASELINE_MAIN:
            raise SystemExit(
                f"known-answer 檢查失敗：refs/heads/main = {main_hash}，預期 {BASELINE_MAIN}。\n"
                "RNG 呼叫順序被改動了，或這不是同一個產生器版本。"
            )
        if origin_hash != BASELINE_ORIGIN_MAIN:
            raise SystemExit(
                f"known-answer 檢查失敗：refs/remotes/origin/main = {origin_hash}，"
                f"預期 {BASELINE_ORIGIN_MAIN}。"
            )

    print(f"python {sys.version.split()[0]}  fast-import {elapsed:.1f}s  commits(all)={commit_count}")
    print(f"refs/heads/main          = {main_hash}")
    print(f"refs/remotes/origin/main = {origin_hash}")


if __name__ == "__main__":
    main()

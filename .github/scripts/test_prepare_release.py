#!/usr/bin/env python3
"""發版腳本的測試。直接跑：`python3 .github/scripts/test_prepare_release.py`。

不用 pytest——這個 repo 沒有 Python 的開發相依，為了幾十行斷言引進一套
測試框架不划算。CI 在 build.yml 裡跑同一行。

測的都是純函式（吃字串、吐結果，不碰 git 也不碰檔案系統）。會動到 git 的
`load_commits` / `build_pr_map` 不在這裡測——它們是薄薄一層 subprocess 包裝，
真正的邏輯都在被測的這幾個函式裡。
"""

from __future__ import annotations

import contextlib
import io
import os
import sys

# 有幾條測試餵的是格式不合的 subject，會走進 `classify()` 的 `warn()`。
# 在 Actions 上 `warn()` 印的是 `::warning::` 工作流程指令，runner 會把它
# 收成一則真的 annotation——這次改動的重點就是讓 warning 有訊號，測試自己
# 每跑一次就放一則假的進去剛好把訊號洗掉。先摘掉這個環境變數。
os.environ.pop("GITHUB_ACTIONS", None)

from check_subject import check, first_meaningful_line  # noqa: E402
from prepare_release import (  # noqa: E402
    BUMP_RANK,
    TYPE_TABLE,
    Commit,
    build_changelog_block,
    bump_version,
    classify,
    subject_problem,
)

failures: list[str] = []


def expect(cond: bool, what: str) -> None:
    if not cond:
        failures.append(what)


def c(subject: str, body: str = "") -> Commit:
    """測試用 commit。hash 只要長度像樣即可，內容不影響被測邏輯。"""
    return Commit("0" * 40, subject, body)


# --- subject_problem：格式判斷 -------------------------------------------

for good in [
    "fix: 修正某個東西 (#57)",
    "feat: 新功能",
    "refactor: 重整 (#60) (#64)",
    "feat!: 破壞相容",
    "fix(config): 帶 scope",
    "chore: release v2.7.2",
]:
    expect(subject_problem(good) is None, f"應該通過卻被擋：{good}")

for bad in [
    "TOML 架構重整 (#64)",  # 就是這次事故的形狀：完全沒有前綴
    "Fix: 首字大寫",
    "chroe: type 打錯字",
    "隨手改一下",
    "",
]:
    expect(subject_problem(bad) is not None, f"應該被擋卻通過：{bad}")


# --- check：git 自己產生的訊息要放行，人手打的不行 ------------------------

for generated in [
    'Revert "fix: 某個修正 (#12)"',
    'Reapply "fix: 某個修正 (#12)"',  # git 2.42+ revert 一個 revert
    "fixup! fix: 某個修正",
    "squash! feat: 新功能",
    "Merge branch 'main' into feature",
    "Merge pull request #64 from YSzEthan/26081201",
]:
    expect(check(generated) is None, f"git 產生的訊息不該被擋：{generated}")

# 白名單是靠雙引號認出 git 的手筆，人手打的同字開頭不該混進去
for human in ["Revert 掉自動更新功能", "Reapply 那個設定"]:
    expect(check(human) is not None, f"人手打的無前綴 subject 不該放行：{human}")

expect(check("") is not None, "空訊息應該被擋")


# --- first_meaningful_line：COMMIT_EDITMSG 會夾註解 -----------------------

expect(
    first_meaningful_line("fix: 正常 (#1)\n\n# 註解\n") == "fix: 正常 (#1)",
    "第一行就是 subject 時取值錯誤",
)
expect(
    first_meaningful_line("# 註解在前\n\nfix: 正常 (#1)\n") == "fix: 正常 (#1)",
    "註解在前時應跳過註解取到 subject",
)
expect(first_meaningful_line("# 全是註解\n#\n") == "", "全註解應回空字串")


# --- classify：版號等級 ---------------------------------------------------

expect(classify([])[0] is None, "空 range 應該不升版（release commit 之後的情況）")
expect(classify([c("feat: 新功能")])[0] == "minor", "feat 應升 minor")
expect(classify([c("fix: 修正")])[0] == "patch", "fix 應升 patch")
expect(classify([c("refactor: 重整")])[0] == "patch", "refactor 應升 patch")
expect(classify([c("chore: 雜務")])[0] is None, "chore 不該升版")

# BREAKING 蓋過一切，連不升版的 type 也一樣
expect(classify([c("docs!: 破壞相容")])[0] == "major", "`!` 應升 major")
expect(
    classify([c("chore: 雜務", "BREAKING CHANGE: 設定格式變了")])[0] == "major",
    "BREAKING CHANGE footer 應升 major",
)

# footer 會續行，且要在下一個空行處收住、不能吃到後面的段落
_, _, breaking = classify(
    [c("feat: 換設定格式", "BREAKING CHANGE: 舊的\n設定檔要重寫\n\n之後的段落")]
)
expect(
    [text for _, text in breaking] == ["舊的 設定檔要重寫"],
    f"BREAKING footer 續行未收成一行或吃過頭：{[t for _, t in breaking]}",
)

# 取最高等級，不是最後一個
expect(
    classify([c("fix: 修正"), c("feat: 新功能"), c("chore: 雜務")])[0] == "minor",
    "混合時應取最高等級",
)

# 格式不合的 commit 只警告不中止，其餘照常計算。這裡是唯一會觸發 warn()
# 的案例，把 stderr 收掉——不然測試通過時也印一行 `warning:`，看起來像失敗。
with contextlib.redirect_stderr(io.StringIO()):
    bump, sections, _ = classify([c("隨手改一下"), c("fix: 修正")])
expect(bump == "patch", "格式不合的 commit 不該影響其他 commit 的版號計算")
logged = [commit.subject for commit, _ in sections["Bug Fixes"]]
expect(logged == ["fix: 修正"], f"格式不合的 commit 不該進 CHANGELOG：{logged}")


# --- build_changelog_block：CHANGELOG 與 GitHub Release 內文的長相 --------

_, sections, _ = classify([c("feat: 新功能"), c("fix: 修正")])
block = build_changelog_block("o/r", "1.0.0", "1.1.0", sections, [], {}, "2026-08-12")
expect(
    block.index("### Features") < block.index("### Bug Fixes"),
    "CHANGELOG 區塊順序應跟著 TYPE_TABLE 的宣告順序",
)
expect("* 新功能 ([0000000]" in block, "沒有 PR 編號時應退回 commit 連結")
expect("## [1.1.0]" in block and "(2026-08-12)" in block, "版本標題或日期缺漏")


# --- bump_version ---------------------------------------------------------

expect(bump_version("2.7.2", "patch") == "2.7.3", "patch 進位錯誤")
expect(bump_version("2.7.2", "minor") == "2.8.0", "minor 進位應歸零 patch")
expect(bump_version("2.7.2", "major") == "3.0.0", "major 進位應歸零 minor/patch")


# --- 表本身的自洽 ---------------------------------------------------------

expect(
    all(level in BUMP_RANK for _, level in TYPE_TABLE.values()),
    "TYPE_TABLE 有 BUMP_RANK 不認得的等級（例如把 patch 打成 Patch）",
)


if failures:
    print(f"FAILED（{len(failures)} 項）：", file=sys.stderr)
    for f in failures:
        print(f"  - {f}", file=sys.stderr)
    raise SystemExit(1)

print("OK")

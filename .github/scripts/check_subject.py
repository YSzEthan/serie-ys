#!/usr/bin/env python3
"""檢查 commit subject（或 PR 標題）符合 conventional commit 規範。

兩個呼叫端共用同一份判斷，規範只有一份定義（`prepare_release.py` 的
`TYPE_TABLE` 與 `subject_problem()`）：

- `lefthook.yml` 的 commit-msg hook：`check_subject.py <COMMIT_EDITMSG 路徑>`
- `.github/workflows/pr-title.yml`：`check_subject.py --text "<PR 標題>"`

兩邊都要檢查的理由見 CONTRIBUTING.md「Commit 訊息」。
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

from prepare_release import TYPE_TABLE, subject_problem

# git 自己產生的訊息，不是人打的，不該被規範擋下來——硬擋只會逼人加
# `--no-verify`，那等於整個 hook 失效。
#
# `Revert` / `Reapply` 帶著雙引號比對，是因為 git 產的一定長這樣
# （`Reapply "…"` 是 2.42 之後 revert 一個 revert 的預設訊息）。少了引號，
# 「Revert 掉自動更新功能」這種人手打、真的沒有前綴的 subject 會被誤放行。
GIT_GENERATED_PREFIXES = (
    "Merge ",
    'Revert "',
    'Reapply "',
    "fixup! ",
    "squash! ",
    "amend! ",
)


def first_meaningful_line(text: str) -> str:
    """取第一行非註解、非空白的內容。

    `COMMIT_EDITMSG` 開頭可能是 git 的說明註解（`# Please enter…`），
    也可能是使用者自己寫在前面的註解行。
    """
    for line in text.splitlines():
        stripped = line.strip()
        if stripped and not stripped.startswith("#"):
            return stripped
    return ""


def check(subject: str) -> str | None:
    """回傳錯誤訊息，通過則 `None`。"""
    if not subject:
        return "commit 訊息是空的"
    if subject.startswith(GIT_GENERATED_PREFIXES):
        return None
    return subject_problem(subject)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("path", nargs="?", help="commit 訊息檔（COMMIT_EDITMSG）")
    parser.add_argument("--text", help="直接檢查這段文字，而不是讀檔")
    args = parser.parse_args()

    if args.text is not None:
        subject = args.text.strip()
    elif args.path:
        subject = first_meaningful_line(Path(args.path).read_text(encoding="utf-8"))
    else:
        parser.error("要給 commit 訊息檔路徑，或用 --text 直接給字串")

    problem = check(subject)
    if problem is None:
        return 0

    print(
        f"commit subject 不符規範：{subject}\n"
        f"  ↳ {problem}\n\n"
        f"格式：<type>: <中文描述> (#issue)\n"
        f"可用 type：{'／'.join(TYPE_TABLE)}\n"
        "範例：fix: gh CLI 呼叫加 timeout (#57)\n\n"
        "詳見 CONTRIBUTING.md「Commit 訊息」。",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main())

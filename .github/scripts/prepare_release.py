#!/usr/bin/env python3
"""在 main 上依 conventional commits 計算下一個版本，更新 Cargo.toml 並產生 CHANGELOG 區塊。

用法：
  prepare_release.py --from v1.9.0                # 正式模式：寫入 Cargo.toml / CHANGELOG.md
  prepare_release.py --from v1.9.0 --dry-run       # 只印出計算結果，不寫檔

沒有任何 commit 觸發 bump 時，印出 NO_BUMP 並以 exit code 0 結束——這不是錯誤，
是「這次 push 不用發版」的正常結果。
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
from datetime import date
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
CARGO_TOML = REPO_ROOT / "Cargo.toml"
CARGO_LOCK = REPO_ROOT / "Cargo.lock"
CHANGELOG = REPO_ROOT / "CHANGELOG.md"

# commit type -> (CHANGELOG section 標題, bump 等級)。等級為 None 代表該
# type 有專屬 CHANGELOG 區塊，但本身不觸發版號變動。
#
# 判準是「這個 commit 有沒有改到使用者下載的那個執行檔」：有就至少 patch，
# 沒有就只列 CHANGELOG。所以 refactor／style／build／revert 都算 patch
# （原始碼或相依變了，產出的 binary 就不是同一個），而 docs／test／ci／
# chore 不算（純文件、測試、CI 設定、雜務不會進 binary）。
#
# 改這張表要同步更新 CONTRIBUTING.md 的對照表——那是給貢獻者看的第二份。
TYPE_TABLE = {
    "feat": ("Features", "minor"),
    "fix": ("Bug Fixes", "patch"),
    "perf": ("Performance", "patch"),
    "refactor": ("Refactors", "patch"),
    "revert": ("Reverts", "patch"),
    "build": ("Build System", "patch"),
    "style": ("Styles", "patch"),
    "docs": ("Documentation", None),
    "test": ("Tests", None),
    "ci": ("CI", None),
    "chore": ("Chores", None),
}
# CHANGELOG section 順序沿用 TYPE_TABLE 的宣告順序，不手抄第二份——新增
# commit type 時只要改上面那張表，這裡自動跟著變，不會漏同步。
SECTION_ORDER = list(dict.fromkeys(section for section, _ in TYPE_TABLE.values()))
BUMP_RANK = {None: 0, "patch": 1, "minor": 2, "major": 3}

SUBJECT_RE = re.compile(
    r"^(?P<type>[a-z]+)(?:\((?P<scope>[^)]*)\))?(?P<breaking>!)?:\s*(?P<desc>.+)$"
)
# footer 值可能換行續寫（沒有 git trailer 縮排慣例），要撈到下一個空行、
# 下一個 footer token，或字串結尾為止，而不是只吃第一行。
BREAKING_FOOTER_RE = re.compile(
    r"^BREAKING[ -]CHANGE:[ \t]*(.+?)(?=\n\n|\n[A-Za-z][\w-]*(?:: | #)|\Z)",
    re.MULTILINE | re.DOTALL,
)


def run_git(*args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=REPO_ROOT, check=True, capture_output=True, text=True
    ).stdout


class Commit:
    __slots__ = ("hash", "subject", "body")

    def __init__(self, hash_: str, subject: str, body: str):
        self.hash = hash_
        self.subject = subject
        self.body = body


def load_commits(base: str) -> list[Commit]:
    """base..HEAD 之間、非 merge commit 的清單，由舊到新。"""
    out = run_git(
        "log", f"{base}..HEAD", "--no-merges", "--reverse", "--format=%H%x1f%s%x1f%b%x1e"
    )
    commits = []
    for record in out.split("\x1e"):
        record = record.strip("\n")
        if not record:
            continue
        h, subject, body = record.split("\x1f")
        commits.append(Commit(h, subject, body))
    return commits


def build_pr_map(base: str) -> dict[str, int]:
    """commit hash -> PR number，只涵蓋透過『Merge pull request #N』併入的 commit。

    非經 PR（例如直接 push 到 main）的 commit 不會出現在這份對照表，呼叫端
    需自行 fallback 到 commit hash 連結。
    """
    pr_map: dict[str, int] = {}
    merges = run_git("log", f"{base}..HEAD", "--merges", "--format=%H %s")
    for line in merges.splitlines():
        if not line.strip():
            continue
        merge_hash, subject = line.split(" ", 1)
        m = re.match(r"Merge pull request #(\d+) from", subject)
        if not m:
            continue
        pr_number = int(m.group(1))
        try:
            members = run_git("rev-list", f"{merge_hash}^1..{merge_hash}^2").split()
        except subprocess.CalledProcessError:
            continue
        for h in members:
            pr_map[h] = pr_number
    return pr_map


def warn(message: str) -> None:
    """留一則警告。在 GitHub Actions 上是 annotation，本機是普通 stderr。

    一律走 stderr：runner 兩條串流都會解析 workflow command，而 stdout 有
    別的用途——`--print-section` 的輸出會被重導向成 GitHub Release 內文
    （見 `release.yml` 的 upload job），警告混進去就會出現在發版說明裡。
    """
    prefix = "::warning::" if os.environ.get("GITHUB_ACTIONS") else "warning: "
    print(f"{prefix}{message}", file=sys.stderr)


def subject_problem(subject: str) -> str | None:
    """回傳 subject 不符規範的原因，符合就回 `None`。

    純函式、不碰 git，`test_prepare_release.py` 直接餵字串測。

    這是規範的唯一定義，三個呼叫端共用：lefthook 的 commit-msg hook 與
    PR 標題 lint（兩者經 `check_subject.py`）在 merge 前擋，`classify()`
    則只是補一則警告——擋在那裡沒有用，commit 已經在 main 上了。
    """
    m = SUBJECT_RE.match(subject)
    if not m:
        return "缺少 conventional commit 前綴（`type: 描述`）"
    ctype = m.group("type")
    if ctype not in TYPE_TABLE:
        return f"`{ctype}` 不是認得的 commit type"
    return None


def classify(
    commits: list[Commit],
) -> tuple[str | None, dict[str, list[tuple[Commit, str]]], list[tuple[Commit, str]]]:
    """回傳 (bump 等級, {section: [(commit, desc)]}, breaking 清單)。"""
    bump: str | None = None
    sections: dict[str, list[tuple[Commit, str]]] = {name: [] for name in SECTION_ORDER}
    breaking: list[tuple[Commit, str]] = []

    for c in commits:
        # 格式不合只警告不中止，理由見 subject_problem()。
        m = SUBJECT_RE.match(c.subject)
        entry = TYPE_TABLE.get(m.group("type")) if m else None
        if entry is None:
            warn(
                f"{c.hash[:7]} {c.subject} ↳ {subject_problem(c.subject)}"
                "（不列入 CHANGELOG 與版號計算）"
            )
            continue

        desc = m.group("desc").strip()
        is_breaking = bool(m.group("breaking"))

        footer_matches = BREAKING_FOOTER_RE.findall(c.body)
        if footer_matches:
            is_breaking = True
            for text in footer_matches:
                # CHANGELOG 一則一行，footer 內的換行收成空白
                breaking.append((c, " ".join(text.split())))
        elif is_breaking:
            breaking.append((c, desc))

        section, own_level = entry
        sections[section].append((c, desc))

        level = "major" if is_breaking else own_level
        if BUMP_RANK[level] > BUMP_RANK[bump]:
            bump = level

    return bump, sections, breaking


def bump_version(current: str, level: str) -> str:
    major, minor, patch = (int(p) for p in current.split("."))
    if level == "major":
        return f"{major + 1}.0.0"
    if level == "minor":
        return f"{major}.{minor + 1}.0"
    return f"{major}.{minor}.{patch + 1}"


def find_package_info(lines: list[str]) -> tuple[int, str, str]:
    """回傳 Cargo.toml [package] 區塊的 (version 所在行號, name, version)。

    name 動態讀出來，不硬編 package 名稱——同一份 name 接著用來在
    Cargo.lock 裡定位對應的 [[package]] 區塊。
    """
    in_package = False
    name: str | None = None
    version_idx: int | None = None
    version: str | None = None
    for i, line in enumerate(lines):
        stripped = line.strip()
        if stripped.startswith("["):
            in_package = stripped == "[package]"
            continue
        if not in_package:
            continue
        m = re.match(r'name\s*=\s*"([^"]+)"', stripped)
        if m:
            name = m.group(1)
            continue
        m = re.match(r'version\s*=\s*"([^"]+)"', stripped)
        if m:
            version_idx, version = i, m.group(1)
    if name is None or version_idx is None:
        raise SystemExit("找不到 [package] 區塊下的 name/version 欄位")
    return version_idx, name, version


def find_lock_version(lines: list[str], package_name: str) -> tuple[int, str]:
    """回傳 Cargo.lock 裡 package_name 的 [[package]] 區塊中 version 欄位的
    (行號, 版本字串)。

    直接文字取代，不呼叫 `cargo`——CI runner 是全新環境，沒有本機 registry
    cache，`cargo metadata --offline` 連 index 都讀不到會直接失敗；而這裡
    要做的只是把本 package 自己的 version 欄位同步成新版號，純本地文字操作，
    不需要真的做依賴解析。
    """
    in_block = False
    for i, line in enumerate(lines):
        stripped = line.strip()
        if stripped == "[[package]]":
            in_block = False
            continue
        if stripped == f'name = "{package_name}"':
            in_block = True
            continue
        if in_block:
            m = re.match(r'version\s*=\s*"([^"]+)"', stripped)
            if m:
                return i, m.group(1)
    raise SystemExit(f"Cargo.lock 找不到 {package_name} 的 version 欄位")


def entry_link(repo: str, commit: Commit, pr_map: dict[str, int]) -> str:
    pr = pr_map.get(commit.hash)
    if pr is not None:
        return f"([#{pr}](https://github.com/{repo}/pull/{pr}))"
    short = commit.hash[:7]
    return f"([{short}](https://github.com/{repo}/commit/{commit.hash}))"


def build_changelog_block(
    repo: str,
    old_version: str,
    new_version: str,
    sections: dict[str, list[tuple[Commit, str]]],
    breaking: list[tuple[Commit, str]],
    pr_map: dict[str, int],
    today: str,
) -> str:
    compare_url = f"https://github.com/{repo}/compare/v{old_version}...v{new_version}"
    lines = [f"## [{new_version}]({compare_url}) ({today})", "", ""]

    if breaking:
        lines.append("### ⚠ BREAKING CHANGES")
        lines.append("")
        for _, text in breaking:
            lines.append(f"* {text}")
        lines.append("")

    present = [(name, items) for name, items in sections.items() if items]
    for idx, (name, items) in enumerate(present):
        if idx > 0:
            lines.append("")
        lines.append(f"### {name}")
        lines.append("")
        for commit, desc in items:
            lines.append(f"* {desc} {entry_link(repo, commit, pr_map)}")
        lines.append("")

    return "\n".join(lines)


def extract_changelog_section(version: str) -> str:
    """回傳 CHANGELOG.md 裡 `## [version]` 那個區塊的內容（含標題，到下一個
    `## [` 前為止）。供 release.yml 的 upload job 取出當次版本的區塊當
    GitHub Release 的說明文字，不用另外傳遞任何中間產物——`prepare` job
    已經 commit 過的 CHANGELOG.md 就是唯一真相。
    """
    text = CHANGELOG.read_text()
    marker = f"## [{version}]"
    start = text.find(marker)
    if start == -1:
        raise SystemExit(f"CHANGELOG.md 找不到 {marker} 這個區塊")
    next_marker = text.find("\n## [", start + len(marker))
    end = next_marker if next_marker != -1 else len(text)
    return text[start:end].rstrip("\n")


def detect_repo() -> str:
    env = os.environ.get("GITHUB_REPOSITORY")
    if env:
        return env
    url = run_git("config", "--get", "remote.origin.url").strip()
    m = re.search(r"[:/]([^/]+/[^/]+?)(?:\.git)?$", url)
    if not m:
        raise SystemExit(f"無法從 remote URL 解析 owner/repo：{url}")
    return m.group(1)


def emit_github_output(name: str, value: str) -> None:
    path = os.environ.get("GITHUB_OUTPUT")
    if not path:
        return
    with open(path, "a") as f:
        f.write(f"{name}={value}\n")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--from", dest="base", help="上一個版本的 tag，例如 v1.9.0")
    parser.add_argument(
        "--dry-run", action="store_true", help="只印出計算結果，不寫入 Cargo.toml / CHANGELOG.md"
    )
    parser.add_argument("--repo", help="owner/repo，預設從 GITHUB_REPOSITORY 或 git remote 解析")
    parser.add_argument(
        "--print-section",
        metavar="VERSION",
        help="印出 CHANGELOG.md 裡指定版本（不含前導 v）的區塊，印完就結束，"
        "不做本檔案其他任何事，也不需要 --from",
    )
    args = parser.parse_args()

    if args.print_section:
        print(extract_changelog_section(args.print_section))
        return 0

    if not args.base:
        parser.error("--from 是必填（除非搭配 --print-section）")

    repo = args.repo or detect_repo()
    commits = load_commits(args.base)
    bump, sections, breaking = classify(commits)

    if bump is None:
        # 沒有這則警告，「發版」與「漏發版」在 run 頁面上長得一模一樣，
        # 都是綠燈——只能等有人發現 tag 沒出來。
        quiet = "／".join(t for t, (_, level) in TYPE_TABLE.items() if level is None)
        warn(
            f"這次 push 沒有觸發發版：{args.base}..HEAD 的 {len(commits)} 個 commit "
            f"沒有任何一個會升版號（只有 {quiet} 這類，或格式不合被略過）。"
        )
        print("NO_BUMP")
        return 0

    cargo_lines = CARGO_TOML.read_text().splitlines(keepends=True)
    version_idx, package_name, old_version = find_package_info(cargo_lines)
    new_version = bump_version(old_version, bump)
    pr_map = build_pr_map(args.base)
    today = date.today().isoformat()
    block = build_changelog_block(repo, old_version, new_version, sections, breaking, pr_map, today)

    print(f"bump={bump}")
    print(f"version={new_version}")
    print("--- CHANGELOG block ---")
    print(block)

    if args.dry_run:
        return 0

    cargo_lines[version_idx] = re.sub(
        r'"[^"]+"', f'"{new_version}"', cargo_lines[version_idx], count=1
    )
    CARGO_TOML.write_text("".join(cargo_lines))

    lock_lines = CARGO_LOCK.read_text().splitlines(keepends=True)
    lock_idx, _ = find_lock_version(lock_lines, package_name)
    lock_lines[lock_idx] = re.sub(r'"[^"]+"', f'"{new_version}"', lock_lines[lock_idx], count=1)
    CARGO_LOCK.write_text("".join(lock_lines))

    header = "# Changelog\n\n"
    text = CHANGELOG.read_text()
    if not text.startswith(header):
        raise SystemExit("CHANGELOG.md 開頭不是預期的 '# Changelog\\n\\n'")
    CHANGELOG.write_text(header + block + "\n" + text[len(header):])

    emit_github_output("version", new_version)
    emit_github_output("tag", f"v{new_version}")

    return 0


if __name__ == "__main__":
    sys.exit(main())

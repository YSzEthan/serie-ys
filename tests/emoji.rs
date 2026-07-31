//! commit 訊息與 stash message 的 emoji shortcode 展開。
//!
//! stash 有兩個獨立的載入路徑（commit list 走 `parse_commit_line`，refs 樹走
//! `load_stashes_as_refs`），漏掉其中一個就會出現「同一筆 stash 在兩個畫面長得不一樣」，
//! 所以這裡對著真的 git repo 一起驗。

use std::path::Path;
use std::process::{Command, Output};

use serie::git::{Ref, Repository, SortCommit};
use tempfile::TempDir;

fn git(path: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(args)
        .current_dir(path)
        .env("GIT_AUTHOR_NAME", "Author Name")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "Committer Name")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_CONFIG_NOSYSTEM", "true")
        .output()
        .unwrap_or_else(|e| panic!("failed to run git {}: {e}", args.join(" ")))
}

#[test]
fn commit_and_stash_text_expand_emoji_shortcodes() {
    let dir = TempDir::new().unwrap();
    let path = dir.path();

    git(path, &["init", "-b", "main"]);
    git(
        path,
        &[
            "commit",
            "--allow-empty",
            "-m",
            ":tada: 上線",
            "-m",
            "細節 :+1:",
        ],
    );
    std::fs::write(path.join("wip.txt"), "wip").unwrap();
    git(
        path,
        &[
            "stash",
            "push",
            "--include-untracked",
            "-m",
            ":sparkles: 做到一半",
        ],
    );

    let repo = Repository::load(path, SortCommit::Chronological, None).unwrap();

    let head = repo
        .all_commits()
        .iter()
        .find(|c| c.subject.contains("上線"))
        .expect("找不到剛建立的 commit");
    assert_eq!(head.subject, "🎉 上線");
    assert!(
        head.body.contains("細節 👍"),
        "body 也要展開：{:?}",
        head.body
    );

    // refs 樹的 stash message 是另一條載入路徑，不會被 commit 那條覆蓋到。
    let stash_message = repo
        .all_refs()
        .into_iter()
        .find_map(|r| match r {
            Ref::Stash { message, .. } => Some(message.clone()),
            _ => None,
        })
        .expect("找不到 stash ref");
    assert!(
        stash_message.contains("✨ 做到一半"),
        "stash message 未展開：{stash_message:?}"
    );
}

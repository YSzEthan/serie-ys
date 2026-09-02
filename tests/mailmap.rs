//! `load_commits_format` 用大寫的 `%aN`/`%aE`/`%cN`/`%cE`，會經 repo 根目錄的
//! `.mailmap` 解析作者／committer 身分。這裡對著真的 git repo 驗兩件事：
//! 有 `.mailmap` 時身分被正規化，沒有時輸出跟原始 commit 資料一致（不是意外的
//! no-op 改動，也沒有副作用）。

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;
use ysgit::git::{Repository, SortCommit};

fn git(path: &Path, args: &[&str]) -> Output {
    let out = Command::new("git")
        .args(args)
        .current_dir(path)
        .env("GIT_AUTHOR_NAME", "Old Name")
        .env("GIT_AUTHOR_EMAIL", "old@example.com")
        .env("GIT_COMMITTER_NAME", "Old Name")
        .env("GIT_COMMITTER_EMAIL", "old@example.com")
        .env("GIT_CONFIG_NOSYSTEM", "true")
        // 比照 tests/graph.rs：開發者 global config 裡的 commit.gpgsign 或改寫訊息的
        // commit-msg hook 會讓這個測試在他機器上紅、在 CI 綠。
        .env("HOME", "/dev/null")
        .output()
        .unwrap_or_else(|e| panic!("failed to run git {}: {e}", args.join(" ")));
    assert!(
        out.status.success(),
        "git {} 失敗: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn assert_identity(path: &Path, subject: &str, name: &str, email: &str) {
    let repo = Repository::load(path, SortCommit::Chronological, None).unwrap();
    let commit = repo
        .all_commits()
        .iter()
        .find(|c| c.subject == subject)
        .expect("找不到目標 commit");
    assert_eq!((&*commit.author_name, &*commit.author_email), (name, email));
    assert_eq!(
        (&*commit.committer_name, &*commit.committer_email),
        (name, email)
    );
}

#[test]
fn mailmap_resolves_author_and_committer_identity() {
    let dir = TempDir::new().unwrap();
    let path = dir.path();

    git(path, &["init", "-b", "main"]);
    git(path, &["commit", "--allow-empty", "-m", "before mailmap"]);
    std::fs::write(
        path.join(".mailmap"),
        "New Name <new@example.com> <old@example.com>\n",
    )
    .unwrap();
    git(path, &["add", ".mailmap"]);
    git(path, &["commit", "-m", "add mailmap"]);

    assert_identity(path, "before mailmap", "New Name", "new@example.com");
}

#[test]
fn no_mailmap_keeps_raw_identity() {
    let dir = TempDir::new().unwrap();
    let path = dir.path();

    git(path, &["init", "-b", "main"]);
    git(path, &["commit", "--allow-empty", "-m", "no mailmap here"]);

    assert_identity(path, "no mailmap here", "Old Name", "old@example.com");
}

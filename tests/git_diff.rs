use std::{path::Path, process::Command};

use ysgit::git::{self, DiffTarget};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn commit_file_diff_shows_only_the_selected_file() -> TestResult {
    let dir = tempfile::tempdir()?;
    let repo = TestRepo::new(dir.path());
    repo.init();
    repo.write_file("a.txt", "line1\n");
    repo.write_file("other.txt", "unrelated\n");
    repo.add_all();
    repo.commit("seed");

    repo.write_file("a.txt", "line1\nline2\n");
    repo.add_all();
    repo.commit("modify a.txt only");

    let repository = git::Repository::load(dir.path(), git::SortCommit::Chronological, None)?;
    let target = DiffTarget::Commit {
        hash: repo.rev_parse_head().as_str().into(),
        path: "a.txt".into(),
    };

    let diff = strip_ansi(&repository.file_diff(&target).unwrap());
    assert!(diff.contains("+line2"), "diff missing added line:\n{diff}");
    assert!(
        !diff.contains("other.txt"),
        "single-file diff leaked an unrelated file:\n{diff}"
    );

    Ok(())
}

#[test]
fn commit_file_diff_initial_commit_diffs_against_empty_tree() -> TestResult {
    let dir = tempfile::tempdir()?;
    let repo = TestRepo::new(dir.path());
    repo.init();
    repo.write_file("only.txt", "hello\n");
    repo.add_all();
    repo.commit("initial commit");

    let repository = git::Repository::load(dir.path(), git::SortCommit::Chronological, None)?;
    let target = DiffTarget::Commit {
        hash: repo.rev_parse_head().as_str().into(),
        path: "only.txt".into(),
    };

    let diff = strip_ansi(&repository.file_diff(&target).unwrap());
    assert!(
        diff.contains("+hello"),
        "initial commit diff should show the whole file as added:\n{diff}"
    );

    Ok(())
}

#[test]
fn untracked_file_diff_is_not_empty() -> TestResult {
    let dir = tempfile::tempdir()?;
    let repo = TestRepo::new(dir.path());
    repo.init();
    repo.write_file("committed.txt", "seed\n");
    repo.add_all();
    repo.commit("seed commit");

    // 不 add，維持 untracked —— 一般 `git diff -- <path>` 對這個狀態輸出
    // 空字串，`file_diff` 要自動改走 `--no-index`。
    repo.write_file("new_file.txt", "brand new content\n");

    let repository = git::Repository::load(dir.path(), git::SortCommit::Chronological, None)?;
    let target = DiffTarget::Untracked {
        path: "new_file.txt".into(),
    };

    let diff = strip_ansi(&repository.file_diff(&target).unwrap());
    assert!(
        !diff.trim().is_empty(),
        "untracked file diff must not be empty"
    );
    assert!(
        diff.contains("+brand new content"),
        "diff missing content:\n{diff}"
    );

    Ok(())
}

#[test]
fn staged_file_diff_shows_index_change() -> TestResult {
    let dir = tempfile::tempdir()?;
    let repo = TestRepo::new(dir.path());
    repo.init();
    repo.write_file("a.txt", "line1\n");
    repo.add_all();
    repo.commit("seed");

    repo.write_file("a.txt", "line1\nstaged-line\n");
    repo.add_all();

    let repository = git::Repository::load(dir.path(), git::SortCommit::Chronological, None)?;
    let target = DiffTarget::Staged {
        path: "a.txt".into(),
    };

    let diff = strip_ansi(&repository.file_diff(&target).unwrap());
    assert!(diff.contains("+staged-line"), "diff:\n{diff}");

    Ok(())
}

#[test]
fn unstaged_file_diff_shows_worktree_change() -> TestResult {
    let dir = tempfile::tempdir()?;
    let repo = TestRepo::new(dir.path());
    repo.init();
    repo.write_file("a.txt", "line1\n");
    repo.add_all();
    repo.commit("seed");

    repo.write_file("a.txt", "line1\nunstaged-line\n");
    // 不 add

    let repository = git::Repository::load(dir.path(), git::SortCommit::Chronological, None)?;
    let target = DiffTarget::Unstaged {
        path: "a.txt".into(),
    };

    let diff = strip_ansi(&repository.file_diff(&target).unwrap());
    assert!(diff.contains("+unstaged-line"), "diff:\n{diff}");

    Ok(())
}

#[test]
fn diff_output_keeps_non_ascii_filename_unescaped() -> TestResult {
    let dir = tempfile::tempdir()?;
    let repo = TestRepo::new(dir.path());
    repo.init();
    repo.write_file("seed.txt", "seed\n");
    repo.add_all();
    repo.commit("seed");

    let filename = "中文檔名.txt";
    repo.write_file(filename, "內容\n");
    repo.add_all();

    let repository = git::Repository::load(dir.path(), git::SortCommit::Chronological, None)?;
    let target = DiffTarget::Staged {
        path: filename.into(),
    };

    let diff = repository.file_diff(&target).unwrap();
    assert!(
        diff.contains(filename),
        "diff should show the raw filename, not an escaped one:\n{diff}"
    );
    assert!(
        !diff.contains("\\346"),
        "filename must not be octal-escaped (core.quotePath):\n{diff}"
    );

    Ok(())
}

/// `--color=always` 把 ANSI 色碼插在 `+`/`-` 標記與行內容之間（各自獨立上色），
/// 所以「+line2」這種跨標記的子字串斷言必須先脫色才有意義。
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for c2 in chars.by_ref() {
                if c2.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

struct TestRepo<'a> {
    path: &'a Path,
}

impl<'a> TestRepo<'a> {
    fn new(path: &'a Path) -> Self {
        Self { path }
    }

    fn init(&self) {
        self.run(&["init", "-b", "master"]);
    }

    fn write_file(&self, name: &str, content: &str) {
        std::fs::write(self.path.join(name), content).unwrap();
    }

    fn add_all(&self) {
        self.run(&["add", "-A"]);
    }

    fn commit(&self, message: &str) {
        self.run(&["commit", "-m", message]);
    }

    fn rev_parse_head(&self) -> String {
        let out = self.run(&["rev-parse", "HEAD"]);
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        let out = Command::new("git")
            .args(args)
            .current_dir(self.path)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .env("GIT_CONFIG_NOSYSTEM", "true")
            .env("HOME", "/dev/null")
            .output()
            .unwrap_or_else(|_| panic!("failed to execute git {}", args.join(" ")));
        assert!(
            out.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }
}

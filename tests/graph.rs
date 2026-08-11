use std::{path::Path, process::Command};

use chrono::{DateTime, Days, NaiveDate, TimeZone, Utc};
use ysgit::{color, git, graph};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const OUTPUT_DIR: &str = "./out/graph";
const SNAPSHOT_DIR: &str = "./tests/graph";

#[test]
fn straight_001() -> TestResult {
    let dir = tempfile::tempdir()?;
    let repo_path = dir.path();

    let git = &GitRepository::new(repo_path);

    git.init();

    let mut base_date = Utc.with_ymd_and_hms(2024, 1, 1, 1, 2, 3).unwrap();
    for i in 1..=100 {
        let msg = &format!("{i:03}");
        let date = &base_date.format("%Y-%m-%d").to_string();
        git.commit(msg, date);
        base_date = base_date.checked_add_days(Days::new(1)).unwrap();
    }

    git.log();

    let options = &[GenerateGraphOption::new(
        "straight_001",
        git::SortCommit::Chronological,
    )];

    copy_git_dir(repo_path, "straight_001");

    generate_and_output_text_graphs(repo_path, options);
    assert_text_graphs(options);

    Ok(())
}

#[test]
fn branch_001() -> TestResult {
    let dir = tempfile::tempdir()?;
    let repo_path = dir.path();

    let git = &GitRepository::new(repo_path);

    git.init();

    git.commit("001", "2024-01-01");
    git.commit("002", "2024-01-02");

    git.checkout("master");
    git.checkout_b("10");
    git.commit("011", "2024-02-01");

    git.checkout("master");
    git.checkout_b("20");
    git.commit("021", "2024-02-02");

    git.checkout("master");
    git.checkout_b("30");
    git.commit("031", "2024-02-03");

    git.checkout("master");
    git.checkout_b("40");
    git.commit("041", "2024-02-04");

    git.checkout("master");
    git.checkout_b("50");
    git.commit("051", "2024-02-05");

    git.checkout("10");
    git.commit("012", "2024-02-06");

    git.checkout("20");
    git.commit("022", "2024-02-07");

    git.checkout("30");
    git.commit("032", "2024-02-08");

    git.checkout("40");
    git.commit("042", "2024-02-09");

    git.checkout("50");
    git.commit("052", "2024-02-10");

    git.checkout("master");
    git.merge(&["10"], "2024-03-01");
    git.merge(&["20"], "2024-03-02");
    git.merge(&["30"], "2024-03-03");
    git.merge(&["40"], "2024-03-04");
    git.merge(&["50"], "2024-03-05");

    git.log();

    let options = &[
        GenerateGraphOption::new("branch_001_chrono", git::SortCommit::Chronological),
        GenerateGraphOption::new("branch_001_topo", git::SortCommit::Topological),
        GenerateGraphOption::new("branch_001_max_count", git::SortCommit::Chronological)
            .with_max_count(10),
    ];

    copy_git_dir(repo_path, "branch_001");

    generate_and_output_text_graphs(repo_path, options);
    assert_text_graphs(options);

    Ok(())
}

#[test]
fn branch_002() -> TestResult {
    let dir = tempfile::tempdir()?;
    let repo_path = dir.path();

    let git = &GitRepository::new(repo_path);

    git.init();

    git.commit("001", "2024-01-01");

    git.checkout("master");
    git.checkout_b("10");
    git.commit("011", "2024-02-01");

    git.checkout_b("20");
    git.commit("021", "2024-02-02");

    git.checkout_b("30");
    git.commit("031", "2024-02-03");

    git.checkout("10");
    git.commit("012", "2024-02-04");

    git.checkout("20");
    git.commit("022", "2024-02-05");

    git.checkout("10");
    git.checkout_b("40");
    git.commit("041", "2024-02-06");

    git.checkout("20");
    git.checkout_b("50");
    git.commit("51", "2024-02-07");

    git.checkout("30");
    git.commit("032", "2024-02-08");

    git.checkout("master");
    git.merge(&["40"], "2024-03-01");

    git.checkout("20");
    git.commit("023", "2024-03-02");

    git.checkout("master");
    git.merge(&["20"], "2024-03-03");

    git.checkout("10");
    git.commit("013", "2024-03-04");

    git.checkout("master");
    git.merge(&["10"], "2024-03-05");

    git.log();

    let options = &[
        GenerateGraphOption::new("branch_002_chrono", git::SortCommit::Chronological),
        GenerateGraphOption::new("branch_002_topo", git::SortCommit::Topological),
        GenerateGraphOption::new("branch_002_max_count", git::SortCommit::Chronological)
            .with_max_count(5),
    ];

    copy_git_dir(repo_path, "branch_002");

    generate_and_output_text_graphs(repo_path, options);
    assert_text_graphs(options);

    Ok(())
}

#[test]
fn branch_003() -> TestResult {
    let dir = tempfile::tempdir()?;
    let repo_path = dir.path();

    let git = &GitRepository::new(repo_path);

    git.init();

    git.commit("001", "2024-01-01");

    git.checkout_b("10");
    git.checkout_b("20");
    git.checkout_b("30");

    git.checkout("master");
    git.commit("002", "2024-01-02");

    git.checkout("10");
    git.commit("011", "2024-02-01");
    git.commit("012", "2024-02-02");

    git.checkout("20");
    git.commit("021", "2024-02-03");

    git.checkout("30");
    git.commit("031", "2024-02-04");

    git.checkout("20");
    git.commit("022", "2024-02-05");

    git.log();

    let options = &[
        GenerateGraphOption::new("branch_003_chrono", git::SortCommit::Chronological),
        GenerateGraphOption::new("branch_003_topo", git::SortCommit::Topological),
    ];

    copy_git_dir(repo_path, "branch_003");

    generate_and_output_text_graphs(repo_path, options);
    assert_text_graphs(options);

    Ok(())
}

#[test]
fn branch_004() -> TestResult {
    let dir = tempfile::tempdir()?;
    let repo_path = dir.path();

    let git = &GitRepository::new(repo_path);

    git.init();

    git.commit("001", "2024-01-01");

    git.checkout_b("10");
    git.commit("011", "2024-02-01");

    git.checkout("master");
    git.merge(&["10"], "2024-02-02");

    git.checkout_b("20");
    git.commit("021", "2024-02-03");

    git.checkout("master");
    git.merge(&["20"], "2024-02-04");

    git.commit("002", "2024-02-05");

    git.checkout_b("30");
    git.checkout_b("40");
    git.checkout_b("50");

    git.checkout("30");
    git.commit("031", "2024-03-01");

    git.checkout("40");
    git.commit("041", "2024-03-02");

    git.checkout("50");
    git.commit("051", "2024-03-03");

    git.checkout("master");
    git.merge(&["40"], "2024-03-04");

    git.checkout_b("60");
    git.commit("061", "2024-04-01");

    git.checkout("50");
    git.commit("052", "2024-04-02");

    git.checkout("30");
    git.commit("032", "2024-04-03");

    git.checkout("master");
    git.commit("003", "2024-04-04");

    git.merge(&["30"], "2024-04-05");
    git.merge(&["50"], "2024-04-06");
    git.merge(&["60"], "2024-04-07");

    git.checkout_b("70");
    git.commit("071", "2024-05-01");

    git.checkout_b("80");
    git.commit("081", "2024-05-02");

    git.checkout("master");
    git.commit("004", "2024-05-03");

    git.log();

    let options = &[
        GenerateGraphOption::new("branch_004_chrono", git::SortCommit::Chronological),
        GenerateGraphOption::new("branch_004_topo", git::SortCommit::Topological),
    ];

    copy_git_dir(repo_path, "branch_004");

    generate_and_output_text_graphs(repo_path, options);
    assert_text_graphs(options);

    Ok(())
}

#[test]
fn branch_005() -> TestResult {
    let dir = tempfile::tempdir()?;
    let repo_path = dir.path();

    let git = &GitRepository::new(repo_path);

    git.init();

    git.commit("001", "2024-01-01");

    git.checkout_b("10");

    git.checkout("master");
    git.commit("002", "2024-01-02");

    git.checkout("10");
    git.commit("011", "2024-02-01");

    git.checkout("master");
    git.commit("003", "2024-02-02");

    git.checkout_b("20");

    git.checkout("master");
    git.merge(&["10"], "2024-03-01");

    git.checkout("20");
    git.commit("021", "2024-03-02");

    git.checkout("master");
    git.merge(&["20"], "2024-03-03");

    git.log();

    let options = &[
        GenerateGraphOption::new("branch_005_chrono", git::SortCommit::Chronological),
        GenerateGraphOption::new("branch_005_topo", git::SortCommit::Topological),
    ];

    copy_git_dir(repo_path, "branch_005");

    generate_and_output_text_graphs(repo_path, options);
    assert_text_graphs(options);

    Ok(())
}

#[test]
fn merge_001() -> TestResult {
    let dir = tempfile::tempdir()?;
    let repo_path = dir.path();

    let git = &GitRepository::new(repo_path);

    git.init();

    git.commit("001", "2024-01-01");

    git.checkout_b("10");
    git.commit("011", "2024-02-01");

    git.checkout("master");
    git.checkout_b("20");
    git.commit("021", "2024-02-02");

    git.checkout("master");
    git.checkout_b("30");
    git.commit("031", "2024-02-03");

    git.checkout("10");
    git.commit("012", "2024-02-04");

    git.checkout("20");
    git.merge(&["10"], "2024-03-01");

    git.checkout("30");
    git.merge(&["10"], "2024-03-02");

    git.checkout("20");
    git.commit("022", "2024-03-03");

    git.checkout_b("40");
    git.commit("041", "2024-03-04");

    git.checkout("10");
    git.merge(&["20"], "2024-03-05");

    git.checkout("30");
    git.commit("032", "2024-03-06");

    git.checkout("10");
    git.merge(&["30"], "2024-03-07");

    git.checkout("40");
    git.merge(&["10"], "2024-03-08");

    git.checkout("master");
    git.merge(&["10"], "2024-03-09");

    git.log();

    let options = &[
        GenerateGraphOption::new("merge_001_chrono", git::SortCommit::Chronological),
        GenerateGraphOption::new("merge_001_topo", git::SortCommit::Topological),
    ];

    copy_git_dir(repo_path, "merge_001");

    generate_and_output_text_graphs(repo_path, options);
    assert_text_graphs(options);

    Ok(())
}

#[test]
fn merge_002() -> TestResult {
    let dir = tempfile::tempdir()?;
    let repo_path = dir.path();

    let git = &GitRepository::new(repo_path);

    git.init();

    git.commit("001", "2024-01-01");

    git.checkout_b("10");
    git.commit("011", "2024-02-01");
    git.commit("012", "2024-02-02");

    git.checkout("master");
    git.checkout_b("20");
    git.commit("021", "2024-02-03");
    git.commit("022", "2024-02-04");

    git.checkout("master");
    git.checkout_b("30");
    git.commit("031", "2024-02-05");
    git.commit("032", "2024-02-06");

    git.checkout_b("40");
    git.commit("041", "2024-02-07");

    git.checkout("20");
    git.merge(&["10", "30"], "2024-03-01");

    git.checkout("master");
    git.merge(&["40"], "2024-03-02");

    git.log();

    let options = &[
        GenerateGraphOption::new("merge_002_chrono", git::SortCommit::Chronological),
        GenerateGraphOption::new("merge_002_topo", git::SortCommit::Topological),
    ];

    copy_git_dir(repo_path, "merge_002");

    generate_and_output_text_graphs(repo_path, options);
    assert_text_graphs(options);

    Ok(())
}

#[test]
fn merge_003() -> TestResult {
    let dir = tempfile::tempdir()?;
    let repo_path = dir.path();

    let git = &GitRepository::new(repo_path);

    git.init();

    git.commit("001", "2024-01-01");

    git.checkout_b("10a");
    git.commit("011", "2024-02-01");

    git.checkout("master");
    git.checkout_b("20");
    git.commit("021", "2024-02-02");

    git.checkout("master");
    git.checkout_b("30");
    git.commit("031", "2024-02-03");

    git.checkout("10a");
    git.checkout_b("10b");
    git.checkout("10a");
    git.commit("012", "2024-02-04");

    git.checkout("20");
    git.merge(&["10a"], "2024-03-01");

    git.checkout("30");
    git.merge(&["10b"], "2024-03-02");

    git.checkout("master");
    git.merge(&["10a"], "2024-04-01");

    git.log();

    let options = &[
        GenerateGraphOption::new("merge_003_chrono", git::SortCommit::Chronological),
        GenerateGraphOption::new("merge_003_topo", git::SortCommit::Topological),
    ];

    copy_git_dir(repo_path, "merge_003");

    generate_and_output_text_graphs(repo_path, options);
    assert_text_graphs(options);

    Ok(())
}

#[test]
fn merge_004() -> TestResult {
    let dir = tempfile::tempdir()?;
    let repo_path = dir.path();

    let git = &GitRepository::new(repo_path);

    git.init();

    git.commit("001", "2024-01-01");

    git.checkout_b("10a");
    git.commit("011", "2024-02-01");

    git.checkout("master");
    git.checkout_b("20");
    git.commit("021", "2024-02-02");

    git.checkout("master");
    git.checkout_b("30");
    git.commit("031", "2024-02-03");

    git.checkout("master");
    git.checkout_b("40");
    git.commit("041", "2024-02-04");

    git.checkout("10a");
    git.checkout_b("10c");

    git.checkout("10a");
    git.commit("012", "2024-02-05");

    git.checkout_b("10b");
    git.checkout("10a");
    git.commit("013", "2024-02-06");

    git.checkout("20");
    git.merge(&["10a"], "2024-03-01");

    git.checkout("30");
    git.merge(&["10b"], "2024-03-02");

    git.checkout("40");
    git.merge(&["10c"], "2024-03-03");

    git.checkout("master");
    git.merge(&["10a"], "2024-04-01");

    git.log();

    let options = &[
        GenerateGraphOption::new("merge_004_chrono", git::SortCommit::Chronological),
        GenerateGraphOption::new("merge_004_topo", git::SortCommit::Topological),
    ];

    copy_git_dir(repo_path, "merge_004");

    generate_and_output_text_graphs(repo_path, options);
    assert_text_graphs(options);

    Ok(())
}

#[test]
fn merge_005() -> TestResult {
    let dir = tempfile::tempdir()?;
    let repo_path = dir.path();

    let git = &GitRepository::new(repo_path);

    git.init();

    git.commit("001", "2024-01-01");

    git.checkout_b("10");
    git.commit("011", "2024-02-01");

    git.checkout_b("20");
    git.commit("021", "2024-02-02");

    git.checkout("10");
    git.commit("012", "2024-02-03");

    git.checkout("master");
    git.merge(&["10"], "2024-03-01");

    git.checkout_b("30");
    git.commit("031", "2024-04-01");
    git.commit("032", "2024-04-02");

    git.checkout("master");
    git.commit("002", "2024-04-03");

    git.checkout_b("40");
    git.commit("041", "2024-05-01");

    git.checkout("master");
    git.merge(&["40"], "2024-05-02");

    git.checkout_b("50");
    git.checkout_b("60");

    git.checkout("50");
    git.commit("051", "2024-06-01");

    git.checkout("60");
    git.commit("061", "2024-06-02");

    git.checkout("master");
    git.merge(&["60"], "2024-06-03");

    git.checkout("master");
    git.merge(&["30"], "2024-06-04");

    git.checkout("master");
    git.merge(&["20"], "2024-06-05");

    git.log();

    let options = &[
        GenerateGraphOption::new("merge_005_chrono", git::SortCommit::Chronological),
        GenerateGraphOption::new("merge_005_topo", git::SortCommit::Topological),
    ];

    copy_git_dir(repo_path, "merge_005");

    generate_and_output_text_graphs(repo_path, options);
    assert_text_graphs(options);

    Ok(())
}

#[test]
fn stash_001() -> TestResult {
    // 測試案例：有多個 stash，最近一筆 commit 是普通 commit
    let dir = tempfile::tempdir()?;
    let repo_path = dir.path();

    let git = &GitRepository::new(repo_path);

    git.init();

    git.commit("001", "2024-01-01");
    git.commit("002", "2024-01-02");

    git.stash("2024-01-03");

    git.commit("003", "2024-01-04");

    git.stash("2024-01-05");

    git.commit("004", "2024-01-06");

    git.checkout_b("10");
    git.checkout("master");

    git.commit("005", "2024-01-07");
    git.commit("006", "2024-01-08");

    git.checkout("10");
    git.stash("2024-01-09");

    git.checkout("master");
    git.commit("007", "2024-01-10");

    let options = &[
        GenerateGraphOption::new("stash_001_chrono", git::SortCommit::Chronological),
        GenerateGraphOption::new("stash_001_topo", git::SortCommit::Topological),
    ];

    copy_git_dir(repo_path, "stash_001");

    generate_and_output_text_graphs(repo_path, options);
    assert_text_graphs(options);

    Ok(())
}

#[test]
fn stash_002() -> TestResult {
    // 測試案例：有多個 stash，最近一筆 commit 是 stash
    let dir = tempfile::tempdir()?;
    let repo_path = dir.path();

    let git = &GitRepository::new(repo_path);

    git.init();

    git.commit("001", "2024-01-01");
    git.commit("002", "2024-01-02");

    git.stash("2024-01-03");

    git.commit("003", "2024-01-04");

    git.stash("2024-01-05");

    git.commit("004", "2024-01-06");

    git.checkout_b("10");
    git.checkout("master");

    git.commit("005", "2024-01-07");
    git.commit("006", "2024-01-08");

    git.checkout("10");
    git.stash("2024-01-09");

    let options = &[
        GenerateGraphOption::new("stash_002_chrono", git::SortCommit::Chronological),
        GenerateGraphOption::new("stash_002_topo", git::SortCommit::Topological),
    ];

    copy_git_dir(repo_path, "stash_002");

    generate_and_output_text_graphs(repo_path, options);
    assert_text_graphs(options);

    Ok(())
}

#[test]
fn stash_003() -> TestResult {
    // 測試案例：無法從任何分支到達的 stash
    let dir = tempfile::tempdir()?;
    let repo_path = dir.path();

    let git = &GitRepository::new(repo_path);

    git.init();

    git.commit("001", "2024-01-01");
    git.commit("002", "2024-01-02");

    git.checkout_b("10");
    git.commit("011", "2024-02-01");

    git.stash("2024-02-02");

    git.checkout("master");
    git.commit("003", "2024-03-01");

    git.branch_d("10");

    let options = &[
        GenerateGraphOption::new("stash_003_chrono", git::SortCommit::Chronological),
        GenerateGraphOption::new("stash_003_topo", git::SortCommit::Topological),
    ];

    copy_git_dir(repo_path, "stash_003");

    generate_and_output_text_graphs(repo_path, options);
    assert_text_graphs(options);

    Ok(())
}

#[test]
fn stash_004() -> TestResult {
    // 測試案例：同一個 commit 上有多個 stash
    let dir = tempfile::tempdir()?;
    let repo_path = dir.path();

    let git = &GitRepository::new(repo_path);

    git.init();

    git.commit("001", "2024-01-01");
    git.commit("002", "2024-01-02");

    git.stash("2024-02-01");
    git.stash("2024-02-02");
    git.stash("2024-02-03");

    git.commit("003", "2024-03-01");

    let options = &[
        GenerateGraphOption::new("stash_004_chrono", git::SortCommit::Chronological),
        GenerateGraphOption::new("stash_004_topo", git::SortCommit::Topological),
    ];

    copy_git_dir(repo_path, "stash_004");

    generate_and_output_text_graphs(repo_path, options);
    assert_text_graphs(options);

    Ok(())
}

#[test]
fn orphan_001() -> TestResult {
    let dir = tempfile::tempdir()?;
    let repo_path = dir.path();

    let git = &GitRepository::new(repo_path);

    git.init();

    git.commit("001", "2024-01-01");
    git.commit("002", "2024-01-02");

    git.checkout_orphan("o1");
    git.commit("011", "2024-01-03");

    git.checkout("master");
    git.commit("003", "2024-01-04");

    git.checkout("o1");
    git.commit("012", "2024-01-05");

    git.checkout("master");
    git.commit("004", "2024-01-06");

    git.checkout_orphan("o2");
    git.commit("021", "2024-01-07");
    git.commit("022", "2024-01-08");

    git.log();

    let options = &[
        GenerateGraphOption::new("orphan_001_chrono", git::SortCommit::Chronological),
        GenerateGraphOption::new("orphan_001_topo", git::SortCommit::Topological),
    ];

    copy_git_dir(repo_path, "orphan_001");

    generate_and_output_text_graphs(repo_path, options);
    assert_text_graphs(options);

    Ok(())
}

#[test]
fn orphan_002() -> TestResult {
    let dir = tempfile::tempdir()?;
    let repo_path = dir.path();

    let git = &GitRepository::new(repo_path);

    git.init();

    git.commit("001", "2024-01-01");
    git.commit("002", "2024-01-02");

    git.checkout_b("010");
    git.commit("011", "2024-01-03");

    git.checkout("master");
    git.merge(&["010"], "2024-01-04");

    git.commit("003", "2024-02-01");

    git.checkout_orphan("o1");
    git.commit("021", "2024-02-02");
    git.commit("022", "2024-02-03");

    git.checkout("master");
    git.commit("004", "2024-02-04");
    git.commit("005", "2024-02-05");

    git.log();

    let options = &[
        GenerateGraphOption::new("orphan_002_chrono", git::SortCommit::Chronological),
        GenerateGraphOption::new("orphan_002_topo", git::SortCommit::Topological),
    ];

    copy_git_dir(repo_path, "orphan_002");

    generate_and_output_text_graphs(repo_path, options);
    assert_text_graphs(options);

    Ok(())
}

#[test]
fn head_001() -> TestResult {
    // 測試案例：detached HEAD
    let dir = tempfile::tempdir()?;
    let repo_path = dir.path();

    let git = &GitRepository::new(repo_path);

    git.init();

    git.commit("001", "2024-01-01");
    git.commit("002", "2024-01-02");

    git.checkout_b("10");
    git.commit("011", "2024-01-03");
    git.commit("012", "2024-01-04");

    let hash = git.rev_parse_head();

    git.checkout("master");
    git.commit("003", "2024-01-05");
    git.commit("004", "2024-01-06");

    git.checkout(hash.as_str());
    git.branch_d("10");

    git.log();

    let options = &[
        GenerateGraphOption::new("head_001_chrono", git::SortCommit::Chronological),
        GenerateGraphOption::new("head_001_topo", git::SortCommit::Topological),
    ];

    copy_git_dir(repo_path, "head_001");

    generate_and_output_text_graphs(repo_path, options);
    assert_text_graphs(options);

    Ok(())
}

#[test]
fn complex_001() -> TestResult {
    let dir = tempfile::tempdir()?;
    let repo_path = dir.path();

    let git = &GitRepository::new(repo_path);

    git.init();

    git.commit("001", "2024-01-01");

    git.checkout_b("10");
    git.checkout_b("20");

    git.checkout("master");
    git.commit("002", "2024-01-02");

    git.checkout("20");
    git.commit("021", "2024-02-01");

    git.checkout("10");
    git.commit("011", "2024-02-02");
    git.commit("012", "2024-02-03");

    git.checkout("master");
    git.checkout_b("30");
    git.commit("031", "2024-02-04");

    git.checkout("10");
    git.commit("013", "2024-03-01");

    git.checkout_b("40");

    git.checkout("20");
    git.merge(&["10"], "2024-03-02");
    git.commit("022", "2024-03-03");

    git.checkout("master");
    git.merge(&["30"], "2024-03-03");
    git.commit("003", "2024-03-04");

    git.checkout("40");
    git.merge(&["master"], "2024-04-01");
    git.commit("041", "2024-04-02");

    git.checkout("master");
    git.merge(&["40"], "2024-04-03");

    git.checkout("20");
    git.checkout_b("50");

    git.checkout("20");
    git.commit("023", "2024-05-01");
    git.commit("024", "2024-05-02");

    git.checkout("50");
    git.merge(&["20"], "2024-05-03");
    git.commit("051", "2024-05-04");

    git.checkout("20");
    git.merge(&["50"], "2024-05-05");

    git.checkout("30");
    git.commit("032", "2024-06-01");

    git.checkout("20");
    git.commit("025", "2024-06-02");

    git.log();

    let options = &[
        GenerateGraphOption::new("complex_001_chrono", git::SortCommit::Chronological),
        GenerateGraphOption::new("complex_001_topo", git::SortCommit::Topological),
    ];

    copy_git_dir(repo_path, "complex_001");

    generate_and_output_text_graphs(repo_path, options);
    assert_text_graphs(options);

    Ok(())
}

/// 測試 `calc_commit_positions` 的 HEAD 欄位保留分支（`reserve_head_col`）。
/// 這需要一個專門設計的 repo：上面既有的 19 個 repo 全都以 HEAD *就是*
/// 最新 commit 收尾，這會讓保留欄無論如何都收斂回第 0 欄（見
/// calc.rs——`head_col_pending` 在 HEAD 那一列處理完的當下就會被清掉，
/// 所以如果 HEAD 的 `pos_y == 0`，就沒有東西需要保留了）。這個 repo
/// 則是刻意讓 HEAD 停在一個沒有子節點、且*不是*最新 commit 的分支
/// 尖端，並帶有具名 ref（`head_has_named_ref` 必須為 true），讓保留
/// 機制真正啟動。
#[test]
fn reserve_col_001() -> TestResult {
    let dir = tempfile::tempdir()?;
    let repo_path = dir.path();

    let git = &GitRepository::new(repo_path);

    git.init();

    git.commit("001", "2024-01-01");
    git.commit("002", "2024-01-02");

    git.checkout_b("10");
    git.commit("011", "2024-01-03");

    git.checkout("master");
    git.commit("003", "2024-01-04");

    git.checkout("10");

    git.log();

    let options = &[
        GenerateGraphOption::new("reserve_col_001_chrono", git::SortCommit::Chronological),
        GenerateGraphOption::new("reserve_col_001_head_col", git::SortCommit::Chronological)
            .with_head_col(),
    ];

    copy_git_dir(repo_path, "reserve_col_001");

    generate_and_output_text_graphs(repo_path, options);
    assert_text_graphs(options);

    Ok(())
}

struct GitRepository<'a> {
    path: &'a Path,
}

impl GitRepository<'_> {
    fn new(path: &'_ Path) -> GitRepository<'_> {
        GitRepository { path }
    }

    fn init(&self) {
        self.run(&["init", "-b", "master"], "");
    }

    fn commit(&self, message: &str, date: &str) {
        let datetime_str = parse_date(date).to_rfc3339();
        self.run(&["commit", "--allow-empty", "-m", message], &datetime_str);
    }

    fn checkout(&self, branch_name: &str) {
        self.run(&["checkout", branch_name], "");
    }

    fn checkout_b(&self, branch_name: &str) {
        self.run(&["checkout", "-b", branch_name], "");
    }

    fn checkout_orphan(&self, branch_name: &str) {
        self.run(&["checkout", "--orphan", branch_name], "");
    }

    fn merge(&self, branch_names: &[&str], date: &str) {
        let datetime_str = parse_date(date).to_rfc3339();
        let mut args = vec!["merge", "--no-ff", "--no-log"];
        args.extend_from_slice(branch_names);
        self.run(&args, &datetime_str);
    }

    fn branch_d(&self, branch_name: &str) {
        self.run(&["branch", "-D", branch_name], "");
    }

    fn stash(&self, date: &str) {
        let dummy_file_path = self.path.join("stash.txt");
        std::fs::File::create(dummy_file_path).unwrap();

        let datetime_str = parse_date(date).to_rfc3339();
        self.run(&["stash", "--include-untracked"], &datetime_str);
    }

    fn rev_parse_head(&self) -> String {
        let output = self.run(&["rev-parse", "HEAD"], "");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn log(&self) {
        let output = self.run(&["log", "--pretty=format:%h %s", "--graph", "--all"], "");
        println!("{}", String::from_utf8(output.stdout).unwrap())
    }

    fn run(&self, args: &[&str], datetime_str: &str) -> std::process::Output {
        let out = Command::new("git")
            .args(args)
            .current_dir(self.path)
            .env("GIT_AUTHOR_NAME", "Author Name")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_AUTHOR_DATE", datetime_str)
            .env("GIT_COMMITTER_NAME", "Committer Name")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_COMMITTER_DATE", datetime_str)
            .env("GIT_CONFIG_NOSYSTEM", "true")
            .env("HOME", "/dev/null")
            .output()
            .unwrap_or_else(|_| panic!("failed to execute git {}", args.join(" ")));
        println!("git {}: returned {}", args.join(" "), out.status,);
        out
    }
}

fn parse_date(date: &str) -> DateTime<Utc> {
    let dt = NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .unwrap()
        .and_hms_opt(1, 2, 3)
        .unwrap();
    Utc.from_utc_datetime(&dt)
}

struct GenerateGraphOption {
    output_name: &'static str,
    sort: git::SortCommit,
    max_count: Option<usize>,
    // 是否要把 repo 實際的 HEAD 傳給 `calc_graph` 並為它保留一欄
    // （`reserve_head_col`）。為 false 時（預設值，上面除了 `reserve_col_001`
    // 之外的每個案例都用這個），HEAD 會被視為未知（`None`）——這跟
    // PNG 時代的 snapshot 產生方式完全一致，當年一律無條件呼叫
    // `calc_graph(&repository, None, false)`。
    reserve_head_col: bool,
}

impl GenerateGraphOption {
    fn new(output_name: &'static str, sort: git::SortCommit) -> GenerateGraphOption {
        GenerateGraphOption {
            output_name,
            sort,
            max_count: None,
            reserve_head_col: false,
        }
    }

    fn with_max_count(mut self, max_count: usize) -> GenerateGraphOption {
        self.max_count = Some(max_count);
        self
    }

    fn with_head_col(mut self) -> GenerateGraphOption {
        self.reserve_head_col = true;
        self
    }

    /// 這個案例擁有的每一份 snapshot：每種欄寬各一份。
    fn keys(&self) -> impl Iterator<Item = SnapshotKey> {
        let name = self.output_name;
        WIDTHS
            .into_iter()
            .map(move |width| SnapshotKey { name, width })
    }
}

/// 單一案例的 graph，只建構一次（一次 `git::Repository::load` +
/// `calc_graph`），再拿去對兩種欄寬與三種風格分別渲染。若為了換字元就
/// 依 width/style 各自重建一次，會讓整個測試套件的 git 子行程數量成倍
/// 增加，卻毫無好處——graph 的拓撲結構完全不依賴 width 或 style，只有
/// `build_text_cells` 的折疊（width）與 `GlyphSet::resolve`（style）才會
/// 受影響。
struct GraphSnapshotSource {
    subjects: Vec<String>,
    double_rows: Vec<Vec<graph::TextCell>>,
    single_rows: Vec<Vec<graph::TextCell>>,
    /// 保留下來是為了讓跨欄寬的不變性檢查能夠逐欄判斷 `Double` 當初是
    /// 否被允許合併它。若從渲染後的 cell 反推，只會是把實作邏輯重講
    /// 一遍而已。
    edges: Vec<Vec<graph::Edge>>,
    colors: Vec<ratatui::style::Color>,
}

impl GraphSnapshotSource {
    fn rows(&self, width: graph::CellWidthType) -> &[Vec<graph::TextCell>] {
        match width {
            graph::CellWidthType::Double => &self.double_rows,
            graph::CellWidthType::Single => &self.single_rows,
        }
    }
}

fn build_graph_snapshot_source(
    repo_path: &Path,
    option: &GenerateGraphOption,
) -> GraphSnapshotSource {
    let repository = git::Repository::load(repo_path, option.sort, option.max_count).unwrap();

    // 只有 `reserve_head_col` 案例才會算出真正的 HEAD hint，其餘案例一律
    // 傳 `None`，跟 PNG 時代產生器當年的做法一致——見
    // `GenerateGraphOption::reserve_head_col` 欄位上的 doc comment。
    let head_hint: Option<git::CommitHash> = if option.reserve_head_col {
        let hash = GitRepository::new(repo_path).rev_parse_head();
        Some(git::CommitHash::from(hash.as_str()))
    } else {
        None
    };

    let graph = graph::calc_graph(&repository, head_hint.as_ref(), option.reserve_head_col);

    let graph_colors = color::GraphColors::default();
    let graph_color_set = color::GraphColorSet::new(&graph_colors);
    let colors = graph_color_set
        .colors
        .iter()
        .map(|c| c.to_ratatui_color())
        .collect::<Vec<_>>();

    let double_rows = graph::build_text_graph(&graph, &colors, graph::CellWidthType::Double);
    let single_rows = graph::build_text_graph(&graph, &colors, graph::CellWidthType::Single);
    let subjects = graph
        .commit_hashes
        .iter()
        .map(|h| repository.commit(h).unwrap().subject.clone())
        .collect();

    GraphSnapshotSource {
        subjects,
        double_rows,
        single_rows,
        edges: graph.edges,
        colors,
    }
}

/// 完全比照文字模式 graph 欄位的畫法渲染，透過 `graph::build_text_graph`
/// 與 `GlyphSet::resolve`，每個 commit 一行：`<graph glyphs>  <subject>`。
/// 不含 hash/date——原因見 #16 plan。
fn render_text_graph_styled(
    source: &GraphSnapshotSource,
    glyphs: graph::GlyphSet,
    width: graph::CellWidthType,
) -> String {
    let mut out = String::new();
    for (subject, cells) in source.subjects.iter().zip(source.rows(width)) {
        let graph_str: String = cells.iter().map(|c| glyphs.resolve(c.glyph)).collect();
        out.push_str(graph_str.trim_end());
        out.push_str("  ");
        out.push_str(subject);
        out.push('\n');
    }
    out
}

/// 一條邊失去它的半格後，是否會不留痕跡地消失。
/// 這裡刻意寫死列舉，而不是從 `graph` 推導出來，這樣 `halves` 裡的
/// 錯誤項目跟這裡的錯誤項目才不會互相抵消——跟 `ANGULAR_SUBST` 與
/// `halves_match_the_original_edge_type_table` 的道理一樣。
fn leaves_no_trace(edge_type: graph::EdgeType) -> bool {
    match edge_type {
        graph::EdgeType::Vertical
        | graph::EdgeType::Up
        | graph::EdgeType::Down
        | graph::EdgeType::Left
        | graph::EdgeType::RightTop
        | graph::EdgeType::RightBottom => true,
        graph::EdgeType::Horizontal
        | graph::EdgeType::Right
        | graph::EdgeType::LeftTop
        | graph::EdgeType::LeftBottom => false,
    }
}

fn is_junction(glyph: graph::Glyph) -> bool {
    matches!(
        glyph,
        graph::Glyph::TeeDown
            | graph::Glyph::TeeUp
            | graph::Glyph::TeeRight
            | graph::Glyph::TeeLeft
            | graph::Glyph::Cross
    )
}

/// 獨立於 renderer 之外，從原始的 edges 重新算出 `Column::can_merge`
/// 的兩個判斷依據。刻意保持兩個各自獨立的值，不縮減成單一 bool，
/// 是因為下面不變性檢查裡顏色的那一半，只在第一個依據成立時才站得
/// 住腳。
///
/// 少於兩條邊的欄位沒有東西可以互相牴觸，所以一律算作 uniform——
/// 合併它反正是個 no-op，判定為 true 只是讓它走一趟相等性檢查，
/// 而不是直接跳過。
fn column_merge_reasons(source: &GraphSnapshotSource, row: usize, col: usize) -> (bool, usize) {
    let in_col: Vec<_> = source.edges[row]
        .iter()
        .filter(|e| e.pos_x == col)
        .collect();
    let color_of = |e: &graph::Edge| source.colors[e.associated_line_pos_x % source.colors.len()];
    let uniform = in_col.windows(2).all(|w| color_of(w[0]) == color_of(w[1]));
    let traceless = in_col
        .iter()
        .filter(|e| leaves_no_trace(e.edge_type))
        .count();
    (uniform, traceless)
}

/// 兩種欄寬之間必須成立的關係，每個案例都會檢查。
///
/// 1. **只要 `Double` 合併了，`Single` 就要畫出一樣的 glyph。** 唯一
///    的例外是方向只往右的欄位：`Double` 會把 symbol 留空、把 `─`
///    畫在 connector 裡，而 `Single` 只有一格可以畫。
/// 2. **只要沒合併，`Double` 就完全不畫任何交會符號**——那一格只會
///    是單一條邊自己的 glyph，絕不會是組合出來的。
///
/// 顏色只在整欄都是同一個顏色時才比較。`Double` 的顏色是從贏者全拿
/// 的那一輪（`place`，同分時後寫入者勝出）取得，`Single` 則是從
/// `Column::symbol_color`（先寫入者勝出）取得，所以在 *traceless*
/// 規則下合併的欄位，兩邊顏色是可能合理地不一致的：兩條同等級但
/// 顏色不同的邊，會依相反的方向決出勝負。只有整欄都是同一顏色時，
/// 這兩種決勝規則之間才沒有分歧的空間。
///
/// 這兩條規則合在一起，把合併規則從雙向都釘死了：symbol 那半格出現
/// 交會符號，代表這一欄本來就可合併；一欄可合併，也代表 symbol 那半
/// 格會跟 `Single` 一致。不管是把規則反過來寫、乾脆拿掉，還是放寬成
/// 「只要有邊就合併」，在這裡都會測試失敗。（這裡取代掉的舊超集關係
/// 就辦不到這點：就算把合併那段邏輯整段刪掉，那條舊關係一樣會通過。）
///
/// 這裡比較的是 `TextCell`，不是渲染後的字元，所以完全不涉及
/// `GlyphSet`。一個聯集最終會解析成哪個交會字元，是由 `graph/text.rs`
/// 的單元測試釘住的，那些測試窮舉了全部 16 種方向組合。
fn assert_cross_width_invariants(option: &GenerateGraphOption, source: &GraphSnapshotSource) {
    let name = option.output_name;
    for (row, (double_row, single_row)) in source
        .double_rows
        .iter()
        .zip(&source.single_rows)
        .enumerate()
    {
        assert_eq!(
            double_row.len(),
            single_row.len() * 2,
            "{name}: row {row} single width isn't half of double width"
        );

        for (col, single_cell) in single_row.iter().enumerate() {
            let symbol = double_row[2 * col];
            let connector = double_row[2 * col + 1];

            let (uniform, traceless) = column_merge_reasons(source, row, col);
            if !(uniform || traceless >= 2) {
                assert!(
                    !is_junction(symbol.glyph),
                    "{name}: row {row} col {col} merged a column it shouldn't have ({:?})",
                    symbol.glyph
                );
                continue;
            }

            let lone_right_stub =
                symbol.glyph == graph::Glyph::Blank && connector.glyph != graph::Glyph::Blank;
            let expected = if lone_right_stub {
                (graph::Glyph::Horiz, connector.color)
            } else {
                (symbol.glyph, symbol.color)
            };
            assert_eq!(
                single_cell.glyph, expected.0,
                "{name}: row {row} col {col} merged column doesn't match single width"
            );
            if uniform {
                assert_eq!(
                    single_cell.color, expected.1,
                    "{name}: row {row} col {col} single-coloured column disagrees on colour"
                );
            }
        }
    }
}

/// 用來辨識單一份 snapshot：某個案例在某種欄寬下的渲染結果。每個案例
/// 兩種欄寬都會檢查，所以欄寬是 snapshot 身分的一部分，而不是案例
/// 本身的屬性。
#[derive(Clone, Copy)]
struct SnapshotKey {
    name: &'static str,
    width: graph::CellWidthType,
}

impl SnapshotKey {
    /// 每種欄寬的檔名後綴都只在這裡寫一次。
    fn width_suffix(self) -> &'static str {
        match self.width {
            graph::CellWidthType::Double => "",
            graph::CellWidthType::Single => "_single",
        }
    }

    fn golden(self) -> String {
        format!("{SNAPSHOT_DIR}/{}{}.txt", self.name, self.width_suffix())
    }

    fn actual(self, style_suffix: &str) -> String {
        format!(
            "{OUTPUT_DIR}/{}{}{style_suffix}.txt",
            self.name,
            self.width_suffix()
        )
    }
}

const WIDTHS: [graph::CellWidthType; 2] =
    [graph::CellWidthType::Double, graph::CellWidthType::Single];

const STYLES: [(&str, graph::GlyphSet); 3] = [
    ("", graph::GlyphSet::ROUNDED),
    ("_angular", graph::GlyphSet::ANGULAR),
    ("_ascii", graph::GlyphSet::ASCII),
];

fn generate_and_output_text_graphs(repo_path: &Path, options: &[GenerateGraphOption]) {
    create_output_dirs(OUTPUT_DIR);
    for option in options {
        let source = build_graph_snapshot_source(repo_path, option);
        assert_cross_width_invariants(option, &source);
        for key in option.keys() {
            for (style_suffix, glyphs) in STYLES {
                let content = render_text_graph_styled(&source, glyphs, key.width);
                std::fs::write(key.actual(style_suffix), content).unwrap();
            }
        }
    }
}

fn create_output_dirs(path: &str) {
    let path = Path::new(path);
    std::fs::create_dir_all(path).unwrap();
}

fn copy_git_dir(path: &Path, name: &str) {
    let dst_path = format!("{OUTPUT_DIR}/{name}");
    // dircpy 的覆蓋行為似乎不如預期，所以在這裡明確刪除
    if Path::new(&dst_path).is_dir() {
        std::fs::remove_dir_all(&dst_path).unwrap();
    }
    dircpy::CopyBuilder::new(path, dst_path).run().unwrap();
}

/// Angular 的四個轉角跟 rounded 是一對一對應。Ascii 則額外把四個轉角
/// 全部折疊成 `+`，並把畫線字元也換掉。這些是寫死的對照表，不是從
/// `GlyphSet::resolve`/`from_style` 推導出來的——用推導的話會讓下面
/// 的不變性檢查變成套套邏輯（對照表寫錯跟 dispatch 寫錯可能剛好互相
/// 抵消，還是會測試通過）。
///
/// 可以放心整段字串替換：全部 41 份 golden snapshot 裡的每個 subject
/// （125 個不重複的 subject）都已驗證是純 ASCII，而這裡的每個來源
/// 字元都是非 ASCII，所以這個替換絕不會誤傷 subject。如果未來的
/// fixture 新增了一個 subject 剛好含有這些字元之一，這個替換就會悄悄
/// 把它弄壞——真的發生的話，要重新驗證「subject 全是 ASCII」這個前提
/// 是否還成立。
const ANGULAR_SUBST: &[(char, char)] = &[('╭', '┌'), ('╮', '┐'), ('╰', '└'), ('╯', '┘')];
const ASCII_SUBST: &[(char, char)] = &[
    ('●', '*'),
    ('◯', 'o'),
    ('│', '|'),
    ('─', '-'),
    ('╭', '+'),
    ('╮', '+'),
    ('╰', '+'),
    ('╯', '+'),
    ('┬', '+'),
    ('┴', '+'),
    ('├', '+'),
    ('┤', '+'),
    ('┼', '+'),
];

fn substitute(input: &str, table: &[(char, char)]) -> String {
    input
        .chars()
        .map(|c| {
            table
                .iter()
                .find(|(from, _)| *from == c)
                .map_or(c, |(_, to)| *to)
        })
        .collect()
}

fn assert_text_graphs(options: &[GenerateGraphOption]) {
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        update_text_snapshots(options);
        // 故意讓它失敗：更新模式的執行不算是驗證，如果這裡也回綠燈，
        // UPDATE_SNAPSHOTS=1 就有可能不小心殘留在 shell 裡，混進正常的
        // `cargo test` 執行，被誤判為真的測試通過。要拿掉這個環境變數
        // 重新執行一次，才是真正的驗證。
        panic!(
            "UPDATE_SNAPSHOTS was set: wrote/updated {} snapshot(s) under {SNAPSHOT_DIR}. \
             This run intentionally fails so it can't be mistaken for a real pass — \
             re-run `cargo test --test graph` without UPDATE_SNAPSHOTS to verify.",
            options.len() * WIDTHS.len()
        );
    }

    let mut errors = Vec::new();
    for option in options {
        for key in option.keys() {
            match compare_text_snapshot(key) {
                Ok(()) => {
                    // 只有在 rounded golden 本身先比對成功後，才檢查風格的
                    // 不變性——不然一個真正的 regression 會同時炸出三個
                    // 重複的失敗，反而把真正的原因埋掉。
                    for (suffix, table) in [("_angular", ANGULAR_SUBST), ("_ascii", ASCII_SUBST)] {
                        if let Err(e) = compare_style_invariant(key, suffix, table) {
                            errors.push(e);
                        }
                    }
                }
                Err(e) => errors.push(e),
            }
        }
    }
    if !errors.is_empty() {
        panic!("{}", errors.join("\n"));
    }
}

fn update_text_snapshots(options: &[GenerateGraphOption]) {
    create_output_dirs(SNAPSHOT_DIR);
    for option in options {
        for key in option.keys() {
            let actual_file = key.actual("");
            let content = std::fs::read_to_string(&actual_file)
                .unwrap_or_else(|e| panic!("failed to read generated output {actual_file}: {e}"));
            std::fs::write(key.golden(), content).unwrap();
        }
    }
}

fn compare_text_snapshot(key: SnapshotKey) -> Result<(), String> {
    let snapshot_file = key.golden();
    let expected = std::fs::read_to_string(&snapshot_file).map_err(|_| {
        format!(
            "missing snapshot {snapshot_file} — run \
             `UPDATE_SNAPSHOTS=1 cargo test --test graph` to generate it"
        )
    })?;

    let actual_file = key.actual("");
    let actual = std::fs::read_to_string(&actual_file).unwrap();

    if actual == expected {
        return Ok(());
    }
    Err(snapshot_diff_message(
        &format!("text graph differs for {snapshot_file}"),
        &actual_file,
        &expected,
        &actual,
    ))
}

/// 檢查 `{name}{suffix}.txt`（由 `generate_and_output_text_graphs` 產生，
/// 不是 golden 檔）是否等於套用了 `table` 之後的 rounded golden。只有
/// 在這個案例的 `compare_text_snapshot` 已經先通過之後才會呼叫。
fn compare_style_invariant(
    key: SnapshotKey,
    suffix: &str,
    table: &[(char, char)],
) -> Result<(), String> {
    let golden = std::fs::read_to_string(key.golden()).unwrap();
    let expected = substitute(&golden, table);

    let actual_file = key.actual(suffix);
    let actual = std::fs::read_to_string(&actual_file).unwrap();

    if actual == expected {
        return Ok(());
    }
    Err(snapshot_diff_message(
        &format!(
            "{suffix} substitution invariant differs for {}",
            key.golden()
        ),
        &actual_file,
        &expected,
        &actual,
    ))
}

fn snapshot_diff_message(label: &str, actual_file: &str, expected: &str, actual: &str) -> String {
    let mut msg = format!("{label}: see {actual_file}");

    let expected_lines: Vec<&str> = expected.lines().collect();
    let actual_lines: Vec<&str> = actual.lines().collect();
    if expected_lines.len() != actual_lines.len() {
        msg.push_str(&format!(
            "\n  line count: expected {}, actual {}",
            expected_lines.len(),
            actual_lines.len()
        ));
    }

    let diff_preview: String = expected_lines
        .iter()
        .zip(actual_lines.iter())
        .enumerate()
        .filter(|(_, (e, a))| e != a)
        .take(5)
        .map(|(i, (e, a))| format!("\n  line {}: expected {e:?}, actual {a:?}", i + 1))
        .collect();
    msg.push_str(&diff_preview);

    msg
}

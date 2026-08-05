use std::{path::Path, process::Command};

use chrono::{DateTime, Days, NaiveDate, TimeZone, Utc};
use serie::{color, config, git, graph};

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
    // Test case for multiple stashes, the most recent commit is normal commit
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
    // Test case for multiple stashes, the most recent commit is stash
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
    // Test case for unreachable stash
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
    // Test case for multiple stashes for the same commit
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
    // Test case for detached HEAD
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

/// Test case for the HEAD column-reservation branch of `calc_commit_positions`
/// (`reserve_head_col`). This needs a purpose-built repo: the existing 19 repos
/// above all end with HEAD *being* the newest commit, which makes the reserved
/// column collapse onto column 0 either way (see calc.rs — `head_col_pending`
/// is cleared the moment HEAD's row is processed, so if HEAD is `pos_y == 0`
/// there's nothing left to reserve for). This repo instead leaves HEAD on a
/// childless branch tip that is *not* the newest commit, and with a named ref
/// (`head_has_named_ref` must be true) so the reservation actually engages.
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
    // Whether to pass the repo's actual HEAD to `calc_graph` and reserve a
    // column for it (`reserve_head_col`). When false (the default, used by
    // every case above except `reserve_col_001`), HEAD is treated as unknown
    // (`None`) — exactly matching the PNG-era snapshot generation, which
    // always called `calc_graph(&repository, None, false)` unconditionally.
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

    /// Every snapshot this case owns: one per cell width.
    fn keys(&self) -> impl Iterator<Item = SnapshotKey> {
        let name = self.output_name;
        WIDTHS
            .into_iter()
            .map(move |width| SnapshotKey { name, width })
    }
}

/// One case's graph, built once (a `git::Repository::load` + `calc_graph`)
/// and rendered against both cell widths and all three styles. Rebuilding
/// per width/style just to change which characters get printed would
/// multiply the number of git subprocess spawns across the whole suite for
/// zero benefit — the graph topology doesn't depend on width or style at
/// all, only `build_text_cells`'s folding (width) and `GlyphSet::resolve`
/// (style) do.
struct GraphSnapshotSource {
    subjects: Vec<String>,
    double_rows: Vec<Vec<graph::TextCell>>,
    single_rows: Vec<Vec<graph::TextCell>>,
    /// Kept so the cross-width invariants can work out, per column, whether
    /// `Double` was allowed to merge it. Deriving that from the rendered
    /// cells would just restate the implementation.
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

    // Only compute a real HEAD hint for `reserve_head_col` cases. Everyone
    // else passes `None`, matching what the PNG-era generator always did —
    // see the field doc-comment on `GenerateGraphOption::reserve_head_col`.
    let head_hint: Option<git::CommitHash> = if option.reserve_head_col {
        let hash = GitRepository::new(repo_path).rev_parse_head();
        Some(git::CommitHash::from(hash.as_str()))
    } else {
        None
    };

    let graph = graph::calc_graph(&repository, head_hint.as_ref(), option.reserve_head_col);

    let graph_color_config = config::GraphColorConfig::default();
    let graph_color_set = color::GraphColorSet::new(&graph_color_config);
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

/// Render exactly as the text-mode graph column does, via
/// `graph::build_text_graph` and `GlyphSet::resolve`, one line per commit:
/// `<graph glyphs>  <subject>`. No hash/date — see #16 plan for why.
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

/// Whether an edge that loses its half-cell disappears without a trace.
/// Spelled out literally rather than derived from `graph`, so a wrong entry
/// in `halves` and a wrong entry here can't cancel out -- same reasoning as
/// `ANGULAR_SUBST` and `halves_match_the_original_edge_type_table`.
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

/// Recomputes `Column::can_merge`'s two reasons from the raw edges,
/// independently of the renderer. Kept separate rather than reduced to one
/// bool because the colour half of the invariant below only holds under the
/// first of them.
///
/// A column with fewer than two edges has no pair to disagree, so it counts
/// as uniform -- merging it is a no-op either way, and saying yes puts it
/// through the equality check rather than skipping it.
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

/// What has to hold between the two widths, checked on every case.
///
/// 1. **Where `Double` merged, `Single` drew the same glyph.** The one
///    exception is a column whose only direction is rightward: `Double`
///    leaves the symbol blank and puts the `─` in the connector, while
///    `Single` has just the one cell to draw it in.
/// 2. **Where it didn't merge, `Double` drew no junction at all** -- the
///    cell holds one edge's own glyph, never a combination.
///
/// The colour is only compared for columns that are entirely one colour.
/// `Double` takes every colour from the winner-takes-all pass (`place`, last
/// writer wins on a tie) while `Single` takes it from `Column::symbol_color`
/// (first writer wins), so a column merged under the *traceless* rule can
/// legitimately disagree: two equal-rank edges of different colours resolve
/// opposite ways. Where the whole column is one colour there is nothing for
/// the two tie-breaks to disagree about.
///
/// Together these pin the merge rule in both directions: a junction in the
/// symbol half implies the column was mergeable, and a mergeable column
/// implies the symbol half agrees with `Single`. Writing the rule backwards,
/// dropping it, or widening it to "merge whenever there are edges" all fail
/// here. (The superset relation this replaced did not: it held just as well
/// with the merge pass deleted entirely.)
///
/// Compares `TextCell` rather than rendered characters, so no `GlyphSet` is
/// involved. Which junction character a union resolves to is pinned by
/// `graph/text.rs`'s unit tests, exhaustive over all 16 direction
/// combinations.
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

/// Identifies one snapshot: a case rendered at one cell width. Both widths
/// are checked in for every case, so the width is part of a snapshot's
/// identity rather than a property of the case.
#[derive(Clone, Copy)]
struct SnapshotKey {
    name: &'static str,
    width: graph::CellWidthType,
}

impl SnapshotKey {
    /// Every width's suffix is spelled once, here.
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
    // dircpy overwrite doesn't seem to work as expected, so delete explicitly
    if Path::new(&dst_path).is_dir() {
        std::fs::remove_dir_all(&dst_path).unwrap();
    }
    dircpy::CopyBuilder::new(path, dst_path).run().unwrap();
}

/// Angular's four corners map 1:1 to rounded's. Ascii additionally folds all
/// four corners onto `+` and switches the line-drawing characters. These are
/// literal tables, not derived from `GlyphSet::resolve`/`from_style` —
/// deriving them would make the invariant below a tautology (a wrong table
/// entry and a wrong dispatch could cancel out and still pass).
///
/// Safe to apply as a whole-string replace: every subject across all 41
/// golden snapshots (125 distinct subjects) is verified pure ASCII, and
/// every source character here is non-ASCII, so this can never mangle a
/// subject. If a future fixture adds a subject containing one of these
/// characters, this substitution would silently corrupt it — re-verify the
/// all-ASCII-subjects premise if that ever happens.
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
        // Fail on purpose: an update run isn't a verification run, and a
        // green run here would let UPDATE_SNAPSHOTS=1 slip into a normal
        // `cargo test` invocation (e.g. left set in a shell) and be mistaken
        // for a real pass. Re-run without the env var to actually verify.
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
                    // Only check style invariants once the rounded golden itself
                    // matches -- otherwise a single real regression shows up as
                    // three redundant failures for the same case, burying the
                    // actual cause.
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
        &format!("text graph differs for {}", snapshot_file),
        &actual_file,
        &expected,
        &actual,
    ))
}

/// Checks that `{name}{suffix}.txt` (generated by `generate_and_output_text_graphs`,
/// not a golden file) equals the rounded golden with `table` applied. Only
/// called after `compare_text_snapshot` already passed for this case.
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

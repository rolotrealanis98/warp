use std::collections::BTreeMap;
use std::path::Path;

use tempfile::TempDir;

use super::{Stack, StackFile, infer_stack};

fn git(repo: &Path, args: &[&str]) -> String {
    let output = command::blocking::Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("failed to run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn commit(repo: &Path, message: &str) {
    git(repo, &["commit", "--allow-empty", "-m", message]);
}

fn branch(repo: &Path, name: &str, from: &str) {
    git(repo, &["switch", "-c", name, from]);
}

/// main <- a <- b <- c (checked out), plus `other` off main and `old`,
/// which is already part of main.
fn linear_repo() -> TempDir {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let repo = dir.path();
    git(repo, &["init", "-b", "main"]);
    git(repo, &["config", "user.email", "test@example.com"]);
    git(repo, &["config", "user.name", "Test"]);
    commit(repo, "initial");
    git(repo, &["branch", "old"]);
    commit(repo, "main 2");
    branch(repo, "other", "main");
    commit(repo, "other 1");
    branch(repo, "a", "main");
    commit(repo, "a 1");
    branch(repo, "b", "a");
    commit(repo, "b 1");
    branch(repo, "c", "b");
    commit(repo, "c 1");
    dir
}

/// (branch, parent, ambiguous) bottom to top.
fn shape(stack: &Stack) -> Vec<(&str, &str, bool)> {
    stack
        .branches
        .iter()
        .map(|b| (b.name.as_str(), b.parent.as_str(), b.ambiguous))
        .collect()
}

#[tokio::test]
async fn infers_linear_stack_and_skips_unrelated_and_merged_branches() {
    let dir = linear_repo();

    let stack = infer_stack(dir.path(), "c", "main", &StackFile::default())
        .await
        .unwrap();

    assert_eq!(
        shape(&stack),
        vec![("a", "main", false), ("b", "a", false), ("c", "b", false)]
    );
}

#[tokio::test]
async fn ambiguous_parent_prefers_longest_chain() {
    let dir = linear_repo();
    let repo = dir.path();
    branch(repo, "x", "main");
    commit(repo, "x 1");
    branch(repo, "top", "b");
    git(repo, &["merge", "--no-ff", "-m", "merge x", "x"]);

    let stack = infer_stack(repo, "top", "main", &StackFile::default())
        .await
        .unwrap();

    assert_eq!(
        shape(&stack),
        vec![("a", "main", false), ("b", "a", false), ("top", "b", true)]
    );
    assert_eq!(stack.branches[2].alternatives, vec!["x".to_string()]);
}

#[tokio::test]
async fn branches_sharing_a_tip_are_ambiguous_and_skipped_below() {
    let dir = linear_repo();
    let repo = dir.path();
    git(repo, &["branch", "b2", "b"]);

    let stack = infer_stack(repo, "c", "main", &StackFile::default())
        .await
        .unwrap();

    assert_eq!(
        shape(&stack),
        vec![("a", "main", false), ("b", "a", false), ("c", "b", true)]
    );
    assert_eq!(stack.branches[2].alternatives, vec!["b2".to_string()]);
}

#[tokio::test]
async fn fresh_top_branch_stacks_on_the_branch_it_was_created_from() {
    let dir = linear_repo();
    let repo = dir.path();
    branch(repo, "d", "c");

    let stack = infer_stack(repo, "d", "main", &StackFile::default())
        .await
        .unwrap();

    assert_eq!(
        shape(&stack),
        vec![
            ("a", "main", false),
            ("b", "a", false),
            ("c", "b", false),
            ("d", "c", false)
        ]
    );
}

#[tokio::test]
async fn pin_overrides_inferred_parent() {
    let dir = linear_repo();
    let file = StackFile {
        pins: BTreeMap::from([("c".to_string(), "a".to_string())]),
        ..Default::default()
    };

    let stack = infer_stack(dir.path(), "c", "main", &file).await.unwrap();

    assert_eq!(shape(&stack), vec![("a", "main", false), ("c", "a", false)]);
    assert!(stack.branches[1].pinned);
}

#[tokio::test]
async fn recorded_parent_keeps_a_moved_parent_in_the_stack() {
    let dir = linear_repo();
    let repo = dir.path();
    let mut file = StackFile::default();
    let before = infer_stack(repo, "c", "main", &file).await.unwrap();
    file.record(&before);
    git(repo, &["switch", "a"]);
    commit(repo, "a 2");

    let stack = infer_stack(repo, "c", "main", &file).await.unwrap();

    let behind: Vec<(&str, bool)> = stack
        .branches
        .iter()
        .map(|b| (b.name.as_str(), b.behind))
        .collect();
    assert_eq!(behind, vec![("a", false), ("b", true), ("c", false)]);
}

#[tokio::test]
async fn without_a_record_a_moved_parent_drops_out() {
    let dir = linear_repo();
    let repo = dir.path();
    git(repo, &["switch", "a"]);
    commit(repo, "a 2");

    let stack = infer_stack(repo, "c", "main", &StackFile::default())
        .await
        .unwrap();

    assert_eq!(shape(&stack), vec![("b", "main", false), ("c", "b", false)]);
}

#[tokio::test]
async fn inserted_branch_wins_over_recorded_parent() {
    let dir = linear_repo();
    let repo = dir.path();
    let mut file = StackFile::default();
    let before = infer_stack(repo, "c", "main", &file).await.unwrap();
    file.record(&before);
    branch(repo, "n", "a");
    commit(repo, "n 1");
    git(repo, &["rebase", "--onto", "n", "a", "b"]);
    git(repo, &["rebase", "--onto", "b", "b@{1}", "c"]);

    let stack = infer_stack(repo, "c", "main", &file).await.unwrap();

    assert_eq!(
        shape(&stack),
        vec![
            ("a", "main", false),
            ("n", "a", false),
            ("b", "n", false),
            ("c", "b", false)
        ]
    );
}

#[tokio::test]
async fn top_already_in_target_has_no_stack() {
    let dir = linear_repo();
    let repo = dir.path();
    git(repo, &["branch", "done", "main"]);

    let stack = infer_stack(repo, "done", "main", &StackFile::default())
        .await
        .unwrap();

    assert_eq!(shape(&stack), vec![]);
}

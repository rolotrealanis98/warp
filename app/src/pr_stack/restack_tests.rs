use std::collections::HashMap;
use std::path::Path;

use tempfile::TempDir;

use super::{RestackOutcome, restack};
use crate::pr_stack::stack::{Stack, infer_stack, load_stack_file, save_stack_file};
use crate::pr_stack::status::PrStatus;

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

fn commit_file(repo: &Path, file: &str, contents: &str, message: &str) {
    std::fs::write(repo.join(file), contents).unwrap();
    git(repo, &["add", file]);
    git(repo, &["commit", "-m", message]);
}

/// main <- a <- b <- c, each branch adding its own file; `c` checked out.
fn stacked_repo() -> TempDir {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let repo = dir.path();
    git(repo, &["init", "-b", "main"]);
    // The restack runs git with the user's global config; keep it hermetic.
    git(repo, &["config", "user.email", "test@example.com"]);
    git(repo, &["config", "user.name", "Test"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
    commit_file(repo, "base.txt", "base\n", "initial");
    git(repo, &["switch", "-c", "a"]);
    commit_file(repo, "a.txt", "a\n", "a 1");
    git(repo, &["switch", "-c", "b"]);
    commit_file(repo, "b.txt", "b\n", "b 1");
    git(repo, &["switch", "-c", "c"]);
    commit_file(repo, "c.txt", "c\n", "c 1");
    dir
}

/// Infers the stack the way the panel does: with the recorded parents, and
/// recording the result.
async fn infer(repo: &Path, top: &str) -> Stack {
    let mut file = load_stack_file(repo).await.unwrap();
    let stack = infer_stack(repo, top, "main", &file).await.unwrap();
    file.record(&stack);
    save_stack_file(repo, &file).await.unwrap();
    stack
}

fn subjects(repo: &Path, range: &str) -> Vec<String> {
    git(repo, &["log", "--format=%s", range])
        .lines()
        .map(str::to_string)
        .collect()
}

#[tokio::test]
async fn restack_moves_branches_onto_their_moved_parent() {
    let dir = stacked_repo();
    let repo = dir.path();
    infer(repo, "c").await;
    git(repo, &["switch", "a"]);
    commit_file(repo, "a2.txt", "a2\n", "a 2");
    let stack = infer(repo, "c").await;

    let outcome = restack(repo, &stack, 0, false, &HashMap::new(), None)
        .await
        .unwrap();

    assert!(matches!(outcome, RestackOutcome::Done { .. }));
    assert_eq!(subjects(repo, "main..c"), vec!["c 1", "b 1", "a 2", "a 1"]);
    assert_eq!(git(repo, &["branch", "--show-current"]), "a");
    let after = infer(repo, "c").await;
    assert!(after.branches.iter().all(|b| !b.behind));
}

#[tokio::test]
async fn conflict_stops_and_keeps_the_stack_shape() {
    let dir = stacked_repo();
    let repo = dir.path();
    infer(repo, "c").await;
    git(repo, &["switch", "b"]);
    commit_file(repo, "a.txt", "changed on b\n", "b 2");
    git(repo, &["switch", "a"]);
    commit_file(repo, "a.txt", "changed on a\n", "a 2");
    let stack = infer(repo, "c").await;

    let outcome = restack(repo, &stack, 0, false, &HashMap::new(), None)
        .await
        .unwrap();

    let RestackOutcome::Conflict {
        branch,
        onto,
        remaining,
        ..
    } = outcome
    else {
        panic!("expected a conflict, got {outcome:?}");
    };
    assert_eq!((branch.as_str(), onto.as_str()), ("b", "a"));
    assert_eq!(remaining, vec!["c".to_string()]);
    assert!(load_stack_file(repo).await.unwrap().restacking);
    git(repo, &["rebase", "--abort"]);
}

#[tokio::test]
async fn merged_bottom_branch_is_dropped() {
    let dir = stacked_repo();
    let repo = dir.path();
    let before = infer(repo, "b").await;
    assert_eq!(before.branches.len(), 2);
    git(repo, &["switch", "main"]);
    git(repo, &["merge", "--squash", "a"]);
    git(repo, &["commit", "-m", "a (squashed)"]);
    let stack = infer(repo, "b").await;
    let prs = HashMap::from([(
        "a".to_string(),
        PrStatus {
            number: 1,
            url: "https://github.com/octo/repo/pull/1".to_string(),
            state: "MERGED".to_string(),
            base: "main".to_string(),
            review: None,
            checks: None,
        },
    )]);

    restack(repo, &stack, 0, false, &prs, None).await.unwrap();

    assert_eq!(subjects(repo, "main..b"), vec!["b 1"]);
    let after = infer(repo, "b").await;
    let shape: Vec<(&str, &str)> = after
        .branches
        .iter()
        .map(|b| (b.name.as_str(), b.parent.as_str()))
        .collect();
    assert_eq!(shape, vec![("b", "main")]);
}

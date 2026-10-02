use std::collections::HashMap;
use std::path::Path;

use super::{
    CommitMessage, body_file_path, fallback_body, fallback_title, parse_body_file,
    render_stack_footer, replace_stack_footer,
};
use crate::pr_stack::stack::{Stack, StackBranch};
use crate::pr_stack::status::PrStatus;

fn commit(subject: &str, body: &str) -> CommitMessage {
    CommitMessage {
        subject: subject.to_string(),
        body: body.to_string(),
    }
}

fn stack_branch(name: &str, parent: &str) -> StackBranch {
    StackBranch {
        name: name.to_string(),
        parent: parent.to_string(),
        tip: String::new(),
        ambiguous: false,
        alternatives: Vec::new(),
        pinned: false,
        behind: false,
    }
}

fn open_pr(number: u64, base: &str) -> PrStatus {
    PrStatus {
        number,
        url: format!("https://github.com/octo/repo/pull/{number}"),
        state: "OPEN".to_string(),
        base: base.to_string(),
        review: None,
        checks: None,
    }
}

#[test]
fn body_file_under_dot_git_resolves_to_the_common_dir() {
    let path = body_file_path(
        ".git/warp-pr/{{branch}}.md",
        "feat/login",
        Path::new("/work/repo-wt"),
        Path::new("/work/repo/.git"),
    );

    assert_eq!(path, Path::new("/work/repo/.git/warp-pr/feat/login.md"));
}

#[test]
fn relative_body_file_resolves_to_the_worktree() {
    let path = body_file_path(
        "notes/{{branch}}.md",
        "login",
        Path::new("/work/repo-wt"),
        Path::new("/work/repo/.git"),
    );

    assert_eq!(path, Path::new("/work/repo-wt/notes/login.md"));
}

#[test]
fn body_file_first_line_is_the_title() {
    let parsed = parse_body_file("\n# EXAMPLE-123 Add login form\n\nAdds the form.\n\n- tests\n");

    assert_eq!(
        parsed,
        Some((
            "EXAMPLE-123 Add login form".to_string(),
            "Adds the form.\n\n- tests".to_string()
        ))
    );
}

#[test]
fn empty_body_file_is_ignored() {
    assert_eq!(parse_body_file("\n  \n"), None);
}

#[test]
fn fallback_title_uses_issue_key_from_branch_name() {
    assert_eq!(
        fallback_title("EXAMPLE-123-add-login-form", &[]),
        "EXAMPLE-123 Add login form"
    );
    assert_eq!(fallback_title("user/EXAMPLE-7_fix", &[]), "EXAMPLE-7 Fix");
}

#[test]
fn fallback_title_with_bare_key_appends_first_commit_subject() {
    assert_eq!(
        fallback_title("EXAMPLE-9", &[commit("Tidy imports", "")]),
        "EXAMPLE-9 Tidy imports"
    );
}

#[test]
fn fallback_title_without_key_uses_oldest_commit_then_branch() {
    let commits = [commit("Add parser", ""), commit("Fix parser", "")];

    assert_eq!(fallback_title("feature-parser", &commits), "Add parser");
    assert_eq!(fallback_title("feature-parser", &[]), "feature-parser");
}

#[test]
fn fallback_body_lists_commits_with_indented_bodies() {
    let commits = [
        commit("Add parser", "Handles nested lists.\n\nAnd quotes."),
        commit("Fix parser", ""),
    ];

    assert_eq!(
        fallback_body(&commits),
        "- Add parser\n  Handles nested lists.\n\n  And quotes.\n- Fix parser"
    );
}

#[test]
fn stack_footer_lists_branches_bottom_to_top() {
    let stack = Stack {
        target: "origin/main".to_string(),
        target_tip: String::new(),
        branches: vec![
            stack_branch("a", "origin/main"),
            stack_branch("b", "a"),
            stack_branch("c", "b"),
        ],
    };
    let prs = HashMap::from([
        ("a".to_string(), open_pr(11, "main")),
        ("b".to_string(), open_pr(12, "a")),
    ]);

    assert_eq!(
        render_stack_footer(&stack, &prs, "b"),
        "<!-- warp-stack -->\n\
         **Stack 2/3** (bottom to top, onto `main`)\n\
         1. #11 `a`\n\
         2. **#12 `b`** (this PR)\n\
         3. `c` (no PR yet)\n\
         <!-- /warp-stack -->"
    );
}

#[test]
fn replacing_the_footer_keeps_the_rest_of_the_body() {
    let body = "Intro\n\n<!-- warp-stack -->\nold\n<!-- /warp-stack -->\n\nOutro";

    assert_eq!(
        replace_stack_footer(body, "<!-- warp-stack -->\nnew\n<!-- /warp-stack -->"),
        "Intro\n\n<!-- warp-stack -->\nnew\n<!-- /warp-stack -->\n\nOutro"
    );
}

#[test]
fn footer_is_appended_when_missing() {
    assert_eq!(
        replace_stack_footer("Intro\n", "<!-- warp-stack -->\n<!-- /warp-stack -->"),
        "Intro\n\n<!-- warp-stack -->\n<!-- /warp-stack -->"
    );
}

use std::path::{Path, PathBuf};

use super::settings::{DEFAULT_REVIEW_OTHER_TEMPLATE, DEFAULT_WATCH_OWN_TEMPLATE};
use super::*;
use crate::terminal::CLIAgent;

fn details() -> PrDetails {
    PrDetails {
        pr: PrRef {
            owner: "octo".to_string(),
            repo: "repo".to_string(),
            number: 12,
        },
        title: "Fix login redirect".to_string(),
        url: "https://github.com/octo/repo/pull/12".to_string(),
        author: "alice".to_string(),
        base: "main".to_string(),
        state: "OPEN".to_string(),
    }
}

fn config() -> TaskAgentConfig {
    TaskAgentConfig {
        branch_template: "{{type}}/{{key}}-{{slug}}".to_string(),
        worktree_path_template: "{{repo_parent}}/{{repo}}.worktrees/{{key}}-{{slug}}".to_string(),
        push_on_create: false,
        fetch_before_branch: true,
        setup_commands: vec!["make deps".to_string()],
        session_title_template: "{{key}} {{short_title}}".to_string(),
        short_title_max_chars: 32,
        prompt_dir: PathBuf::from("/tmp/prompts"),
    }
}

fn draft() -> TaskAgentRequest {
    TaskAgentRequest::with_cli(PathBuf::from("/src/octo/repo"), CLIAgent::Claude)
}

#[test]
fn details_parse_reads_gh_pr_view_json() {
    let json = r#"{"title": "Fix login redirect", "url": "https://github.com/octo/repo/pull/12",
        "author": {"login": "alice", "is_bot": false}, "baseRefName": "main", "state": "OPEN"}"#;

    assert_eq!(PrDetails::parse(details().pr, json).unwrap(), details());
}

#[test]
fn details_gh_args_view_the_pull_request_in_its_repository() {
    assert_eq!(
        PrDetails::gh_args(&details().pr),
        vec![
            "pr",
            "view",
            "12",
            "-R",
            "octo/repo",
            "--json",
            "title,url,author,baseRefName,state"
        ]
    );
}

#[test]
fn review_other_template_renders_pull_request_and_checkout() {
    let prompt = render_pr_prompt(
        DEFAULT_REVIEW_OTHER_TEMPLATE,
        &details(),
        Path::new("/src/octo/repo.worktrees/PR-12-fix-login-redirect"),
        "review/PR-12-fix-login-redirect",
    );

    assert_eq!(
        prompt,
        "Review pull request #12 in octo/repo: Fix login redirect\n\
         https://github.com/octo/repo/pull/12\n\n\
         The pull request (by alice, into main) is checked out in \
         /src/octo/repo.worktrees/PR-12-fix-login-redirect. Read the change against main and \
         make sure you understand it before giving feedback.\n\n\
         Warp may send you updates about this pull request: new commits, review comments, \
         reviews and check results. Read and understand them, but do not act on them unless \
         asked."
    );
}

#[test]
fn watch_own_template_mentions_the_checkout() {
    let prompt = render_pr_prompt(
        DEFAULT_WATCH_OWN_TEMPLATE,
        &details(),
        Path::new("/src/octo/repo"),
        "",
    );

    assert!(prompt.starts_with("Watch my pull request #12 in octo/repo: Fix login redirect\n"));
    assert!(prompt.contains("It is checked out in /src/octo/repo (base main)."));
}

#[test]
fn custom_template_drops_paragraphs_of_empty_variables() {
    let prompt = render_pr_prompt(
        "PR {{number}} on {{branch}}\n\n{{branch}}\n\nDone",
        &details(),
        Path::new("/src/octo/repo"),
        "",
    );

    assert_eq!(prompt, "PR 12 on \n\nDone");
}

#[test]
fn checkout_command_resets_a_named_branch_to_the_pull_request() {
    assert_eq!(
        checkout_command(&details().pr, Some("review/PR-12 fix")),
        "gh pr checkout 12 -R octo/repo -b 'review/PR-12 fix' --force"
    );
    assert_eq!(
        checkout_command(&details().pr, None),
        "gh pr checkout 12 -R octo/repo"
    );
}

#[test]
fn worktree_request_branches_from_base_then_checks_out_the_pull_request() {
    let request = pr_request(&details(), CheckoutMode::Worktree, draft(), &config());

    assert_eq!(request.key.as_deref(), Some("PR-12"));
    assert_eq!(
        request.branch.as_deref(),
        Some("review/PR-12-fix-login-redirect")
    );
    assert_eq!(
        plan_launch(&request, &config()).commands,
        vec![
            "git -C /src/octo/repo fetch origin || true",
            "git -C /src/octo/repo worktree add -b review/PR-12-fix-login-redirect \
             /src/octo/repo.worktrees/PR-12-fix-login-redirect origin/main",
            "cd /src/octo/repo.worktrees/PR-12-fix-login-redirect",
            "gh pr checkout 12 -R octo/repo -b review/PR-12-fix-login-redirect --force",
            "make deps",
            "claude",
        ]
    );
}

#[test]
fn here_request_checks_out_the_pull_request_in_place() {
    let request = pr_request(&details(), CheckoutMode::Here, draft(), &config());

    assert_eq!(request.branch, None);
    assert_eq!(
        plan_launch(&request, &config()).commands,
        vec!["gh pr checkout 12 -R octo/repo", "claude"]
    );
}

#[test]
fn prompt_kind_watches_own_pull_requests() {
    assert_eq!(
        PromptKind::for_author("Alice", Some("alice")),
        PromptKind::WatchOwn
    );
    assert_eq!(
        PromptKind::for_author("alice", Some("bob")),
        PromptKind::ReviewOther
    );
    assert_eq!(
        PromptKind::for_author("alice", None),
        PromptKind::ReviewOther
    );
}

#[test]
fn checkout_mode_round_trips_through_the_setting() {
    for mode in CheckoutMode::ALL {
        assert_eq!(CheckoutMode::from_setting(mode.setting_value()), mode);
    }
    assert_eq!(CheckoutMode::from_setting(" Branch "), CheckoutMode::Branch);
    assert_eq!(CheckoutMode::from_setting("bogus"), CheckoutMode::Worktree);
}

#[test]
fn tab_title_names_the_pull_request() {
    assert_eq!(tab_title(&details(), 32), "PR #12 Fix login redirect");
}

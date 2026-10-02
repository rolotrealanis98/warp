use std::collections::HashMap;
use std::path::PathBuf;

use super::settings::{
    DEFAULT_BRANCH_TEMPLATE, DEFAULT_PROMPT_TEMPLATE, DEFAULT_SESSION_TITLE_TEMPLATE,
    DEFAULT_WORKTREE_PATH_TEMPLATE,
};
use super::*;

fn config() -> TaskAgentConfig {
    TaskAgentConfig {
        branch_template: DEFAULT_BRANCH_TEMPLATE.to_string(),
        worktree_path_template: DEFAULT_WORKTREE_PATH_TEMPLATE.to_string(),
        push_on_create: true,
        fetch_before_branch: true,
        setup_commands: vec!["make deps".to_string()],
        session_title_template: DEFAULT_SESSION_TITLE_TEMPLATE.to_string(),
        short_title_max_chars: 32,
        prompt_dir: PathBuf::from("/tmp/prompts"),
    }
}

fn request(checkout: Checkout) -> TaskAgentRequest {
    TaskAgentRequest {
        title: "Fix login redirect".to_string(),
        key: Some("EXAMPLE-123".to_string()),
        checkout,
        prompt: "Do the task".to_string(),
        ..TaskAgentRequest::with_cli(PathBuf::from("/src/octo/repo"), CLIAgent::Claude)
    }
}

#[test]
fn slug_lowercases_and_collapses_non_alphanumerics() {
    assert_eq!(
        slug("  Fix: the Login -- redirect!! "),
        "fix-the-login-redirect"
    );
}

#[test]
fn slug_replaces_non_ascii_letters() {
    assert_eq!(slug("Café crème"), "caf-cr-me");
}

#[test]
fn slug_is_capped_without_trailing_dash() {
    let long = "a".repeat(39) + " bbbbbbbb";
    assert_eq!(slug(&long), "a".repeat(39));
}

#[test]
fn short_title_keeps_short_titles() {
    assert_eq!(short_title("Fix login redirect", 32), "Fix login redirect");
}

#[test]
fn short_title_cuts_at_word_boundary_with_ellipsis() {
    assert_eq!(
        short_title("Fix the login redirect loop on expired sessions", 20),
        "Fix the login…"
    );
}

#[test]
fn short_title_cuts_a_single_long_word() {
    assert_eq!(short_title("Supercalifragilistic", 10), "Supercali…");
}

#[test]
fn short_title_with_zero_max_is_empty() {
    assert_eq!(short_title("Anything", 0), "");
}

#[test]
fn branch_template_renders_type_key_and_slug() {
    let request = request(Checkout::Here);
    let vars = template_vars(&request, 32);

    assert_eq!(
        resolve_branch(&request, &config(), &vars),
        "feat/EXAMPLE-123-fix-login-redirect"
    );
}

#[test]
fn branch_template_drops_dashes_left_by_missing_key() {
    let request = TaskAgentRequest {
        key: None,
        branch_type: Some("fix".to_string()),
        ..request(Checkout::Here)
    };
    let vars = template_vars(&request, 32);

    assert_eq!(
        resolve_branch(&request, &config(), &vars),
        "fix/fix-login-redirect"
    );
}

#[test]
fn explicit_branch_wins_over_template() {
    let request = TaskAgentRequest {
        branch: Some(" my-branch ".to_string()),
        ..request(Checkout::Here)
    };
    let vars = template_vars(&request, 32);

    assert_eq!(resolve_branch(&request, &config(), &vars), "my-branch");
}

#[test]
fn prompt_template_drops_empty_paragraphs() {
    let request = TaskAgentRequest {
        body: None,
        url: Some("https://example.atlassian.net/browse/EXAMPLE-123".to_string()),
        ..request(Checkout::Here)
    };

    assert_eq!(
        render_prompt(DEFAULT_PROMPT_TEMPLATE, &request, "feat/x"),
        "Work on this task: EXAMPLE-123 Fix login redirect\n\n\
         https://example.atlassian.net/browse/EXAMPLE-123"
    );
}

#[test]
fn session_title_renders_key_and_short_title() {
    let vars = template_vars(
        &TaskAgentRequest {
            title: "Fix the login redirect loop on expired sessions".to_string(),
            ..request(Checkout::Here)
        },
        20,
    );

    assert_eq!(
        session_title(DEFAULT_SESSION_TITLE_TEMPLATE, &vars),
        "EXAMPLE-123 Fix the login…"
    );
}

#[test]
fn session_title_without_key_has_no_leading_space() {
    let vars = template_vars(
        &TaskAgentRequest {
            key: None,
            ..request(Checkout::Here)
        },
        32,
    );

    assert_eq!(
        session_title(DEFAULT_SESSION_TITLE_TEMPLATE, &vars),
        "Fix login redirect"
    );
}

#[test]
fn worktree_plan_fetches_creates_worktree_pushes_sets_up_and_starts_agent() {
    let plan = plan_launch(
        &request(Checkout::Worktree {
            base: "origin/main".to_string(),
        }),
        &config(),
    );

    assert_eq!(
        plan.commands,
        vec![
            "git -C /src/octo/repo fetch origin || true",
            "git -C /src/octo/repo worktree add -b feat/EXAMPLE-123-fix-login-redirect \
             /src/octo/repo.worktrees/EXAMPLE-123-fix-login-redirect origin/main",
            "cd /src/octo/repo.worktrees/EXAMPLE-123-fix-login-redirect",
            "git push -u origin feat/EXAMPLE-123-fix-login-redirect || true",
            "make deps",
            "claude \"$(cat /tmp/prompts/EXAMPLE-123-fix-login-redirect.md)\"",
        ]
    );
    assert_eq!(plan.cwd, PathBuf::from("/src/octo/repo"));
    assert_eq!(
        plan.worktree_path,
        Some(PathBuf::from(
            "/src/octo/repo.worktrees/EXAMPLE-123-fix-login-redirect"
        ))
    );
    assert_eq!(
        plan.prompt_file,
        Some((
            PathBuf::from("/tmp/prompts/EXAMPLE-123-fix-login-redirect.md"),
            "Do the task".to_string()
        ))
    );
    assert_eq!(plan.tab_title, "EXAMPLE-123 Fix login redirect");
}

#[test]
fn branch_plan_switches_in_place_without_fetch_push_or_setup_when_disabled() {
    let config = TaskAgentConfig {
        push_on_create: false,
        fetch_before_branch: false,
        ..config()
    };
    let request = TaskAgentRequest {
        run_setup: false,
        prompt: String::new(),
        ..request(Checkout::Branch {
            base: String::new(),
        })
    };

    let plan = plan_launch(&request, &config);

    assert_eq!(
        plan.commands,
        vec![
            "git switch -c feat/EXAMPLE-123-fix-login-redirect",
            "claude"
        ]
    );
    assert_eq!(plan.worktree_path, None);
    assert_eq!(plan.prompt_file, None);
}

#[test]
fn here_plan_only_starts_the_agent() {
    let plan = plan_launch(&request(Checkout::Here), &config());

    assert_eq!(
        plan.commands,
        vec!["claude \"$(cat /tmp/prompts/EXAMPLE-123-fix-login-redirect.md)\""]
    );
    assert_eq!(plan.branch, None);
    assert_eq!(plan.session.checkout, Checkout::Here);
}

#[test]
fn agent_without_prompt_argument_gets_prompt_after_start() {
    let request = TaskAgentRequest {
        cli: CLIAgent::Gemini,
        ..request(Checkout::Here)
    };

    let plan = plan_launch(&request, &config());

    assert_eq!(plan.commands, vec!["gemini"]);
    assert_eq!(plan.fallback_prompt, Some("Do the task".to_string()));
    assert_eq!(plan.prompt_file, None);
}

#[test]
fn paths_and_branches_with_spaces_are_shell_quoted() {
    let request = TaskAgentRequest {
        branch: Some("feat/a b".to_string()),
        repo_root: PathBuf::from("/my repos/repo"),
        ..request(Checkout::Branch {
            base: "main".to_string(),
        })
    };
    let config = TaskAgentConfig {
        fetch_before_branch: false,
        push_on_create: false,
        setup_commands: Vec::new(),
        ..config()
    };

    let plan = plan_launch(&request, &config);

    assert_eq!(plan.commands[0], "git switch -c 'feat/a b' main");
}

#[test]
fn template_vars_expose_repo_name_and_parent() {
    let vars: HashMap<String, String> = template_vars(&request(Checkout::Here), 32);

    assert_eq!(vars["repo"], "repo");
    assert_eq!(vars["repo_parent"], "/src/octo");
}

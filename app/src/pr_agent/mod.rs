//! PR review agent: check a pull request out (through the task agent launcher), start a CLI
//! agent on it with a prompt from a user-editable template, watch the pull request for new
//! commits, reviews, comments and check results, and mirror the agent's own review comments into
//! the Code Review panel.
//!
//! Warp is plumbing only: what the agent does with the pull request is up to the prompt
//! templates (`pr_agent.*` settings). Gated by `FeatureFlag::PrReviewAgent`.

mod mirror;
mod modal;
mod model;
mod parse;
pub(crate) mod settings;
mod watcher;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::FutureExt as _;
use futures::future::BoxFuture;
pub(crate) use modal::{PrAgentModal, PrAgentModalEvent};
pub(crate) use model::{PrAgentModel, PrWatchRequest};
pub(crate) use parse::{PrRef, parse_pr_ref};
use serde::Deserialize;
use warpui::keymap::EditableBinding;
use warpui::keymap::macros::*;
use warpui::{AppContext, SingletonEntity};

use self::mirror::Login;
use self::settings::PrAgentSettings;
use crate::features::FeatureFlag;
use crate::task_agent::settings::TaskAgentSettings;
use crate::task_agent::{
    Checkout, TaskAgentConfig, TaskAgentRequest, plan_launch, render_paragraphs, short_title,
};
use crate::util::bindings::BindingGroup;
use crate::workspace::WorkspaceAction;

/// Longest a single `gh` call may take before it counts as failed.
const GH_TIMEOUT: Duration = Duration::from_secs(30);
/// `{{type}}` of the branch a pull request is checked out on.
const BRANCH_TYPE: &str = "review";

/// Registers the modal's keybindings and the command palette entry.
pub(crate) fn init(app: &mut AppContext) {
    modal::init(app);
    app.register_editable_bindings([EditableBinding::new(
        "workspace:pr_agent_review",
        "PR agent: review pull request…",
        WorkspaceAction::OpenPrAgentModal,
    )
    .with_enabled(|| FeatureFlag::PrReviewAgent.is_enabled())
    .with_group(BindingGroup::Navigation.as_str())
    .with_context_predicate(id!("Workspace"))]);
}

/// Where the pull request is checked out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CheckoutMode {
    /// A new worktree, branched from the pull request's base and reset to its head.
    Worktree,
    /// A new branch in the current checkout, reset to the pull request's head.
    Branch,
    /// `gh pr checkout` in the current checkout.
    Here,
}

impl CheckoutMode {
    pub(crate) const ALL: [CheckoutMode; 3] = [
        CheckoutMode::Worktree,
        CheckoutMode::Branch,
        CheckoutMode::Here,
    ];

    /// Parses the `pr_agent.default_checkout` setting; anything unknown means a worktree.
    pub(crate) fn from_setting(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "branch" => CheckoutMode::Branch,
            "here" => CheckoutMode::Here,
            _ => CheckoutMode::Worktree,
        }
    }

    pub(crate) fn setting_value(self) -> &'static str {
        match self {
            CheckoutMode::Worktree => "worktree",
            CheckoutMode::Branch => "branch",
            CheckoutMode::Here => "here",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            CheckoutMode::Worktree => "New worktree",
            CheckoutMode::Branch => "New branch here",
            CheckoutMode::Here => "Current checkout",
        }
    }
}

/// Which prompt template starts the agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PromptKind {
    /// Someone else's pull request: `pr_agent.review_other_template`.
    ReviewOther,
    /// The user's own pull request: `pr_agent.watch_own_template`.
    WatchOwn,
}

impl PromptKind {
    pub(crate) const ALL: [PromptKind; 2] = [PromptKind::ReviewOther, PromptKind::WatchOwn];

    /// `WatchOwn` when the pull request's author is the `gh` user.
    pub(crate) fn for_author(author: &str, viewer: Option<&str>) -> Self {
        if viewer.is_some_and(|viewer| viewer.eq_ignore_ascii_case(author)) {
            PromptKind::WatchOwn
        } else {
            PromptKind::ReviewOther
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            PromptKind::ReviewOther => "Review someone else's PR",
            PromptKind::WatchOwn => "Watch my own PR",
        }
    }

    pub(crate) fn template(self, settings: &PrAgentSettings) -> String {
        match self {
            PromptKind::ReviewOther => settings.review_other_template.clone(),
            PromptKind::WatchOwn => settings.watch_own_template.clone(),
        }
    }
}

/// What the modal shows and the prompt template uses about a pull request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PrDetails {
    pub pr: PrRef,
    pub title: String,
    pub url: String,
    /// Login of the author.
    pub author: String,
    /// Base branch name, e.g. `main`.
    pub base: String,
    /// `OPEN`, `CLOSED` or `MERGED`.
    pub state: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrDetailsJson {
    title: String,
    url: String,
    #[serde(default)]
    author: Option<Login>,
    base_ref_name: String,
    #[serde(default)]
    state: String,
}

impl PrDetails {
    /// `gh` arguments that print the JSON [`PrDetails::parse`] reads.
    pub(crate) fn gh_args(pr: &PrRef) -> Vec<String> {
        pr_view_args(pr, "title,url,author,baseRefName,state")
    }

    pub(crate) fn parse(pr: PrRef, json: &str) -> serde_json::Result<Self> {
        let details: PrDetailsJson = serde_json::from_str(json)?;
        Ok(Self {
            pr,
            title: details.title,
            url: details.url,
            author: details
                .author
                .map(|author| author.login)
                .unwrap_or_default(),
            base: details.base_ref_name,
            state: details.state,
        })
    }
}

/// `gh pr view <n> -R owner/repo --json <fields>`.
pub(crate) fn pr_view_args(pr: &PrRef, fields: &str) -> Vec<String> {
    [
        "pr",
        "view",
        &pr.number.to_string(),
        "-R",
        &pr.slug(),
        "--json",
        fields,
    ]
    .map(str::to_string)
    .into()
}

/// `gh` arguments that print the login of the authenticated user.
pub(crate) fn viewer_args() -> Vec<String> {
    ["api", "user", "--jq", ".login"].map(str::to_string).into()
}

/// Runs `gh` in `cwd` with the user's interactive `PATH` (a GUI launch lacks Homebrew's) and a
/// timeout; resolves to stdout.
pub(crate) fn run_gh(
    app: &mut AppContext,
    cwd: PathBuf,
    args: Vec<String>,
) -> BoxFuture<'static, anyhow::Result<String>> {
    #[cfg(feature = "local_tty")]
    let path_env = crate::terminal::local_shell::LocalShellState::handle(app)
        .update(app, |shell_state, ctx| {
            shell_state.get_interactive_path_env_var(ctx)
        });
    #[cfg(not(feature = "local_tty"))]
    let path_env = {
        let _ = app;
        futures::future::ready(None::<String>)
    };
    async move {
        let path_env = path_env.await;
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let run = crate::util::git::run_gh_command(&cwd, &args, path_env.as_deref());
        let timeout = warpui::r#async::Timer::after(GH_TIMEOUT);
        futures::pin_mut!(run);
        match futures::future::select(run, timeout).await {
            futures::future::Either::Left((result, _)) => result,
            futures::future::Either::Right(_) => Err(anyhow::anyhow!("gh timed out")),
        }
    }
    .boxed()
}

/// Renders a PR prompt template. Variables: `number`, `title`, `url`, `author`, `base`, `repo`
/// (`owner/repo`), `checkout_path`, `branch`; empty paragraphs are dropped.
pub(crate) fn render_pr_prompt(
    template: &str,
    details: &PrDetails,
    checkout_path: &Path,
    branch: &str,
) -> String {
    let vars = HashMap::from([
        ("number".to_string(), details.pr.number.to_string()),
        ("title".to_string(), details.title.clone()),
        ("url".to_string(), details.url.clone()),
        ("author".to_string(), details.author.clone()),
        ("base".to_string(), details.base.clone()),
        ("repo".to_string(), details.pr.slug()),
        (
            "checkout_path".to_string(),
            checkout_path.to_string_lossy().into_owned(),
        ),
        ("branch".to_string(), branch.to_string()),
    ]);
    render_paragraphs(template, &vars)
}

/// The `gh pr checkout` command run inside the new checkout. With a `branch` (worktree and branch
/// modes), the branch the launcher just created is reset to the pull request's head, so the
/// pull request's own branch may stay checked out elsewhere.
pub(crate) fn checkout_command(pr: &PrRef, branch: Option<&str>) -> String {
    let mut command = format!("gh pr checkout {} -R {}", pr.number, pr.slug());
    if let Some(branch) = branch {
        command.push_str(&format!(" -b {} --force", shell_words::quote(branch)));
    }
    command
}

/// The launcher settings for a pull request in `repo_root`: like a task, except the new branch is
/// never pushed (it only mirrors the pull request's head).
pub(crate) fn launch_config(repo_root: &Path, app: &AppContext) -> TaskAgentConfig {
    TaskAgentConfig {
        push_on_create: false,
        ..TaskAgentSettings::as_ref(app).config_for(repo_root)
    }
}

/// Turns a task draft (repository, CLI, defaults) into the request that checks `details` out per
/// `mode`: key `PR-<n>`, the pull request's title and URL, a branch from the pull request's base
/// that `gh pr checkout` then resets to its head. The prompt is left to the caller.
pub(crate) fn pr_request(
    details: &PrDetails,
    mode: CheckoutMode,
    draft: TaskAgentRequest,
    config: &TaskAgentConfig,
) -> TaskAgentRequest {
    let base = format!("origin/{}", details.base);
    let mut request = TaskAgentRequest {
        title: details.title.clone(),
        key: Some(format!("PR-{}", details.pr.number)),
        url: Some(details.url.clone()),
        branch_type: Some(BRANCH_TYPE.to_string()),
        checkout: match mode {
            CheckoutMode::Worktree => Checkout::Worktree { base },
            CheckoutMode::Branch => Checkout::Branch { base },
            CheckoutMode::Here => Checkout::Here,
        },
        branch: None,
        prompt: String::new(),
        checkout_commands: Vec::new(),
        ..draft
    };
    let branch = plan_launch(&request, config).branch;
    request.checkout_commands = vec![checkout_command(&details.pr, branch.as_deref())];
    request.branch = branch;
    request
}

/// Tab title of a PR agent session: `PR #12 <short title>`.
pub(crate) fn tab_title(details: &PrDetails, short_title_max_chars: usize) -> String {
    format!(
        "PR #{} {}",
        details.pr.number,
        short_title(&details.title, short_title_max_chars)
    )
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;

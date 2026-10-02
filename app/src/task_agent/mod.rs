//! Task agent launcher: start a CLI coding agent on a task in a new worktree, a new branch, or
//! the current checkout, and name the session after the task.
//!
//! Gated by `FeatureFlag::TaskAgentLauncher`.

mod modal;
mod session;
pub(crate) mod settings;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub(crate) use modal::{TaskAgentModal, TaskAgentModalEvent, TaskAgentModalMode};
pub(crate) use session::{TaskSession, TaskSessionsModel};
use warpui::keymap::macros::*;
use warpui::keymap::{BindingDescription, EditableBinding};
use warpui::{AppContext, SingletonEntity};

use self::settings::TaskAgentSettings;
use crate::features::FeatureFlag;
use crate::settings_view::SettingsSection;
use crate::terminal::CLIAgent;
use crate::util::bindings::{BindingGroup, CustomAction, MAC_MENUS_CONTEXT};
use crate::workspace::WorkspaceAction;

/// Maximum length of `{{slug}}`.
const SLUG_MAX_CHARS: usize = 40;
const DEFAULT_BRANCH_TYPE: &str = "feat";

/// Registers the modal's keybindings and the command palette entries.
pub(crate) fn init(app: &mut AppContext) {
    modal::init(app);
    let enabled = || FeatureFlag::TaskAgentLauncher.is_enabled();
    app.register_editable_bindings([
        EditableBinding::new(
            "workspace:new_claude_code_tab",
            "New Claude Code tab",
            WorkspaceAction::NewClaudeCodeTab,
        )
        .with_group(BindingGroup::Navigation.as_str())
        .with_context_predicate(id!("Workspace"))
        .with_custom_action(CustomAction::NewClaudeCodeTab),
        EditableBinding::new(
            "workspace:task_agent_start",
            BindingDescription::new("Task agent: start on task…")
                .with_custom_description(MAC_MENUS_CONTEXT, "Start agent on task…"),
            WorkspaceAction::OpenTaskAgentModal,
        )
        .with_enabled(enabled)
        .with_group(BindingGroup::Navigation.as_str())
        .with_context_predicate(id!("Workspace"))
        .with_custom_action(CustomAction::StartTaskAgent),
        EditableBinding::new(
            "workspace:task_agent_rename_session",
            "Task agent: rename this session from task…",
            WorkspaceAction::RenameSessionFromTask,
        )
        .with_enabled(enabled)
        .with_group(BindingGroup::Navigation.as_str())
        .with_context_predicate(id!("Workspace")),
        EditableBinding::new(
            "workspace:task_agent_open_settings",
            "Task agent: open settings",
            WorkspaceAction::ShowSettingsPage(SettingsSection::TaskAgents),
        )
        .with_enabled(enabled)
        .with_group(BindingGroup::Settings.as_str())
        .with_context_predicate(id!("Workspace")),
    ]);
}

/// A request to start a CLI agent on a task. This is the entry point other features use
/// (manual palette entry, issue trackers, pull requests):
///
/// ```ignore
/// let request = TaskAgentRequest {
///     title: "Fix login redirect".into(),
///     key: Some("EXAMPLE-123".into()),
///     ..TaskAgentRequest::draft(repo_root, ctx)
/// };
/// // Let the user review it (repo, checkout mode, base, branch, CLI, prompt) and launch; the
/// // modal fills an empty `branch`/`prompt` from the templates:
/// workspace.open_task_agent_modal(TaskAgentModalMode::Launch, request, ctx);
/// // Or launch without the modal (set `prompt` yourself, e.g. with `render_prompt`):
/// let config = TaskAgentSettings::as_ref(ctx).config_for(&request.repo_root);
/// workspace.launch_task_agent(plan_launch(&request, &config), ctx);
/// ```
///
/// [`plan_launch`] is pure: it turns a request plus resolved settings into the startup
/// commands, working directory and tab title. `Workspace::launch_task_agent` writes the prompt
/// file, opens the tab, and records the task as [`TaskSession`] metadata on the agent's
/// terminal view (read it back through [`TaskSessionsModel`]).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TaskAgentRequest {
    /// Feeds `{{title}}`, `{{short_title}}` and `{{slug}}`.
    pub title: String,
    /// Tracker key such as `EXAMPLE-123`; feeds `{{key}}`.
    pub key: Option<String>,
    /// Task description (markdown); feeds `{{body}}`.
    pub body: Option<String>,
    /// Link to the task; feeds `{{url}}`.
    pub url: Option<String>,
    /// Feeds `{{type}}` (e.g. `feat`, `fix`); `None` means `feat`.
    pub branch_type: Option<String>,
    pub repo_root: PathBuf,
    pub checkout: Checkout,
    /// Branch to create; `None` renders the branch template. Ignored for [`Checkout::Here`].
    pub branch: Option<String>,
    pub cli: CLIAgent,
    /// Initial prompt for the agent; empty starts the agent without one.
    pub prompt: String,
    /// Run the repository's setup commands. Ignored for [`Checkout::Here`].
    pub run_setup: bool,
    /// Commands run inside the checkout right after it exists, before the push and the setup
    /// commands (e.g. `gh pr checkout` to put a pull request's head on the new branch).
    pub checkout_commands: Vec<String>,
}

impl TaskAgentRequest {
    /// An empty request for `repo_root` with the user's defaults: new worktree from the remote
    /// default branch, the configured CLI, setup commands on, no prompt.
    pub(crate) fn draft(repo_root: PathBuf, app: &AppContext) -> Self {
        let cli = TaskAgentSettings::as_ref(app).default_cli(&repo_root);
        Self::with_cli(repo_root, cli)
    }

    /// Like [`Self::draft`], with an explicit CLI and no settings lookup.
    pub(crate) fn with_cli(repo_root: PathBuf, cli: CLIAgent) -> Self {
        Self {
            title: String::new(),
            key: None,
            body: None,
            url: None,
            branch_type: None,
            repo_root,
            checkout: Checkout::Worktree {
                base: String::new(),
            },
            branch: None,
            cli,
            prompt: String::new(),
            run_setup: true,
            checkout_commands: Vec::new(),
        }
    }
}

/// Where the agent works. An empty `base` means the current `HEAD` of the repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Checkout {
    /// A new branch in a new `git worktree` next to the repository.
    Worktree { base: String },
    /// A new branch in the repository's own checkout.
    Branch { base: String },
    /// The repository as it is, with no git side effects.
    Here,
}

/// Everything [`plan_launch`] needs besides the request: the user's settings resolved for one
/// repository (see `TaskAgentSettings::config_for`).
#[derive(Clone, Debug)]
pub(crate) struct TaskAgentConfig {
    pub branch_template: String,
    pub worktree_path_template: String,
    pub push_on_create: bool,
    pub fetch_before_branch: bool,
    pub setup_commands: Vec<String>,
    pub session_title_template: String,
    pub short_title_max_chars: usize,
    /// Directory the initial prompt is written to before the agent reads it.
    pub prompt_dir: PathBuf,
}

/// The concrete launch derived from a [`TaskAgentRequest`].
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LaunchPlan {
    /// Startup commands, run one block at a time; the queue stops at the first failure.
    pub commands: Vec<String>,
    /// Directory the new pane starts in.
    pub cwd: PathBuf,
    pub tab_title: String,
    pub branch: Option<String>,
    pub worktree_path: Option<PathBuf>,
    /// File to write before the commands run; the agent command reads it as its first prompt.
    pub prompt_file: Option<(PathBuf, String)>,
    /// Prompt to type into the agent once its session starts, for agents that take no
    /// prompt argument.
    pub fallback_prompt: Option<String>,
    pub session: TaskSession,
}

/// Turns a request into startup commands, cwd and tab title. Pure: no IO.
pub(crate) fn plan_launch(request: &TaskAgentRequest, config: &TaskAgentConfig) -> LaunchPlan {
    let mut vars = template_vars(request, config.short_title_max_chars);
    let repo = quote_path(&request.repo_root);

    let (branch, worktree_path, cwd, mut commands) = match &request.checkout {
        Checkout::Here => (None, None, request.repo_root.clone(), Vec::new()),
        Checkout::Worktree { base } => {
            let branch = resolve_branch(request, config, &vars);
            vars.insert("branch".to_string(), branch.clone());
            let path = PathBuf::from(tidy_path(&handlebars::render_template(
                &config.worktree_path_template,
                &vars,
            )));
            let quoted_path = quote_path(&path);
            let mut commands = Vec::new();
            if config.fetch_before_branch {
                commands.push(format!("git -C {repo} fetch origin || true"));
            }
            commands.push(
                format!(
                    "git -C {repo} worktree add -b {} {quoted_path} {}",
                    quote(&branch),
                    quote(base)
                )
                .trim_end()
                .to_string(),
            );
            commands.push(format!("cd {quoted_path}"));
            (
                Some(branch),
                Some(path),
                request.repo_root.clone(),
                commands,
            )
        }
        Checkout::Branch { base } => {
            let branch = resolve_branch(request, config, &vars);
            vars.insert("branch".to_string(), branch.clone());
            let mut commands = Vec::new();
            if config.fetch_before_branch {
                commands.push("git fetch origin || true".to_string());
            }
            commands.push(
                format!("git switch -c {} {}", quote(&branch), quote(base))
                    .trim_end()
                    .to_string(),
            );
            (Some(branch), None, request.repo_root.clone(), commands)
        }
    };
    commands.extend(request.checkout_commands.iter().cloned());

    if let Some(branch) = &branch {
        if config.push_on_create {
            // ponytail: best-effort, so a repo without a reachable origin still starts the agent.
            commands.push(format!("git push -u origin {} || true", quote(branch)));
        }
        if request.run_setup {
            commands.extend(config.setup_commands.iter().cloned());
        }
    }

    let prompt = request.prompt.trim();
    let takes_prompt_arg = matches!(request.cli, CLIAgent::Claude | CLIAgent::Codex);
    let cli = request.cli.command_prefix();
    let (prompt_file, fallback_prompt) = if prompt.is_empty() {
        commands.push(cli.to_string());
        (None, None)
    } else if takes_prompt_arg {
        let name = tidy_branch(&format!("{}-{}", vars["key"], vars["slug"])).replace('/', "-");
        let name = if name.is_empty() {
            "task"
        } else {
            name.as_str()
        };
        let file = config.prompt_dir.join(format!("{name}.md"));
        // ponytail: `"$(cat …)"` assumes a POSIX-style shell (bash, zsh, fish >= 3.4).
        commands.push(format!("{cli} \"$(cat {})\"", quote_path(&file)));
        (Some((file, prompt.to_string())), None)
    } else {
        commands.push(cli.to_string());
        (None, Some(prompt.to_string()))
    };

    let tab_title = session_title(&config.session_title_template, &vars);

    LaunchPlan {
        commands,
        cwd,
        tab_title,
        branch: branch.clone(),
        worktree_path,
        prompt_file,
        fallback_prompt,
        session: TaskSession {
            key: request.key.clone().filter(|key| !key.trim().is_empty()),
            title: request.title.clone(),
            url: request.url.clone(),
            repo_root: request.repo_root.clone(),
            branch,
            checkout: request.checkout.clone(),
        },
    }
}

/// The branch name the request would create: its explicit branch, else the branch template.
pub(crate) fn resolve_branch(
    request: &TaskAgentRequest,
    config: &TaskAgentConfig,
    vars: &HashMap<String, String>,
) -> String {
    request
        .branch
        .as_deref()
        .map(str::trim)
        .filter(|branch| !branch.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| tidy_branch(&handlebars::render_template(&config.branch_template, vars)))
}

/// Renders `template` (the prompt template) for `request`, dropping empty paragraphs left by
/// unset variables.
pub(crate) fn render_prompt(template: &str, request: &TaskAgentRequest, branch: &str) -> String {
    let mut vars = template_vars(request, usize::MAX);
    vars.insert("branch".to_string(), branch.to_string());
    render_paragraphs(template, &vars)
}

/// Renders `template` with `vars`, dropping empty paragraphs left by unset variables.
pub(crate) fn render_paragraphs(template: &str, vars: &HashMap<String, String>) -> String {
    let rendered = handlebars::render_template(template, vars);
    rendered
        .split("\n\n")
        .map(|paragraph| paragraph.trim_matches('\n'))
        .filter(|paragraph| !paragraph.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Renders the session title template with [`template_vars`] on one line; falls back to the
/// title when it renders empty.
pub(crate) fn session_title(template: &str, vars: &HashMap<String, String>) -> String {
    let rendered = handlebars::render_template(template, vars);
    let collapsed = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        vars.get("title").cloned().unwrap_or_default()
    } else {
        collapsed
    }
}

/// Template variables shared by all task templates. Unset values render empty.
pub(crate) fn template_vars(
    request: &TaskAgentRequest,
    short_title_max_chars: usize,
) -> HashMap<String, String> {
    let repo = request
        .repo_root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let repo_parent = request
        .repo_root
        .parent()
        .map(|parent| parent.to_string_lossy().into_owned())
        .unwrap_or_default();
    let branch_type = request
        .branch_type
        .as_deref()
        .map(str::trim)
        .filter(|branch_type| !branch_type.is_empty())
        .unwrap_or(DEFAULT_BRANCH_TYPE);
    let text = |value: &Option<String>| value.as_deref().unwrap_or_default().trim().to_string();

    HashMap::from([
        ("key".to_string(), text(&request.key)),
        ("title".to_string(), request.title.trim().to_string()),
        (
            "short_title".to_string(),
            short_title(&request.title, short_title_max_chars),
        ),
        ("slug".to_string(), slug(&request.title)),
        ("type".to_string(), branch_type.to_string()),
        ("body".to_string(), text(&request.body)),
        ("url".to_string(), text(&request.url)),
        ("repo".to_string(), repo),
        ("repo_parent".to_string(), repo_parent),
        (
            "repo_root".to_string(),
            request.repo_root.to_string_lossy().into_owned(),
        ),
        ("branch".to_string(), String::new()),
    ])
}

/// Lowercase ASCII alphanumerics; every other run of characters becomes one `-`; trimmed and
/// capped at [`SLUG_MAX_CHARS`].
pub(crate) fn slug(text: &str) -> String {
    let mut slug = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.truncate(SLUG_MAX_CHARS);
    slug.trim_end_matches('-').to_string()
}

/// `title` cut to at most `max_chars` characters (including a trailing `…`), preferring a word
/// boundary.
pub(crate) fn short_title(title: &str, max_chars: usize) -> String {
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.chars().count() <= max_chars {
        return title;
    }
    if max_chars == 0 {
        return String::new();
    }
    let head: String = title.chars().take(max_chars - 1).collect();
    let cut = match head.rfind(' ') {
        Some(space) if space > 0 => &head[..space],
        _ => head.as_str(),
    };
    format!("{}…", cut.trim_end())
}

/// Cleans a rendered branch: whitespace becomes `-`, runs of `-` collapse, and each `/`
/// segment loses leading/trailing `-` (left by empty variables); empty segments are dropped.
fn tidy_branch(branch: &str) -> String {
    let dashed: String = branch
        .chars()
        .map(|c| if c.is_whitespace() { '-' } else { c })
        .collect();
    dashed
        .split('/')
        .map(|segment| {
            let mut collapsed = String::new();
            for c in segment.chars() {
                if !(c == '-' && collapsed.ends_with('-')) {
                    collapsed.push(c);
                }
            }
            collapsed.trim_matches('-').to_string()
        })
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

/// Trims `-` left by empty variables from each path segment and drops empty segments.
// ponytail: also trims a user directory that genuinely starts or ends with `-`.
fn tidy_path(path: &str) -> String {
    let is_absolute = path.starts_with('/');
    let tidy = path
        .split('/')
        .map(|segment| segment.trim_matches('-'))
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    if is_absolute {
        format!("/{tidy}")
    } else {
        tidy
    }
}

fn quote(value: &str) -> String {
    if value.is_empty() {
        String::new()
    } else {
        shell_words::quote(value).into_owned()
    }
}

fn quote_path(path: &Path) -> String {
    shell_words::quote(&path.to_string_lossy()).into_owned()
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;

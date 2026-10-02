//! Settings for the task agent launcher (`task_agents.*` in the settings file).

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use settings::macros::define_settings_group;
use settings::{SupportedPlatforms, SyncToCloud};

use super::TaskAgentConfig;
use crate::terminal::CLIAgent;

pub(crate) const DEFAULT_BRANCH_TEMPLATE: &str = "{{type}}/{{key}}-{{slug}}";
pub(crate) const DEFAULT_WORKTREE_PATH_TEMPLATE: &str =
    "{{repo_parent}}/{{repo}}.worktrees/{{key}}-{{slug}}";
pub(crate) const DEFAULT_PROMPT_TEMPLATE: &str =
    "Work on this task: {{key}} {{title}}\n\n{{body}}\n\n{{url}}";
pub(crate) const DEFAULT_SESSION_TITLE_TEMPLATE: &str = "{{key}} {{short_title}}";
pub(crate) const DEFAULT_SHORT_TITLE_MAX_CHARS: usize = 32;

/// Per-repository overrides, keyed by repository root path in [`TaskAgentSettings::per_repo`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RepoTaskAgentOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path_template: Option<String>,
    /// Commands run in the new checkout before the agent starts (e.g. dependency install).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setup_commands: Vec<String>,
    /// Command name of the CLI agent to use for this repository (e.g. `claude`, `codex`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_cli: Option<String>,
}

impl settings_value::SettingsValue for RepoTaskAgentOverrides {}

define_settings_group!(TaskAgentSettings, settings: [
    branch_template: TaskAgentBranchTemplate {
        type: String,
        default: DEFAULT_BRANCH_TEMPLATE.to_string(),
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "task_agents.branch_template",
        description: "Template for the branch a task agent works on. Variables: type, key, slug, title, repo.",
    },
    worktree_path_template: TaskAgentWorktreePathTemplate {
        type: String,
        default: DEFAULT_WORKTREE_PATH_TEMPLATE.to_string(),
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "task_agents.worktree_path_template",
        description: "Template for the directory of a new task worktree. Variables: repo_parent, repo, key, slug, branch.",
    },
    default_cli: TaskAgentDefaultCli {
        type: String,
        default: "claude".to_string(),
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "task_agents.default_cli",
        description: "Command name of the CLI agent started for a task (e.g. claude, codex).",
    },
    push_on_create: TaskAgentPushOnCreate {
        type: bool,
        default: true,
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "task_agents.push_on_create",
        description: "Whether a new task branch is pushed to origin with upstream tracking.",
    },
    fetch_before_branch: TaskAgentFetchBeforeBranch {
        type: bool,
        default: true,
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "task_agents.fetch_before_branch",
        description: "Whether origin is fetched before a task branch is created.",
    },
    prompt_template: TaskAgentPromptTemplate {
        type: String,
        default: DEFAULT_PROMPT_TEMPLATE.to_string(),
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "task_agents.prompt_template",
        description: "Template for the initial prompt sent to a task agent. Variables: key, title, body, url, branch, repo.",
    },
    session_title_template: TaskAgentSessionTitleTemplate {
        type: String,
        default: DEFAULT_SESSION_TITLE_TEMPLATE.to_string(),
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "task_agents.session_title_template",
        description: "Template for the tab title of a task session. Variables: key, short_title, title, branch, repo.",
    },
    short_title_max_chars: TaskAgentShortTitleMaxChars {
        type: usize,
        default: DEFAULT_SHORT_TITLE_MAX_CHARS,
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "task_agents.short_title_max_chars",
        description: "Maximum length of {{short_title}} in session titles.",
    },
    prefer_agent_title: TaskAgentPreferAgentTitle {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "task_agents.prefer_agent_title",
        description: "Whether the agent's own session title wins over the task title for the tab.",
    },
    per_repo: TaskAgentPerRepo {
        type: HashMap<String, RepoTaskAgentOverrides>,
        default: HashMap::default(),
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "task_agents.per_repo",
        max_table_depth: 2,
        description: "Per-repository overrides keyed by repository root path.",
    },
]);

impl TaskAgentSettings {
    fn overrides(&self, repo_root: &Path) -> Option<&RepoTaskAgentOverrides> {
        self.per_repo.get(&*repo_root.to_string_lossy())
    }

    /// The CLI agent to start for `repo_root`, from the per-repo override or the global default.
    pub(crate) fn default_cli(&self, repo_root: &Path) -> CLIAgent {
        let name = self
            .overrides(repo_root)
            .and_then(|overrides| overrides.default_cli.as_deref())
            .unwrap_or(self.default_cli.as_str());
        cli_from_command(name).unwrap_or(CLIAgent::Claude)
    }

    /// Resolves the global settings plus the per-repo overrides for `repo_root`.
    pub(crate) fn config_for(&self, repo_root: &Path) -> TaskAgentConfig {
        let overrides = self.overrides(repo_root).cloned().unwrap_or_default();
        TaskAgentConfig {
            branch_template: overrides
                .branch_template
                .unwrap_or_else(|| self.branch_template.clone()),
            worktree_path_template: overrides
                .worktree_path_template
                .unwrap_or_else(|| self.worktree_path_template.clone()),
            push_on_create: *self.push_on_create,
            fetch_before_branch: *self.fetch_before_branch,
            setup_commands: overrides.setup_commands,
            session_title_template: self.session_title_template.clone(),
            short_title_max_chars: *self.short_title_max_chars,
            prompt_dir: warp_core::paths::cache_dir().join("task_agent_prompts"),
        }
    }
}

/// Maps a command name such as `claude` to its [`CLIAgent`].
pub(crate) fn cli_from_command(name: &str) -> Option<CLIAgent> {
    let name = name.trim();
    enum_iterator::all::<CLIAgent>().find(|agent| agent.command_prefixes().contains(&name))
}

//! Settings for the PR stack view (`[pr_stack]` in the settings file).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use settings::macros::define_settings_group;
use settings::{SupportedPlatforms, SyncToCloud};

use super::stats::{ClassificationRule, default_rules};

#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[schemars(
    description = "Order of the rows in the PR stack view.",
    rename_all = "snake_case"
)]
#[serde(rename_all = "snake_case")]
pub enum PrStackRowOrder {
    /// The branch closest to the target first.
    #[default]
    BottomToTop,
    TopToBottom,
}

impl PrStackRowOrder {
    pub const ALL: [Self; 2] = [Self::BottomToTop, Self::TopToBottom];

    pub fn label(self) -> &'static str {
        match self {
            Self::BottomToTop => "Bottom to top",
            Self::TopToBottom => "Top to bottom",
        }
    }
}

define_settings_group!(PrStackSettings, settings: [
    targets: PrStackTargets {
        type: HashMap<String, String>,
        default: HashMap::new(),
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "pr_stack.targets",
        description: "Target branch per repository, keyed by the repository's root path. Defaults to the origin default branch. A `target` in .git/warp-stack.json takes precedence.",
    },
    classification: PrStackClassification {
        type: Vec<ClassificationRule>,
        default: default_rules(),
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "pr_stack.classification",
        description: "Ordered path rules that bucket changed files into tests, docs, or config. The first match wins; unmatched files are code.",
    },
    pr_body_file_template: PrStackPrBodyFileTemplate {
        type: String,
        default: ".git/warp-pr/{{branch}}.md".to_string(),
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "pr_stack.pr_body_file_template",
        description: "Where a PR title and body are read from when creating a PR (first line is the title). Paths under .git/ live in the git common dir.",
    },
    pr_prepare_command: PrStackPrPrepareCommand {
        type: String,
        default: String::new(),
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "pr_stack.pr_prepare_command",
        description: "Text sent to the branch's agent pane to write the PR body file when it is missing. {{branch}} is replaced with the branch name. Empty disables it.",
    },
    auto_restack: PrStackAutoRestack {
        type: bool,
        default: true,
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "pr_stack.auto_restack",
        description: "Restack automatically when the bottom PR merges or the target branch advances.",
    },
    poll_interval_secs: PrStackPollIntervalSecs {
        type: u64,
        default: 120,
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "pr_stack.poll_interval_secs",
        description: "How often the stack view fetches and refreshes PR status, in seconds. 0 disables polling.",
    },
    row_order: PrStackRowOrderSetting {
        type: PrStackRowOrder,
        default: PrStackRowOrder::BottomToTop,
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "pr_stack.row_order",
        description: "Order of the rows in the PR stack view.",
    },
]);

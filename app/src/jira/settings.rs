//! Settings for the Jira integration (`jira.*` in the settings file). The API token is not a
//! setting: it lives in the OS keychain (see `super::save_token`).

use std::collections::HashMap;

use settings::macros::define_settings_group;
use settings::{SupportedPlatforms, SyncToCloud};

pub(crate) const DEFAULT_JQL: &str =
    "assignee = currentUser() AND statusCategory != Done ORDER BY updated DESC";

fn default_issue_type_to_branch_type() -> HashMap<String, String> {
    HashMap::from([
        ("Bug".to_string(), "fix".to_string()),
        ("Story".to_string(), "feat".to_string()),
        ("Task".to_string(), "feat".to_string()),
    ])
}

define_settings_group!(JiraSettings, settings: [
    site_url: JiraSiteUrl {
        type: String,
        default: String::new(),
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "jira.site_url",
        description: "Jira Cloud site, e.g. https://example.atlassian.net.",
    },
    email: JiraEmail {
        type: String,
        default: String::new(),
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "jira.email",
        description: "Email of the Jira account the API token belongs to.",
    },
    default_jql: JiraDefaultJql {
        type: String,
        default: DEFAULT_JQL.to_string(),
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "jira.default_jql",
        description: "JQL for the issue list the Jira picker opens with.",
    },
    project_keys: JiraProjectKeys {
        type: Vec<String>,
        default: Vec::new(),
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "jira.project_keys",
        description: "Limits the default issue list to these projects (empty: all projects).",
    },
    issue_type_to_branch_type: JiraIssueTypeToBranchType {
        type: HashMap<String, String>,
        default: default_issue_type_to_branch_type(),
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "jira.issue_type_to_branch_type",
        description: "Branch type ({{type}} in branch templates) per issue type; unlisted types use feat.",
    },
]);

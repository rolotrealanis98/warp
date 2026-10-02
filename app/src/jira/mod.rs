//! Jira integration: an issue picker feeding the task agent launcher, plus explicit per-pane
//! write actions (transition, comment). Site URL and email are settings; the API token lives in
//! the OS keychain under [`TOKEN_KEY`].
//!
//! Gated by `FeatureFlag::JiraIntegration`. Nothing is written to Jira when an agent starts.

mod picker;
pub(crate) mod settings;

use std::collections::HashMap;

pub(crate) use jira_client::Issue;
use jira_client::{JiraClient, JiraError};
pub(crate) use picker::{JiraIssuePicker, JiraIssuePickerEvent, PickerMode};
use warpui::keymap::EditableBinding;
use warpui::keymap::macros::*;
use warpui::{AppContext, Entity, SingletonEntity};
use warpui_extras::secure_storage::{self, AppContextExt};

use self::settings::JiraSettings;
use crate::features::FeatureFlag;
use crate::task_agent::TaskAgentRequest;
use crate::util::bindings::BindingGroup;
use crate::workspace::WorkspaceAction;

/// Secure storage key of the Jira API token.
const TOKEN_KEY: &str = "jira_api_token";
const DEFAULT_BRANCH_TYPE: &str = "feat";
const JQL_PREFIX: &str = "jql:";
/// Issues fetched per query.
pub(crate) const MAX_RESULTS: u32 = 50;

/// What to do with the issue picked in the picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssueAction {
    StartAgent,
    RenameSession,
    OpenInBrowser,
    CopyKey,
}

/// Command palette entries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JiraCommand {
    PickIssue(IssueAction),
    /// Transition the active pane's task issue.
    Transition,
    /// Comment on the active pane's task issue.
    Comment,
}

pub(crate) fn init(app: &mut AppContext) {
    picker::init(app);
    let enabled = || FeatureFlag::JiraIntegration.is_enabled();
    let binding = |name: &'static str, description: &'static str, command: JiraCommand| {
        EditableBinding::new(name, description, WorkspaceAction::Jira(command))
            .with_enabled(enabled)
            .with_group(BindingGroup::Navigation.as_str())
            .with_context_predicate(id!("Workspace"))
    };
    app.register_editable_bindings([
        binding(
            "workspace:jira_start_agent",
            "Jira: start agent on issue…",
            JiraCommand::PickIssue(IssueAction::StartAgent),
        ),
        binding(
            "workspace:jira_rename_session",
            "Jira: rename this session from issue…",
            JiraCommand::PickIssue(IssueAction::RenameSession),
        ),
        binding(
            "workspace:jira_open_issue",
            "Jira: open issue in browser…",
            JiraCommand::PickIssue(IssueAction::OpenInBrowser),
        ),
        binding(
            "workspace:jira_copy_issue_key",
            "Jira: copy issue key…",
            JiraCommand::PickIssue(IssueAction::CopyKey),
        ),
        binding(
            "workspace:jira_transition",
            "Jira: transition…",
            JiraCommand::Transition,
        ),
        binding(
            "workspace:jira_comment",
            "Jira: add comment…",
            JiraCommand::Comment,
        ),
    ]);
}

#[derive(thiserror::Error, Debug)]
pub(crate) enum ClientError {
    #[error(
        "Jira is not set up: add the site URL, email and API token in Settings > Agents > Jira"
    )]
    NotConfigured,
    #[error(transparent)]
    Jira(#[from] JiraError),
}

/// A client for the configured site, email and keychain token.
pub(crate) fn client(app: &AppContext) -> Result<JiraClient, ClientError> {
    let settings = JiraSettings::as_ref(app);
    let site_url = settings.site_url.trim();
    let email = settings.email.trim();
    let token = read_token(app).unwrap_or_default();
    if site_url.is_empty() || email.is_empty() || token.is_empty() {
        return Err(ClientError::NotConfigured);
    }
    Ok(JiraClient::new(site_url, email, &token)?)
}

pub(crate) fn read_token(app: &AppContext) -> Option<String> {
    match app.secure_storage().read_value(TOKEN_KEY) {
        Ok(token) => Some(token).filter(|token| !token.trim().is_empty()),
        Err(secure_storage::Error::NotFound) => None,
        Err(err) => {
            log::warn!("[Jira] Failed to read the API token from secure storage: {err:#}");
            None
        }
    }
}

pub(crate) fn save_token(app: &AppContext, token: &str) -> Result<(), secure_storage::Error> {
    app.secure_storage().write_value(TOKEN_KEY, token.trim())
}

pub(crate) fn clear_token(app: &AppContext) -> Result<(), secure_storage::Error> {
    match app.secure_storage().remove_value(TOKEN_KEY) {
        Err(secure_storage::Error::NotFound) => Ok(()),
        result => result,
    }
}

/// The issue's page on the configured site.
pub(crate) fn browse_url(key: &str, app: &AppContext) -> Option<String> {
    jira_client::browse_url(&JiraSettings::as_ref(app).site_url, key).ok()
}

/// The last default issue list, shared by every picker so it opens with something to show while
/// it refreshes.
#[derive(Default)]
pub(crate) struct JiraModel {
    issues: Vec<Issue>,
}

impl JiraModel {
    pub(crate) fn issues(&self) -> &[Issue] {
        &self.issues
    }

    pub(crate) fn set_issues(&mut self, issues: Vec<Issue>) {
        self.issues = issues;
    }
}

impl Entity for JiraModel {
    type Event = ();
}

impl SingletonEntity for JiraModel {}

/// How the picker interprets its search box.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PickerQuery<'a> {
    /// Fuzzy filter over the issues already loaded.
    Fuzzy(&'a str),
    /// A server-side JQL search (`jql:` prefix, any case), run on Enter.
    Jql(&'a str),
}

pub(crate) fn parse_query(input: &str) -> PickerQuery<'_> {
    let trimmed = input.trim_start();
    match trimmed.get(..JQL_PREFIX.len()) {
        Some(prefix) if prefix.eq_ignore_ascii_case(JQL_PREFIX) => {
            PickerQuery::Jql(trimmed[JQL_PREFIX.len()..].trim())
        }
        _ => PickerQuery::Fuzzy(input.trim()),
    }
}

/// Indices of `labels` matching `query`, best match first; all of them, in order, for an empty
/// query.
pub(crate) fn fuzzy_filter<'a>(labels: impl Iterator<Item = &'a str>, query: &str) -> Vec<usize> {
    let mut matches: Vec<(usize, i64)> = labels
        .enumerate()
        .filter_map(|(index, label)| {
            if query.is_empty() {
                return Some((index, 0));
            }
            fuzzy_match::match_indices_case_insensitive(label, query)
                .map(|result| (index, result.score))
        })
        .collect();
    // Stable sort keeps the server order (e.g. recently updated) among equal scores.
    matches.sort_by_key(|&(_, score)| std::cmp::Reverse(score));
    matches.into_iter().map(|(index, _)| index).collect()
}

/// The text an issue row is matched against.
pub(crate) fn issue_search_label(issue: &Issue) -> String {
    format!(
        "{} {} {} {} {}",
        issue.key,
        issue.summary,
        issue.status,
        issue.issue_type,
        issue.assignee.as_deref().unwrap_or_default()
    )
}

/// Whether `text` looks like an issue key such as `EXAMPLE-123`.
pub(crate) fn looks_like_issue_key(text: &str) -> bool {
    let Some((project, number)) = text.trim().rsplit_once('-') else {
        return false;
    };
    project.starts_with(|c: char| c.is_ascii_alphabetic())
        && project
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !number.is_empty()
        && number.chars().all(|c| c.is_ascii_digit())
}

/// The default list query: `jql`, limited to `project_keys` when any are set. The project clause
/// goes before a trailing `ORDER BY`.
pub(crate) fn scoped_jql(jql: &str, project_keys: &[String]) -> String {
    let keys: Vec<&str> = project_keys
        .iter()
        .map(|key| key.trim())
        .filter(|key| !key.is_empty())
        .collect();
    let jql = jql.trim();
    if keys.is_empty() {
        return jql.to_string();
    }
    let projects = format!("project in ({})", keys.join(", "));
    let (filter, order) = match jql.to_ascii_lowercase().rfind("order by") {
        Some(index) => (jql[..index].trim(), Some(jql[index..].trim())),
        None => (jql, None),
    };
    let filter = if filter.is_empty() {
        projects
    } else {
        format!("{projects} AND ({filter})")
    };
    match order {
        Some(order) => format!("{filter} {order}"),
        None => filter,
    }
}

/// The branch type (`{{type}}`) for an issue type: an exact match in `mapping`, then a
/// case-insensitive one, else `feat`.
pub(crate) fn branch_type(issue_type: &str, mapping: &HashMap<String, String>) -> String {
    mapping
        .get(issue_type)
        .or_else(|| {
            mapping
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(issue_type))
                .map(|(_, branch_type)| branch_type)
        })
        .map(|branch_type| branch_type.trim())
        .filter(|branch_type| !branch_type.is_empty())
        .unwrap_or(DEFAULT_BRANCH_TYPE)
        .to_string()
}

/// `draft` filled from `issue`. The prompt stays empty so the launcher renders its template.
pub(crate) fn task_request(
    issue: &Issue,
    site_url: &str,
    mapping: &HashMap<String, String>,
    draft: TaskAgentRequest,
) -> TaskAgentRequest {
    TaskAgentRequest {
        title: issue.summary.clone(),
        key: Some(issue.key.clone()),
        body: issue.description.clone(),
        url: jira_client::browse_url(site_url, &issue.key).ok(),
        branch_type: Some(branch_type(&issue.issue_type, mapping)),
        prompt: String::new(),
        ..draft
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;

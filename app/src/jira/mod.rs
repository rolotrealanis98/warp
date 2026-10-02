//! Jira integration: an issue picker feeding the task agent launcher, plus explicit per-pane
//! write actions (transition, comment). Site URL and email are settings; the API token lives in
//! the OS keychain under [`TOKEN_KEY`].
//!
//! Gated by `FeatureFlag::JiraIntegration`. Nothing is written to Jira when an agent starts.

pub(crate) mod settings;

use jira_client::{JiraClient, JiraError};
use warpui::{AppContext, SingletonEntity};
use warpui_extras::secure_storage::{self, AppContextExt};

use self::settings::JiraSettings;

/// Secure storage key of the Jira API token.
const TOKEN_KEY: &str = "jira_api_token";

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

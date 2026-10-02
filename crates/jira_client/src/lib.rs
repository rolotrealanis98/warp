//! Minimal Jira Cloud REST v3 client: issue search, issue details, transitions, comments and
//! assignment. Authenticates with an account email and an API token (HTTP basic auth).
//!
//! The token is only ever sent as the basic-auth password; it is never logged, formatted into
//! errors, or exposed through `Debug`.

mod adf;

use std::time::Duration;

pub use adf::{adf_to_markdown, text_to_adf};
use reqwest::{StatusCode, Url};
use serde_json::{Value, json};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Fields the issue list needs; descriptions are fetched per issue.
const SEARCH_FIELDS: [&str; 5] = ["summary", "status", "assignee", "issuetype", "labels"];

#[derive(thiserror::Error, Debug)]
pub enum JiraError {
    #[error("the Jira site URL must be an https:// address, e.g. https://example.atlassian.net")]
    InvalidSiteUrl,
    #[error("Jira rejected the email or API token")]
    Unauthorized,
    #[error("the account cannot access this in Jira")]
    Forbidden,
    #[error("not found in Jira")]
    NotFound,
    /// A 400 response, e.g. invalid JQL; carries Jira's own message.
    #[error("{0}")]
    Rejected(String),
    #[error("Jira returned HTTP {0}")]
    Status(u16),
    #[error("could not reach Jira")]
    Network(#[source] reqwest::Error),
    #[error("unexpected response from Jira")]
    Decode(#[from] serde_json::Error),
}

/// An issue as shown in a picker. `description` (markdown) and `sprint` are only filled by
/// [`JiraClient::get_issue`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Issue {
    pub key: String,
    pub summary: String,
    pub status: String,
    pub issue_type: String,
    pub assignee: Option<String>,
    pub labels: Vec<String>,
    pub description: Option<String>,
    pub sprint: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Transition {
    pub id: String,
    pub name: String,
    /// Status the issue moves to.
    pub to_status: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Account {
    pub account_id: String,
    pub display_name: String,
}

pub struct JiraClient {
    http: http_client::Client,
    base: Url,
    email: String,
    token: String,
}

impl JiraClient {
    pub fn new(site_url: &str, email: &str, token: &str) -> Result<Self, JiraError> {
        Ok(Self {
            http: http_client::Client::new(),
            base: site_base_url(site_url)?,
            email: email.trim().to_string(),
            token: token.trim().to_string(),
        })
    }

    /// Issues matching `jql`, at most `max_results`.
    pub async fn search(&self, jql: &str, max_results: u32) -> Result<Vec<Issue>, JiraError> {
        let body = json!({ "jql": jql, "maxResults": max_results, "fields": SEARCH_FIELDS });
        let request = self.http.post(self.url(&["search", "jql"])).json(&body);
        let response: Value = self.send_json(request).await?;
        Ok(parse_search(&response))
    }

    /// One issue with its description converted to markdown and its sprint, if any.
    pub async fn get_issue(&self, key: &str) -> Result<Issue, JiraError> {
        let mut url = self.url(&["issue", key]);
        url.query_pairs_mut().append_pair("expand", "names");
        let response: Value = self.send_json(self.http.get(url)).await?;
        parse_issue(&response).ok_or(JiraError::NotFound)
    }

    /// Transitions the issue can take from its current status.
    pub async fn transitions(&self, key: &str) -> Result<Vec<Transition>, JiraError> {
        let url = self.url(&["issue", key, "transitions"]);
        let response: Value = self.send_json(self.http.get(url)).await?;
        Ok(parse_transitions(&response))
    }

    pub async fn transition(&self, key: &str, transition_id: &str) -> Result<(), JiraError> {
        let body = json!({ "transition": { "id": transition_id } });
        let url = self.url(&["issue", key, "transitions"]);
        self.send(self.http.post(url).json(&body)).await.map(drop)
    }

    /// Adds `text` as a comment; blank lines separate paragraphs.
    pub async fn add_comment(&self, key: &str, text: &str) -> Result<(), JiraError> {
        let body = json!({ "body": text_to_adf(text) });
        let url = self.url(&["issue", key, "comment"]);
        self.send(self.http.post(url).json(&body)).await.map(drop)
    }

    pub async fn assign_to_me(&self, key: &str) -> Result<(), JiraError> {
        let me = self.myself().await?;
        let body = json!({ "accountId": me.account_id });
        let url = self.url(&["issue", key, "assignee"]);
        self.send(self.http.put(url).json(&body)).await.map(drop)
    }

    /// Checks the site, email and token by fetching the authenticated account.
    pub async fn test_connection(&self) -> Result<Account, JiraError> {
        self.myself().await
    }

    async fn myself(&self) -> Result<Account, JiraError> {
        let response: Value = self.send_json(self.http.get(self.url(&["myself"]))).await?;
        let text = |name: &str| {
            response
                .get(name)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        Ok(Account {
            account_id: text("accountId"),
            display_name: text("displayName"),
        })
    }

    /// `<site>/rest/api/3/<segments>`, with each segment percent-encoded.
    fn url(&self, segments: &[&str]) -> Url {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .expect("site_base_url only accepts base URLs")
            .pop_if_empty()
            .extend(["rest", "api", "3"])
            .extend(segments);
        url
    }

    async fn send(
        &self,
        request: http_client::RequestBuilder<'_>,
    ) -> Result<http_client::Response, JiraError> {
        let response = request
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(network_error)?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        Err(match status {
            StatusCode::UNAUTHORIZED => JiraError::Unauthorized,
            StatusCode::FORBIDDEN => JiraError::Forbidden,
            StatusCode::NOT_FOUND => JiraError::NotFound,
            StatusCode::BAD_REQUEST => {
                let body = response.text().await.unwrap_or_default();
                JiraError::Rejected(error_message(&body))
            }
            other => JiraError::Status(other.as_u16()),
        })
    }

    async fn send_json(
        &self,
        request: http_client::RequestBuilder<'_>,
    ) -> Result<Value, JiraError> {
        let body = self
            .send(request)
            .await?
            .text()
            .await
            .map_err(network_error)?;
        Ok(serde_json::from_str(&body)?)
    }
}

/// Drops the request URL (site and issue key) so errors are safe to log.
fn network_error(err: reqwest::Error) -> JiraError {
    JiraError::Network(err.without_url())
}

/// Parses the user's site setting: `example.atlassian.net` or `https://example.atlassian.net/`.
/// Only https is accepted because the API token travels in every request.
pub fn site_base_url(site_url: &str) -> Result<Url, JiraError> {
    let site = site_url.trim().trim_end_matches('/');
    if site.is_empty() {
        return Err(JiraError::InvalidSiteUrl);
    }
    let with_scheme = if site.contains("://") {
        site.to_string()
    } else {
        format!("https://{site}")
    };
    let url = Url::parse(&with_scheme).map_err(|_| JiraError::InvalidSiteUrl)?;
    if url.scheme() != "https" || url.host_str().is_none() || url.cannot_be_a_base() {
        return Err(JiraError::InvalidSiteUrl);
    }
    Ok(url)
}

/// The issue's page: `<site>/browse/<KEY>`.
pub fn browse_url(site_url: &str, key: &str) -> Result<String, JiraError> {
    let mut url = site_base_url(site_url)?;
    url.path_segments_mut()
        .expect("site_base_url only accepts base URLs")
        .pop_if_empty()
        .extend(["browse", key.trim()]);
    Ok(url.into())
}

fn parse_search(response: &Value) -> Vec<Issue> {
    response
        .get("issues")
        .and_then(Value::as_array)
        .map(|issues| issues.iter().filter_map(parse_issue).collect())
        .unwrap_or_default()
}

fn parse_issue(issue: &Value) -> Option<Issue> {
    let text = |pointer: &str| {
        issue
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    Some(Issue {
        key: text("/key")?,
        summary: text("/fields/summary").unwrap_or_default(),
        status: text("/fields/status/name").unwrap_or_default(),
        issue_type: text("/fields/issuetype/name").unwrap_or_default(),
        assignee: text("/fields/assignee/displayName"),
        labels: issue
            .pointer("/fields/labels")
            .and_then(Value::as_array)
            .map(|labels| {
                labels
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        description: issue
            .pointer("/fields/description")
            .filter(|description| description.is_object())
            .map(adf_to_markdown)
            .filter(|description| !description.is_empty()),
        sprint: sprint_name(issue),
    })
}

/// The active sprint (else the latest) from the field the `names` expansion calls "Sprint".
// ponytail: reads the object form Jira Cloud returns; the legacy `com.atlassian…[name=…]` string
// form is ignored.
fn sprint_name(issue: &Value) -> Option<String> {
    let names = issue.get("names")?.as_object()?;
    let (field, _) = names
        .iter()
        .find(|(_, name)| name.as_str() == Some("Sprint"))?;
    let sprints = issue.get("fields")?.get(field)?.as_array()?;
    let sprint = sprints
        .iter()
        .find(|sprint| sprint.get("state").and_then(Value::as_str) == Some("active"))
        .or(sprints.last())?;
    sprint.get("name")?.as_str().map(str::to_string)
}

fn parse_transitions(response: &Value) -> Vec<Transition> {
    response
        .get("transitions")
        .and_then(Value::as_array)
        .map(|transitions| {
            transitions
                .iter()
                .filter_map(|transition| {
                    let text = |pointer: &str| {
                        transition
                            .pointer(pointer)
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    };
                    Some(Transition {
                        id: text("/id")?,
                        name: text("/name").unwrap_or_default(),
                        to_status: text("/to/name").unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Jira's `errorMessages` and field `errors` from a 400 body, joined; a generic message when the
/// body has neither.
fn error_message(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return "Jira rejected the request".to_string();
    };
    let mut messages: Vec<String> = value
        .get("errorMessages")
        .and_then(Value::as_array)
        .map(|messages| {
            messages
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if let Some(errors) = value.get("errors").and_then(Value::as_object) {
        messages.extend(
            errors
                .iter()
                .filter_map(|(field, message)| Some(format!("{field}: {}", message.as_str()?))),
        );
    }
    if messages.is_empty() {
        "Jira rejected the request".to_string()
    } else {
        messages.join("; ")
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

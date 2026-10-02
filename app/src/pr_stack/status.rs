//! PR status for the branches of a stack: one `gh pr list` per repo, matched
//! to branches client-side.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context as _, Result};
use serde::Deserialize;

use crate::util::git::run_gh_command;

#[cfg(test)]
#[path = "status_tests.rs"]
mod tests;

/// PRs fetched per refresh, newest first. Older PRs fall off the list.
const PR_LIST_LIMIT: &str = "100";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChecksState {
    Passing,
    Failing,
    Pending,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrStatus {
    pub number: u64,
    pub url: String,
    /// `OPEN`, `CLOSED` or `MERGED`.
    pub state: String,
    pub base: String,
    /// `APPROVED`, `CHANGES_REQUESTED`, `REVIEW_REQUIRED`, or `None`.
    pub review: Option<String>,
    pub checks: Option<ChecksState>,
}

impl PrStatus {
    pub fn is_open(&self) -> bool {
        self.state == "OPEN"
    }

    pub fn is_merged(&self) -> bool {
        self.state == "MERGED"
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhPr {
    number: u64,
    url: String,
    state: String,
    base_ref_name: String,
    head_ref_name: String,
    #[serde(default)]
    review_decision: Option<String>,
    #[serde(default)]
    status_check_rollup: Option<Vec<GhCheck>>,
}

/// A `CheckRun` (status + conclusion) or a `StatusContext` (state).
#[derive(Deserialize)]
struct GhCheck {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

/// Fetches PRs for the repo keyed by head branch.
pub async fn fetch_prs(repo: &Path, path_env: Option<&str>) -> Result<HashMap<String, PrStatus>> {
    let out = run_gh_command(
        repo,
        &[
            "pr",
            "list",
            "--state",
            "all",
            "--limit",
            PR_LIST_LIMIT,
            "--json",
            "number,url,state,baseRefName,headRefName,reviewDecision,statusCheckRollup,mergedAt",
        ],
        path_env,
    )
    .await?;
    parse_pr_list(&out)
}

/// Parses `gh pr list --json` output. When a branch has several PRs the
/// newest (first listed) wins.
pub fn parse_pr_list(json: &str) -> Result<HashMap<String, PrStatus>> {
    let prs: Vec<GhPr> = serde_json::from_str(json).context("Failed to parse gh pr list")?;
    let mut by_head = HashMap::new();
    for pr in prs {
        by_head.entry(pr.head_ref_name).or_insert_with(|| PrStatus {
            number: pr.number,
            url: pr.url,
            state: pr.state,
            base: pr.base_ref_name,
            review: pr.review_decision.filter(|r| !r.is_empty()),
            checks: pr.status_check_rollup.as_deref().and_then(summarize_checks),
        });
    }
    Ok(by_head)
}

fn summarize_checks(checks: &[GhCheck]) -> Option<ChecksState> {
    if checks.is_empty() {
        return None;
    }
    let outcome = |check: &GhCheck| {
        let result = check.conclusion.as_deref().or(check.state.as_deref());
        match result {
            Some(
                "FAILURE" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED"
                | "STARTUP_FAILURE",
            ) => ChecksState::Failing,
            Some("PENDING" | "EXPECTED") | None => ChecksState::Pending,
            _ if check.status.as_deref().is_some_and(|s| s != "COMPLETED") => ChecksState::Pending,
            _ => ChecksState::Passing,
        }
    };
    let outcomes: Vec<ChecksState> = checks.iter().map(outcome).collect();
    Some(if outcomes.contains(&ChecksState::Failing) {
        ChecksState::Failing
    } else if outcomes.contains(&ChecksState::Pending) {
        ChecksState::Pending
    } else {
        ChecksState::Passing
    })
}

/// Short branch name a PR base is compared against (`origin/main` -> `main`).
pub fn base_name(parent: &str) -> &str {
    parent.strip_prefix("origin/").unwrap_or(parent)
}

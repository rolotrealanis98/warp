//! Pull request snapshots from `gh`, the events between two snapshots, and the message the agent
//! receives for them. Pure: the polling itself lives in [`super::model`].

use std::collections::HashSet;
use std::fmt::Write as _;

use serde::Deserialize;

use super::PrRef;
use super::mirror::{Login, ReviewComment};

/// Fields requested from `gh pr view --json` on every poll.
pub(crate) const PR_VIEW_FIELDS: &str =
    "headRefOid,commits,reviews,comments,statusCheckRollup,reviewDecision";

/// Longest comment or review body forwarded to the agent; the link has the rest.
const MAX_BODY_CHARS: usize = 1500;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct PrViewJson {
    head_ref_oid: String,
    commits: Vec<CommitJson>,
    reviews: Vec<ReviewJson>,
    comments: Vec<IssueCommentJson>,
    status_check_rollup: Vec<CheckJson>,
    review_decision: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct CommitJson {
    oid: String,
    message_headline: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ReviewJson {
    id: String,
    author: Option<Login>,
    state: String,
    body: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct IssueCommentJson {
    id: String,
    author: Option<Login>,
    body: String,
}

/// A `CheckRun` (`name`, `status`, `conclusion`) or a `StatusContext` (`context`, `state`).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CheckJson {
    name: Option<String>,
    context: Option<String>,
    status: Option<String>,
    conclusion: Option<String>,
    state: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Commit {
    pub oid: String,
    pub headline: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Review {
    pub id: String,
    pub author: String,
    pub state: String,
    pub body: String,
}

/// A conversation comment (`path` is `None`) or a line comment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Comment {
    pub id: String,
    pub author: String,
    pub path: Option<String>,
    pub line: Option<u64>,
    pub body: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChecksState {
    NoChecks,
    Pending,
    Passing,
    Failing,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ChecksSummary {
    pub passed: usize,
    pub pending: usize,
    /// Names of the failed checks.
    pub failed: Vec<String>,
}

impl ChecksSummary {
    pub(crate) fn state(&self) -> ChecksState {
        if !self.failed.is_empty() {
            ChecksState::Failing
        } else if self.pending > 0 {
            ChecksState::Pending
        } else if self.passed > 0 {
            ChecksState::Passing
        } else {
            ChecksState::NoChecks
        }
    }
}

/// What the watcher knows about a pull request after one poll.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PrSnapshot {
    pub head_oid: String,
    pub commits: Vec<Commit>,
    pub reviews: Vec<Review>,
    pub comments: Vec<Comment>,
    pub checks: ChecksSummary,
    /// `APPROVED`, `CHANGES_REQUESTED` or `REVIEW_REQUIRED`; `None` when no review is required.
    pub review_decision: Option<String>,
}

impl PrSnapshot {
    /// Builds a snapshot from `gh pr view --json` [`PR_VIEW_FIELDS`] output and the pull
    /// request's line comments.
    pub(crate) fn parse(
        pr_view_json: &str,
        review_comments: &[ReviewComment],
    ) -> serde_json::Result<Self> {
        let view: PrViewJson = serde_json::from_str(pr_view_json)?;
        let login = |author: Option<Login>| author.map(|author| author.login).unwrap_or_default();

        let mut comments: Vec<Comment> = view
            .comments
            .into_iter()
            .map(|comment| Comment {
                id: comment.id,
                author: login(comment.author),
                path: None,
                line: None,
                body: comment.body,
            })
            .collect();
        comments.extend(review_comments.iter().map(|comment| Comment {
            id: comment.id.to_string(),
            author: comment.author().to_string(),
            path: Some(comment.path.clone()).filter(|path| !path.is_empty()),
            line: comment.line.or(comment.original_line),
            body: comment.body.clone(),
        }));

        Ok(Self {
            head_oid: view.head_ref_oid,
            commits: view
                .commits
                .into_iter()
                .map(|commit| Commit {
                    oid: commit.oid,
                    headline: commit.message_headline,
                })
                .collect(),
            reviews: view
                .reviews
                .into_iter()
                .map(|review| Review {
                    id: review.id,
                    author: login(review.author),
                    state: review.state,
                    body: review.body,
                })
                .collect(),
            comments,
            checks: summarize_checks(&view.status_check_rollup),
            review_decision: view.review_decision.filter(|decision| !decision.is_empty()),
        })
    }
}

fn summarize_checks(checks: &[CheckJson]) -> ChecksSummary {
    let mut summary = ChecksSummary::default();
    for check in checks {
        let name = check
            .name
            .clone()
            .or_else(|| check.context.clone())
            .unwrap_or_default();
        let outcome = match (&check.state, &check.status, &check.conclusion) {
            // StatusContext.
            (Some(state), _, _) => match state.as_str() {
                "SUCCESS" => ChecksState::Passing,
                "PENDING" | "EXPECTED" => ChecksState::Pending,
                _ => ChecksState::Failing,
            },
            // CheckRun that has not finished.
            (None, Some(status), _) if status != "COMPLETED" => ChecksState::Pending,
            (None, _, Some(conclusion)) => match conclusion.as_str() {
                "SUCCESS" | "NEUTRAL" | "SKIPPED" => ChecksState::Passing,
                "" => ChecksState::Pending,
                _ => ChecksState::Failing,
            },
            (None, _, None) => ChecksState::Pending,
        };
        match outcome {
            ChecksState::Passing => summary.passed += 1,
            ChecksState::Pending => summary.pending += 1,
            ChecksState::Failing => summary.failed.push(name),
            ChecksState::NoChecks => {}
        }
    }
    summary
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PrEvent {
    /// The head moved; `headlines` are the commits that were not there before.
    NewCommits {
        head: String,
        headlines: Vec<String>,
    },
    NewReviewComment {
        author: String,
        path: Option<String>,
        line: Option<u64>,
        body: String,
    },
    NewReview {
        author: String,
        state: String,
        body: String,
    },
    /// Checks finished: they now all pass, or at least one failed.
    ChecksChanged(ChecksSummary),
}

/// The events between two snapshots. Comments and reviews by `viewer` (the `gh` user, which is
/// what the agent posts as) are left out so the agent is not told about its own output.
pub(crate) fn diff_snapshots(
    old: &PrSnapshot,
    new: &PrSnapshot,
    viewer: Option<&str>,
) -> Vec<PrEvent> {
    let is_viewer = |author: &str| viewer.is_some_and(|viewer| author.eq_ignore_ascii_case(viewer));
    let mut events = Vec::new();

    if !new.head_oid.is_empty() && new.head_oid != old.head_oid {
        let known: HashSet<&str> = old
            .commits
            .iter()
            .map(|commit| commit.oid.as_str())
            .collect();
        events.push(PrEvent::NewCommits {
            head: short_oid(&new.head_oid).to_string(),
            headlines: new
                .commits
                .iter()
                .filter(|commit| !known.contains(commit.oid.as_str()))
                .map(|commit| commit.headline.clone())
                .collect(),
        });
    }

    let known: HashSet<&str> = old
        .reviews
        .iter()
        .map(|review| review.id.as_str())
        .collect();
    for review in &new.reviews {
        // A body-less COMMENTED review only wraps line comments, which are reported themselves.
        let is_comment_wrapper = review.state == "COMMENTED" && review.body.trim().is_empty();
        if known.contains(review.id.as_str())
            || is_viewer(&review.author)
            || review.state == "PENDING"
            || is_comment_wrapper
        {
            continue;
        }
        events.push(PrEvent::NewReview {
            author: review.author.clone(),
            state: review.state.clone(),
            body: review.body.clone(),
        });
    }

    let known: HashSet<&str> = old
        .comments
        .iter()
        .map(|comment| comment.id.as_str())
        .collect();
    for comment in &new.comments {
        if known.contains(comment.id.as_str()) || is_viewer(&comment.author) {
            continue;
        }
        events.push(PrEvent::NewReviewComment {
            author: comment.author.clone(),
            path: comment.path.clone(),
            line: comment.line,
            body: comment.body.clone(),
        });
    }

    let state = new.checks.state();
    if state != old.checks.state() && matches!(state, ChecksState::Passing | ChecksState::Failing) {
        events.push(PrEvent::ChecksChanged(new.checks.clone()));
    }
    events
}

/// The message the agent receives for `events`, one bullet per event.
pub(crate) fn format_events(pr: &PrRef, events: &[PrEvent]) -> String {
    let mut message = format!("Update on pull request {pr}:");
    for event in events {
        match event {
            PrEvent::NewCommits { head, headlines } => match headlines.len() {
                0 => {
                    let _ = write!(message, "\n- The branch head moved to {head}.");
                }
                count => {
                    let noun = if count == 1 { "commit" } else { "commits" };
                    let _ = write!(
                        message,
                        "\n- {count} new {noun} (head {head}): {}",
                        headlines.join("; ")
                    );
                }
            },
            PrEvent::NewReviewComment {
                author,
                path,
                line,
                body,
            } => {
                let place = match (path, line) {
                    (Some(path), Some(line)) => format!(" on {path}:{line}"),
                    (Some(path), None) => format!(" on {path}"),
                    (None, _) => String::new(),
                };
                let _ = write!(message, "\n- New comment from {author}{place}:");
                push_quoted(&mut message, body);
            }
            PrEvent::NewReview {
                author,
                state,
                body,
            } => {
                let _ = write!(
                    message,
                    "\n- {author} reviewed: {}.",
                    state.to_lowercase().replace('_', " ")
                );
                push_quoted(&mut message, body);
            }
            PrEvent::ChecksChanged(checks) => {
                if checks.failed.is_empty() {
                    let _ = write!(message, "\n- All {} checks passed.", checks.passed);
                } else {
                    let _ = write!(
                        message,
                        "\n- Checks failed: {} ({} passed, {} pending).",
                        checks.failed.join(", "),
                        checks.passed,
                        checks.pending
                    );
                }
            }
        }
    }
    message
}

/// Appends `body` as an indented quote, cut at [`MAX_BODY_CHARS`].
fn push_quoted(message: &mut String, body: &str) {
    let body = body.trim();
    if body.is_empty() {
        return;
    }
    let mut cut: String = body.chars().take(MAX_BODY_CHARS).collect();
    if cut.len() < body.len() {
        cut.push('…');
    }
    for line in cut.lines() {
        let _ = write!(message, "\n  > {line}");
    }
}

fn short_oid(oid: &str) -> &str {
    oid.get(..7).unwrap_or(oid)
}

#[cfg(test)]
#[path = "watcher_tests.rs"]
mod tests;

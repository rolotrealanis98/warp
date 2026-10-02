//! Pull request line comments from `gh api repos/{owner}/{repo}/pulls/{n}/comments`, and their
//! conversion into review comments for the Code Review panel.

use std::collections::HashSet;

use ai::agent::action::{
    CommentSide, InsertReviewComment, InsertedCommentLine, InsertedCommentLocation,
};
use serde::Deserialize;

use super::PrRef;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub(crate) struct Login {
    #[serde(default)]
    pub login: String,
}

/// One line (or file) comment on a pull request, as the REST API returns it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct ReviewComment {
    pub id: u64,
    #[serde(default)]
    pub user: Option<Login>,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub path: String,
    /// Line in the current diff; `None` once the comment is outdated.
    #[serde(default)]
    pub line: Option<u64>,
    /// Line in the diff the comment was made on, which `diff_hunk` ends at.
    #[serde(default)]
    pub original_line: Option<u64>,
    #[serde(default)]
    pub side: Option<String>,
    #[serde(default)]
    pub diff_hunk: String,
    #[serde(default)]
    pub in_reply_to_id: Option<u64>,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub updated_at: String,
}

impl ReviewComment {
    pub(crate) fn author(&self) -> &str {
        self.user.as_ref().map_or("", |user| user.login.as_str())
    }

    /// The comment in the shape the Code Review panel imports
    /// (`convert_insert_review_comments`), anchored where its diff hunk ends.
    pub(crate) fn to_insert_review_comment(&self) -> InsertReviewComment {
        let line = self
            .original_line
            .or(self.line)
            .map(|line| line as usize)
            .map(|line| InsertedCommentLine {
                comment_line_range: line..line + 1,
                diff_hunk_line_range: line..line + 1,
                diff_hunk_text: self.diff_hunk.clone(),
                side: Some(if self.side.as_deref() == Some("LEFT") {
                    CommentSide::Left
                } else {
                    CommentSide::Right
                }),
            });
        InsertReviewComment {
            comment_id: self.id.to_string(),
            author: self.author().to_string(),
            last_modified_timestamp: self.updated_at.clone(),
            comment_body: self.body.clone(),
            parent_comment_id: self.in_reply_to_id.map(|id| id.to_string()),
            comment_location: (!self.path.is_empty()).then(|| InsertedCommentLocation {
                relative_file_path: self.path.clone(),
                line,
            }),
            html_url: (!self.html_url.is_empty()).then(|| self.html_url.clone()),
        }
    }
}

/// `gh api` arguments listing every line comment of `pr`.
pub(crate) fn review_comments_args(pr: &PrRef) -> Vec<String> {
    vec![
        "api".to_string(),
        "--paginate".to_string(),
        format!(
            "repos/{}/{}/pulls/{}/comments?per_page=100",
            pr.owner, pr.repo, pr.number
        ),
    ]
}

/// Parses `gh api --paginate` output, which is one JSON array per page written back to back.
pub(crate) fn parse_review_comments(json: &str) -> serde_json::Result<Vec<ReviewComment>> {
    let pages = serde_json::Deserializer::from_str(json)
        .into_iter::<Vec<ReviewComment>>()
        .collect::<serde_json::Result<Vec<_>>>()?;
    Ok(pages.into_iter().flatten().collect())
}

/// Comments written by `viewer` (the `gh` user, so the agent) that are not in `mirrored` yet.
pub(crate) fn comments_to_mirror<'a>(
    comments: &'a [ReviewComment],
    viewer: &str,
    mirrored: &HashSet<u64>,
) -> Vec<&'a ReviewComment> {
    comments
        .iter()
        .filter(|comment| comment.author().eq_ignore_ascii_case(viewer))
        .filter(|comment| !mirrored.contains(&comment.id))
        .collect()
}

#[cfg(test)]
#[path = "mirror_tests.rs"]
mod tests;

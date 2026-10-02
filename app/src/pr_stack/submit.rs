//! Creating the PR for a stack branch: title/body resolution, push, `gh pr
//! create` against the stack parent, and the stack footer kept in every open
//! PR of the stack.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use warp_util::git::run_git_command;

use super::stack::Stack;
use super::status::{PrStatus, base_name};
use crate::util::git::{PrInfo, create_pr_with_base, run_gh_command, run_push};

#[cfg(test)]
#[path = "submit_tests.rs"]
mod tests;

const FOOTER_START: &str = "<!-- warp-stack -->";
const FOOTER_END: &str = "<!-- /warp-stack -->";

/// Resolves the PR body file for `branch`. A template starting with `.git/`
/// lands in the git common dir (shared by worktrees); other relative paths
/// are relative to the repo root.
pub fn body_file_path(template: &str, branch: &str, repo: &Path, common_dir: &Path) -> PathBuf {
    let path = template.replace("{{branch}}", branch);
    match path.strip_prefix(".git/") {
        Some(rest) => common_dir.join(rest),
        None => repo.join(path),
    }
}

/// Splits a PR body file into (title, body): the first non-empty line (with
/// any leading `#` stripped) is the title, the rest is the body.
pub fn parse_body_file(contents: &str) -> Option<(String, String)> {
    let mut lines = contents.lines().skip_while(|l| l.trim().is_empty());
    let title = lines
        .next()?
        .trim()
        .trim_start_matches('#')
        .trim()
        .to_string();
    if title.is_empty() {
        return None;
    }
    let body = lines.collect::<Vec<_>>().join("\n").trim().to_string();
    Some((title, body))
}

/// A commit's subject and body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitMessage {
    pub subject: String,
    pub body: String,
}

/// Commits on `branch` since `parent`, oldest first.
pub async fn commits_since(repo: &Path, parent: &str, branch: &str) -> Result<Vec<CommitMessage>> {
    let range = format!("{parent}..{branch}");
    let out = run_git_command(repo, &["log", "--reverse", "--format=%s%x1f%b%x1e", &range]).await?;
    Ok(out
        .split('\u{1e}')
        .filter_map(|record| {
            let (subject, body) = record.trim_start_matches('\n').split_once('\u{1f}')?;
            Some(CommitMessage {
                subject: subject.trim().to_string(),
                body: body.trim().to_string(),
            })
        })
        .collect())
}

/// Fallback title for branches the task launcher did not start (those use
/// the task's key and short title): `KEY short title` when the branch name
/// carries an issue key (`EXAMPLE-123-short-title`, optionally under a
/// `prefix/`), otherwise the oldest commit subject, otherwise the branch name.
pub fn fallback_title(branch: &str, commits: &[CommitMessage]) -> String {
    let name = branch.rsplit('/').next().unwrap_or(branch);
    let mut parts = name.splitn(3, ['-', '_']);
    if let (Some(project), Some(number), rest) = (parts.next(), parts.next(), parts.next())
        && project.len() > 1
        && project
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        && project.starts_with(|c: char| c.is_ascii_uppercase())
        && !number.is_empty()
        && number.chars().all(|c| c.is_ascii_digit())
    {
        let key = format!("{project}-{number}");
        let short = rest.unwrap_or_default().replace(['-', '_'], " ");
        let short = short.trim();
        if short.is_empty() {
            return commits
                .first()
                .map_or(key.clone(), |c| format!("{key} {}", c.subject));
        }
        let mut chars = short.chars();
        let first = chars.next().map(|c| c.to_uppercase().collect::<String>());
        return format!("{key} {}{}", first.unwrap_or_default(), chars.as_str());
    }
    commits
        .first()
        .map_or_else(|| branch.to_string(), |c| c.subject.clone())
}

/// Fallback body: each commit's subject as a bullet with its body indented
/// below it.
pub fn fallback_body(commits: &[CommitMessage]) -> String {
    let mut body = String::new();
    for commit in commits {
        body.push_str(&format!("- {}\n", commit.subject));
        for line in commit.body.lines() {
            if line.trim().is_empty() {
                body.push('\n');
            } else {
                body.push_str(&format!("  {line}\n"));
            }
        }
    }
    body.trim_end().to_string()
}

/// The stack footer for the PR of `current`, listing branches bottom → top.
pub fn render_stack_footer(
    stack: &Stack,
    prs: &HashMap<String, PrStatus>,
    current: &str,
) -> String {
    let branches: Vec<_> = stack
        .branches
        .iter()
        .filter(|b| !prs.get(&b.name).is_some_and(PrStatus::is_merged))
        .collect();
    let position = branches
        .iter()
        .position(|b| b.name == current)
        .map_or(0, |i| i + 1);
    let mut footer = format!(
        "{FOOTER_START}\n**Stack {position}/{}** (bottom to top, onto `{}`)\n",
        branches.len(),
        base_name(&stack.target)
    );
    for (i, branch) in branches.iter().enumerate() {
        let entry = match prs.get(&branch.name).filter(|pr| pr.is_open()) {
            Some(pr) => format!("#{} `{}`", pr.number, branch.name),
            None => format!("`{}` (no PR yet)", branch.name),
        };
        if branch.name == current {
            footer.push_str(&format!("{}. **{entry}** (this PR)\n", i + 1));
        } else {
            footer.push_str(&format!("{}. {entry}\n", i + 1));
        }
    }
    footer.push_str(FOOTER_END);
    footer
}

/// Replaces the stack footer in `body`, or appends it when absent.
pub fn replace_stack_footer(body: &str, footer: &str) -> String {
    if let Some(start) = body.find(FOOTER_START)
        && let Some(end) = body[start..].find(FOOTER_END)
    {
        let end = start + end + FOOTER_END.len();
        return format!("{}{footer}{}", &body[..start], &body[end..]);
    }
    let body = body.trim_end();
    if body.is_empty() {
        footer.to_string()
    } else {
        format!("{body}\n\n{footer}")
    }
}

/// Pushes `branch` and opens its PR against `base`.
pub async fn create_pr(
    repo: &Path,
    branch: &str,
    base: &str,
    title: &str,
    body: &str,
    path_env: Option<&str>,
) -> Result<PrInfo> {
    run_push(repo, branch, path_env).await?;
    create_pr_with_base(repo, Some(branch), base, Some(title), Some(body), path_env).await
}

/// Rewrites the stack footer in every open PR of the stack.
pub async fn rewrite_footers(
    repo: &Path,
    stack: &Stack,
    prs: &HashMap<String, PrStatus>,
    path_env: Option<&str>,
) -> Result<()> {
    for branch in &stack.branches {
        let Some(pr) = prs.get(&branch.name).filter(|pr| pr.is_open()) else {
            continue;
        };
        let number = pr.number.to_string();
        let body = run_gh_command(
            repo,
            &["pr", "view", &number, "--json", "body", "--jq", ".body"],
            path_env,
        )
        .await?;
        let body = body.trim_end_matches('\n');
        let updated = replace_stack_footer(body, &render_stack_footer(stack, prs, &branch.name));
        if updated != body {
            run_gh_command(repo, &["pr", "edit", &number, "--body", &updated], path_env).await?;
        }
    }
    Ok(())
}

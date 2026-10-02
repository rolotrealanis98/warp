//! Sync and restack: rebase stack branches onto their parents, push the ones
//! with PRs, and retarget PRs whose parent merged. Conflicts stop the run and
//! are left for the user or their agent; nothing is resolved automatically.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use warp_util::git::{run_git_command, run_git_command_with_env};

use super::stack::{Stack, is_ancestor, load_stack_file, save_stack_file};
use super::status::{PrStatus, base_name};
use crate::util::git::run_gh_command;

#[cfg(test)]
#[path = "restack_tests.rs"]
mod tests;

/// What a restack did.
#[derive(Debug)]
pub enum RestackOutcome {
    Done {
        /// Branches whose PR base was changed to the target after their
        /// parent merged.
        retargeted: Vec<String>,
    },
    Conflict {
        branch: String,
        onto: String,
        worktree: PathBuf,
        /// Branches above the conflicting one that were not restacked.
        remaining: Vec<String>,
    },
}

/// Maps checked-out branch names to their worktree paths.
pub async fn worktrees(repo: &Path) -> Result<HashMap<String, PathBuf>> {
    let out = run_git_command(repo, &["worktree", "list", "--porcelain"]).await?;
    let mut map = HashMap::new();
    let mut path: Option<PathBuf> = None;
    for line in out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(PathBuf::from(p));
        } else if let Some(branch) = line.strip_prefix("branch refs/heads/")
            && let Some(path) = &path
        {
            map.insert(branch.to_string(), path.clone());
        }
    }
    Ok(map)
}

async fn rebase_in_progress(dir: &Path) -> bool {
    for marker in ["rebase-merge", "rebase-apply"] {
        if let Ok(out) = run_git_command(dir, &["rev-parse", "--git-path", marker]).await {
            let path = PathBuf::from(out.trim());
            let path = if path.is_absolute() {
                path
            } else {
                dir.join(path)
            };
            if path.exists() {
                return true;
            }
        }
    }
    false
}

/// Where `branch`'s own commits start on top of `parent`. `--fork-point`
/// consults the parent's reflog, so it still finds the old parent tip after
/// the parent was rebased (e.g. by an earlier, interrupted restack).
async fn old_base(repo: &Path, parent: &str, branch: &str) -> Result<String> {
    let out = match run_git_command(repo, &["merge-base", "--fork-point", parent, branch]).await {
        Ok(out) if !out.trim().is_empty() => out,
        _ => run_git_command(repo, &["merge-base", parent, branch]).await?,
    };
    Ok(out.trim().to_string())
}

async fn rev_parse(repo: &Path, rev: &str) -> Result<String> {
    Ok(run_git_command(repo, &["rev-parse", rev])
        .await?
        .trim()
        .to_string())
}

fn merged_branches<'a>(stack: &'a Stack, prs: &HashMap<String, PrStatus>) -> HashSet<&'a str> {
    stack
        .branches
        .iter()
        .filter(|b| prs.get(&b.name).is_some_and(PrStatus::is_merged))
        .map(|b| b.name.as_str())
        .collect()
}

/// Restacks branches `from..` of `stack` (bottom = 0): each is rebased onto
/// its nearest unmerged parent (or the target), keeping only its own commits
/// (`git rebase --onto <parent> <old-base> <branch>`). Branches with an open
/// PR are force-pushed with lease. Merged branches are dropped. With `only`,
/// just branch `from` is synced.
///
/// The stack shape is recorded in the stack file while this runs and kept
/// after a conflict, so the stack view survives the half-rebased state.
pub async fn restack(
    repo: &Path,
    stack: &Stack,
    from: usize,
    only: bool,
    prs: &HashMap<String, PrStatus>,
    path_env: Option<&str>,
) -> Result<RestackOutcome> {
    let mut file = load_stack_file(repo).await?;
    file.record(stack);
    file.restacking = true;
    save_stack_file(repo, &file).await?;

    let outcome = restack_branches(repo, stack, from, only, prs, path_env).await;
    if !matches!(outcome, Ok(RestackOutcome::Conflict { .. })) {
        let merged = merged_branches(stack, prs);
        file.restacking = false;
        file.pins
            .retain(|_, parent| !merged.contains(parent.as_str()));
        save_stack_file(repo, &file).await?;
    }
    outcome
}

async fn restack_branches(
    repo: &Path,
    stack: &Stack,
    from: usize,
    only: bool,
    prs: &HashMap<String, PrStatus>,
    path_env: Option<&str>,
) -> Result<RestackOutcome> {
    if let Err(err) = run_git_command_with_env(repo, &["fetch", "origin"], path_env).await {
        log::warn!("PR stack: fetch before restack failed, using local refs: {err:#}");
    }

    let merged = merged_branches(stack, prs);
    let end = if only {
        (from + 1).min(stack.branches.len())
    } else {
        stack.branches.len()
    };
    let worktrees = worktrees(repo).await?;
    // Branches not checked out anywhere are rebased in `repo`'s checkout, which
    // is switched back afterwards (`--detach` when it was not on a branch).
    let original_head =
        match run_git_command(repo, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await {
            Ok(branch) => vec![branch.trim().to_string()],
            Err(_) => vec!["--detach".to_string(), rev_parse(repo, "HEAD").await?],
        };
    let mut switched_repo_checkout = false;

    // Old bases are computed before anything moves.
    struct Step {
        branch: String,
        onto: String,
        old_base: String,
        parent_merged: bool,
    }
    let mut steps = Vec::new();
    for index in from..end {
        let branch = &stack.branches[index];
        if merged.contains(branch.name.as_str()) {
            continue;
        }
        let onto = stack.branches[..index]
            .iter()
            .rev()
            .find(|b| !merged.contains(b.name.as_str()))
            .map_or(stack.target.clone(), |b| b.name.clone());
        steps.push(Step {
            old_base: old_base(repo, &branch.parent, &branch.name).await?,
            parent_merged: merged.contains(branch.parent.as_str()),
            branch: branch.name.clone(),
            onto,
        });
    }

    let mut retargeted = Vec::new();
    for (i, step) in steps.iter().enumerate() {
        // Skipped when already on top of its new parent (e.g. resumed after
        // a conflict was resolved by hand).
        if !is_ancestor(repo, &step.onto, &step.branch).await {
            let dir = match worktrees.get(&step.branch) {
                Some(dir) => dir.clone(),
                None => {
                    switched_repo_checkout = true;
                    repo.to_path_buf()
                }
            };
            let result = run_git_command_with_env(
                &dir,
                &["rebase", "--onto", &step.onto, &step.old_base, &step.branch],
                path_env,
            )
            .await;
            // `git rebase` exits 1 with stdout on conflicts, which the runner
            // reports as success, so check the repo state either way.
            if rebase_in_progress(&dir).await {
                return Ok(RestackOutcome::Conflict {
                    branch: step.branch.clone(),
                    onto: step.onto.clone(),
                    worktree: dir,
                    remaining: steps[i + 1..].iter().map(|s| s.branch.clone()).collect(),
                });
            }
            result.map_err(|err| anyhow!("Rebasing {} failed: {err:#}", step.branch))?;
        }

        let Some(pr) = prs.get(&step.branch).filter(|pr| pr.is_open()) else {
            continue;
        };
        let remote = rev_parse(repo, &format!("origin/{}", step.branch))
            .await
            .ok();
        if remote != Some(rev_parse(repo, &step.branch).await?) {
            run_git_command_with_env(
                repo,
                &["push", "--force-with-lease", "origin", &step.branch],
                path_env,
            )
            .await?;
        }
        if step.parent_merged && pr.base != base_name(&step.onto) {
            let number = pr.number.to_string();
            run_gh_command(
                repo,
                &["pr", "edit", &number, "--base", base_name(&step.onto)],
                path_env,
            )
            .await?;
            retargeted.push(step.branch.clone());
        }
    }

    if switched_repo_checkout {
        let mut args = vec!["switch"];
        args.extend(original_head.iter().map(String::as_str));
        run_git_command(repo, &args).await?;
    }
    Ok(RestackOutcome::Done { retargeted })
}

/// The message handed to the agent pane when a rebase stops on conflicts.
pub fn conflict_handoff_message(
    branch: &str,
    onto: &str,
    worktree: &Path,
    remaining: &[String],
) -> String {
    let mut text = format!(
        "Restacking the PR stack stopped on a rebase conflict.\n\
         Branch: {branch} (rebasing onto {onto})\n\
         Worktree: {}\n\
         Resolve the conflicts there, then run `git rebase --continue` \
         (or `git rebase --abort` to undo). Do not push; once the rebase is \
         done, use \"Restack from here\" in the PR stack view to push and \
         restack the rest.",
        worktree.display()
    );
    if !remaining.is_empty() {
        text.push_str(&format!(
            "\nStill to restack after this: {}",
            remaining.join(", ")
        ));
    }
    text
}

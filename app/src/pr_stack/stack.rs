//! Stack inference: which local branches form a chain from the top branch
//! down to the upstream target, inferred from git ancestry.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow};
use futures::future::join_all;
use serde::{Deserialize, Serialize};
use warp_util::git::run_git_command;

use crate::util::git::get_all_branches;

#[cfg(test)]
#[path = "stack_tests.rs"]
mod tests;

/// Upper bound on local branches considered for inference.
const MAX_BRANCHES: usize = 200;

/// Name of the untracked stack file inside the git common dir.
const STACK_FILE_NAME: &str = "warp-stack.json";

/// One branch of the stack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackBranch {
    pub name: String,
    /// The branch below this one, or the stack target for the bottom branch.
    pub parent: String,
    /// Tip commit SHA.
    pub tip: String,
    /// More than one branch qualified as the parent; `parent` won by having
    /// the longest chain below it.
    pub ambiguous: bool,
    /// The other parent candidates when `ambiguous`.
    pub alternatives: Vec<String>,
    /// The parent came from a pin in the stack file.
    pub pinned: bool,
    /// The parent has commits that are not in this branch (for the bottom
    /// branch: the target advanced, i.e. the stack needs a sync).
    pub behind: bool,
}

/// A stack of branches, ordered bottom (closest to the target) to top.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stack {
    pub target: String,
    pub target_tip: String,
    pub branches: Vec<StackBranch>,
}

impl Stack {
    pub fn index_of(&self, branch: &str) -> Option<usize> {
        self.branches.iter().position(|b| b.name == branch)
    }
}

/// Persisted per-repo stack state: `.git/warp-stack.json`. It lives inside the
/// git common dir so it is shared by worktrees and never committed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackFile {
    /// Overrides the target for this repo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Manual parent pins: child branch -> parent branch (or the target).
    #[serde(default)]
    pub pins: BTreeMap<String, String>,
    /// Last known parent of each stacked branch, recorded on every load.
    /// Ancestry alone loses a parent once it gets new commits or is rebased;
    /// the recorded parent still applies while the two share commits beyond
    /// the target.
    #[serde(default)]
    pub parents: BTreeMap<String, String>,
    /// A restack stopped part-way (on a conflict). Until one finishes,
    /// `parents` apply even where rebased branches no longer share commits.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub restacking: bool,
}

impl StackFile {
    /// Records the stack's parent relations; returns whether anything changed.
    pub fn record(&mut self, stack: &Stack) -> bool {
        let mut changed = false;
        for branch in &stack.branches {
            if self.parents.get(&branch.name) != Some(&branch.parent) {
                self.parents
                    .insert(branch.name.clone(), branch.parent.clone());
                changed = true;
            }
        }
        changed
    }
}

/// Returns the absolute git common dir (`.git` of the main worktree).
pub async fn git_common_dir(repo: &Path) -> Result<PathBuf> {
    let out = run_git_command(repo, &["rev-parse", "--git-common-dir"]).await?;
    let dir = PathBuf::from(out.trim());
    Ok(if dir.is_absolute() {
        dir
    } else {
        repo.join(dir)
    })
}

pub async fn load_stack_file(repo: &Path) -> Result<StackFile> {
    let path = git_common_dir(repo).await?.join(STACK_FILE_NAME);
    match async_fs::read_to_string(&path).await {
        Ok(contents) => serde_json::from_str(&contents).context("Invalid stack file"),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(StackFile::default()),
        Err(err) => Err(err).context("Failed to read stack file"),
    }
}

pub async fn save_stack_file(repo: &Path, file: &StackFile) -> Result<()> {
    let path = git_common_dir(repo).await?.join(STACK_FILE_NAME);
    let contents = serde_json::to_string_pretty(file)?;
    async_fs::write(&path, contents)
        .await
        .context("Failed to write stack file")
}

/// `git merge-base --is-ancestor`: true when `ancestor` is reachable from
/// `descendant` (also true for equal commits).
pub async fn is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> bool {
    run_git_command(repo, &["merge-base", "--is-ancestor", ancestor, descendant])
        .await
        .is_ok()
}

/// Whether `a` and `b` share commits that are not in `target`.
async fn share_commits_beyond(repo: &Path, a: &str, b: &str, target: &str) -> bool {
    match run_git_command(repo, &["merge-base", a, b]).await {
        Ok(base) => !is_ancestor(repo, base.trim(), target).await,
        Err(_) => false,
    }
}

async fn rev_parse(repo: &Path, refs: &[&str]) -> Result<Vec<String>> {
    let mut args = vec!["rev-parse"];
    args.extend_from_slice(refs);
    let out = run_git_command(repo, &args).await?;
    let shas: Vec<String> = out.lines().map(|l| l.trim().to_string()).collect();
    if shas.len() != refs.len() {
        return Err(anyhow!("rev-parse returned an unexpected number of lines"));
    }
    Ok(shas)
}

/// Infers the stack ending at `top` and sitting on `target`.
///
/// Candidates are local branches reachable from `top` but not from `target`.
/// Each level's parent is, in order: a pin; the recorded parent while it
/// still shares commits beyond the target (unless a candidate sits between
/// them); the closest candidate below, where ties pick the longest chain and
/// are flagged ambiguous; the target.
pub async fn infer_stack(repo: &Path, top: &str, target: &str, file: &StackFile) -> Result<Stack> {
    let target_tip = rev_parse(repo, &[target])
        .await
        .with_context(|| format!("Unknown target {target}"))?
        .remove(0);
    if is_ancestor(repo, top, target).await {
        // The top branch is already part of the target: nothing stacked.
        return Ok(Stack {
            target: target.to_string(),
            target_tip,
            branches: Vec::new(),
        });
    }

    let target_short = target.strip_prefix("origin/").unwrap_or(target);
    let branches: Vec<String> = get_all_branches(repo, Some(MAX_BRANCHES), false)
        .await?
        .into_iter()
        .filter(|b| !b.is_main && b.name != top && b.name != target && b.name != target_short)
        .map(|b| b.name)
        .collect();

    // ponytail: one git process per probe; fine for tens of branches, swap
    // for `for-each-ref --merged/--no-merged` if repos with thousands appear.
    let below_top = filter_async(&branches, |b| is_ancestor(repo, b, top)).await;
    let candidates = filter_async(&below_top, |b| async move {
        !is_ancestor(repo, b, target).await
    })
    .await;

    let known = |name: &str| name == top || branches.iter().any(|b| b == name);
    let recorded: Vec<(String, String)> = file
        .parents
        .iter()
        .filter(|(child, parent)| known(child) && (*parent == target || known(parent)))
        .map(|(child, parent)| (child.clone(), parent.clone()))
        .collect();
    let valid = join_all(recorded.iter().map(|(child, parent)| async move {
        file.restacking
            || parent == target
            || share_commits_beyond(repo, child, parent, target).await
    }))
    .await;
    let recorded: BTreeMap<String, String> = recorded
        .into_iter()
        .zip(valid)
        .filter_map(|(relation, valid)| valid.then_some(relation))
        .collect();

    // Ancestry among every branch the walk can reach (the top's ancestors
    // among the candidates are known).
    let mut nodes: Vec<String> = candidates.clone();
    for parent in recorded.values() {
        if parent != target && parent != top && !nodes.contains(parent) {
            nodes.push(parent.clone());
        }
    }
    let mut names: Vec<&str> = vec![top];
    names.extend(nodes.iter().map(String::as_str));
    let mut tips: HashMap<String, String> = names
        .iter()
        .map(|n| n.to_string())
        .zip(rev_parse(repo, &names).await?)
        .collect();
    let pairs: Vec<(String, String)> = nodes
        .iter()
        .flat_map(|a| {
            nodes
                .iter()
                .filter(move |b| *b != a)
                .map(move |b| (a.clone(), b.clone()))
        })
        .collect();
    let results = join_all(pairs.iter().map(|(a, b)| is_ancestor(repo, a, b))).await;
    let mut ancestry: HashSet<(String, String)> = pairs
        .into_iter()
        .zip(results)
        .filter_map(|(pair, is_anc)| is_anc.then_some(pair))
        .collect();
    ancestry.extend(candidates.iter().map(|c| (c.clone(), top.to_string())));

    let chain = build_chain(
        &Inputs {
            top,
            target,
            candidates: &candidates,
            branches: &branches,
            tips: &tips,
            pins: &file.pins,
            recorded: &recorded,
        },
        |a, b| ancestry.contains(&(a.to_string(), b.to_string())),
    );
    // Pinned parents need not be candidates; fetch their tips too.
    let missing: Vec<&str> = chain
        .iter()
        .map(|link| link.name.as_str())
        .filter(|name| !tips.contains_key(*name))
        .collect();
    if !missing.is_empty() {
        let shas = rev_parse(repo, &missing).await?;
        tips.extend(missing.iter().map(|n| n.to_string()).zip(shas));
    }

    let behind = join_all(
        chain
            .iter()
            .map(|link| async move { !is_ancestor(repo, &link.parent, &link.name).await }),
    )
    .await;

    let branches = chain
        .into_iter()
        .zip(behind)
        .rev()
        .map(|(link, behind)| StackBranch {
            tip: tips.get(&link.name).cloned().unwrap_or_default(),
            name: link.name,
            parent: link.parent,
            ambiguous: !link.alternatives.is_empty(),
            alternatives: link.alternatives,
            pinned: link.pinned,
            behind,
        })
        .collect();
    Ok(Stack {
        target: target.to_string(),
        target_tip,
        branches,
    })
}

async fn filter_async<'a, F, Fut>(items: &'a [String], pred: F) -> Vec<String>
where
    F: Fn(&'a str) -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let keep = join_all(items.iter().map(|i| pred(i))).await;
    items
        .iter()
        .zip(keep)
        .filter(|(_, keep)| *keep)
        .map(|(item, _)| item.clone())
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
struct Link {
    name: String,
    parent: String,
    alternatives: Vec<String>,
    pinned: bool,
}

struct Inputs<'a> {
    top: &'a str,
    target: &'a str,
    /// Branches reachable from `top` but not from `target`.
    candidates: &'a [String],
    /// All local branches except `top` and the target.
    branches: &'a [String],
    tips: &'a HashMap<String, String>,
    pins: &'a BTreeMap<String, String>,
    /// Recorded parents that are still valid.
    recorded: &'a BTreeMap<String, String>,
}

/// Walks from `top` down to `target`, returning links top → bottom.
/// `is_ancestor(a, b)` must answer for pairs of candidates and recorded
/// parents, and for `(candidate, top)`.
fn build_chain(inputs: &Inputs<'_>, is_ancestor: impl Fn(&str, &str) -> bool) -> Vec<Link> {
    let Inputs {
        top,
        target,
        candidates,
        branches,
        tips,
        pins,
        recorded,
    } = *inputs;
    let same_tip = |a: &str, b: &str| tips.get(a).is_some_and(|t| tips.get(b) == Some(t));
    let strictly_below = |a: &str, b: &str| is_ancestor(a, b) && !same_tip(a, b);
    let mut chain = Vec::new();
    let mut visited: HashSet<&str> = HashSet::from([top]);
    let mut current = top;

    loop {
        let pin = pins.get(current).filter(|pin| {
            (*pin == target || branches.contains(pin)) && !visited.contains(pin.as_str())
        });

        // A branch sharing the current tip only counts as its parent for the
        // top branch (a fresh branch created on another one).
        let below: Vec<&str> = candidates
            .iter()
            .map(String::as_str)
            .filter(|c| !visited.contains(c) && is_ancestor(c, current))
            .filter(|c| current == top || !same_tip(c, current))
            .collect();
        let closest: Vec<&str> = below
            .iter()
            .copied()
            .filter(|c| !below.iter().any(|d| strictly_below(c, d)))
            .collect();
        let chain_len = |c: &str| below.iter().filter(|d| strictly_below(d, c)).count();
        // Longest chain wins; ties go to the alphabetically first name.
        let inferred = closest
            .iter()
            .copied()
            .max_by(|a, b| chain_len(a).cmp(&chain_len(b)).then(b.cmp(a)));
        // The recorded parent loses only to a candidate sitting between it
        // and the current branch (e.g. a branch inserted into the stack).
        let recorded = recorded
            .get(current)
            .map(String::as_str)
            .filter(|parent| !visited.contains(parent))
            .filter(|parent| match inferred {
                Some(inferred) => *parent != target && !strictly_below(parent, inferred),
                None => true,
            });

        let (parent, alternatives, pinned) = match (pin, recorded, inferred) {
            (Some(pin), _, _) => (pin.as_str(), Vec::new(), true),
            (None, Some(recorded), _) => (recorded, Vec::new(), false),
            (None, None, Some(inferred)) => (
                inferred,
                closest
                    .iter()
                    .filter(|c| **c != inferred)
                    .map(|c| c.to_string())
                    .collect(),
                false,
            ),
            (None, None, None) => (target, Vec::new(), false),
        };
        chain.push(Link {
            name: current.to_string(),
            parent: parent.to_string(),
            alternatives,
            pinned,
        });
        if parent == target {
            break;
        }
        visited.insert(parent);
        current = parent;
    }
    chain
}

//! Pull request references typed by the user: a GitHub pull request URL or `owner/repo#12`.

use std::fmt;

/// A pull request on github.com.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub(crate) struct PrRef {
    pub owner: String,
    pub repo: String,
    pub number: u64,
}

impl PrRef {
    /// `owner/repo`, the form `gh -R` takes.
    pub(crate) fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }
}

impl fmt::Display for PrRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}#{}", self.owner, self.repo, self.number)
    }
}

/// Parses `https://github.com/owner/repo/pull/12` (scheme optional; trailing segments such as
/// `/files`, a query or a fragment ignored) or `owner/repo#12`. Owner and repository names are
/// restricted to the characters GitHub allows, so the result is safe to put in a shell command.
// ponytail: github.com only; GitHub Enterprise needs a host passed as `-R host/owner/repo`.
pub(crate) fn parse_pr_ref(input: &str) -> Option<PrRef> {
    let input = input.trim();
    let without_scheme = input
        .strip_prefix("https://")
        .or_else(|| input.strip_prefix("http://"))
        .unwrap_or(input);
    let mut segments = without_scheme.split(['/', '?', '#']);
    let host = segments.next()?;
    if !matches!(host, "github.com" | "www.github.com") {
        let (slug, number) = input.split_once('#')?;
        let (owner, repo) = slug.split_once('/')?;
        return pr_ref(owner, repo, number);
    }
    let owner = segments.next()?;
    let repo = segments.next()?;
    if segments.next()? != "pull" {
        return None;
    }
    pr_ref(owner, repo, segments.next()?)
}

fn pr_ref(owner: &str, repo: &str, number: &str) -> Option<PrRef> {
    let is_name = |name: &str| {
        !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    let number = number.trim().parse().ok().filter(|number| *number > 0)?;
    (is_name(owner) && is_name(repo)).then(|| PrRef {
        owner: owner.to_string(),
        repo: repo.to_string(),
        number,
    })
}

#[cfg(test)]
#[path = "parse_tests.rs"]
mod tests;

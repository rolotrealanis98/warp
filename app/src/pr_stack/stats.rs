//! Per-branch, stack-relative change stats: files, +/-, and changed lines
//! split into code / comments / tests / docs / config.

use std::path::Path;

use anyhow::Result;
use globset::{GlobBuilder, GlobMatcher};
use serde::{Deserialize, Serialize};
use warp_util::git::run_git_command;

#[cfg(test)]
#[path = "stats_tests.rs"]
mod tests;

/// Skip comment counting when a branch diff is larger than this.
const MAX_COMMENT_SCAN_BYTES: usize = 8_000_000;

#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[schemars(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum LineBucket {
    Code,
    Tests,
    Docs,
    Config,
}

/// A path rule. Patterns without a `/` match the file name; others match the
/// repo-relative path. The first matching rule wins; unmatched files are code.
#[derive(
    Clone,
    Debug,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
pub struct ClassificationRule {
    pub pattern: String,
    pub bucket: LineBucket,
}

pub fn default_rules() -> Vec<ClassificationRule> {
    let rule = |pattern: &str, bucket| ClassificationRule {
        pattern: pattern.to_string(),
        bucket,
    };
    // ponytail: inline `#[cfg(test)]` modules count as code; detecting them
    // needs content parsing, not path rules.
    vec![
        rule("**/tests/**", LineBucket::Tests),
        rule("**/test/**", LineBucket::Tests),
        rule("**/__tests__/**", LineBucket::Tests),
        rule("*_test.*", LineBucket::Tests),
        rule("*.test.*", LineBucket::Tests),
        rule("*.spec.*", LineBucket::Tests),
        rule("*_tests.rs", LineBucket::Tests),
        rule("test_*.py", LineBucket::Tests),
        rule("*.md", LineBucket::Docs),
        rule("*.rst", LineBucket::Docs),
        rule("*.txt", LineBucket::Docs),
        rule("docs/**", LineBucket::Docs),
        rule("Cargo.toml", LineBucket::Config),
        rule("Cargo.lock", LineBucket::Config),
        rule("package*.json", LineBucket::Config),
        rule("*.lock", LineBucket::Config),
        rule("*-lock.yaml", LineBucket::Config),
        rule("*.toml", LineBucket::Config),
        rule("*.yml", LineBucket::Config),
        rule("*.yaml", LineBucket::Config),
        rule("*.json", LineBucket::Config),
        rule(".github/**", LineBucket::Config),
        rule(".*", LineBucket::Config),
    ]
}

/// Compiled classification rules.
pub struct Classifier {
    rules: Vec<(GlobMatcher, bool, LineBucket)>,
}

impl Classifier {
    /// Invalid patterns are skipped with a warning.
    pub fn new(rules: &[ClassificationRule]) -> Self {
        let rules = rules
            .iter()
            .filter_map(|rule| {
                match GlobBuilder::new(&rule.pattern)
                    .literal_separator(true)
                    .build()
                {
                    Ok(glob) => Some((
                        glob.compile_matcher(),
                        rule.pattern.contains('/'),
                        rule.bucket,
                    )),
                    Err(err) => {
                        log::warn!("Ignoring invalid PR stack classification pattern: {err}");
                        None
                    }
                }
            })
            .collect();
        Self { rules }
    }

    pub fn classify(&self, path: &str) -> LineBucket {
        let file_name = path.rsplit('/').next().unwrap_or(path);
        self.rules
            .iter()
            .find(|(matcher, full_path, _)| {
                matcher.is_match(if *full_path { path } else { file_name })
            })
            .map_or(LineBucket::Code, |(_, _, bucket)| *bucket)
    }
}

/// Changed-line totals for one branch relative to its stack parent. Buckets
/// count added plus removed lines; `code` excludes `comments`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BranchStats {
    pub files: usize,
    pub additions: usize,
    pub deletions: usize,
    pub code: usize,
    pub comments: usize,
    pub tests: usize,
    pub docs: usize,
    pub config: usize,
}

/// Computes stats for `parent...branch` (changes on `branch` since it forked
/// from `parent`).
pub async fn branch_stats(
    repo: &Path,
    parent: &str,
    branch: &str,
    classifier: &Classifier,
) -> Result<BranchStats> {
    let range = format!("{parent}...{branch}");
    let numstat = run_git_command(repo, &["diff", "--numstat", "-z", &range]).await?;
    let mut stats = BranchStats::default();
    let mut code_lines = 0;
    for (path, added, removed) in parse_numstat_z(&numstat) {
        stats.files += 1;
        stats.additions += added;
        stats.deletions += removed;
        match classifier.classify(&path) {
            LineBucket::Code => code_lines += added + removed,
            LineBucket::Tests => stats.tests += added + removed,
            LineBucket::Docs => stats.docs += added + removed,
            LineBucket::Config => stats.config += added + removed,
        }
    }
    if code_lines > 0 {
        let diff = run_git_command(
            repo,
            &["diff", "-U0", "--no-color", "--no-ext-diff", &range],
        )
        .await?;
        if diff.len() <= MAX_COMMENT_SCAN_BYTES {
            stats.comments =
                count_comment_lines(&diff, |path| classifier.classify(path) == LineBucket::Code);
        }
    }
    stats.code = code_lines.saturating_sub(stats.comments);
    Ok(stats)
}

/// Parses `git diff --numstat -z`: `added\tremoved\tpath\0`, or for renames
/// `added\tremoved\t\0old\0new\0`. Binary files report `-` and count as 0.
fn parse_numstat_z(out: &str) -> Vec<(String, usize, usize)> {
    let mut entries = Vec::new();
    let mut tokens = out.split('\0');
    while let Some(token) = tokens.next() {
        let mut fields = token.splitn(3, '\t');
        let (Some(added), Some(removed), Some(path)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let path = if path.is_empty() {
            // Rename: skip the old path, keep the new one.
            tokens.next();
            tokens.next().unwrap_or_default().to_string()
        } else {
            path.to_string()
        };
        entries.push((
            path,
            added.trim().parse().unwrap_or(0),
            removed.trim().parse().unwrap_or(0),
        ));
    }
    entries
}

/// Comment syntax for one file: the language's line prefix (from the
/// `languages` crate) plus a block pair from a small table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommentSyntax {
    line: Option<String>,
    block: Option<(&'static str, &'static str)>,
}

pub fn comment_syntax(path: &str) -> CommentSyntax {
    let line = languages::language_by_local_filename(Path::new(path))
        .and_then(|language| language.comment_prefix.clone());
    let extension = path.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
    let block = match extension {
        "rs" | "c" | "h" | "cc" | "cpp" | "hpp" | "java" | "kt" | "scala" | "js" | "jsx"
        | "mjs" | "cjs" | "ts" | "tsx" | "go" | "swift" | "cs" | "css" | "scss" | "php"
        | "dart" | "sql" => Some(("/*", "*/")),
        "html" | "htm" | "xml" | "vue" | "svelte" => Some(("<!--", "-->")),
        "py" | "pyi" => Some(("\"\"\"", "\"\"\"")),
        "rb" => Some(("=begin", "=end")),
        "lua" => Some(("--[[", "]]")),
        "hs" => Some(("{-", "-}")),
        _ => None,
    };
    CommentSyntax { line, block }
}

/// Counts added and removed comment lines in a `git diff -U0` for the files
/// `is_code` accepts.
///
/// ponytail: per-line heuristic without context lines, so a block comment
/// opened outside a hunk is only caught by its `*` continuation lines.
pub fn count_comment_lines(diff: &str, is_code: impl Fn(&str) -> bool) -> usize {
    let mut count = 0;
    let mut in_header = false;
    let mut old_path: Option<&str> = None;
    let mut syntax: Option<CommentSyntax> = None;
    let mut in_block = false;
    let mut last_sign = ' ';
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            in_header = true;
            syntax = None;
            old_path = None;
            continue;
        }
        if line.starts_with("@@") {
            in_header = false;
            in_block = false;
            continue;
        }
        if in_header {
            // `--- a/x` then `+++ b/x`; deletions end in `+++ /dev/null`.
            if let Some(path) = line.strip_prefix("--- ") {
                old_path = path.strip_prefix("a/");
            } else if let Some(path) = line.strip_prefix("+++ ") {
                let path = path.strip_prefix("b/").or(old_path);
                syntax = path.filter(|p| is_code(p)).map(comment_syntax);
            }
            continue;
        }
        let Some(sign @ ('+' | '-')) = line.chars().next() else {
            continue;
        };
        if sign != last_sign {
            in_block = false;
            last_sign = sign;
        }
        if let Some(syntax) = &syntax
            && is_comment_line(&line[1..], syntax, &mut in_block)
        {
            count += 1;
        }
    }
    count
}

fn is_comment_line(content: &str, syntax: &CommentSyntax, in_block: &mut bool) -> bool {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return false;
    }
    if let Some((start, end)) = syntax.block {
        if *in_block {
            if trimmed.contains(end) {
                *in_block = false;
            }
            return true;
        }
        if let Some(rest) = trimmed.strip_prefix(start) {
            *in_block = !rest.contains(end);
            return true;
        }
        if start == "/*" && (trimmed == "*" || trimmed.starts_with("* ") || trimmed == "*/") {
            return true;
        }
    }
    syntax
        .line
        .as_deref()
        .is_some_and(|prefix| trimmed.starts_with(prefix))
}

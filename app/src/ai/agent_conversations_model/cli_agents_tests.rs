use std::fs::{self, File};
use std::path::Path;
use std::time::Duration;

use chrono::TimeZone as _;

use super::*;
use crate::ai::agent_sdk::driver::harness::claude_transcript::encode_cwd;

const SESSION_A: &str = "00000000-0000-4000-8000-00000000000a";
const SESSION_B: &str = "00000000-0000-4000-8000-00000000000b";
const SESSION_C: &str = "00000000-0000-4000-8000-00000000000c";

/// Writes a transcript `<projects>/<encoded cwd>/<name>` holding `lines`, modified at
/// `modified_secs` after the epoch.
fn write_transcript(projects: &Path, cwd: &Path, name: &str, lines: &str, modified_secs: u64) {
    let dir = projects.join(encode_cwd(cwd));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    fs::write(&path, lines).unwrap();
    set_modified(&path, modified_secs);
}

fn set_modified(path: &Path, modified_secs: u64) {
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(modified_secs))
        .unwrap();
}

fn prompt_line(prompt: &str) -> String {
    format!(r#"{{"type":"user","message":{{"role":"user","content":"{prompt}"}}}}"#) + "\n"
}

#[test]
fn scan_lists_prompted_sessions_of_given_directories_newest_first() {
    let projects = tempfile::tempdir().unwrap();
    let repo = Path::new("/work/example-repo");
    let other = Path::new("/work/other-repo");
    write_transcript(
        projects.path(),
        repo,
        &format!("{SESSION_A}.jsonl"),
        &prompt_line("Fix the build"),
        100,
    );
    write_transcript(
        projects.path(),
        repo,
        &format!("{SESSION_B}.jsonl"),
        &prompt_line("Add a test"),
        200,
    );
    write_transcript(
        projects.path(),
        repo,
        &format!("{SESSION_C}.jsonl"),
        "{\"type\":\"file-history-snapshot\"}\n",
        300,
    );
    write_transcript(
        projects.path(),
        repo,
        "notes.jsonl",
        &prompt_line("Not a session"),
        400,
    );
    write_transcript(
        projects.path(),
        other,
        "00000000-0000-4000-8000-00000000000d.jsonl",
        &prompt_line("Elsewhere"),
        500,
    );

    let history = scan_claude_history(
        projects.path(),
        &[repo.to_path_buf()],
        &mut TitleCache::new(),
    );

    assert_eq!(
        history,
        vec![
            ClaudeHistorySession {
                session_id: SESSION_B.parse().unwrap(),
                cwd: repo.to_path_buf(),
                title: "Add a test".to_owned(),
                last_updated: Utc.timestamp_opt(200, 0).unwrap(),
            },
            ClaudeHistorySession {
                session_id: SESSION_A.parse().unwrap(),
                cwd: repo.to_path_buf(),
                title: "Fix the build".to_owned(),
                last_updated: Utc.timestamp_opt(100, 0).unwrap(),
            },
        ]
    );
}

#[test]
fn scan_reuses_the_cached_title_while_the_file_is_unchanged() {
    let projects = tempfile::tempdir().unwrap();
    let repo = Path::new("/work/example-repo");
    let name = format!("{SESSION_A}.jsonl");
    write_transcript(projects.path(), repo, &name, &prompt_line("First"), 100);
    let mut cache = TitleCache::new();
    scan_claude_history(projects.path(), &[repo.to_path_buf()], &mut cache);
    write_transcript(projects.path(), repo, &name, &prompt_line("Second"), 100);

    let history = scan_claude_history(projects.path(), &[repo.to_path_buf()], &mut cache);

    assert_eq!(history[0].title, "First");
}

#[test]
fn scan_rereads_the_title_after_the_file_changes() {
    let projects = tempfile::tempdir().unwrap();
    let repo = Path::new("/work/example-repo");
    let name = format!("{SESSION_A}.jsonl");
    write_transcript(projects.path(), repo, &name, &prompt_line("First"), 100);
    let mut cache = TitleCache::new();
    scan_claude_history(projects.path(), &[repo.to_path_buf()], &mut cache);
    write_transcript(projects.path(), repo, &name, &prompt_line("Second"), 200);

    let history = scan_claude_history(projects.path(), &[repo.to_path_buf()], &mut cache);

    assert_eq!(history[0].title, "Second");
}

#[test]
fn remember_cwd_moves_a_known_directory_to_the_front() {
    let mut rows = CliAgentRows::default();
    rows.remember_cwd(PathBuf::from("/work/a"));
    rows.remember_cwd(PathBuf::from("/work/b"));

    rows.remember_cwd(PathBuf::from("/work/a"));

    assert_eq!(
        rows.cwds,
        vec![PathBuf::from("/work/a"), PathBuf::from("/work/b")]
    );
}

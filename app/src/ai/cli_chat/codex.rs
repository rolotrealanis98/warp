//! Codex CLI transcript adapter.
//!
//! Codex appends one `{timestamp, type, payload}` record per line to
//! `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<ts>-<session-id>.jsonl`. The first
//! line is `session_meta`, naming the session's cwd. Conversation items are
//! `response_item` records (messages, reasoning, tool calls and their outputs);
//! `event_msg` records mark turn boundaries. Codex writes no subagent
//! transcripts the chat view can nest.

#[cfg(not(target_family = "wasm"))]
use std::path::Path;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};

use super::source::{ChatEvent, CliTranscriptSource, JsonlTail, json_lines, parse_timestamp};

pub(crate) struct CodexTranscript {
    tail: JsonlTail,
}

impl CodexTranscript {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            tail: JsonlTail::new(path),
        }
    }

    /// Resolves the rollout file of a session: the path reported by the Codex
    /// plugin, else the session id, else the newest rollout for `cwd` written
    /// since `opened_after`.
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn locate(
        transcript_path: Option<&str>,
        session_id: Option<&str>,
        cwd: Option<&str>,
        opened_after: DateTime<Utc>,
    ) -> Option<PathBuf> {
        use crate::ai::agent_sdk::driver::harness::codex_transcript::{
            codex_sessions_root, find_session_file,
        };

        // Rollout file names end with the session id; a reported path naming
        // another session is stale.
        if let Some(path) = transcript_path.map(PathBuf::from).filter(|path| {
            session_id.is_none_or(|id| {
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .is_some_and(|stem| stem.ends_with(id))
            })
        }) {
            return path.is_file().then_some(path);
        }
        let root = codex_sessions_root().ok()?;
        if let Some(id) = session_id.and_then(|id| uuid::Uuid::parse_str(id).ok()) {
            return find_session_file(&root, id);
        }
        newest_rollout_for_cwd(&root, Path::new(cwd?), opened_after)
    }

    #[cfg(target_family = "wasm")]
    pub(crate) fn locate(
        _transcript_path: Option<&str>,
        _session_id: Option<&str>,
        _cwd: Option<&str>,
        _opened_after: DateTime<Utc>,
    ) -> Option<PathBuf> {
        None
    }
}

impl CliTranscriptSource for CodexTranscript {
    fn read_incremental(&mut self) -> Vec<ChatEvent> {
        parse_records(&self.tail.read_lines())
    }

    fn subagent(&self, _tool_call_id: &str) -> Option<Box<dyn CliTranscriptSource>> {
        None
    }
}

/// The newest rollout under `root` whose session ran in `cwd` and that was
/// written since `opened_after`.
// ponytail: without a session id (no Codex plugin) the pane is matched by cwd
// and mtime, so a concurrent Codex session in the same directory can win, and a
// `codex resume` of a session from an earlier day is missed (only day
// directories from `opened_after` on are scanned). The plugin's id removes both.
#[cfg(not(target_family = "wasm"))]
fn newest_rollout_for_cwd(root: &Path, cwd: &Path, opened_after: DateTime<Utc>) -> Option<PathBuf> {
    use std::fs;
    use std::time::SystemTime;

    use chrono::Datelike as _;

    let since = SystemTime::from(opened_after);
    // Day directories use local dates; one day of slack each way covers any
    // offset from UTC.
    let mut day = opened_after.date_naive().pred_opt()?;
    let last_day = Utc::now().date_naive().succ_opt()?;
    let mut newest: Option<(SystemTime, PathBuf)> = None;
    while day <= last_day {
        let dir = root
            .join(format!("{:04}", day.year()))
            .join(format!("{:02}", day.month()))
            .join(format!("{:02}", day.day()));
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            let Some(modified) = entry.metadata().ok().and_then(|meta| meta.modified().ok()) else {
                continue;
            };
            let is_rollout = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"));
            if is_rollout
                && modified >= since
                && newest.as_ref().is_none_or(|(seen, _)| modified > *seen)
                && session_cwd(&path).as_deref() == Some(cwd)
            {
                newest = Some((modified, path));
            }
        }
        day = day.succ_opt()?;
    }
    newest.map(|(_, path)| path)
}

/// The cwd recorded in a rollout's `session_meta` first line.
#[cfg(not(target_family = "wasm"))]
fn session_cwd(path: &Path) -> Option<PathBuf> {
    use std::io::{BufRead as _, BufReader, Read as _};

    use crate::ai::agent_sdk::driver::harness::codex_transcript::parse_session_meta;

    /// `session_meta` embeds the base instructions, typically tens of KB.
    const MAX_META_LINE_BYTES: u64 = 1024 * 1024;
    let mut line = Vec::new();
    BufReader::new(std::fs::File::open(path).ok()?)
        .take(MAX_META_LINE_BYTES)
        .read_until(b'\n', &mut line)
        .ok()?;
    let first = serde_json::from_slice::<Value>(&line).ok()?;
    parse_session_meta(Some(&first)).map(|meta| meta.cwd)
}

#[derive(Deserialize)]
struct Record {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    payload: Value,
}

/// Maps complete JSONL lines to chat events. Malformed lines and records that
/// carry no conversation content are skipped.
fn parse_records(bytes: &[u8]) -> Vec<ChatEvent> {
    let mut events = Vec::new();
    for record in json_lines::<Record>(bytes) {
        push_record_events(record, &mut events);
    }
    events
}

fn push_record_events(record: Record, events: &mut Vec<ChatEvent>) {
    let at = parse_timestamp(record.timestamp.as_deref());
    let payload = &record.payload;
    let field = |key: &str| payload.get(key).and_then(Value::as_str);
    match (record.kind.as_str(), field("type")) {
        ("response_item", Some("message")) => {
            let text = match field("role") {
                Some("user") => user_prompt(payload),
                Some("assistant") => content_text(payload),
                // Developer and system messages are instructions, not chat.
                _ => return,
            };
            if text.trim().is_empty() {
                return;
            }
            events.push(match field("role") {
                Some("user") => ChatEvent::UserMessage { text, at },
                _ => ChatEvent::AssistantText { text, at },
            });
        }
        ("response_item", Some("reasoning")) => {
            // Usually encrypted; only the summary is readable.
            let text = payload
                .get("summary")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n\n");
            if !text.trim().is_empty() {
                events.push(ChatEvent::Thinking { text });
            }
        }
        ("response_item", Some("function_call" | "custom_tool_call")) => {
            let (Some(id), Some(name)) = (field("call_id"), field("name")) else {
                return;
            };
            let name = match field("namespace") {
                Some(namespace) => format!("{namespace}__{name}"),
                None => name.to_owned(),
            };
            let raw = match payload.get("arguments") {
                Some(Value::String(arguments)) => serde_json::from_str(arguments)
                    .unwrap_or_else(|_| Value::String(arguments.clone())),
                _ => payload.get("input").cloned().unwrap_or(Value::Null),
            };
            events.push(ChatEvent::ToolCall {
                id: id.to_owned(),
                input: tool_input(&name, raw),
                name,
                at,
            });
        }
        ("response_item", Some("function_call_output" | "custom_tool_call_output")) => {
            let Some(id) = field("call_id") else {
                return;
            };
            let content = output_text(payload.get("output"));
            events.push(ChatEvent::ToolResult {
                tool_use_id: id.to_owned(),
                is_error: looks_failed(&content),
                content,
                at,
            });
        }
        ("event_msg", Some("task_complete")) => events.push(ChatEvent::TurnEnded { at }),
        ("event_msg", Some("turn_aborted")) => {
            events.push(ChatEvent::Notice {
                text: "Interrupted".to_owned(),
            });
            events.push(ChatEvent::TurnEnded { at });
        }
        ("compacted", _) => events.push(ChatEvent::Notice {
            text: "Conversation compacted".to_owned(),
        }),
        _ => {}
    }
}

/// The text a user typed. Codex prepends context it injects (environment,
/// AGENTS.md, plugin hints) as extra parts of user messages; those are
/// dropped.
fn user_prompt(message: &Value) -> String {
    content_parts(message)
        .filter_map(|part| match part.get("type").and_then(Value::as_str) {
            Some("input_text") => part
                .get("text")
                .and_then(Value::as_str)
                .filter(|text| {
                    let text = text.trim_start();
                    !text.starts_with('<') && !text.starts_with("# AGENTS.md")
                })
                .map(str::to_owned),
            Some("input_image") => Some("[image]".to_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn content_text(message: &Value) -> String {
    content_parts(message)
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

fn content_parts(message: &Value) -> impl Iterator<Item = &Value> {
    message
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

/// Tool output is a string or an array of text / image parts.
fn output_text(output: Option<&Value>) -> String {
    match output {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| match part.get("type").and_then(Value::as_str) {
                Some("input_image") => Some("[image]"),
                _ => part.get("text").and_then(Value::as_str),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        None | Some(Value::Null) => String::new(),
        Some(other) => other.to_string(),
    }
}

/// Codex flags failures only in the header lines before `Output:`:
/// `Process exited with code 1`, or `Script failed` / `Script error` for
/// code-mode `exec`.
fn looks_failed(output: &str) -> bool {
    output
        .lines()
        .take(6)
        .take_while(|line| *line != "Output:")
        .any(|line| {
            line.starts_with("Script failed")
                || line.starts_with("Script error")
                || line
                    .strip_prefix("Process exited with code ")
                    .is_some_and(|code| code.trim() != "0")
        })
}

/// Puts a command under `command` and a patch under `patch` (its first file
/// under `file_path`): the keys the chat view renders for command and edit
/// cards.
fn tool_input(name: &str, raw: Value) -> Value {
    match (name, raw) {
        // Code mode: the tool input is a script, shown like a command.
        ("exec", Value::String(script)) => json!({ "command": script }),
        ("apply_patch", Value::String(patch)) => patch_input(patch),
        ("apply_patch", Value::Object(fields)) => match fields.get("input") {
            Some(Value::String(patch)) => patch_input(patch.clone()),
            _ => Value::Object(fields),
        },
        ("exec_command", Value::Object(mut fields)) => {
            if let Some(cmd) = fields.get("cmd").cloned() {
                fields.insert("command".to_owned(), cmd);
            }
            Value::Object(fields)
        }
        ("shell", Value::Object(mut fields)) => {
            // `["bash", "-lc", "<script>"]` shows as the script.
            let command = fields.get("command").and_then(Value::as_array).map(|argv| {
                let argv = argv.iter().filter_map(Value::as_str).collect::<Vec<_>>();
                match argv.as_slice() {
                    [_, "-lc" | "-c", script] => (*script).to_owned(),
                    _ => argv.join(" "),
                }
            });
            if let Some(command) = command {
                fields.insert("command".to_owned(), Value::String(command));
            }
            Value::Object(fields)
        }
        (_, Value::String(input)) => json!({ "input": input }),
        (_, raw) => raw,
    }
}

fn patch_input(patch: String) -> Value {
    let file = patch.lines().find_map(|line| {
        line.strip_prefix("*** Update File: ")
            .or_else(|| line.strip_prefix("*** Add File: "))
            .or_else(|| line.strip_prefix("*** Delete File: "))
            .map(str::to_owned)
    });
    json!({ "file_path": file, "patch": patch })
}

#[cfg(test)]
#[path = "codex_tests.rs"]
mod tests;

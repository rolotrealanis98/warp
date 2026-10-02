//! GitHub Copilot CLI transcript adapter.
//!
//! Copilot CLI appends one `{type, data, id, parentId, timestamp}` event per
//! line to `$COPILOT_HOME/session-state/<session-id>/events.jsonl` (default
//! `~/.copilot`); `workspace.yaml` in the same directory names the session's
//! `cwd`. Permission prompts are logged too (`permission.requested`) and show
//! as attention items.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

use super::source::{ChatEvent, CliTranscriptSource, JsonlTail, json_lines, parse_timestamp};

pub(crate) struct CopilotTranscript {
    tail: JsonlTail,
    /// Whether the latest assistant message requested tools. Copilot logs an
    /// `assistant.turn_end` after every model call; only one without tool
    /// requests hands the turn back to the user.
    awaiting_tools: bool,
}

impl CopilotTranscript {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            tail: JsonlTail::new(path),
            awaiting_tools: false,
        }
    }

    /// Resolves the `events.jsonl` of a session: a reported path, else the
    /// session id, else the newest session for `cwd` written since
    /// `opened_after`.
    pub(crate) fn locate(
        transcript_path: Option<&str>,
        session_id: Option<&str>,
        cwd: Option<&str>,
        opened_after: DateTime<Utc>,
    ) -> Option<PathBuf> {
        if let Some(path) = transcript_path.map(PathBuf::from) {
            return path.is_file().then_some(path);
        }
        let root = copilot_home()?;
        if let Some(session_id) = session_id {
            let path = root
                .join("session-state")
                .join(session_id)
                .join("events.jsonl");
            return path.is_file().then_some(path);
        }
        newest_session_for_cwd(&root, Path::new(cwd?), opened_after)
    }
}

impl CliTranscriptSource for CopilotTranscript {
    fn read_incremental(&mut self) -> Vec<ChatEvent> {
        parse_records(&self.tail.read_lines(), &mut self.awaiting_tools)
    }

    fn subagent(&self, _tool_call_id: &str) -> Option<Box<dyn CliTranscriptSource>> {
        None
    }
}

fn copilot_home() -> Option<PathBuf> {
    std::env::var_os("COPILOT_HOME")
        .map(PathBuf::from)
        .or_else(|| Some(dirs::home_dir()?.join(".copilot")))
}

/// The `events.jsonl` under `root` of the newest session in `cwd` written
/// since `opened_after`, preferring sessions Copilot lists as open.
// ponytail: matched by cwd and mtime, so a concurrent Copilot session in the
// same directory can win; Copilot reports no session id to the pane.
fn newest_session_for_cwd(root: &Path, cwd: &Path, opened_after: DateTime<Utc>) -> Option<PathBuf> {
    use std::collections::{HashMap, HashSet};
    use std::fs;
    use std::time::SystemTime;

    let since = SystemTime::from(opened_after);
    let open: HashSet<String> = fs::read(root.join("open-sessions-state.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<HashMap<String, Value>>(&bytes).ok())
        .map(|sessions| sessions.into_keys().collect())
        .unwrap_or_default();
    fs::read_dir(root.join("session-state"))
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let dir = entry.path();
            let events = dir.join("events.jsonl");
            let modified = fs::metadata(&events).ok()?.modified().ok()?;
            if modified < since || workspace_cwd(&dir).as_deref() != Some(cwd) {
                return None;
            }
            let is_open = entry
                .file_name()
                .to_str()
                .is_some_and(|id| open.contains(id));
            Some(((is_open, modified), events))
        })
        .max_by_key(|(rank, _)| *rank)
        .map(|(_, events)| events)
}

/// The top-level `cwd:` of a session's `workspace.yaml`.
fn workspace_cwd(session_dir: &Path) -> Option<PathBuf> {
    std::fs::read_to_string(session_dir.join("workspace.yaml"))
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("cwd:"))
        .map(|cwd| PathBuf::from(cwd.trim().trim_matches(['"', '\''])))
}

#[derive(Deserialize)]
struct Record {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    data: Value,
}

/// Maps complete JSONL lines to chat events. `awaiting_tools` carries turn
/// state across reads.
fn parse_records(bytes: &[u8], awaiting_tools: &mut bool) -> Vec<ChatEvent> {
    let mut events = Vec::new();
    for record in json_lines::<Record>(bytes) {
        push_record_events(record, awaiting_tools, &mut events);
    }
    events
}

fn push_record_events(record: Record, awaiting_tools: &mut bool, events: &mut Vec<ChatEvent>) {
    let at = parse_timestamp(record.timestamp.as_deref());
    let data = &record.data;
    let field = |key: &str| data.get(key).and_then(Value::as_str);
    let notice = |text: &str| ChatEvent::Notice {
        text: text.to_owned(),
    };
    match record.kind.as_str() {
        "user.message" => {
            if let Some(text) = field("content").filter(|text| !text.trim().is_empty()) {
                events.push(ChatEvent::UserMessage {
                    text: text.to_owned(),
                    at,
                });
            }
        }
        "assistant.message" => {
            if let Some(text) = field("reasoningText").filter(|text| !text.trim().is_empty()) {
                events.push(ChatEvent::Thinking {
                    text: text.to_owned(),
                });
            }
            if let Some(text) = field("content").filter(|text| !text.trim().is_empty()) {
                events.push(ChatEvent::AssistantText {
                    text: text.to_owned(),
                    at,
                });
            }
            *awaiting_tools = data
                .get("toolRequests")
                .and_then(Value::as_array)
                .is_some_and(|requests| !requests.is_empty());
        }
        "assistant.turn_end" if !*awaiting_tools => events.push(ChatEvent::TurnEnded { at }),
        "tool.execution_start" => {
            let (Some(id), Some(name)) = (field("toolCallId"), field("toolName")) else {
                return;
            };
            events.push(ChatEvent::ToolCall {
                id: id.to_owned(),
                name: name.to_owned(),
                input: data.get("arguments").cloned().unwrap_or(Value::Null),
                at,
            });
        }
        "tool.execution_complete" => {
            let Some(id) = field("toolCallId") else {
                return;
            };
            let text_at = |object: &str, key: &str| {
                data.get(object)
                    .and_then(|value| value.get(key))
                    .and_then(Value::as_str)
            };
            let content = text_at("result", "content")
                .or_else(|| text_at("error", "message"))
                .unwrap_or_default();
            events.push(ChatEvent::ToolResult {
                tool_use_id: id.to_owned(),
                content: content.to_owned(),
                is_error: data.get("success").and_then(Value::as_bool) == Some(false),
                at,
            });
        }
        "permission.requested" => {
            let request = data.get("permissionRequest");
            let request_field = |key: &str| {
                request
                    .and_then(|request| request.get(key))
                    .and_then(Value::as_str)
            };
            let what = request_field("intention")
                .or_else(|| request_field("kind"))
                .unwrap_or("a tool call");
            let mut text = format!("Permission: {what}");
            if let Some(command) = request_field("fullCommandText") {
                text.push_str(&format!("\n$ {command}"));
            }
            events.push(ChatEvent::Attention { text });
        }
        "permission.completed" => {
            let kind = data
                .get("result")
                .and_then(|result| result.get("kind"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            events.push(if kind.starts_with("approved") {
                notice("Permission granted")
            } else {
                notice("Permission denied")
            });
        }
        "session.resume" => events.push(notice("Session resumed")),
        "session.shutdown" => events.push(notice("Session ended")),
        _ => {}
    }
}

#[cfg(test)]
#[path = "copilot_tests.rs"]
mod tests;

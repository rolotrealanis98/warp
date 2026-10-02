//! Claude Code transcript adapter.
//!
//! Claude Code appends one JSON record per line to
//! `<config>/projects/<encoded-cwd>/<session-id>.jsonl`. Each assistant record
//! carries a single content block (text, thinking, or tool_use); tool results
//! come back as user records whose content is an array of `tool_result`
//! blocks. Subagent transcripts live next to it under
//! `<session-id>/subagents/agent-<id>.jsonl`, each with a `.meta.json` naming
//! the parent's `Agent` tool call.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

#[cfg(not(target_family = "wasm"))]
use super::source::read_from;
use super::source::{ChatEvent, CliTranscriptSource, JsonlTail, json_lines, parse_timestamp};

pub(crate) struct ClaudeTranscript {
    tail: JsonlTail,
    /// Directory holding this session's subagent transcripts. Subagents of
    /// subagents are stored flat in the same directory.
    subagents_dir: PathBuf,
}

impl ClaudeTranscript {
    pub(crate) fn new(path: PathBuf) -> Self {
        let subagents_dir = path.with_extension("").join("subagents");
        Self::with_subagents_dir(path, subagents_dir)
    }

    fn with_subagents_dir(path: PathBuf, subagents_dir: PathBuf) -> Self {
        Self {
            tail: JsonlTail::new(path),
            subagents_dir,
        }
    }

    /// Resolves the transcript file of a session.
    ///
    /// Prefers the path reported by the Warp plugin, then the session id
    /// (under the cwd's project directory, else any project directory), and
    /// finally the newest transcript in the cwd's project directory.
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn locate(
        transcript_path: Option<&str>,
        session_id: Option<&str>,
        cwd: Option<&str>,
    ) -> Option<PathBuf> {
        use crate::ai::agent_sdk::driver::harness::claude_transcript::{
            claude_config_dir, encode_cwd,
        };

        // A reported path that names another session is stale (e.g. after
        // `/clear`, until the plugin reports the new one).
        if let Some(path) = transcript_path.map(PathBuf::from).filter(|path| {
            session_id.is_none_or(|id| path.file_stem().is_some_and(|stem| stem == id))
        }) {
            return path.is_file().then_some(path);
        }
        let projects = claude_config_dir().ok()?.join("projects");
        if let Some(session_id) = session_id {
            let file_name = format!("{session_id}.jsonl");
            if let Some(cwd) = cwd {
                let path = projects.join(encode_cwd(Path::new(cwd))).join(&file_name);
                if path.is_file() {
                    return Some(path);
                }
            }
            // Session ids are unique, so any project directory will do when the
            // cwd encoding does not match Claude's.
            return fs::read_dir(&projects)
                .ok()?
                .flatten()
                .map(|entry| entry.path().join(&file_name))
                .find(|path| path.is_file());
        }
        // ponytail: without a session id (plugin not installed) take the newest
        // transcript in the cwd's project directory; a concurrent session in the
        // same directory can win. The plugin's session id removes the guess.
        newest_jsonl(&projects.join(encode_cwd(Path::new(cwd?))))
    }

    #[cfg(target_family = "wasm")]
    pub(crate) fn locate(
        _transcript_path: Option<&str>,
        _session_id: Option<&str>,
        _cwd: Option<&str>,
    ) -> Option<PathBuf> {
        None
    }
}

impl CliTranscriptSource for ClaudeTranscript {
    fn read_incremental(&mut self) -> Vec<ChatEvent> {
        parse_records(&self.tail.read_lines())
    }

    fn subagent(&self, tool_call_id: &str) -> Option<Box<dyn CliTranscriptSource>> {
        for entry in fs::read_dir(&self.subagents_dir).ok()?.flatten() {
            let meta_path = entry.path();
            let Some(stem) = meta_path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_suffix(".meta.json"))
            else {
                continue;
            };
            let Some(meta) = fs::read(&meta_path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<SubagentMeta>(&bytes).ok())
            else {
                continue;
            };
            if meta.tool_use_id.as_deref() == Some(tool_call_id) {
                let path = meta_path.with_file_name(format!("{stem}.jsonl"));
                return Some(Box::new(Self::with_subagents_dir(
                    path,
                    self.subagents_dir.clone(),
                )));
            }
        }
        None
    }
}

#[cfg(not(target_family = "wasm"))]
fn newest_jsonl(dir: &Path) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
        .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

#[derive(Deserialize)]
struct SubagentMeta {
    #[serde(rename = "toolUseId")]
    tool_use_id: Option<String>,
}

#[derive(Deserialize)]
struct Record {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    message: Option<Message>,
    #[serde(default, rename = "isMeta")]
    is_meta: bool,
    #[serde(default, rename = "isCompactSummary")]
    is_compact_summary: bool,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default, rename = "aiTitle")]
    ai_title: Option<String>,
    #[serde(default, rename = "customTitle")]
    custom_title: Option<String>,
}

#[derive(Deserialize)]
struct Message {
    #[serde(default)]
    content: Option<Content>,
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Content {
    Text(String),
    Blocks(Vec<Block>),
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Block {
    Text {
        #[serde(default)]
        text: String,
    },
    Thinking {
        #[serde(default)]
        thinking: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        #[serde(default)]
        content: Value,
        #[serde(default)]
        is_error: Option<bool>,
    },
    Image,
    #[serde(other)]
    Other,
}

/// Maps complete JSONL lines to chat events. Malformed lines and record types
/// that carry no conversation content are skipped.
pub(crate) fn parse_records(bytes: &[u8]) -> Vec<ChatEvent> {
    let mut events = Vec::new();
    for record in json_lines::<Record>(bytes) {
        push_record_events(record, &mut events);
    }
    events
}

/// Bytes read from each end of a transcript when titling it.
#[cfg(not(target_family = "wasm"))]
const TITLE_SCAN_BYTES: u64 = 256 * 1024;

/// The title of a stored session: its latest custom title, else its latest generated title,
/// else the first line of its first prompt. `None` for a session without a prompt.
///
/// Claude Code re-appends title records throughout a session, so the file's last bytes
/// usually hold the current title and its first bytes the first prompt.
// ponytail: reads at most 2 x TITLE_SCAN_BYTES per file; a title written only in the middle of
// a larger transcript is missed and the first prompt stands in.
#[cfg(not(target_family = "wasm"))]
pub(crate) fn read_session_title(path: &Path) -> Option<String> {
    let len = fs::metadata(path).ok()?.len();
    let head = read_from(path, 0, TITLE_SCAN_BYTES).ok()?;
    let tail = match len.checked_sub(TITLE_SCAN_BYTES) {
        Some(start) if start > 0 => read_from(path, start, TITLE_SCAN_BYTES).ok()?,
        _ => Vec::new(),
    };
    session_title(&head, &tail)
}

/// [`read_session_title`] over a transcript's first and last bytes. The partial lines at the
/// cut points do not parse and are skipped.
#[cfg(not(target_family = "wasm"))]
fn session_title(head: &[u8], tail: &[u8]) -> Option<String> {
    let (mut custom, mut generated, mut first_prompt) = (None, None, None);
    for event in parse_records(head).into_iter().chain(parse_records(tail)) {
        match event {
            ChatEvent::Title {
                text,
                is_custom: true,
            } => custom = Some(text),
            ChatEvent::Title {
                text,
                is_custom: false,
            } => generated = Some(text),
            ChatEvent::UserMessage { text, .. } if first_prompt.is_none() => {
                first_prompt = Some(text);
            }
            _ => {}
        }
    }
    custom
        .or(generated)
        .or(first_prompt)?
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

fn push_record_events(record: Record, events: &mut Vec<ChatEvent>) {
    let at = parse_timestamp(record.timestamp.as_deref());
    match record.kind.as_str() {
        "user" if !record.is_meta => {
            if record.is_compact_summary {
                events.push(ChatEvent::Notice {
                    text: "Conversation compacted".to_owned(),
                });
                return;
            }
            match record.message.and_then(|message| message.content) {
                Some(Content::Text(text)) => push_user_text(text, at, events),
                Some(Content::Blocks(blocks)) => push_user_blocks(blocks, at, events),
                None => {}
            }
        }
        "assistant" => {
            let Some(message) = record.message else {
                return;
            };
            match message.content {
                Some(Content::Text(text)) if !text.trim().is_empty() => {
                    events.push(ChatEvent::AssistantText { text, at });
                }
                Some(Content::Blocks(blocks)) => push_assistant_blocks(blocks, at, events),
                _ => {}
            }
            if matches!(
                message.stop_reason.as_deref(),
                Some("end_turn" | "stop_sequence")
            ) {
                events.push(ChatEvent::TurnEnded { at });
            }
        }
        "ai-title" => {
            if let Some(text) = record.ai_title {
                events.push(ChatEvent::Title {
                    text,
                    is_custom: false,
                });
            }
        }
        "custom-title" => {
            if let Some(text) = record.custom_title {
                events.push(ChatEvent::Title {
                    text,
                    is_custom: true,
                });
            }
        }
        _ => {}
    }
}

fn push_assistant_blocks(
    blocks: Vec<Block>,
    at: Option<DateTime<Utc>>,
    events: &mut Vec<ChatEvent>,
) {
    for block in blocks {
        match block {
            Block::Text { text } if !text.trim().is_empty() => {
                events.push(ChatEvent::AssistantText { text, at });
            }
            // Redacted thinking blocks carry only a signature.
            Block::Thinking { thinking } if !thinking.trim().is_empty() => {
                events.push(ChatEvent::Thinking { text: thinking });
            }
            Block::ToolUse { id, name, input } => {
                events.push(ChatEvent::ToolCall {
                    id,
                    name,
                    input,
                    at,
                });
            }
            _ => {}
        }
    }
}

fn push_user_text(text: String, at: Option<DateTime<Utc>>, events: &mut Vec<ChatEvent>) {
    let trimmed = text.trim_start();
    if trimmed.starts_with("[Request interrupted") {
        events.push(ChatEvent::Notice {
            text: "Interrupted".to_owned(),
        });
        events.push(ChatEvent::TurnEnded { at });
    } else if trimmed.starts_with("<command-name>") {
        let name = tag_contents(trimmed, "command-name").unwrap_or_default();
        let args = tag_contents(trimmed, "command-args").unwrap_or_default();
        events.push(ChatEvent::Notice {
            text: format!("{name} {args}").trim().to_owned(),
        });
    } else if trimmed.starts_with("<bash-input>") {
        let input = tag_contents(trimmed, "bash-input").unwrap_or_default();
        let output = [
            tag_contents(trimmed, "bash-stdout"),
            tag_contents(trimmed, "bash-stderr"),
        ]
        .into_iter()
        .flatten()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
        let text = if output.is_empty() {
            format!("! {input}")
        } else {
            format!("! {input}\n{output}")
        };
        events.push(ChatEvent::Notice { text });
    } else if trimmed.starts_with('<') {
        let text = strip_tags(trimmed);
        if !text.is_empty() {
            events.push(ChatEvent::Notice { text });
        }
    } else if !trimmed.is_empty() {
        events.push(ChatEvent::UserMessage { text, at });
    }
}

fn push_user_blocks(blocks: Vec<Block>, at: Option<DateTime<Utc>>, events: &mut Vec<ChatEvent>) {
    let mut prompt_parts = Vec::new();
    for block in blocks {
        match block {
            Block::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => events.push(ChatEvent::ToolResult {
                tool_use_id,
                content: tool_result_text(&content),
                is_error: is_error.unwrap_or(false),
                at,
            }),
            Block::Text { text } => prompt_parts.push(text),
            Block::Image => prompt_parts.push("[image]".to_owned()),
            _ => {}
        }
    }
    if !prompt_parts.is_empty() {
        push_user_text(prompt_parts.join("\n"), at, events);
    }
}

/// Flattens `tool_result.content`, which is either a string or an array of
/// `text` / `image` / other parts.
fn tool_result_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(|part| match part.get("type").and_then(Value::as_str) {
                Some("text") => part
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                Some("image") => "[image]".to_owned(),
                _ => String::new(),
            })
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Returns the text between `<tag>` and `</tag>`.
fn tag_contents<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = start + text[start..].find(&close)?;
    Some(&text[start..end])
}

/// Removes `<tag>` / `</tag>` markers (lowercase names with dashes or
/// underscores), keeping the text between them.
fn strip_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let name_end = after
            .find(|c: char| !(c.is_ascii_lowercase() || c == '-' || c == '_' || c == '/'))
            .unwrap_or(after.len());
        if name_end > 0 && after[name_end..].starts_with('>') {
            rest = &after[name_end + 1..];
        } else {
            out.push('<');
            rest = after;
        }
    }
    out.push_str(rest);
    out.trim().to_owned()
}

#[cfg(test)]
#[path = "claude_tests.rs"]
mod tests;

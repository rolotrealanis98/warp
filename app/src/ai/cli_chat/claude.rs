//! Claude Code transcript adapter.
//!
//! Claude Code appends one JSON record per line to
//! `<config>/projects/<encoded-cwd>/<session-id>.jsonl`. Each assistant record
//! carries a single content block (text, thinking, or tool_use); tool results
//! come back as user records whose content is an array of `tool_result`
//! blocks. Subagent transcripts live next to it under
//! `<session-id>/subagents/agent-<id>.jsonl`, each with a `.meta.json` naming
//! the parent's `Agent` tool call.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

use super::source::{ChatEvent, CliTranscriptSource};

/// Upper bound on bytes read per poll. A record longer than this is skipped
/// rather than stalling the tail.
const MAX_READ_BYTES_PER_POLL: u64 = 32 * 1024 * 1024;

pub(crate) struct ClaudeTranscript {
    path: PathBuf,
    /// Directory holding this session's subagent transcripts. Subagents of
    /// subagents are stored flat in the same directory.
    subagents_dir: PathBuf,
    /// Byte offset just past the last complete line consumed.
    offset: u64,
    /// Set while discarding a record longer than one poll's read budget.
    skipping_oversized_record: bool,
}

impl ClaudeTranscript {
    pub(crate) fn new(path: PathBuf) -> Self {
        let subagents_dir = path.with_extension("").join("subagents");
        Self::with_subagents_dir(path, subagents_dir)
    }

    fn with_subagents_dir(path: PathBuf, subagents_dir: PathBuf) -> Self {
        Self {
            path,
            subagents_dir,
            offset: 0,
            skipping_oversized_record: false,
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

    /// Consumes complete lines from `bytes`, which start at `self.offset`.
    fn consume(&mut self, bytes: &[u8], read_budget: u64) -> Vec<ChatEvent> {
        let mut chunk = bytes;
        if self.skipping_oversized_record {
            let Some(newline) = chunk.iter().position(|b| *b == b'\n') else {
                self.offset += chunk.len() as u64;
                return Vec::new();
            };
            self.skipping_oversized_record = false;
            self.offset += newline as u64 + 1;
            chunk = &chunk[newline + 1..];
        }
        let complete_len = chunk
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(0, |newline| newline + 1);
        if complete_len == 0 && chunk.len() as u64 >= read_budget {
            log::debug!("[cli chat] skipping a transcript record over the read budget");
            self.skipping_oversized_record = true;
            self.offset += chunk.len() as u64;
            return Vec::new();
        }
        self.offset += complete_len as u64;
        parse_records(&chunk[..complete_len])
    }
}

impl CliTranscriptSource for ClaudeTranscript {
    fn read_incremental(&mut self) -> Vec<ChatEvent> {
        match read_from(&self.path, self.offset, MAX_READ_BYTES_PER_POLL) {
            Ok(bytes) => self.consume(&bytes, MAX_READ_BYTES_PER_POLL),
            Err(err) => {
                log::debug!("[cli chat] transcript not readable yet: {err}");
                Vec::new()
            }
        }
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

fn read_from(path: &Path, offset: u64, budget: u64) -> io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::new();
    file.take(budget).read_to_end(&mut bytes)?;
    Ok(bytes)
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
    for line in bytes.split(|b| *b == b'\n') {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match serde_json::from_slice::<Record>(line) {
            Ok(record) => push_record_events(record, &mut events),
            Err(err) => log::debug!("[cli chat] skipping malformed transcript line: {err}"),
        }
    }
    events
}

fn push_record_events(record: Record, events: &mut Vec<ChatEvent>) {
    let at = record
        .timestamp
        .as_deref()
        .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
        .map(|ts| ts.with_timezone(&Utc));
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

fn push_assistant_blocks(blocks: Vec<Block>, at: Option<DateTime<Utc>>, events: &mut Vec<ChatEvent>) {
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

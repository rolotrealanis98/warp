//! Agent-neutral transcript events, the adapter trait each CLI agent implements,
//! and the JSONL tailing the adapters share.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Upper bound on bytes read per poll. A record longer than this is skipped
/// rather than stalling the tail.
const MAX_READ_BYTES_PER_POLL: u64 = 32 * 1024 * 1024;

/// One renderable fact read from a CLI agent's session transcript.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ChatEvent {
    /// A prompt the user typed.
    UserMessage {
        text: String,
        at: Option<DateTime<Utc>>,
    },
    /// User-side input that is not a prompt: slash commands, shell-mode
    /// input and output, interrupts, background notices.
    Notice {
        text: String,
    },
    AssistantText {
        text: String,
        at: Option<DateTime<Utc>>,
    },
    Thinking {
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        input: Value,
        at: Option<DateTime<Utc>>,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
        at: Option<DateTime<Utc>>,
    },
    /// The assistant finished its turn (or the user interrupted it).
    TurnEnded {
        at: Option<DateTime<Utc>>,
    },
    /// Session title. `is_custom` is true for a title the user set, which
    /// takes precedence over a generated one.
    Title {
        text: String,
        is_custom: bool,
    },
}

/// Reads a CLI agent's on-disk session transcript incrementally.
///
/// Implementations do blocking file IO and are driven from a background
/// thread, hence `Send`.
pub(crate) trait CliTranscriptSource: Send {
    /// Returns events for records appended since the previous call. The first
    /// call reads from the start of the transcript.
    fn read_incremental(&mut self) -> Vec<ChatEvent>;

    /// The transcript of the subagent spawned by tool call `tool_call_id`, if
    /// the agent writes one.
    fn subagent(&self, tool_call_id: &str) -> Option<Box<dyn CliTranscriptSource>>;
}

/// Follows an append-only JSONL file, handing out complete lines only.
pub(super) struct JsonlTail {
    path: PathBuf,
    /// Byte offset just past the last complete line consumed.
    offset: u64,
    /// Set while discarding a record longer than one poll's read budget.
    skipping_oversized_record: bool,
}

impl JsonlTail {
    pub(super) fn new(path: PathBuf) -> Self {
        Self {
            path,
            offset: 0,
            skipping_oversized_record: false,
        }
    }

    /// The complete lines appended since the previous call.
    pub(super) fn read_lines(&mut self) -> Vec<u8> {
        match read_from(&self.path, self.offset, MAX_READ_BYTES_PER_POLL) {
            Ok(bytes) => self.consume(bytes, MAX_READ_BYTES_PER_POLL),
            Err(err) => {
                log::debug!("[cli chat] transcript not readable yet: {err}");
                Vec::new()
            }
        }
    }

    /// Takes the complete lines from `bytes`, which start at `self.offset`.
    pub(super) fn consume(&mut self, mut bytes: Vec<u8>, read_budget: u64) -> Vec<u8> {
        if self.skipping_oversized_record {
            let Some(newline) = bytes.iter().position(|b| *b == b'\n') else {
                self.offset += bytes.len() as u64;
                return Vec::new();
            };
            self.skipping_oversized_record = false;
            self.offset += newline as u64 + 1;
            bytes.drain(..=newline);
        }
        let complete_len = bytes
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(0, |newline| newline + 1);
        if complete_len == 0 && bytes.len() as u64 >= read_budget {
            log::debug!("[cli chat] skipping a transcript record over the read budget");
            self.skipping_oversized_record = true;
            self.offset += bytes.len() as u64;
            return Vec::new();
        }
        self.offset += complete_len as u64;
        bytes.truncate(complete_len);
        bytes
    }

    #[cfg(test)]
    pub(super) fn offset(&self) -> u64 {
        self.offset
    }
}

pub(super) fn read_from(path: &Path, offset: u64, budget: u64) -> io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::new();
    file.take(budget).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Deserializes each non-blank line of `bytes`, skipping malformed ones.
pub(super) fn json_lines<T: DeserializeOwned>(bytes: &[u8]) -> impl Iterator<Item = T> + '_ {
    bytes
        .split(|b| *b == b'\n')
        .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
        .filter_map(|line| {
            serde_json::from_slice(line)
                .inspect_err(|err| {
                    log::debug!("[cli chat] skipping malformed transcript line: {err}")
                })
                .ok()
        })
}

pub(super) fn parse_timestamp(timestamp: Option<&str>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(timestamp?)
        .ok()
        .map(|ts| ts.with_timezone(&Utc))
}

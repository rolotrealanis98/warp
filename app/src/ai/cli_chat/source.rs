//! Agent-neutral transcript events and the adapter trait each CLI agent implements.

use chrono::{DateTime, Utc};
use serde_json::Value;

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
    Notice { text: String },
    AssistantText {
        text: String,
        at: Option<DateTime<Utc>>,
    },
    Thinking { text: String },
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
    TurnEnded { at: Option<DateTime<Utc>> },
    /// Session title. `is_custom` is true for a title the user set, which
    /// takes precedence over a generated one.
    Title { text: String, is_custom: bool },
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

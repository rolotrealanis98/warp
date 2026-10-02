//! Chat model for one pane: the main thread, lazily tailed subagent threads,
//! and the poll loop that feeds them from the agent's transcript.

use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use markdown_parser::{FormattedText, parse_markdown};
use serde_json::Value;
use warpui::r#async::Timer;
use warpui::{Entity, EntityId, ModelContext, SingletonEntity};

use super::source::{ChatEvent, CliTranscriptSource};
use super::{locate_transcript, open_transcript};
use crate::terminal::CLIAgent;
use crate::terminal::cli_agent_sessions::CLIAgentSessionsModel;

// ponytail: fixed-interval stat-and-read poll per open chat view; switch to a
// `watcher` subscription on the transcript if many panes poll at once.
const POLL_INTERVAL: Duration = Duration::from_millis(300);

/// One entry of a rendered conversation.
#[derive(Debug)]
pub(crate) enum ChatItem {
    User {
        text: String,
        at: Option<DateTime<Utc>>,
    },
    Notice {
        text: String,
    },
    /// Waiting on the user (e.g. a permission prompt); never collapsed.
    Attention {
        text: String,
    },
    Assistant {
        text: String,
        markdown: Option<Arc<FormattedText>>,
        at: Option<DateTime<Utc>>,
    },
    Thinking {
        text: String,
    },
    Tool(ToolItem),
    TurnEnd,
}

#[derive(Debug)]
pub(crate) struct ToolItem {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) input: Value,
    pub(crate) started_at: Option<DateTime<Utc>>,
    /// `None` while the call is pending (running or awaiting permission).
    pub(crate) outcome: Option<ToolOutcome>,
}

#[derive(Debug)]
pub(crate) struct ToolOutcome {
    pub(crate) content: String,
    pub(crate) is_error: bool,
    pub(crate) finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolKind {
    Read,
    Edit,
    Command,
    Search,
    Web,
    Mcp,
    Subagent,
    Other,
}

impl ToolKind {
    fn nouns(self) -> (&'static str, &'static str) {
        match self {
            ToolKind::Read => ("read", "reads"),
            ToolKind::Edit => ("edit", "edits"),
            ToolKind::Command => ("command", "commands"),
            ToolKind::Search => ("search", "searches"),
            ToolKind::Web => ("web request", "web requests"),
            ToolKind::Mcp => ("MCP call", "MCP calls"),
            ToolKind::Subagent => ("subagent", "subagents"),
            ToolKind::Other => ("tool call", "tool calls"),
        }
    }
}

impl ToolItem {
    /// Claude Code tool names first, then Codex (`exec`, `exec_command`,
    /// `shell`, `apply_patch`) and Copilot CLI (`bash`, `view`, `edit`, ...).
    pub(crate) fn kind(&self) -> ToolKind {
        match self.name.as_str() {
            "Read" | "NotebookRead" | "LS" | "view" => ToolKind::Read,
            "Edit" | "MultiEdit" | "Write" | "NotebookEdit" | "apply_patch" | "edit" | "create" => {
                ToolKind::Edit
            }
            "Bash" | "BashOutput" | "KillShell" | "PowerShell" | "exec" | "exec_command"
            | "shell" | "bash" => ToolKind::Command,
            "Grep" | "Glob" | "ToolSearch" | "rg" | "grep" | "glob" => ToolKind::Search,
            "WebFetch" | "WebSearch" | "web_fetch" => ToolKind::Web,
            "Agent" | "Task" => ToolKind::Subagent,
            name if name.starts_with("mcp__") => ToolKind::Mcp,
            _ => ToolKind::Other,
        }
    }

    /// `(server, tool)` for MCP tools named `mcp__<server>__<tool>`.
    pub(crate) fn mcp_server_and_tool(&self) -> Option<(&str, &str)> {
        self.name.strip_prefix("mcp__")?.split_once("__")
    }

    fn input_str(&self, key: &str) -> Option<&str> {
        self.input.get(key).and_then(Value::as_str)
    }

    /// The file an edit-like tool wrote to.
    pub(crate) fn edited_file(&self) -> Option<&str> {
        (self.kind() == ToolKind::Edit)
            .then(|| {
                self.input_str("file_path")
                    .or(self.input_str("notebook_path"))
                    .or(self.input_str("path"))
            })
            .flatten()
    }

    /// A one-line description of the call's input for the card header.
    pub(crate) fn summary(&self) -> String {
        let text = match self.name.as_str() {
            "Bash" | "PowerShell" | "exec" | "exec_command" | "shell" | "bash" => {
                self.input_str("command")
            }
            "Read" | "Edit" | "MultiEdit" | "Write" | "apply_patch" => self.input_str("file_path"),
            "view" | "edit" | "create" => self.input_str("path"),
            "NotebookEdit" => self.input_str("notebook_path"),
            "Grep" | "Glob" | "rg" | "grep" | "glob" => self.input_str("pattern"),
            "WebFetch" | "web_fetch" => self.input_str("url"),
            "WebSearch" => self.input_str("query"),
            "Agent" | "Task" => self.input_str("description"),
            "TodoWrite" => {
                let count = self
                    .input
                    .get("todos")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                return format!("{count} todos");
            }
            _ => self
                .input
                .as_object()
                .and_then(|fields| fields.values().find_map(Value::as_str)),
        };
        text.and_then(|text| text.lines().next())
            .unwrap_or_default()
            .to_owned()
    }

    pub(crate) fn duration(&self) -> Option<chrono::Duration> {
        let finished = self.outcome.as_ref()?.finished_at?;
        Some(finished - self.started_at?)
    }

    pub(crate) fn is_error(&self) -> bool {
        self.outcome
            .as_ref()
            .is_some_and(|outcome| outcome.is_error)
    }

    /// Only finished, successful, non-subagent calls collapse into a group;
    /// failures, pending calls and subagent threads always stay visible.
    fn is_groupable(&self) -> bool {
        self.outcome
            .as_ref()
            .is_some_and(|outcome| !outcome.is_error)
            && self.kind() != ToolKind::Subagent
    }
}

/// A rendered row: a single item, or a run of consecutive successful tool
/// calls shown as one group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Row {
    Item(usize),
    ToolGroup(Range<usize>),
}

/// Collapses runs of two or more groupable tool calls into [`Row::ToolGroup`].
pub(crate) fn rows(items: &[ChatItem]) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut index = 0;
    while index < items.len() {
        let run_end = items[index..]
            .iter()
            .position(|item| !matches!(item, ChatItem::Tool(tool) if tool.is_groupable()))
            .map_or(items.len(), |offset| index + offset);
        if run_end - index >= 2 {
            rows.push(Row::ToolGroup(index..run_end));
            index = run_end;
        } else {
            rows.push(Row::Item(index));
            index += 1;
        }
    }
    rows
}

/// "3 reads · 2 edits · 1 command", in order of first appearance.
pub(crate) fn group_label<'a>(tools: impl IntoIterator<Item = &'a ToolItem>) -> String {
    let mut counts: Vec<(ToolKind, usize)> = Vec::new();
    for tool in tools {
        let kind = tool.kind();
        match counts.iter_mut().find(|(seen, _)| *seen == kind) {
            Some((_, count)) => *count += 1,
            None => counts.push((kind, 1)),
        }
    }
    counts
        .into_iter()
        .map(|(kind, count)| {
            let (singular, plural) = kind.nouns();
            format!("{count} {}", if count == 1 { singular } else { plural })
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// What a finished turn did, for the end-of-turn card.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct TurnSummary {
    /// Unique edited files with the id of the first call that touched each.
    pub(crate) files: Vec<(String, String)>,
    pub(crate) commands: usize,
    /// Ids of failed calls.
    pub(crate) errors: Vec<String>,
}

/// Summarizes the turn that ends at `items[end]`. `None` when the turn made
/// no tool calls.
pub(crate) fn turn_summary(items: &[ChatItem], end: usize) -> Option<TurnSummary> {
    let start = items[..end]
        .iter()
        .rposition(|item| matches!(item, ChatItem::TurnEnd))
        .map_or(0, |index| index + 1);
    let mut summary = TurnSummary::default();
    let mut any_tool = false;
    for item in &items[start..end] {
        let ChatItem::Tool(tool) = item else {
            continue;
        };
        any_tool = true;
        if let Some(file) = tool.edited_file()
            && !summary.files.iter().any(|(seen, _)| seen == file)
        {
            summary.files.push((file.to_owned(), tool.id.clone()));
        }
        if tool.kind() == ToolKind::Command {
            summary.commands += 1;
        }
        if tool.is_error() {
            summary.errors.push(tool.id.clone());
        }
    }
    any_tool.then_some(summary)
}

/// The turn in progress, if the last prompt has not been answered yet.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ActiveTurn {
    pub(crate) started_at: Option<DateTime<Utc>>,
    pub(crate) last_tool: Option<String>,
    pub(crate) running_subagents: usize,
}

#[derive(Debug, Default)]
pub(crate) struct Thread {
    pub(crate) items: Vec<ChatItem>,
    tool_positions: HashMap<String, usize>,
    ai_title: Option<String>,
    custom_title: Option<String>,
}

impl Thread {
    pub(crate) fn apply(&mut self, events: Vec<ChatEvent>) {
        for event in events {
            match event {
                ChatEvent::UserMessage { text, at } => self.items.push(ChatItem::User { text, at }),
                ChatEvent::Notice { text } => self.items.push(ChatItem::Notice { text }),
                ChatEvent::Attention { text } => self.items.push(ChatItem::Attention { text }),
                ChatEvent::AssistantText { text, at } => {
                    // ponytail: markdown is parsed on the main thread at ingest; move it
                    // into the background read if very long sessions stall on open.
                    let markdown = parse_markdown(&text).ok().map(Arc::new);
                    self.items.push(ChatItem::Assistant { text, markdown, at });
                }
                ChatEvent::Thinking { text } => self.items.push(ChatItem::Thinking { text }),
                ChatEvent::ToolCall {
                    id,
                    name,
                    input,
                    at,
                } => {
                    self.tool_positions.insert(id.clone(), self.items.len());
                    self.items.push(ChatItem::Tool(ToolItem {
                        id,
                        name,
                        input,
                        started_at: at,
                        outcome: None,
                    }));
                }
                ChatEvent::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                    at,
                } => {
                    if let Some(ChatItem::Tool(tool)) = self
                        .tool_positions
                        .get(&tool_use_id)
                        .and_then(|index| self.items.get_mut(*index))
                    {
                        tool.outcome = Some(ToolOutcome {
                            content,
                            is_error,
                            finished_at: at,
                        });
                    }
                }
                ChatEvent::TurnEnded { .. } => {
                    if !matches!(self.items.last(), None | Some(ChatItem::TurnEnd)) {
                        self.items.push(ChatItem::TurnEnd);
                    }
                }
                ChatEvent::Title { text, is_custom } => {
                    if is_custom {
                        self.custom_title = Some(text);
                    } else {
                        self.ai_title = Some(text);
                    }
                }
            }
        }
    }

    pub(crate) fn title(&self) -> Option<&str> {
        self.custom_title.as_deref().or(self.ai_title.as_deref())
    }

    pub(crate) fn active_turn(&self) -> Option<ActiveTurn> {
        let last_prompt = self
            .items
            .iter()
            .rposition(|item| matches!(item, ChatItem::User { .. }))?;
        let ChatItem::User { at, .. } = &self.items[last_prompt] else {
            return None;
        };
        let since = &self.items[last_prompt + 1..];
        if since.iter().any(|item| matches!(item, ChatItem::TurnEnd)) {
            return None;
        }
        let tools = since.iter().filter_map(|item| match item {
            ChatItem::Tool(tool) => Some(tool),
            _ => None,
        });
        Some(ActiveTurn {
            started_at: *at,
            last_tool: tools.clone().next_back().map(|tool| tool.name.clone()),
            running_subagents: tools
                .filter(|tool| tool.kind() == ToolKind::Subagent && tool.outcome.is_none())
                .count(),
        })
    }
}

pub(crate) enum CliChatModelEvent {
    Updated,
}

struct SubagentTail {
    source: Option<Box<dyn CliTranscriptSource>>,
    thread: Thread,
}

/// A subagent's tool call id, its source, and the events just read from it.
type SubagentRead = (String, Option<Box<dyn CliTranscriptSource>>, Vec<ChatEvent>);

struct PollResult {
    path: Option<PathBuf>,
    reset: bool,
    source: Option<Box<dyn CliTranscriptSource>>,
    events: Vec<ChatEvent>,
    subagents: Vec<SubagentRead>,
}

/// Tails the transcript of the CLI agent session running in one terminal pane.
pub(crate) struct CliChatModel {
    terminal_view_id: EntityId,
    /// The pane's working directory when the view opened; used to find the
    /// transcript when the agent did not report its own cwd.
    pane_cwd: Option<String>,
    /// When the agent command started. Agents found by cwd only accept
    /// transcripts written since then.
    opened_after: DateTime<Utc>,
    path: Option<PathBuf>,
    source: Option<Box<dyn CliTranscriptSource>>,
    read_in_flight: bool,
    thread: Thread,
    /// Subagent threads keyed by the parent's tool call id. Only threads the
    /// user expanded are tailed.
    subagents: HashMap<String, SubagentTail>,
}

impl Entity for CliChatModel {
    type Event = CliChatModelEvent;
}

impl CliChatModel {
    pub(crate) fn new(
        terminal_view_id: EntityId,
        pane_cwd: Option<String>,
        opened_after: DateTime<Utc>,
        ctx: &mut ModelContext<Self>,
    ) -> Self {
        let mut model = Self {
            terminal_view_id,
            pane_cwd,
            opened_after,
            path: None,
            source: None,
            read_in_flight: false,
            thread: Thread::default(),
            subagents: HashMap::new(),
        };
        model.poll(ctx);
        model
    }

    pub(crate) fn thread(&self) -> &Thread {
        &self.thread
    }

    pub(crate) fn subagent_thread(&self, tool_call_id: &str) -> Option<&Thread> {
        self.subagents.get(tool_call_id).map(|tail| &tail.thread)
    }

    /// Starts tailing the subagent spawned by `tool_call_id`.
    pub(crate) fn watch_subagent(&mut self, tool_call_id: &str) {
        self.subagents
            .entry(tool_call_id.to_owned())
            .or_insert_with(|| SubagentTail {
                source: None,
                thread: Thread::default(),
            });
    }

    /// Feeds events as if read from the transcript (`subagent` names the
    /// parent tool call of a subagent thread).
    #[cfg(test)]
    pub(crate) fn apply_for_test(&mut self, subagent: Option<&str>, events: Vec<ChatEvent>) {
        match subagent {
            Some(id) => {
                self.watch_subagent(id);
                if let Some(tail) = self.subagents.get_mut(id) {
                    tail.thread.apply(events);
                }
            }
            None => self.thread.apply(events),
        }
    }

    fn schedule_poll(&mut self, ctx: &mut ModelContext<Self>) {
        ctx.spawn(Timer::after(POLL_INTERVAL), |me, _, ctx| me.poll(ctx));
    }

    fn poll(&mut self, ctx: &mut ModelContext<Self>) {
        if self.read_in_flight {
            return;
        }
        let Some(session) = CLIAgentSessionsModel::as_ref(ctx).session(self.terminal_view_id)
        else {
            // The session ended; the owning view closes the chat shortly.
            self.schedule_poll(ctx);
            return;
        };
        let agent: CLIAgent = session.agent;
        let context = &session.session_context;
        let transcript_path = context.transcript_path.clone();
        let session_id = context.session_id.clone();
        let cwd = context.cwd.clone().or_else(|| self.pane_cwd.clone());
        let opened_after = self.opened_after;

        let current_path = self.path.clone();
        let mut source = self.source.take();
        let subagents: Vec<_> = self
            .subagents
            .iter_mut()
            .map(|(id, tail)| (id.clone(), tail.source.take()))
            .collect();
        self.read_in_flight = true;

        ctx.spawn(
            async move {
                let located = locate_transcript(
                    agent,
                    transcript_path.as_deref(),
                    session_id.as_deref(),
                    cwd.as_deref(),
                    opened_after,
                );
                let reset = located.is_some() && located != current_path;
                let path = if reset { located } else { current_path };
                if reset {
                    source = path.clone().and_then(|path| open_transcript(agent, path));
                }
                let events = source
                    .as_mut()
                    .map(|source| source.read_incremental())
                    .unwrap_or_default();
                let subagents = if reset {
                    Vec::new()
                } else {
                    subagents
                        .into_iter()
                        .map(|(id, sub_source)| {
                            let mut sub_source =
                                sub_source.or_else(|| source.as_ref()?.subagent(&id));
                            let events = sub_source
                                .as_mut()
                                .map(|source| source.read_incremental())
                                .unwrap_or_default();
                            (id, sub_source, events)
                        })
                        .collect()
                };
                PollResult {
                    path,
                    reset,
                    source,
                    events,
                    subagents,
                }
            },
            |me, result, ctx| {
                me.read_in_flight = false;
                me.apply_poll(result, ctx);
                me.schedule_poll(ctx);
            },
        );
    }

    fn apply_poll(&mut self, result: PollResult, ctx: &mut ModelContext<Self>) {
        let mut changed = result.reset;
        if result.reset {
            self.thread = Thread::default();
            self.subagents.clear();
        }
        self.path = result.path;
        self.source = result.source;
        if !result.events.is_empty() {
            self.thread.apply(result.events);
            changed = true;
        }
        for (id, source, events) in result.subagents {
            if let Some(tail) = self.subagents.get_mut(&id) {
                tail.source = source;
                if !events.is_empty() {
                    tail.thread.apply(events);
                    changed = true;
                }
            }
        }
        if changed {
            ctx.emit(CliChatModelEvent::Updated);
        }
    }
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;

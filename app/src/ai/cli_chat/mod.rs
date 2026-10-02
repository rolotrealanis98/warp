//! Chat view for third-party CLI agent sessions (fork feature, see FORK.md).
//!
//! A terminal pane running a CLI agent can swap its rendering for a chat
//! transcript built from the agent's own session log, while the agent keeps
//! running in the same PTY. Prompts typed in the chat composer are written to
//! that PTY. Claude Code, Codex and Copilot CLI are supported; another agent
//! needs a [`source::CliTranscriptSource`] implementation and a match arm in
//! [`supports_agent`] / [`locate_transcript`] / [`open_transcript`].
//!
//! Gated by `FeatureFlag::CliAgentChatView`.

mod claude;
mod codex;
mod copilot;
pub(crate) mod model;
mod settings;
mod source;
mod view;

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use warp_core::features::FeatureFlag;

use self::claude::ClaudeTranscript;
#[cfg(not(target_family = "wasm"))]
pub(crate) use self::claude::read_session_title;
use self::codex::CodexTranscript;
use self::copilot::CopilotTranscript;
pub(crate) use self::settings::*;
use self::source::CliTranscriptSource;
pub(crate) use self::view::{CliChatView, CliChatViewEvent};
use crate::terminal::CLIAgent;

pub(crate) fn init(app: &mut warpui::AppContext) {
    view::init(app);
}

/// Name of the editable binding that toggles a pane between its terminal
/// and chat renderings.
pub(crate) const TOGGLE_CLI_CHAT_VIEW_BINDING: &str = "terminal:toggle_cli_chat_view";

/// Whether the chat view can be offered for a session of `agent`.
pub(crate) fn supports_agent(agent: CLIAgent) -> bool {
    FeatureFlag::CliAgentChatView.is_enabled()
        && matches!(
            agent,
            CLIAgent::Claude | CLIAgent::Codex | CLIAgent::Copilot
        )
}

/// `opened_after` bounds the cwd fallback of agents that have no session id
/// in the pane: only transcripts written since then match.
fn locate_transcript(
    agent: CLIAgent,
    transcript_path: Option<&str>,
    session_id: Option<&str>,
    cwd: Option<&str>,
    opened_after: DateTime<Utc>,
) -> Option<PathBuf> {
    match agent {
        CLIAgent::Claude => ClaudeTranscript::locate(transcript_path, session_id, cwd),
        CLIAgent::Codex => CodexTranscript::locate(transcript_path, session_id, cwd, opened_after),
        CLIAgent::Copilot => {
            CopilotTranscript::locate(transcript_path, session_id, cwd, opened_after)
        }
        _ => None,
    }
}

fn open_transcript(agent: CLIAgent, path: PathBuf) -> Option<Box<dyn CliTranscriptSource>> {
    match agent {
        CLIAgent::Claude => Some(Box::new(ClaudeTranscript::new(path))),
        CLIAgent::Codex => Some(Box::new(CodexTranscript::new(path))),
        CLIAgent::Copilot => Some(Box::new(CopilotTranscript::new(path))),
        _ => None,
    }
}

//! Chat view for third-party CLI agent sessions (fork feature, see FORK.md).
//!
//! A terminal pane running a CLI agent can swap its rendering for a chat
//! transcript built from the agent's own session log, while the agent keeps
//! running in the same PTY. Prompts typed in the chat composer are written to
//! that PTY. Claude Code is the only agent supported in v1; another agent needs
//! a [`source::CliTranscriptSource`] implementation and a match arm in
//! [`locate_transcript`] / [`open_transcript`].
//!
//! Gated by `FeatureFlag::CliAgentChatView`.

mod claude;
pub(crate) mod model;
mod settings;
mod source;
mod view;

use std::path::PathBuf;

use warp_core::features::FeatureFlag;

use self::claude::ClaudeTranscript;
use self::source::CliTranscriptSource;
pub(crate) use self::settings::*;
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
    FeatureFlag::CliAgentChatView.is_enabled() && agent == CLIAgent::Claude
}

fn locate_transcript(
    agent: CLIAgent,
    transcript_path: Option<&str>,
    session_id: Option<&str>,
    cwd: Option<&str>,
) -> Option<PathBuf> {
    match agent {
        CLIAgent::Claude => ClaudeTranscript::locate(transcript_path, session_id, cwd),
        _ => None,
    }
}

fn open_transcript(agent: CLIAgent, path: PathBuf) -> Option<Box<dyn CliTranscriptSource>> {
    match agent {
        CLIAgent::Claude => Some(Box::new(ClaudeTranscript::new(path))),
        _ => None,
    }
}

//! Fork: agent dashboard and conversation list rows for CLI agent sessions — live panes from
//! `CLIAgentSessionsModel` and past Claude Code sessions read from its transcripts on disk.

use std::collections::HashMap;
#[cfg(not(target_family = "wasm"))]
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use uuid::Uuid;
use warp_cli::agent::Harness;
use warpui::{AppContext, EntityId};

use super::AgentRunDisplayStatus;
use super::entry::{
    AgentConversationBackingData, AgentConversationCapabilities, AgentConversationDisplayData,
    AgentConversationEntry, AgentConversationEntryId, AgentConversationIdentity,
    AgentConversationPrincipal, AgentConversationProvenance, PrincipalType, current_user_name,
    current_user_uid,
};
use crate::ai::ambient_agents::{AgentSource, ExecutionLocation};
use crate::task_agent::TaskSession;
use crate::terminal::CLIAgent;
use crate::terminal::cli_agent_sessions::CLIAgentSession;

/// Most past sessions listed, newest first.
// ponytail: a fixed cap keeps a scan to at most ~25 MiB of reads (512 KiB per transcript, then
// cached by mtime); page through older sessions if anyone needs them here.
#[cfg(not(target_family = "wasm"))]
const MAX_HISTORY_SESSIONS: usize = 50;

/// Most directories remembered for the history scan.
const MAX_REMEMBERED_CWDS: usize = 20;

/// A past Claude Code session found on disk.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ClaudeHistorySession {
    pub session_id: Uuid,
    pub cwd: PathBuf,
    pub title: String,
    pub last_updated: DateTime<Utc>,
}

/// Titles by transcript path, valid while the file's modification time is unchanged.
pub(super) type TitleCache = HashMap<PathBuf, (SystemTime, Option<String>)>;

/// What the conversations model keeps for CLI agent rows.
#[derive(Default)]
pub(super) struct CliAgentRows {
    /// Past sessions from the last scan, newest first.
    pub history: Vec<ClaudeHistorySession>,
    pub titles: TitleCache,
    /// Directories CLI agents ran in during this run, newest first; scanned besides the
    /// workspace's known repositories.
    pub cwds: Vec<PathBuf>,
    /// When each live CLI pane last reported activity; orders its row.
    pub activity: HashMap<EntityId, DateTime<Utc>>,
}

impl CliAgentRows {
    pub fn remember_cwd(&mut self, cwd: PathBuf) {
        self.cwds.retain(|known| *known != cwd);
        self.cwds.insert(0, cwd);
        self.cwds.truncate(MAX_REMEMBERED_CWDS);
    }
}

/// Lists the newest sessions Claude Code stored for `cwds` under `projects_dir`
/// (`<projects_dir>/<encoded-cwd>/<session-id>.jsonl`), titled from their transcripts.
/// Sessions without a prompt are left out. `cache` carries titles across scans and is pruned
/// to the files listed.
#[cfg(not(target_family = "wasm"))]
pub(super) fn scan_claude_history(
    projects_dir: &Path,
    cwds: &[PathBuf],
    cache: &mut TitleCache,
) -> Vec<ClaudeHistorySession> {
    use crate::ai::agent_sdk::driver::harness::claude_transcript::encode_cwd;
    use crate::ai::cli_chat::read_session_title;

    let mut files = Vec::new();
    let mut scanned_dirs = Vec::new();
    for cwd in cwds {
        let dir = projects_dir.join(encode_cwd(cwd));
        if scanned_dirs.contains(&dir) {
            continue;
        }
        scanned_dirs.push(dir.clone());
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "jsonl") {
                continue;
            }
            let Some(session_id) = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(|stem| stem.parse::<Uuid>().ok())
            else {
                continue;
            };
            let Ok(modified) = entry.metadata().and_then(|metadata| metadata.modified()) else {
                continue;
            };
            files.push((modified, session_id, cwd.clone(), path));
        }
    }
    files.sort_by_key(|(modified, ..)| std::cmp::Reverse(*modified));
    files.truncate(MAX_HISTORY_SESSIONS);
    cache.retain(|cached, _| files.iter().any(|(.., path)| path == cached));

    files
        .into_iter()
        .filter_map(|(modified, session_id, cwd, path)| {
            let title = match cache.get(&path) {
                Some((cached_at, title)) if *cached_at == modified => title.clone(),
                _ => {
                    let title = read_session_title(&path);
                    cache.insert(path, (modified, title.clone()));
                    title
                }
            }?;
            Some(ClaudeHistorySession {
                session_id,
                cwd,
                title,
                last_updated: modified.into(),
            })
        })
        .collect()
}

/// The row for a CLI agent running in terminal pane `terminal_view_id`. A tracker task the pane
/// was launched for names the row; otherwise the agent's latest prompt does, then the title the
/// pane was launched with.
pub(super) fn entry_for_cli_session(
    terminal_view_id: EntityId,
    session: &CLIAgentSession,
    task: Option<&TaskSession>,
    last_updated: DateTime<Utc>,
    app: &AppContext,
) -> AgentConversationEntry {
    let context = &session.session_context;
    let task_title = task
        .map(|task| match &task.key {
            Some(key) => format!("{key} {}", task.title),
            None => task.title.clone(),
        })
        .map(|title| title.trim().to_string())
        .filter(|title| !title.is_empty());
    let title = if task.is_some_and(|task| task.key.is_some()) {
        task_title.or_else(|| context.display_title())
    } else {
        context.display_title().or(task_title)
    }
    .unwrap_or_else(|| session.agent.display_name().to_string());
    let status =
        AgentRunDisplayStatus::from_conversation_status(&session.status.to_conversation_status());
    cli_entry(
        AgentConversationEntryId::CliSession(terminal_view_id),
        CliEntryDisplay {
            title,
            initial_query: context.latest_user_prompt(),
            last_updated,
            status,
            working_directory: context.cwd.clone(),
            harness: harness_for(session.agent),
        },
        app,
    )
}

/// The row for a past Claude Code session; opening it resumes the session.
pub(super) fn entry_for_claude_history(
    history: &ClaudeHistorySession,
    app: &AppContext,
) -> AgentConversationEntry {
    cli_entry(
        AgentConversationEntryId::ClaudeHistory(history.session_id),
        CliEntryDisplay {
            title: history.title.clone(),
            initial_query: None,
            last_updated: history.last_updated,
            status: AgentRunDisplayStatus::ConversationSucceeded,
            working_directory: Some(history.cwd.to_string_lossy().into_owned()),
            harness: Some(Harness::Claude),
        },
        app,
    )
}

struct CliEntryDisplay {
    title: String,
    initial_query: Option<String>,
    last_updated: DateTime<Utc>,
    status: AgentRunDisplayStatus,
    working_directory: Option<String>,
    harness: Option<Harness>,
}

fn cli_entry(
    id: AgentConversationEntryId,
    display: CliEntryDisplay,
    app: &AppContext,
) -> AgentConversationEntry {
    AgentConversationEntry {
        id,
        identity: AgentConversationIdentity {
            local_conversation_id: None,
            ambient_agent_task_id: None,
            server_conversation_token: None,
            session_id: None,
        },
        provenance: AgentConversationProvenance::LocalInteractive,
        execution_location: Some(ExecutionLocation::Local),
        display: AgentConversationDisplayData {
            title: display.title,
            initial_query: display.initial_query,
            created_at: display.last_updated,
            last_updated: display.last_updated,
            status: display.status,
            creator: AgentConversationPrincipal {
                name: current_user_name(app),
                uid: current_user_uid(app),
                principal_type: Some(PrincipalType::User),
            },
            executor: None,
            request_usage: None,
            cost_in_cents: None,
            run_time: None,
            session_status: None,
            source: Some(AgentSource::Interactive),
            working_directory: display.working_directory,
            environment_id: None,
            harness: display.harness,
            artifacts: Vec::new(),
        },
        backing: AgentConversationBackingData {
            has_loaded_conversation: false,
            has_local_persisted_data: false,
            has_cloud_data: false,
            has_ambient_run: false,
        },
        capabilities: AgentConversationCapabilities {
            can_open: true,
            can_copy_link: false,
            can_share: false,
            can_delete: false,
            can_fork_locally: false,
            can_cancel: false,
        },
    }
}

fn harness_for(agent: CLIAgent) -> Option<Harness> {
    match agent {
        CLIAgent::Claude => Some(Harness::Claude),
        CLIAgent::Codex => Some(Harness::Codex),
        CLIAgent::Gemini => Some(Harness::Gemini),
        CLIAgent::OpenCode => Some(Harness::OpenCode),
        _ => None,
    }
}

#[cfg(test)]
#[path = "cli_agents_tests.rs"]
mod tests;

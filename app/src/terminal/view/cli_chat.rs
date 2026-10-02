//! Swaps a terminal pane between its terminal rendering and the chat
//! rendering of the CLI agent running in it (see `crate::ai::cli_chat`).
//! Only the rendering changes; the PTY and the agent process are untouched.

use chrono::Utc;
use warpui::{AppContext, SingletonEntity, ViewContext};

use super::TerminalView;
use crate::ai::cli_chat::{self, CliChatView, CliChatViewEvent, CliChatViewSettings};
use crate::terminal::cli_agent_sessions::{CLIAgentSessionsModel, CLIAgentSessionsModelEvent};

impl TerminalView {
    /// Whether this pane runs an agent session the chat view supports.
    pub(super) fn can_show_cli_chat_view(&self, app: &AppContext) -> bool {
        CLIAgentSessionsModel::as_ref(app)
            .session(self.view_id)
            .is_some_and(|session| cli_chat::supports_agent(session.agent))
    }

    pub(super) fn is_cli_chat_view_shown(&self) -> bool {
        self.cli_chat_view.is_some()
    }

    pub(super) fn toggle_cli_chat_view(&mut self, ctx: &mut ViewContext<Self>) {
        if self.cli_chat_view.is_some() {
            self.hide_cli_chat_view(ctx);
        } else {
            self.show_cli_chat_view(ctx);
        }
    }

    fn show_cli_chat_view(&mut self, ctx: &mut ViewContext<Self>) {
        if self.cli_chat_view.is_some() || !self.can_show_cli_chat_view(ctx) {
            return;
        }
        let Some(agent) = CLIAgentSessionsModel::as_ref(ctx)
            .session(self.view_id)
            .map(|session| session.agent)
        else {
            return;
        };
        let terminal_view_id = self.view_id;
        let pane_cwd = self.pwd();
        // The agent command's start: agents found by cwd only match transcripts
        // written since then, which excludes earlier sessions in the same cwd.
        let opened_after = self
            .model
            .lock()
            .block_list()
            .active_block()
            .start_ts()
            .map_or_else(Utc::now, |start| start.with_timezone(&Utc));
        let chat_view = ctx.add_typed_action_view(|ctx| {
            CliChatView::new(terminal_view_id, agent, pane_cwd, opened_after, ctx)
        });
        ctx.subscribe_to_view(&chat_view, |me, _, event, ctx| {
            me.handle_cli_chat_view_event(event, ctx);
        });
        ctx.focus(&chat_view);
        self.cli_chat_view = Some(chat_view);
        ctx.notify();
    }

    fn hide_cli_chat_view(&mut self, ctx: &mut ViewContext<Self>) {
        if self.cli_chat_view.take().is_some() {
            self.redetermine_global_focus(ctx);
            ctx.notify();
        }
    }

    /// Sends focus to the chat composer while the chat view is shown. Returns
    /// whether it did.
    pub(super) fn focus_cli_chat_view_if_shown(&self, ctx: &mut ViewContext<Self>) -> bool {
        let Some(chat_view) = &self.cli_chat_view else {
            return false;
        };
        chat_view.update(ctx, |chat_view, ctx| chat_view.focus_composer(ctx));
        true
    }

    fn handle_cli_chat_view_event(
        &mut self,
        event: &CliChatViewEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        match event {
            CliChatViewEvent::Submit(text) => {
                #[cfg(feature = "local_tty")]
                self.submit_text_to_cli_agent_pty(text.clone(), ctx);
                #[cfg(not(feature = "local_tty"))]
                let _ = text;
            }
            CliChatViewEvent::SendKey(key) => {
                self.write_viewer_bytes_to_pty(key.bytes().to_vec(), ctx);
            }
            CliChatViewEvent::ShowTerminal => self.hide_cli_chat_view(ctx),
        }
    }

    /// Opens the chat view when a supported session starts (if the user asked
    /// for that) and closes it when the session ends.
    pub(super) fn handle_cli_chat_session_event(
        &mut self,
        event: &CLIAgentSessionsModelEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        if event.terminal_view_id() != self.view_id {
            return;
        }
        match event {
            CLIAgentSessionsModelEvent::Started { agent, .. }
                if cli_chat::supports_agent(*agent)
                    && *CliChatViewSettings::as_ref(ctx).open_on_session_start =>
            {
                self.show_cli_chat_view(ctx);
            }
            CLIAgentSessionsModelEvent::Ended { .. } => self.hide_cli_chat_view(ctx),
            _ => {}
        }
    }
}

#[cfg(test)]
#[path = "cli_chat_tests.rs"]
mod tests;

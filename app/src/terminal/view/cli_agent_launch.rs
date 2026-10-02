//! Fork: the terminal's agent entry points start Claude Code instead of Warp's agent — the agent
//! keystroke and zero-state button, `/claude`, and the "Ask Claude Code" footer button.

use warpui::{SingletonEntity, ViewContext};

use super::TerminalView;
use crate::ai::blocklist::block_context_from_terminal_model;
use crate::task_agent::settings::TaskAgentSettings;
use crate::task_agent::{claude_code_here, plan_launch};
use crate::terminal::cli_agent_sessions::CLIAgentSessionsModel;
use crate::workspace::WorkspaceAction;

impl TerminalView {
    /// A new agent conversation: toggles the chat view of the CLI agent running in this pane,
    /// else starts Claude Code with `initial_prompt` or, without one, the typed input.
    pub(super) fn start_or_show_cli_agent(
        &mut self,
        initial_prompt: Option<String>,
        ctx: &mut ViewContext<Self>,
    ) {
        if CLIAgentSessionsModel::as_ref(ctx)
            .session(self.view_id)
            .is_some()
        {
            self.toggle_cli_chat_view(ctx);
            return;
        }
        let prompt = initial_prompt.unwrap_or_else(|| self.input.as_ref(ctx).buffer_text(ctx));
        self.input.update(ctx, |input, ctx| {
            input.clear_buffer_and_reset_undo_stack(ctx)
        });
        self.start_claude_code(prompt.trim().to_string(), ctx);
    }

    /// Starts Claude Code with `prompt` (empty for none) as its first message: in this pane when
    /// its shell is idle, else in a new tab in this pane's directory.
    pub(crate) fn start_claude_code(&mut self, prompt: String, ctx: &mut ViewContext<Self>) {
        // The prompt reaches the agent through a local file.
        let Some(cwd) = self.active_session_path_if_local(ctx) else {
            self.show_error_toast(
                "Claude Code can only be started from a local session".to_string(),
                ctx,
            );
            return;
        };
        let is_busy = self
            .model
            .lock()
            .block_list()
            .active_block()
            .is_active_and_long_running();
        if is_busy {
            ctx.dispatch_typed_action(&WorkspaceAction::OpenClaudeCodeTab { cwd, prompt });
            return;
        }
        let config = TaskAgentSettings::as_ref(ctx).config_for(&cwd);
        let plan = plan_launch(&claude_code_here(cwd, prompt, ctx), &config);
        if let Err(err) = plan.write_prompt_file() {
            log::error!("Failed to write the Claude Code prompt file: {err}");
            self.show_error_toast(
                format!("Could not start Claude Code: failed to write its prompt ({err})"),
                ctx,
            );
            return;
        }
        self.execute_command_or_set_pending(&plan.commands.join(" && "), ctx);
    }

    /// "Ask Claude Code" about the active long-running command: starts Claude Code with the
    /// command and its output so far (the context the native agent would attach).
    ///
    /// The footer offers this only while no CLI agent runs in the pane (an agent's own footer
    /// replaces it), so the question always goes to a new session.
    pub(super) fn ask_claude_code_about_active_block(&mut self, ctx: &mut ViewContext<Self>) {
        let context = {
            let model = self.model.lock();
            let block_id = model.block_list().active_block().id().clone();
            block_context_from_terminal_model(&model, &block_id, false)
        };
        let Some(context) = context else {
            return;
        };
        self.start_claude_code(
            active_command_prompt(&context.command, &context.output),
            ctx,
        );
    }
}

/// The first message for a question about a running command.
fn active_command_prompt(command: &str, output: &str) -> String {
    format!(
        "This command is running in my terminal. Help me with it.\n\n```\n$ {}\n{}\n```",
        command.trim(),
        output.trim_end()
    )
}

#[cfg(test)]
#[path = "cli_agent_launch_tests.rs"]
mod tests;

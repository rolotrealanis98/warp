//! Terminal-side hooks for the PR review agent (`crate::pr_agent`): delivering pull request
//! updates to the CLI agent and showing the agent's review comments in the Code Review panel.

use std::path::Path;

use ai::agent::action::InsertReviewComment;
use warp_util::local_or_remote_path::LocalOrRemotePath;
use warpui::{SingletonEntity, ViewContext};

use super::{Event, TerminalView};
use crate::code_review::comments::convert_insert_review_comments;
use crate::code_review::telemetry_event::CodeReviewPaneEntrypoint;
use crate::pane_group::CodeReviewPanelArg;
use crate::terminal::cli_agent_sessions::{CLIAgentSessionStatus, CLIAgentSessionsModel};

impl TerminalView {
    /// Submits `text` to the pane's CLI agent when it is waiting for input (its last turn ended)
    /// and the user is not composing in the rich input. Returns whether it was sent; otherwise the
    /// caller keeps it for later. Never steals focus.
    // ponytail: text the user typed directly into the agent's own prompt is not visible to Warp
    // and would be submitted together with the update.
    pub(crate) fn deliver_pr_update(&mut self, text: String, ctx: &mut ViewContext<Self>) -> bool {
        let is_waiting_for_input = CLIAgentSessionsModel::as_ref(ctx)
            .session(self.view_id)
            .is_some_and(|session| {
                matches!(
                    session.status,
                    CLIAgentSessionStatus::Success
                        | CLIAgentSessionStatus::Failed { .. }
                        | CLIAgentSessionStatus::Cancelled
                )
            });
        if !is_waiting_for_input || self.is_cli_agent_rich_input_open(ctx) {
            return false;
        }
        #[cfg(feature = "local_tty")]
        {
            self.submit_text_to_cli_agent_pty(text, ctx);
            true
        }
        #[cfg(not(feature = "local_tty"))]
        {
            let _ = text;
            false
        }
    }

    /// Opens the Code Review panel for the checkout at `repo_path` (diffed against
    /// `base_branch`) and adds `comments` to it.
    pub(crate) fn show_pr_review_comments(
        &mut self,
        repo_path: &Path,
        comments: &[InsertReviewComment],
        base_branch: &str,
        ctx: &mut ViewContext<Self>,
    ) {
        let comments = convert_insert_review_comments(comments);
        if comments.is_empty() {
            return;
        }
        let diff_mode = self.diff_mode_for_branch(Some(base_branch), ctx);
        let repo_path = LocalOrRemotePath::Local(repo_path.to_path_buf());
        ctx.emit(Event::InsertCodeReviewComments {
            repo_path: repo_path.clone(),
            comments,
            diff_mode,
            open_code_review: Some(CodeReviewPanelArg {
                repo_path: Some(repo_path),
                terminal_view: self.view_handle.clone(),
                entrypoint: CodeReviewPaneEntrypoint::InvokedByAgent,
                focus_new_pane: false,
                cli_agent: self.active_cli_agent(ctx),
            }),
        });
    }
}

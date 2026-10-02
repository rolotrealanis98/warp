//! Workspace glue for the PR review agent (`crate::pr_agent`).

use std::path::PathBuf;

use warpui::elements::Fill as ElementFill;
use warpui::ui_components::components::{Coords, UiComponentStyles};
use warpui::{EntityId, SingletonEntity, ViewContext};

use super::Workspace;
use crate::features::FeatureFlag;
use crate::modal::{Modal, ModalEvent, ModalViewState};
use crate::pr_agent::{
    PrAgentModal, PrAgentModalEvent, PrAgentModel, PrWatchRequest, launch_config, tab_title,
};
use crate::task_agent::plan_launch;
use crate::task_agent::settings::TaskAgentSettings;

const MODAL_WIDTH: f32 = 560.;
const MODAL_HEIGHT: f32 = 640.;
/// Room left for the modal's title row inside `MODAL_HEIGHT`.
const MODAL_HEADER_ALLOWANCE: f32 = 70.;

impl Workspace {
    pub(super) fn build_pr_agent_modal(
        ctx: &mut ViewContext<Self>,
    ) -> ModalViewState<Modal<PrAgentModal>> {
        let body = ctx.add_typed_action_view(PrAgentModal::new);
        ctx.subscribe_to_view(&body, |me, _, event, ctx| {
            me.handle_pr_agent_modal_event(event, ctx);
        });
        let modal = ctx.add_typed_action_view(|ctx| {
            Modal::new(Some("Review pull request".to_string()), body, ctx)
                .with_modal_style(UiComponentStyles {
                    width: Some(MODAL_WIDTH),
                    height: Some(MODAL_HEIGHT),
                    ..Default::default()
                })
                .with_body_style(UiComponentStyles {
                    padding: Some(Coords::uniform(0.)),
                    height: Some(MODAL_HEIGHT - MODAL_HEADER_ALLOWANCE),
                    background: Some(ElementFill::None),
                    ..Default::default()
                })
        });
        ctx.subscribe_to_view(&modal, |me, _, event, ctx| match event {
            ModalEvent::Close => me.close_pr_agent_modal(ctx),
        });
        ModalViewState::new(modal)
    }

    /// Opens the PR agent modal for the active session's repository.
    pub(super) fn open_pr_agent_modal(&mut self, ctx: &mut ViewContext<Self>) {
        if !FeatureFlag::PrReviewAgent.is_enabled() {
            return;
        }
        let repo_root = self
            .active_session_view(ctx)
            .and_then(|view| {
                view.as_ref(ctx)
                    .current_local_repo_path()
                    .map(PathBuf::from)
            })
            .unwrap_or_default();
        self.pr_agent_modal.view.update(ctx, |modal, ctx| {
            modal
                .body()
                .update(ctx, |body, ctx| body.on_open(repo_root, ctx));
        });
        self.pr_agent_modal.open();
        self.current_workspace_state.is_pr_agent_modal_open = true;
        ctx.notify();
    }

    fn close_pr_agent_modal(&mut self, ctx: &mut ViewContext<Self>) {
        self.pr_agent_modal.close();
        self.current_workspace_state.is_pr_agent_modal_open = false;
        self.focus_active_tab(ctx);
        ctx.notify();
    }

    fn handle_pr_agent_modal_event(
        &mut self,
        event: &PrAgentModalEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        let PrAgentModalEvent::Launch(launch) = event else {
            self.close_pr_agent_modal(ctx);
            return;
        };
        self.close_pr_agent_modal(ctx);

        let repo_root = launch.request.repo_root.clone();
        let mut plan = plan_launch(&launch.request, &launch_config(&repo_root, ctx));
        plan.tab_title = tab_title(
            &launch.details,
            *TaskAgentSettings::as_ref(ctx).short_title_max_chars,
        );
        let checkout_path = plan
            .worktree_path
            .clone()
            .unwrap_or_else(|| plan.cwd.clone());
        let Some(terminal) = self.launch_task_agent(plan, ctx) else {
            return;
        };
        let request = PrWatchRequest {
            details: launch.details.clone(),
            viewer: launch.viewer.clone(),
            repo_root,
            checkout_path,
        };
        PrAgentModel::handle(ctx).update(ctx, |model, ctx| model.watch(&terminal, request, ctx));
    }

    /// Opens the pull request a PR agent pane watches and clears its unread count.
    pub(super) fn open_pr_agent_pull_request(
        &mut self,
        terminal_view_id: EntityId,
        ctx: &mut ViewContext<Self>,
    ) {
        if let Some(url) = PrAgentModel::handle(ctx)
            .update(ctx, |model, ctx| model.mark_read(terminal_view_id, ctx))
        {
            ctx.open_url(&url);
        }
    }
}

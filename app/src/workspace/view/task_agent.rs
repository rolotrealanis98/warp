//! Workspace glue for the task agent launcher (`crate::task_agent`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use warpui::elements::Fill as ElementFill;
use warpui::ui_components::components::{Coords, UiComponentStyles};
use warpui::{SingletonEntity, ViewContext, ViewHandle};

use super::Workspace;
use crate::ai::persisted_workspace::PersistedWorkspace;
use crate::features::FeatureFlag;
use crate::launch_configs::launch_config::{CommandTemplate, PaneMode, PaneTemplateType};
use crate::modal::{Modal, ModalEvent, ModalViewState};
use crate::pane_group::PanesLayout;
use crate::task_agent::settings::TaskAgentSettings;
use crate::task_agent::{
    Checkout, LaunchPlan, TaskAgentModal, TaskAgentModalEvent, TaskAgentModalMode,
    TaskAgentRequest, TaskSession, TaskSessionsModel, plan_launch, session_title, template_vars,
};
use crate::terminal::{CLIAgent, TerminalView};
use crate::view_components::DismissibleToast;

const MODAL_WIDTH: f32 = 560.;
const MODAL_HEIGHT: f32 = 680.;
/// Room left for the modal's title row inside `MODAL_HEIGHT`.
const MODAL_HEADER_ALLOWANCE: f32 = 70.;

impl Workspace {
    pub(super) fn build_task_agent_modal(
        ctx: &mut ViewContext<Self>,
    ) -> ModalViewState<Modal<TaskAgentModal>> {
        let body = ctx.add_typed_action_view(TaskAgentModal::new);
        ctx.subscribe_to_view(&body, |me, _, event, ctx| {
            me.handle_task_agent_modal_event(event, ctx);
        });
        let modal = ctx.add_typed_action_view(|ctx| {
            Modal::new(Some("Start task agent".to_string()), body, ctx)
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
            ModalEvent::Close => me.close_task_agent_modal(ctx),
        });
        ModalViewState::new(modal)
    }

    /// Opens the task agent modal for the active session's repository.
    pub(super) fn open_task_agent_modal_for_active_session(
        &mut self,
        mode: TaskAgentModalMode,
        ctx: &mut ViewContext<Self>,
    ) {
        if !FeatureFlag::TaskAgentLauncher.is_enabled() {
            return;
        }
        let mut request = self.task_agent_draft_for_active_session(ctx);
        if mode == TaskAgentModalMode::Rename
            && let Some(task) = self
                .active_session_view(ctx)
                .and_then(|view| TaskSessionsModel::as_ref(ctx).get(view.id()).cloned())
        {
            request.key = task.key;
            request.title = task.title;
        }
        self.open_task_agent_modal(mode, request, ctx);
    }

    /// An empty request for the active session's repository: its git root, else its cwd, else
    /// the first known workspace.
    pub(super) fn task_agent_draft_for_active_session(
        &self,
        ctx: &mut ViewContext<Self>,
    ) -> TaskAgentRequest {
        let repo_root = self
            .active_session_view(ctx)
            .and_then(|view| {
                let view = view.as_ref(ctx);
                view.current_local_repo_path()
                    .map(PathBuf::from)
                    .or_else(|| view.pwd().map(PathBuf::from))
            })
            .or_else(|| {
                PersistedWorkspace::as_ref(ctx)
                    .workspaces()
                    .next()
                    .map(|workspace| workspace.path.clone())
            })
            .unwrap_or_default();
        TaskAgentRequest::draft(repo_root, ctx)
    }

    /// Opens the task agent modal prefilled from `request`.
    pub(crate) fn open_task_agent_modal(
        &mut self,
        mode: TaskAgentModalMode,
        request: TaskAgentRequest,
        ctx: &mut ViewContext<Self>,
    ) {
        let title = match mode {
            TaskAgentModalMode::Launch => "Start task agent",
            TaskAgentModalMode::Rename => "Rename session from task",
        };
        self.task_agent_modal.view.update(ctx, |modal, ctx| {
            modal.set_title(Some(title.to_string()));
            modal
                .body()
                .update(ctx, |body, ctx| body.on_open(mode, request, ctx));
        });
        self.task_agent_modal.open();
        self.current_workspace_state.is_task_agent_modal_open = true;
        ctx.notify();
    }

    fn close_task_agent_modal(&mut self, ctx: &mut ViewContext<Self>) {
        self.task_agent_modal.close();
        self.current_workspace_state.is_task_agent_modal_open = false;
        self.focus_active_tab(ctx);
        ctx.notify();
    }

    fn handle_task_agent_modal_event(
        &mut self,
        event: &TaskAgentModalEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        match event {
            TaskAgentModalEvent::Close => self.close_task_agent_modal(ctx),
            TaskAgentModalEvent::Launch(request) => {
                self.close_task_agent_modal(ctx);
                let config = TaskAgentSettings::as_ref(ctx).config_for(&request.repo_root);
                self.launch_task_agent(plan_launch(request, &config), ctx);
            }
            TaskAgentModalEvent::Rename { key, title } => {
                self.close_task_agent_modal(ctx);
                self.rename_active_session_from_task(key.clone(), title.clone(), ctx);
            }
        }
    }

    /// Writes the prompt file, opens a tab running the plan's commands, and records the task on
    /// the new terminal pane, which it returns.
    pub(crate) fn launch_task_agent(
        &mut self,
        plan: LaunchPlan,
        ctx: &mut ViewContext<Self>,
    ) -> Option<ViewHandle<TerminalView>> {
        if let Some((path, contents)) = &plan.prompt_file {
            let written = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(path, contents));
            if let Err(err) = written {
                log::error!("Failed to write the task agent prompt file: {err}");
                self.toast_stack.update(ctx, |toast_stack, ctx| {
                    toast_stack.add_ephemeral_toast(
                        DismissibleToast::error(format!(
                            "Could not start the task agent: failed to write its prompt ({err})"
                        )),
                        ctx,
                    );
                });
                return None;
            }
        }

        let prefer_agent_title = *TaskAgentSettings::as_ref(ctx).prefer_agent_title;
        let pane = PaneTemplateType::PaneTemplate {
            cwd: plan.cwd.clone(),
            commands: plan
                .commands
                .iter()
                .map(|command| CommandTemplate {
                    exec: command.clone(),
                })
                .collect(),
            is_focused: Some(true),
            pane_mode: PaneMode::Terminal,
            shell: None,
        };
        self.add_tab_with_pane_layout(
            PanesLayout::Template(pane),
            Arc::new(HashMap::new()),
            (!prefer_agent_title).then(|| plan.tab_title.clone()),
            ctx,
        );

        let Some(terminal) = self
            .active_tab_pane_group()
            .as_ref(ctx)
            .active_session_view(ctx)
        else {
            log::warn!("Task agent tab opened without a terminal pane");
            return None;
        };
        let terminal_view_id = terminal.id();
        TaskSessionsModel::handle(ctx).update(ctx, |sessions, _| {
            sessions.set(terminal_view_id, plan.session.clone());
            if let Some(prompt) = plan.fallback_prompt.clone() {
                sessions.set_pending_prompt(terminal.downgrade(), terminal_view_id, prompt);
            }
        });
        refresh_pane_header(&terminal, ctx);
        Some(terminal)
    }

    /// Opens a tab in the active session's directory running Claude Code. Without an active
    /// session the tab starts in the default directory.
    pub(super) fn open_claude_code_tab(&mut self, ctx: &mut ViewContext<Self>) {
        let cwd = self
            .active_session_view(ctx)
            .and_then(|view| view.as_ref(ctx).pwd_if_local(ctx).map(PathBuf::from))
            .unwrap_or_default();
        let pane = PaneTemplateType::PaneTemplate {
            cwd,
            commands: vec![CommandTemplate {
                exec: CLIAgent::Claude.command_prefix().to_string(),
            }],
            is_focused: Some(true),
            pane_mode: PaneMode::Terminal,
            shell: None,
        };
        self.add_tab_with_pane_layout(
            PanesLayout::Template(pane),
            Arc::new(HashMap::new()),
            None,
            ctx,
        );
    }

    /// Names the active tab after a task and records the task on its terminal pane, keeping any
    /// repository/branch already recorded for it.
    fn rename_active_session_from_task(
        &mut self,
        key: Option<String>,
        title: String,
        ctx: &mut ViewContext<Self>,
    ) {
        let pane_group = self.active_tab_pane_group().clone();
        let Some(terminal) = pane_group.as_ref(ctx).active_session_view(ctx) else {
            return;
        };
        let terminal_view_id = terminal.id();
        let existing = TaskSessionsModel::as_ref(ctx)
            .get(terminal_view_id)
            .cloned();
        let task = match existing {
            Some(task) => TaskSession { key, title, ..task },
            None => {
                let terminal = terminal.as_ref(ctx);
                TaskSession {
                    key,
                    title,
                    url: None,
                    repo_root: terminal
                        .current_local_repo_path()
                        .map(PathBuf::from)
                        .unwrap_or_default(),
                    branch: terminal.current_git_branch(ctx),
                    checkout: Checkout::Here,
                }
            }
        };

        let settings = TaskAgentSettings::as_ref(ctx);
        let request = TaskAgentRequest {
            title: task.title.clone(),
            key: task.key.clone(),
            ..TaskAgentRequest::draft(task.repo_root.clone(), ctx)
        };
        let mut vars = template_vars(&request, *settings.short_title_max_chars);
        vars.insert(
            "branch".to_string(),
            task.branch.clone().unwrap_or_default(),
        );
        let tab_title = session_title(&settings.session_title_template, &vars);

        TaskSessionsModel::handle(ctx).update(ctx, |sessions, _| {
            sessions.set(terminal_view_id, task);
        });
        refresh_pane_header(&terminal, ctx);
        pane_group.update(ctx, |pane_group, ctx| pane_group.set_title(&tab_title, ctx));
        ctx.notify();
    }
}

/// Re-renders the pane so the header (and its task key chip) reflects new task metadata. The
/// pane only re-checks `should_render_header` when it re-renders, and refreshing the overflow
/// items is the existing path that notifies it.
fn refresh_pane_header(terminal: &ViewHandle<TerminalView>, ctx: &mut ViewContext<Workspace>) {
    let pane_configuration = terminal.as_ref(ctx).pane_configuration().clone();
    pane_configuration.update(ctx, |pane_configuration, ctx| {
        pane_configuration.refresh_pane_header_overflow_menu_items(ctx);
        pane_configuration.notify_header_content_changed(ctx);
    });
}

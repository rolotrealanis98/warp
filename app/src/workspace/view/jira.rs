//! Workspace glue for the Jira integration (`crate::jira`).

use warpui::clipboard::ClipboardContent;
use warpui::elements::Fill as ElementFill;
use warpui::ui_components::components::{Coords, UiComponentStyles};
use warpui::{SingletonEntity, ViewContext};

use super::Workspace;
use crate::features::FeatureFlag;
use crate::jira::settings::JiraSettings;
use crate::jira::{
    self, IssueAction, JiraCommand, JiraIssuePicker, JiraIssuePickerEvent, PickerMode,
};
use crate::modal::{Modal, ModalEvent, ModalViewState};
use crate::settings_view::SettingsSection;
use crate::task_agent::{TaskAgentModalMode, TaskSessionsModel};
use crate::view_components::DismissibleToast;
use crate::workspace::WorkspaceAction;

const MODAL_WIDTH: f32 = 820.;
const MODAL_HEIGHT: f32 = 600.;
/// Room left for the modal's title row inside `MODAL_HEIGHT`.
const MODAL_HEADER_ALLOWANCE: f32 = 70.;

impl Workspace {
    pub(super) fn build_jira_modal(
        ctx: &mut ViewContext<Self>,
    ) -> ModalViewState<Modal<JiraIssuePicker>> {
        let body = ctx.add_typed_action_view(JiraIssuePicker::new);
        ctx.subscribe_to_view(&body, |me, _, event, ctx| {
            me.handle_jira_picker_event(event, ctx);
        });
        let modal = ctx.add_typed_action_view(|ctx| {
            Modal::new(Some("Jira".to_string()), body, ctx)
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
            ModalEvent::Close => me.close_jira_modal(ctx),
        });
        ModalViewState::new(modal)
    }

    pub(super) fn handle_jira_command(
        &mut self,
        command: JiraCommand,
        ctx: &mut ViewContext<Self>,
    ) {
        if !FeatureFlag::JiraIntegration.is_enabled() {
            return;
        }
        let (mode, title) = match command {
            JiraCommand::PickIssue(action) => {
                let title = match action {
                    IssueAction::StartAgent => "Start agent on Jira issue",
                    IssueAction::RenameSession => "Rename session from Jira issue",
                    IssueAction::OpenInBrowser => "Open Jira issue in browser",
                    IssueAction::CopyKey => "Copy Jira issue key",
                };
                (PickerMode::Issues(action), title.to_string())
            }
            JiraCommand::Transition | JiraCommand::Comment => {
                let key = self.active_session_view(ctx).and_then(|terminal| {
                    TaskSessionsModel::as_ref(ctx)
                        .get(terminal.id())
                        .and_then(|task| task.key.clone())
                });
                let Some(key) = key else {
                    self.show_jira_toast(
                        DismissibleToast::error(
                            "This pane has no issue key. Start it from a Jira issue, or use \
                             \"Jira: rename this session from issue…\" first."
                                .to_string(),
                        ),
                        ctx,
                    );
                    return;
                };
                if command == JiraCommand::Transition {
                    (
                        PickerMode::Transitions { key: key.clone() },
                        format!("Transition {key}"),
                    )
                } else {
                    (
                        PickerMode::Comment { key: key.clone() },
                        format!("Comment on {key}"),
                    )
                }
            }
        };

        self.jira_modal.view.update(ctx, |modal, ctx| {
            modal.set_title(Some(title));
            modal.body().update(ctx, |body, ctx| body.open(mode, ctx));
        });
        self.jira_modal.open();
        self.current_workspace_state.is_jira_modal_open = true;
        ctx.notify();
    }

    fn close_jira_modal(&mut self, ctx: &mut ViewContext<Self>) {
        self.jira_modal.close();
        self.current_workspace_state.is_jira_modal_open = false;
        self.focus_active_tab(ctx);
        ctx.notify();
    }

    fn show_jira_toast(
        &mut self,
        toast: DismissibleToast<WorkspaceAction>,
        ctx: &mut ViewContext<Self>,
    ) {
        self.toast_stack.update(ctx, |toast_stack, ctx| {
            toast_stack.add_ephemeral_toast(toast, ctx);
        });
    }

    fn handle_jira_picker_event(
        &mut self,
        event: &JiraIssuePickerEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        match event {
            JiraIssuePickerEvent::Close => self.close_jira_modal(ctx),
            JiraIssuePickerEvent::OpenSettings => {
                self.close_jira_modal(ctx);
                self.show_settings_with_section(Some(SettingsSection::Jira), ctx);
            }
            JiraIssuePickerEvent::Done(message) => {
                self.close_jira_modal(ctx);
                self.show_jira_toast(DismissibleToast::success(message.clone()), ctx);
            }
            JiraIssuePickerEvent::IssueChosen { action, issue } => {
                self.close_jira_modal(ctx);
                match action {
                    IssueAction::StartAgent | IssueAction::RenameSession => {
                        let draft = self.task_agent_draft_for_active_session(ctx);
                        let settings = JiraSettings::as_ref(ctx);
                        let request = jira::task_request(
                            issue,
                            &settings.site_url,
                            &settings.issue_type_to_branch_type,
                            draft,
                        );
                        let mode = if *action == IssueAction::StartAgent {
                            TaskAgentModalMode::Launch
                        } else {
                            TaskAgentModalMode::Rename
                        };
                        self.open_task_agent_modal(mode, request, ctx);
                    }
                    IssueAction::OpenInBrowser => {
                        if let Some(url) = jira::browse_url(&issue.key, ctx) {
                            ctx.open_url(&url);
                        }
                    }
                    IssueAction::CopyKey => {
                        ctx.clipboard()
                            .write(ClipboardContent::plain_text(issue.key.clone()));
                        self.show_jira_toast(
                            DismissibleToast::success(format!("Copied {}", issue.key)),
                            ctx,
                        );
                    }
                }
            }
        }
    }
}

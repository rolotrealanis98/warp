//! Settings page for the task agent launcher (`task_agents.*`).

use std::collections::HashMap;

use warp_core::settings::{Setting as _, ToggleableSetting as _};
use warp_errors::report_if_error;
use warpui::elements::{Element, Flex, MouseStateHandle, ParentElement};
use warpui::ui_components::button::ButtonVariant;
use warpui::ui_components::components::{UiComponent, UiComponentStyles};
use warpui::ui_components::switch::SwitchStateHandle;
use warpui::{AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle};

use super::settings_page::{
    LocalOnlyIconState, MatchData, PageTitle, PageType, SettingsPageMeta, SettingsPageViewHandle,
    SettingsWidget, render_body_item, render_body_item_label,
};
use super::{SettingsSection, ToggleState};
use crate::appearance::Appearance;
use crate::editor::{EditorOptions, EditorView, Event as EditorEvent, SingleLineEditorOptions};
use crate::features::FeatureFlag;
use crate::task_agent::settings::{TaskAgentSettings, cli_from_command};
use crate::terminal::CLIAgent;
use crate::view_components::{Dropdown, DropdownItem};
use crate::workspace::WorkspaceAction;

const PROMPT_EDITOR_HEIGHT: f32 = 120.;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum TextField {
    BranchTemplate,
    WorktreePathTemplate,
    PromptTemplate,
    SessionTitleTemplate,
    ShortTitleMaxChars,
}

impl TextField {
    const ALL: [TextField; 5] = [
        TextField::BranchTemplate,
        TextField::WorktreePathTemplate,
        TextField::PromptTemplate,
        TextField::SessionTitleTemplate,
        TextField::ShortTitleMaxChars,
    ];

    fn value(self, settings: &TaskAgentSettings) -> String {
        match self {
            TextField::BranchTemplate => settings.branch_template.clone(),
            TextField::WorktreePathTemplate => settings.worktree_path_template.clone(),
            TextField::PromptTemplate => settings.prompt_template.clone(),
            TextField::SessionTitleTemplate => settings.session_title_template.clone(),
            TextField::ShortTitleMaxChars => (*settings.short_title_max_chars).to_string(),
        }
    }

    fn label(self) -> &'static str {
        match self {
            TextField::BranchTemplate => "Branch name template",
            TextField::WorktreePathTemplate => "Worktree path template",
            TextField::PromptTemplate => "Initial prompt template",
            TextField::SessionTitleTemplate => "Session title template",
            TextField::ShortTitleMaxChars => "Short title length",
        }
    }

    fn description(self) -> &'static str {
        match self {
            TextField::BranchTemplate => {
                "Variables: {{type}}, {{key}}, {{slug}}, {{title}}, {{repo}}."
            }
            TextField::WorktreePathTemplate => {
                "Variables: {{repo_parent}}, {{repo}}, {{key}}, {{slug}}, {{branch}}."
            }
            TextField::PromptTemplate => {
                "Sent to the agent as its first message. Variables: {{key}}, {{title}}, {{body}}, {{url}}, {{branch}}, {{repo}}."
            }
            TextField::SessionTitleTemplate => {
                "Tab title for a task session. Variables: {{key}}, {{short_title}}, {{title}}, {{branch}}, {{repo}}."
            }
            TextField::ShortTitleMaxChars => "Maximum characters of {{short_title}}.",
        }
    }

    fn search_terms(self) -> &'static str {
        match self {
            TextField::BranchTemplate => "task agent branch name template git",
            TextField::WorktreePathTemplate => "task agent worktree path directory template git",
            TextField::PromptTemplate => "task agent initial prompt template message",
            TextField::SessionTitleTemplate => "task agent session tab title template name",
            TextField::ShortTitleMaxChars => "task agent short title length characters",
        }
    }

    fn widget_id(self) -> &'static str {
        match self {
            TextField::BranchTemplate => "task_agents_branch_template",
            TextField::WorktreePathTemplate => "task_agents_worktree_path_template",
            TextField::PromptTemplate => "task_agents_prompt_template",
            TextField::SessionTitleTemplate => "task_agents_session_title_template",
            TextField::ShortTitleMaxChars => "task_agents_short_title_max_chars",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToggleField {
    PushOnCreate,
    FetchBeforeBranch,
    PreferAgentTitle,
}

impl ToggleField {
    fn value(self, settings: &TaskAgentSettings) -> bool {
        match self {
            ToggleField::PushOnCreate => *settings.push_on_create,
            ToggleField::FetchBeforeBranch => *settings.fetch_before_branch,
            ToggleField::PreferAgentTitle => *settings.prefer_agent_title,
        }
    }

    fn label(self) -> &'static str {
        match self {
            ToggleField::PushOnCreate => "Push new branches to origin",
            ToggleField::FetchBeforeBranch => "Fetch origin before branching",
            ToggleField::PreferAgentTitle => "Prefer the agent's own session title",
        }
    }

    fn search_terms(self) -> &'static str {
        match self {
            ToggleField::PushOnCreate => "task agent push upstream origin branch create",
            ToggleField::FetchBeforeBranch => "task agent fetch origin before branch",
            ToggleField::PreferAgentTitle => "task agent prefer agent session title tab name",
        }
    }

    fn widget_id(self) -> &'static str {
        match self {
            ToggleField::PushOnCreate => "task_agents_push_on_create",
            ToggleField::FetchBeforeBranch => "task_agents_fetch_before_branch",
            ToggleField::PreferAgentTitle => "task_agents_prefer_agent_title",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TaskAgentsPageAction {
    Toggle(ToggleField),
    SetDefaultCli(CLIAgent),
}

pub struct TaskAgentsPageView {
    page: PageType<Self>,
    editors: HashMap<TextField, ViewHandle<EditorView>>,
    default_cli_dropdown: ViewHandle<Dropdown<TaskAgentsPageAction>>,
}

impl TaskAgentsPageView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let editors = TextField::ALL
            .into_iter()
            .map(|field| (field, Self::build_editor(field, ctx)))
            .collect();

        let default_cli_dropdown = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = Dropdown::new(ctx);
            dropdown.set_top_bar_max_width(240.);
            dropdown.set_items(
                enum_iterator::all::<CLIAgent>()
                    .filter(|agent| !matches!(agent, CLIAgent::Unknown | CLIAgent::WarpTui))
                    .map(|agent| {
                        DropdownItem::new(
                            agent.display_name(),
                            TaskAgentsPageAction::SetDefaultCli(agent),
                        )
                    })
                    .collect(),
                ctx,
            );
            dropdown
        });

        ctx.subscribe_to_model(&TaskAgentSettings::handle(ctx), |me, _, _, ctx| {
            me.sync_from_settings(ctx);
        });

        let widgets: Vec<Box<dyn SettingsWidget<View = Self>>> = vec![
            Box::new(TextSettingWidget(TextField::BranchTemplate)),
            Box::new(TextSettingWidget(TextField::WorktreePathTemplate)),
            Box::new(DefaultCliWidget),
            Box::new(ToggleSettingWidget::new(ToggleField::FetchBeforeBranch)),
            Box::new(ToggleSettingWidget::new(ToggleField::PushOnCreate)),
            Box::new(TextSettingWidget(TextField::PromptTemplate)),
            Box::new(TextSettingWidget(TextField::SessionTitleTemplate)),
            Box::new(TextSettingWidget(TextField::ShortTitleMaxChars)),
            Box::new(ToggleSettingWidget::new(ToggleField::PreferAgentTitle)),
            Box::new(PerRepoWidget::default()),
        ];

        let mut view = Self {
            page: PageType::new_uncategorized(widgets, Some(PageTitle::new("Task agents"))),
            editors,
            default_cli_dropdown,
        };
        view.sync_from_settings(ctx);
        view
    }

    fn build_editor(field: TextField, ctx: &mut ViewContext<Self>) -> ViewHandle<EditorView> {
        let editor = ctx.add_typed_action_view(|ctx| match field {
            TextField::PromptTemplate => EditorView::new(
                EditorOptions {
                    soft_wrap: true,
                    ..Default::default()
                },
                ctx,
            ),
            TextField::BranchTemplate
            | TextField::WorktreePathTemplate
            | TextField::SessionTitleTemplate
            | TextField::ShortTitleMaxChars => {
                EditorView::single_line(SingleLineEditorOptions::default(), ctx)
            }
        });
        ctx.subscribe_to_view(&editor, move |me, _, event, ctx| {
            if matches!(event, EditorEvent::Blurred | EditorEvent::Enter) {
                me.save(field, ctx);
            }
        });
        editor
    }

    fn sync_from_settings(&mut self, ctx: &mut ViewContext<Self>) {
        for (field, editor) in &self.editors {
            let value = field.value(TaskAgentSettings::as_ref(ctx));
            if editor.as_ref(ctx).buffer_text(ctx) != value {
                editor.update(ctx, |editor, ctx| editor.set_buffer_text(&value, ctx));
            }
        }
        let default_cli = cli_from_command(&TaskAgentSettings::as_ref(ctx).default_cli)
            .unwrap_or(CLIAgent::Claude);
        self.default_cli_dropdown.update(ctx, |dropdown, ctx| {
            dropdown.set_selected_by_action(TaskAgentsPageAction::SetDefaultCli(default_cli), ctx);
        });
        ctx.notify();
    }

    fn save(&mut self, field: TextField, ctx: &mut ViewContext<Self>) {
        let Some(editor) = self.editors.get(&field) else {
            return;
        };
        let text = editor.as_ref(ctx).buffer_text(ctx);
        if text == field.value(TaskAgentSettings::as_ref(ctx)) {
            return;
        }
        TaskAgentSettings::handle(ctx).update(ctx, |settings, ctx| match field {
            TextField::BranchTemplate => {
                report_if_error!(settings.branch_template.set_value(text, ctx))
            }
            TextField::WorktreePathTemplate => {
                report_if_error!(settings.worktree_path_template.set_value(text, ctx))
            }
            TextField::PromptTemplate => {
                report_if_error!(settings.prompt_template.set_value(text, ctx))
            }
            TextField::SessionTitleTemplate => {
                report_if_error!(settings.session_title_template.set_value(text, ctx))
            }
            TextField::ShortTitleMaxChars => {
                if let Ok(max_chars) = text.trim().parse::<usize>() {
                    report_if_error!(settings.short_title_max_chars.set_value(max_chars, ctx));
                }
            }
        });
        // Restores the stored value when the input was rejected (e.g. not a number).
        self.sync_from_settings(ctx);
    }
}

impl Entity for TaskAgentsPageView {
    type Event = ();
}

impl TypedActionView for TaskAgentsPageView {
    type Action = TaskAgentsPageAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        TaskAgentSettings::handle(ctx).update(ctx, |settings, ctx| match action {
            TaskAgentsPageAction::Toggle(ToggleField::PushOnCreate) => {
                report_if_error!(settings.push_on_create.toggle_and_save_value(ctx));
            }
            TaskAgentsPageAction::Toggle(ToggleField::FetchBeforeBranch) => {
                report_if_error!(settings.fetch_before_branch.toggle_and_save_value(ctx));
            }
            TaskAgentsPageAction::Toggle(ToggleField::PreferAgentTitle) => {
                report_if_error!(settings.prefer_agent_title.toggle_and_save_value(ctx));
            }
            TaskAgentsPageAction::SetDefaultCli(cli) => {
                report_if_error!(
                    settings
                        .default_cli
                        .set_value(cli.command_prefix().to_string(), ctx)
                );
            }
        });
        ctx.notify();
    }
}

impl View for TaskAgentsPageView {
    fn ui_name() -> &'static str {
        "TaskAgentsSettingsPage"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        self.page.render(self, app)
    }
}

impl SettingsPageMeta for TaskAgentsPageView {
    fn section() -> SettingsSection {
        SettingsSection::TaskAgents
    }

    fn should_render(&self, _ctx: &AppContext) -> bool {
        FeatureFlag::TaskAgentLauncher.is_enabled()
    }

    fn update_filter(&mut self, query: &str, ctx: &mut ViewContext<Self>) -> MatchData {
        self.page.update_filter(query, ctx)
    }

    fn scroll_to_widget(&mut self, widget_id: &'static str) {
        self.page.scroll_to_widget(widget_id)
    }

    fn clear_highlighted_widget(&mut self) {
        self.page.clear_highlighted_widget();
    }
}

impl From<ViewHandle<TaskAgentsPageView>> for SettingsPageViewHandle {
    fn from(view_handle: ViewHandle<TaskAgentsPageView>) -> Self {
        SettingsPageViewHandle::TaskAgents(view_handle)
    }
}

/// Label, description, then a full-width text input.
fn render_text_row(
    label: &str,
    description: &str,
    input: Box<dyn Element>,
    appearance: &Appearance,
) -> Box<dyn Element> {
    Flex::column()
        .with_child(render_body_item_label::<TaskAgentsPageAction>(
            label.to_string(),
            None,
            None,
            LocalOnlyIconState::Hidden,
            ToggleState::Enabled,
            appearance,
        ))
        .with_child(
            appearance
                .ui_builder()
                .paragraph(description.to_string())
                .with_style(UiComponentStyles {
                    font_size: Some(12.),
                    ..Default::default()
                })
                .build()
                .finish(),
        )
        .with_child(
            warpui::elements::Container::new(input)
                .with_margin_top(6.)
                .with_margin_bottom(16.)
                .finish(),
        )
        .finish()
}

struct TextSettingWidget(TextField);

impl SettingsWidget for TextSettingWidget {
    type View = TaskAgentsPageView;

    fn widget_id(&self) -> &'static str {
        self.0.widget_id()
    }

    fn search_terms(&self) -> &str {
        self.0.search_terms()
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        _: &AppContext,
    ) -> Box<dyn Element> {
        let Some(editor) = view.editors.get(&self.0) else {
            return warpui::elements::Empty::new().finish();
        };
        let mut input = appearance.ui_builder().text_input(editor.clone());
        if self.0 == TextField::PromptTemplate {
            input = input.with_style(UiComponentStyles {
                height: Some(PROMPT_EDITOR_HEIGHT),
                ..Default::default()
            });
        }
        render_text_row(
            self.0.label(),
            self.0.description(),
            input.build().finish(),
            appearance,
        )
    }
}

struct ToggleSettingWidget {
    field: ToggleField,
    switch_state: SwitchStateHandle,
}

impl ToggleSettingWidget {
    fn new(field: ToggleField) -> Self {
        Self {
            field,
            switch_state: Default::default(),
        }
    }
}

impl SettingsWidget for ToggleSettingWidget {
    type View = TaskAgentsPageView;

    fn widget_id(&self) -> &'static str {
        self.field.widget_id()
    }

    fn search_terms(&self) -> &str {
        self.field.search_terms()
    }

    fn render(
        &self,
        _: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let field = self.field;
        render_body_item::<TaskAgentsPageAction>(
            field.label().to_string(),
            None,
            LocalOnlyIconState::Hidden,
            ToggleState::Enabled,
            appearance,
            appearance
                .ui_builder()
                .switch(self.switch_state.clone())
                .check(field.value(TaskAgentSettings::as_ref(app)))
                .build()
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(TaskAgentsPageAction::Toggle(field));
                })
                .finish(),
            None,
        )
    }
}

struct DefaultCliWidget;

impl SettingsWidget for DefaultCliWidget {
    type View = TaskAgentsPageView;

    fn search_terms(&self) -> &str {
        "task agent default cli agent claude codex"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        _: &AppContext,
    ) -> Box<dyn Element> {
        render_body_item::<TaskAgentsPageAction>(
            "Default agent".to_string(),
            None,
            LocalOnlyIconState::Hidden,
            ToggleState::Enabled,
            appearance,
            warpui::elements::ChildView::new(&view.default_cli_dropdown).finish(),
            Some("CLI agent started for a task unless a repository overrides it.".to_string()),
        )
    }
}

#[derive(Default)]
struct PerRepoWidget {
    button_mouse_state: MouseStateHandle,
}

impl SettingsWidget for PerRepoWidget {
    type View = TaskAgentsPageView;

    fn search_terms(&self) -> &str {
        "task agent per repository repo overrides setup commands"
    }

    fn render(
        &self,
        _: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let count = TaskAgentSettings::as_ref(app).per_repo.len();
        // ponytail: per-repo overrides are edited in settings.toml; add a table editor if needed.
        let button = appearance
            .ui_builder()
            .button(ButtonVariant::Secondary, self.button_mouse_state.clone())
            .with_text_label("Edit in settings file".to_string())
            .build()
            .on_click(|ctx, _, _| ctx.dispatch_typed_action(WorkspaceAction::OpenSettingsFile))
            .finish();
        render_body_item::<TaskAgentsPageAction>(
            format!("Per-repository overrides ({count})"),
            None,
            LocalOnlyIconState::Hidden,
            ToggleState::Enabled,
            appearance,
            button,
            Some(
                "[task_agents.per_repo.\"/path/to/repo\"] with branch_template, \
                 worktree_path_template, setup_commands = [...], default_cli."
                    .to_string(),
            ),
        )
    }
}

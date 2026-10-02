//! Settings page for the Jira integration (`jira.*`; the API token lives in the OS keychain).

use std::collections::HashMap;

use warp_core::settings::Setting as _;
use warp_errors::report_if_error;
use warpui::elements::{Container, Element, Empty, Flex, MouseStateHandle, ParentElement};
use warpui::ui_components::button::ButtonVariant;
use warpui::ui_components::components::{UiComponent, UiComponentStyles};
use warpui::{AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle};

use super::settings_page::{
    LocalOnlyIconState, MatchData, PageTitle, PageType, SettingsPageMeta, SettingsPageViewHandle,
    SettingsWidget, render_body_item, render_body_item_label,
};
use super::{SettingsSection, ToggleState};
use crate::appearance::Appearance;
use crate::editor::{EditorView, Event as EditorEvent, SingleLineEditorOptions};
use crate::features::FeatureFlag;
use crate::jira::settings::JiraSettings;
use crate::workspace::WorkspaceAction;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum TextField {
    SiteUrl,
    Email,
    DefaultJql,
    ProjectKeys,
}

impl TextField {
    const ALL: [TextField; 4] = [
        TextField::SiteUrl,
        TextField::Email,
        TextField::DefaultJql,
        TextField::ProjectKeys,
    ];

    fn value(self, settings: &JiraSettings) -> String {
        match self {
            TextField::SiteUrl => settings.site_url.clone(),
            TextField::Email => settings.email.clone(),
            TextField::DefaultJql => settings.default_jql.clone(),
            TextField::ProjectKeys => settings.project_keys.join(", "),
        }
    }

    fn label(self) -> &'static str {
        match self {
            TextField::SiteUrl => "Site URL",
            TextField::Email => "Account email",
            TextField::DefaultJql => "Default issue list (JQL)",
            TextField::ProjectKeys => "Projects",
        }
    }

    fn description(self) -> &'static str {
        match self {
            TextField::SiteUrl => "Your Jira Cloud site.",
            TextField::Email => "The Atlassian account the API token belongs to.",
            TextField::DefaultJql => {
                "What the issue picker lists when it opens. Type jql: in the picker for other searches."
            }
            TextField::ProjectKeys => {
                "Comma-separated project keys that limit the default list. Empty: all projects."
            }
        }
    }

    fn placeholder(self) -> &'static str {
        match self {
            TextField::SiteUrl => "https://example.atlassian.net",
            TextField::Email => "you@example.com",
            TextField::DefaultJql => crate::jira::settings::DEFAULT_JQL,
            TextField::ProjectKeys => "EXAMPLE, OTHER",
        }
    }

    fn search_terms(self) -> &'static str {
        match self {
            TextField::SiteUrl => "jira site url atlassian host",
            TextField::Email => "jira account email user",
            TextField::DefaultJql => "jira default jql query issue list filter",
            TextField::ProjectKeys => "jira projects project keys filter",
        }
    }

    fn widget_id(self) -> &'static str {
        match self {
            TextField::SiteUrl => "jira_site_url",
            TextField::Email => "jira_email",
            TextField::DefaultJql => "jira_default_jql",
            TextField::ProjectKeys => "jira_project_keys",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum ConnectionState {
    Idle,
    Testing,
    Connected(String),
    Failed(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum JiraPageAction {
    TestConnection,
    ClearToken,
}

pub struct JiraPageView {
    page: PageType<Self>,
    editors: HashMap<TextField, ViewHandle<EditorView>>,
    token_editor: ViewHandle<EditorView>,
    /// Whether the keychain holds a token; `None` until the page is first shown, so the keychain
    /// is not read when the settings view is built.
    has_token: Option<bool>,
    token_message: Option<String>,
    connection: ConnectionState,
}

impl JiraPageView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let editors = TextField::ALL
            .into_iter()
            .map(|field| (field, Self::build_editor(field, ctx)))
            .collect();

        let token_editor = ctx.add_typed_action_view(|ctx| {
            let mut editor = EditorView::single_line(
                SingleLineEditorOptions {
                    is_password: true,
                    ..Default::default()
                },
                ctx,
            );
            editor.set_placeholder_text("Paste an API token and press Enter", ctx);
            editor
        });
        ctx.subscribe_to_view(&token_editor, |me, _, event, ctx| {
            if matches!(event, EditorEvent::Blurred | EditorEvent::Enter) {
                me.save_token(ctx);
            }
        });

        ctx.subscribe_to_model(&JiraSettings::handle(ctx), |me, _, _, ctx| {
            me.sync_from_settings(ctx);
        });

        let widgets: Vec<Box<dyn SettingsWidget<View = Self>>> = vec![
            Box::new(TextSettingWidget(TextField::SiteUrl)),
            Box::new(TextSettingWidget(TextField::Email)),
            Box::new(ApiTokenWidget::default()),
            Box::new(TestConnectionWidget::default()),
            Box::new(TextSettingWidget(TextField::DefaultJql)),
            Box::new(TextSettingWidget(TextField::ProjectKeys)),
            Box::new(BranchTypeWidget::default()),
        ];

        let mut view = Self {
            page: PageType::new_uncategorized(widgets, Some(PageTitle::new("Jira"))),
            editors,
            token_editor,
            has_token: None,
            token_message: None,
            connection: ConnectionState::Idle,
        };
        view.sync_from_settings(ctx);
        view
    }

    fn build_editor(field: TextField, ctx: &mut ViewContext<Self>) -> ViewHandle<EditorView> {
        let editor = ctx.add_typed_action_view(|ctx| {
            let mut editor = EditorView::single_line(SingleLineEditorOptions::default(), ctx);
            editor.set_placeholder_text(field.placeholder(), ctx);
            editor
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
            let value = field.value(JiraSettings::as_ref(ctx));
            if editor.as_ref(ctx).buffer_text(ctx) != value {
                editor.update(ctx, |editor, ctx| editor.set_buffer_text(&value, ctx));
            }
        }
        ctx.notify();
    }

    fn save(&mut self, field: TextField, ctx: &mut ViewContext<Self>) {
        let Some(editor) = self.editors.get(&field) else {
            return;
        };
        let text = editor.as_ref(ctx).buffer_text(ctx).trim().to_string();
        if text == field.value(JiraSettings::as_ref(ctx)) {
            return;
        }
        JiraSettings::handle(ctx).update(ctx, |settings, ctx| match field {
            TextField::SiteUrl => report_if_error!(settings.site_url.set_value(text, ctx)),
            TextField::Email => report_if_error!(settings.email.set_value(text, ctx)),
            TextField::DefaultJql => report_if_error!(settings.default_jql.set_value(text, ctx)),
            TextField::ProjectKeys => {
                let keys = text
                    .split(',')
                    .map(|key| key.trim().to_string())
                    .filter(|key| !key.is_empty())
                    .collect();
                report_if_error!(settings.project_keys.set_value(keys, ctx))
            }
        });
        self.connection = ConnectionState::Idle;
        self.sync_from_settings(ctx);
    }

    fn save_token(&mut self, ctx: &mut ViewContext<Self>) {
        let token = self.token_editor.as_ref(ctx).buffer_text(ctx);
        if token.trim().is_empty() {
            return;
        }
        self.token_message = Some(match crate::jira::save_token(ctx, &token) {
            Ok(()) => {
                self.has_token = Some(true);
                "Saved to the keychain.".to_string()
            }
            Err(err) => {
                log::error!("[Jira] Failed to save the API token to secure storage: {err:#}");
                format!("Could not save the token: {err}")
            }
        });
        self.token_editor
            .update(ctx, |editor, ctx| editor.clear_buffer(ctx));
        self.connection = ConnectionState::Idle;
        ctx.notify();
    }

    fn clear_token(&mut self, ctx: &mut ViewContext<Self>) {
        self.token_message = Some(match crate::jira::clear_token(ctx) {
            Ok(()) => {
                self.has_token = Some(false);
                "Token removed from the keychain.".to_string()
            }
            Err(err) => {
                log::error!("[Jira] Failed to remove the API token from secure storage: {err:#}");
                format!("Could not remove the token: {err}")
            }
        });
        self.connection = ConnectionState::Idle;
        ctx.notify();
    }

    fn test_connection(&mut self, ctx: &mut ViewContext<Self>) {
        for field in [TextField::SiteUrl, TextField::Email] {
            self.save(field, ctx);
        }
        self.save_token(ctx);
        let client = match crate::jira::client(ctx) {
            Ok(client) => client,
            Err(err) => {
                self.connection = ConnectionState::Failed(err.to_string());
                ctx.notify();
                return;
            }
        };
        self.connection = ConnectionState::Testing;
        ctx.spawn(
            async move { client.test_connection().await },
            |me, result, ctx| {
                me.connection = match result {
                    Ok(account) => ConnectionState::Connected(account.display_name),
                    Err(err) => {
                        log::warn!("[Jira] Connection test failed: {err}");
                        ConnectionState::Failed(err.to_string())
                    }
                };
                ctx.notify();
            },
        );
        ctx.notify();
    }
}

impl Entity for JiraPageView {
    type Event = ();
}

impl TypedActionView for JiraPageView {
    type Action = JiraPageAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            JiraPageAction::TestConnection => self.test_connection(ctx),
            JiraPageAction::ClearToken => self.clear_token(ctx),
        }
    }
}

impl View for JiraPageView {
    fn ui_name() -> &'static str {
        "JiraSettingsPage"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        self.page.render(self, app)
    }
}

impl SettingsPageMeta for JiraPageView {
    fn section() -> SettingsSection {
        SettingsSection::Jira
    }

    fn on_page_selected(&mut self, _allow_steal_focus: bool, ctx: &mut ViewContext<Self>) {
        self.has_token = Some(crate::jira::read_token(ctx).is_some());
        ctx.notify();
    }

    fn should_render(&self, _ctx: &AppContext) -> bool {
        FeatureFlag::JiraIntegration.is_enabled()
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

impl From<ViewHandle<JiraPageView>> for SettingsPageViewHandle {
    fn from(view_handle: ViewHandle<JiraPageView>) -> Self {
        SettingsPageViewHandle::Jira(view_handle)
    }
}

/// Label, description, then the input (and anything after it).
fn render_field_row(
    label: &str,
    description: &str,
    input: Box<dyn Element>,
    appearance: &Appearance,
) -> Box<dyn Element> {
    Flex::column()
        .with_child(render_body_item_label::<JiraPageAction>(
            label.to_string(),
            None,
            None,
            LocalOnlyIconState::Hidden,
            ToggleState::Enabled,
            appearance,
        ))
        .with_child(render_note(description, appearance))
        .with_child(
            Container::new(input)
                .with_margin_top(6.)
                .with_margin_bottom(16.)
                .finish(),
        )
        .finish()
}

fn render_note(text: &str, appearance: &Appearance) -> Box<dyn Element> {
    appearance
        .ui_builder()
        .paragraph(text.to_string())
        .with_style(UiComponentStyles {
            font_size: Some(12.),
            ..Default::default()
        })
        .build()
        .finish()
}

fn render_button(
    label: &str,
    mouse_state: MouseStateHandle,
    action: JiraPageAction,
    appearance: &Appearance,
) -> Box<dyn Element> {
    appearance
        .ui_builder()
        .button(ButtonVariant::Secondary, mouse_state)
        .with_text_label(label.to_string())
        .build()
        .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
        .finish()
}

struct TextSettingWidget(TextField);

impl SettingsWidget for TextSettingWidget {
    type View = JiraPageView;

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
            return Empty::new().finish();
        };
        render_field_row(
            self.0.label(),
            self.0.description(),
            appearance
                .ui_builder()
                .text_input(editor.clone())
                .build()
                .finish(),
            appearance,
        )
    }
}

#[derive(Default)]
struct ApiTokenWidget {
    clear_button_mouse_state: MouseStateHandle,
}

impl SettingsWidget for ApiTokenWidget {
    type View = JiraPageView;

    fn widget_id(&self) -> &'static str {
        "jira_api_token"
    }

    fn search_terms(&self) -> &str {
        "jira api token keychain secret password clear"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        _: &AppContext,
    ) -> Box<dyn Element> {
        let state = match view.has_token {
            Some(true) => "A token is stored in the OS keychain. Paste a new one to replace it.",
            Some(false) => "No token stored. Create one in your Atlassian account settings.",
            None => "Stored in the OS keychain, never in the settings file.",
        };
        let mut column = Flex::column()
            .with_child(
                appearance
                    .ui_builder()
                    .text_input(view.token_editor.clone())
                    .build()
                    .finish(),
            )
            .with_child(
                Container::new(render_button(
                    "Clear token",
                    self.clear_button_mouse_state.clone(),
                    JiraPageAction::ClearToken,
                    appearance,
                ))
                .with_margin_top(8.)
                .finish(),
            );
        if let Some(message) = &view.token_message {
            column.add_child(
                Container::new(render_note(message, appearance))
                    .with_margin_top(6.)
                    .finish(),
            );
        }
        render_field_row("API token", state, column.finish(), appearance)
    }
}

#[derive(Default)]
struct TestConnectionWidget {
    button_mouse_state: MouseStateHandle,
}

impl SettingsWidget for TestConnectionWidget {
    type View = JiraPageView;

    fn widget_id(&self) -> &'static str {
        "jira_test_connection"
    }

    fn search_terms(&self) -> &str {
        "jira test connection check credentials"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        _: &AppContext,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let (message, is_error) = match &view.connection {
            ConnectionState::Idle => (None, false),
            ConnectionState::Testing => (Some("Testing…".to_string()), false),
            ConnectionState::Connected(name) if name.is_empty() => {
                (Some("Connected.".to_string()), false)
            }
            ConnectionState::Connected(name) => (Some(format!("Connected as {name}.")), false),
            ConnectionState::Failed(error) => (Some(error.clone()), true),
        };
        let mut row = Flex::column().with_child(render_button(
            "Test connection",
            self.button_mouse_state.clone(),
            JiraPageAction::TestConnection,
            appearance,
        ));
        if let Some(message) = message {
            let color = if is_error {
                theme.ui_error_color()
            } else {
                theme.sub_text_color(theme.background()).into_solid()
            };
            row.add_child(
                Container::new(
                    appearance
                        .ui_builder()
                        .paragraph(message)
                        .with_style(UiComponentStyles {
                            font_color: Some(color),
                            ..Default::default()
                        })
                        .build()
                        .finish(),
                )
                .with_margin_top(6.)
                .finish(),
            );
        }
        Container::new(row.finish())
            .with_margin_bottom(16.)
            .finish()
    }
}

#[derive(Default)]
struct BranchTypeWidget {
    button_mouse_state: MouseStateHandle,
}

impl SettingsWidget for BranchTypeWidget {
    type View = JiraPageView;

    fn search_terms(&self) -> &str {
        "jira issue type branch type bug fix feat mapping"
    }

    fn render(
        &self,
        _: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let mut mapping: Vec<String> = JiraSettings::as_ref(app)
            .issue_type_to_branch_type
            .iter()
            .map(|(issue_type, branch_type)| format!("{issue_type} → {branch_type}"))
            .collect();
        mapping.sort();
        // ponytail: the mapping is edited in settings.toml; add a table editor if needed.
        let button = appearance
            .ui_builder()
            .button(ButtonVariant::Secondary, self.button_mouse_state.clone())
            .with_text_label("Edit in settings file".to_string())
            .build()
            .on_click(|ctx, _, _| ctx.dispatch_typed_action(WorkspaceAction::OpenSettingsFile))
            .finish();
        render_body_item::<JiraPageAction>(
            "Branch type per issue type".to_string(),
            None,
            LocalOnlyIconState::Hidden,
            ToggleState::Enabled,
            appearance,
            button,
            Some(format!(
                "{} (others: feat). [jira.issue_type_to_branch_type] in the settings file.",
                mapping.join(", ")
            )),
        )
    }
}

//! Settings page for the PR stack view (`FeatureFlag::PrStackView`).

use settings::{Setting as _, ToggleableSetting as _};
use warp_errors::report_if_error;
use warpui::elements::{ChildView, Element};
use warpui::ui_components::components::{UiComponent, UiComponentStyles};
use warpui::ui_components::switch::SwitchStateHandle;
use warpui::{AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle};

use super::settings_page::{
    LocalOnlyIconState, MatchData, PageTitle, PageType, SettingsPageMeta, SettingsPageViewHandle,
    SettingsWidget, render_body_item,
};
use super::{SettingsSection, ToggleState};
use crate::appearance::Appearance;
use crate::editor::{EditorView, Event as EditorEvent, SingleLineEditorOptions, TextOptions};
use crate::features::FeatureFlag;
use crate::pr_stack::PrStackSettings;
use crate::pr_stack::settings::PrStackRowOrder;
use crate::view_components::{Dropdown, DropdownItem};

const TEXT_INPUT_WIDTH: f32 = 280.;

#[derive(Clone, Debug, PartialEq)]
pub enum PrStackPageAction {
    ToggleAutoRestack,
    SetRowOrder(PrStackRowOrder),
}

/// The text settings edited on this page.
#[derive(Clone, Copy)]
enum TextSetting {
    PollInterval,
    PrepareCommand,
    BodyFileTemplate,
}

pub struct PrStackPageView {
    page: PageType<Self>,
    row_order_dropdown: ViewHandle<Dropdown<PrStackPageAction>>,
    poll_interval_editor: ViewHandle<EditorView>,
    prepare_command_editor: ViewHandle<EditorView>,
    body_file_template_editor: ViewHandle<EditorView>,
}

impl PrStackPageView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let row_order_dropdown = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = Dropdown::new(ctx);
            dropdown.set_top_bar_max_width(TEXT_INPUT_WIDTH);
            dropdown
        });
        let poll_interval_editor = Self::text_editor(TextSetting::PollInterval, ctx);
        let prepare_command_editor = Self::text_editor(TextSetting::PrepareCommand, ctx);
        let body_file_template_editor = Self::text_editor(TextSetting::BodyFileTemplate, ctx);

        let view = Self {
            page: PageType::new_uncategorized(
                vec![
                    Box::new(AutoRestackWidget::default()),
                    Box::new(TextSettingWidget(TextSetting::PrepareCommand)),
                    Box::new(TextSettingWidget(TextSetting::BodyFileTemplate)),
                    Box::new(RowOrderWidget),
                    Box::new(TextSettingWidget(TextSetting::PollInterval)),
                    Box::new(SettingsFileWidget),
                ],
                Some(PageTitle::new("PR stack")),
            ),
            row_order_dropdown,
            poll_interval_editor,
            prepare_command_editor,
            body_file_template_editor,
        };
        view.sync_from_settings(ctx);
        ctx.subscribe_to_model(&PrStackSettings::handle(ctx), |view, _, _, ctx| {
            view.sync_from_settings(ctx);
            ctx.notify();
        });
        view
    }

    fn text_editor(setting: TextSetting, ctx: &mut ViewContext<Self>) -> ViewHandle<EditorView> {
        let editor = ctx.add_typed_action_view(|ctx| {
            let font_size = Appearance::as_ref(ctx).ui_font_size() - 2.;
            EditorView::single_line(
                SingleLineEditorOptions {
                    text: TextOptions {
                        font_size_override: Some(font_size),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ctx,
            )
        });
        ctx.subscribe_to_view(&editor, move |view, _, event, ctx| {
            if matches!(event, EditorEvent::Enter | EditorEvent::Blurred) {
                view.save_text_setting(setting, ctx);
            }
        });
        editor
    }

    fn editor(&self, setting: TextSetting) -> &ViewHandle<EditorView> {
        match setting {
            TextSetting::PollInterval => &self.poll_interval_editor,
            TextSetting::PrepareCommand => &self.prepare_command_editor,
            TextSetting::BodyFileTemplate => &self.body_file_template_editor,
        }
    }

    fn sync_from_settings(&self, ctx: &mut ViewContext<Self>) {
        let settings = PrStackSettings::as_ref(ctx);
        let poll = settings.poll_interval_secs.value().to_string();
        let command = settings.pr_prepare_command.value().clone();
        let template = settings.pr_body_file_template.value().clone();
        let order = *settings.row_order.value();
        for (editor, text) in [
            (&self.poll_interval_editor, poll),
            (&self.prepare_command_editor, command),
            (&self.body_file_template_editor, template),
        ] {
            editor.update(ctx, |editor, ctx| {
                if editor.buffer_text(ctx) != text {
                    editor.set_buffer_text(&text, ctx);
                }
            });
        }
        self.row_order_dropdown.update(ctx, |dropdown, ctx| {
            dropdown.set_items(
                PrStackRowOrder::ALL
                    .into_iter()
                    .map(|order| {
                        DropdownItem::new(order.label(), PrStackPageAction::SetRowOrder(order))
                    })
                    .collect(),
                ctx,
            );
            dropdown.set_selected_by_action(PrStackPageAction::SetRowOrder(order), ctx);
        });
    }

    fn save_text_setting(&mut self, setting: TextSetting, ctx: &mut ViewContext<Self>) {
        let text = self.editor(setting).as_ref(ctx).buffer_text(ctx);
        PrStackSettings::handle(ctx).update(ctx, |settings, ctx| match setting {
            TextSetting::PollInterval => {
                // Invalid numbers are ignored; the editor resyncs below.
                if let Ok(secs) = text.trim().parse::<u64>() {
                    report_if_error!(settings.poll_interval_secs.set_value(secs, ctx));
                }
            }
            TextSetting::PrepareCommand => {
                report_if_error!(
                    settings
                        .pr_prepare_command
                        .set_value(text.trim().to_string(), ctx)
                );
            }
            TextSetting::BodyFileTemplate => {
                report_if_error!(
                    settings
                        .pr_body_file_template
                        .set_value(text.trim().to_string(), ctx)
                );
            }
        });
        self.sync_from_settings(ctx);
    }
}

impl Entity for PrStackPageView {
    type Event = ();
}

impl TypedActionView for PrStackPageView {
    type Action = PrStackPageAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        PrStackSettings::handle(ctx).update(ctx, |settings, ctx| match action {
            PrStackPageAction::ToggleAutoRestack => {
                report_if_error!(settings.auto_restack.toggle_and_save_value(ctx));
            }
            PrStackPageAction::SetRowOrder(order) => {
                report_if_error!(settings.row_order.set_value(*order, ctx));
            }
        });
        ctx.notify();
    }
}

impl View for PrStackPageView {
    fn ui_name() -> &'static str {
        "PrStackSettingsPage"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        self.page.render(self, app)
    }
}

impl SettingsPageMeta for PrStackPageView {
    fn section() -> SettingsSection {
        SettingsSection::PrStack
    }

    fn should_render(&self, _ctx: &AppContext) -> bool {
        FeatureFlag::PrStackView.is_enabled()
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

impl From<ViewHandle<PrStackPageView>> for SettingsPageViewHandle {
    fn from(view_handle: ViewHandle<PrStackPageView>) -> Self {
        SettingsPageViewHandle::PrStack(view_handle)
    }
}

#[derive(Default)]
struct AutoRestackWidget {
    switch_state: SwitchStateHandle,
}

impl SettingsWidget for AutoRestackWidget {
    type View = PrStackPageView;

    fn search_terms(&self) -> &str {
        "pr stack auto restack rebase merged target sync"
    }

    fn render(
        &self,
        _view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        render_body_item::<PrStackPageAction>(
            "Restack automatically".into(),
            None,
            LocalOnlyIconState::Hidden,
            ToggleState::Enabled,
            appearance,
            appearance
                .ui_builder()
                .switch(self.switch_state.clone())
                .check(*PrStackSettings::as_ref(app).auto_restack.value())
                .build()
                .on_click(|ctx, _, _| {
                    ctx.dispatch_typed_action(PrStackPageAction::ToggleAutoRestack);
                })
                .finish(),
            Some(
                "Rebase the stack when the bottom PR merges or the target branch moves. \
                 Conflicts are handed to the agent pane, never resolved automatically."
                    .into(),
            ),
        )
    }
}

struct TextSettingWidget(TextSetting);

impl SettingsWidget for TextSettingWidget {
    type View = PrStackPageView;

    fn search_terms(&self) -> &str {
        match self.0 {
            TextSetting::PollInterval => "pr stack poll interval refresh seconds status",
            TextSetting::PrepareCommand => "pr stack prepare command agent title body create pr",
            TextSetting::BodyFileTemplate => {
                "pr stack body file template title description create pr"
            }
        }
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        _app: &AppContext,
    ) -> Box<dyn Element> {
        let (label, description) = match self.0 {
            TextSetting::PollInterval => (
                "Status refresh interval (seconds)",
                "How often the stack view fetches and reloads PR status while it is open. 0 turns it off.",
            ),
            TextSetting::PrepareCommand => (
                "PR prepare command",
                "Sent to the branch's agent pane when the PR body file is missing, for example a \
                 slash command that writes it. {{branch}} becomes the branch name. Leave empty to \
                 use the commit-based template.",
            ),
            TextSetting::BodyFileTemplate => (
                "PR body file",
                "Where the PR title (first line) and body are read from. Paths under .git/ live in \
                 the shared git directory, so they are never committed.",
            ),
        };
        render_body_item::<PrStackPageAction>(
            label.into(),
            None,
            LocalOnlyIconState::Hidden,
            ToggleState::Enabled,
            appearance,
            appearance
                .ui_builder()
                .text_input(view.editor(self.0).clone())
                .with_style(UiComponentStyles {
                    width: Some(TEXT_INPUT_WIDTH),
                    ..Default::default()
                })
                .build()
                .finish(),
            Some(description.into()),
        )
    }
}

struct RowOrderWidget;

impl SettingsWidget for RowOrderWidget {
    type View = PrStackPageView;

    fn search_terms(&self) -> &str {
        "pr stack row order bottom top"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        _app: &AppContext,
    ) -> Box<dyn Element> {
        render_body_item::<PrStackPageAction>(
            "Row order".into(),
            None,
            LocalOnlyIconState::Hidden,
            ToggleState::Enabled,
            appearance,
            ChildView::new(&view.row_order_dropdown).finish(),
            None,
        )
    }
}

/// Targets and classification rules are structured, so they are edited in
/// the settings file rather than here.
struct SettingsFileWidget;

impl SettingsWidget for SettingsFileWidget {
    type View = PrStackPageView;

    fn search_terms(&self) -> &str {
        "pr stack target branch repository classification rules tests docs config settings file"
    }

    fn render(
        &self,
        _view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let settings = PrStackSettings::as_ref(app);
        let summary = format!(
            "{} repository targets, {} classification rules",
            settings.targets.value().len(),
            settings.classification.value().len()
        );
        render_body_item::<PrStackPageAction>(
            "Targets and classification rules".into(),
            None,
            LocalOnlyIconState::Hidden,
            ToggleState::Enabled,
            appearance,
            appearance.ui_builder().span(summary).build().finish(),
            Some(
                "Edit pr_stack.targets (target branch per repository path) and \
                 pr_stack.classification (ordered path patterns for tests, docs and config) in \
                 the settings file. A target in .git/warp-stack.json overrides both."
                    .into(),
            ),
        )
    }
}

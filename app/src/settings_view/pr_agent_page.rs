//! Settings page for the PR review agent (`pr_agent.*`).

use std::collections::HashMap;

use warp_core::settings::{Setting as _, ToggleableSetting as _};
use warp_errors::report_if_error;
use warpui::elements::{ChildView, Container, Element, Empty, Flex, ParentElement};
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
use crate::pr_agent::CheckoutMode;
use crate::pr_agent::settings::PrAgentSettings;
use crate::view_components::{Dropdown, DropdownItem};

const TEMPLATE_EDITOR_HEIGHT: f32 = 140.;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum TextField {
    PollInterval,
    ReviewOtherTemplate,
    WatchOwnTemplate,
}

impl TextField {
    const ALL: [TextField; 3] = [
        TextField::PollInterval,
        TextField::ReviewOtherTemplate,
        TextField::WatchOwnTemplate,
    ];

    fn value(self, settings: &PrAgentSettings) -> String {
        match self {
            TextField::PollInterval => (*settings.poll_interval_secs).to_string(),
            TextField::ReviewOtherTemplate => settings.review_other_template.clone(),
            TextField::WatchOwnTemplate => settings.watch_own_template.clone(),
        }
    }

    fn label(self) -> &'static str {
        match self {
            TextField::PollInterval => "Poll interval (seconds)",
            TextField::ReviewOtherTemplate => "Prompt for reviewing someone else's pull request",
            TextField::WatchOwnTemplate => "Prompt for watching your own pull request",
        }
    }

    fn description(self) -> &'static str {
        match self {
            TextField::PollInterval => {
                "How often a watched pull request is checked for new commits, reviews, comments \
                 and check results. Minimum 15."
            }
            TextField::ReviewOtherTemplate | TextField::WatchOwnTemplate => {
                "Sent to the agent as its first message. Variables: {{number}}, {{title}}, \
                 {{url}}, {{author}}, {{base}}, {{repo}}, {{checkout_path}}, {{branch}}."
            }
        }
    }

    fn search_terms(self) -> &'static str {
        match self {
            TextField::PollInterval => "pr pull request agent poll interval seconds watch",
            TextField::ReviewOtherTemplate => {
                "pr pull request agent review prompt template someone else"
            }
            TextField::WatchOwnTemplate => "pr pull request agent watch own prompt template mine",
        }
    }

    fn widget_id(self) -> &'static str {
        match self {
            TextField::PollInterval => "pr_agent_poll_interval_secs",
            TextField::ReviewOtherTemplate => "pr_agent_review_other_template",
            TextField::WatchOwnTemplate => "pr_agent_watch_own_template",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PrAgentPageAction {
    ToggleAutoForward,
    SetDefaultCheckout(CheckoutMode),
}

pub struct PrAgentPageView {
    page: PageType<Self>,
    editors: HashMap<TextField, ViewHandle<EditorView>>,
    checkout_dropdown: ViewHandle<Dropdown<PrAgentPageAction>>,
}

impl PrAgentPageView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let editors = TextField::ALL
            .into_iter()
            .map(|field| (field, Self::build_editor(field, ctx)))
            .collect();

        let checkout_dropdown = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = Dropdown::new(ctx);
            dropdown.set_top_bar_max_width(240.);
            dropdown.set_items(
                CheckoutMode::ALL
                    .into_iter()
                    .map(|mode| {
                        DropdownItem::new(mode.label(), PrAgentPageAction::SetDefaultCheckout(mode))
                    })
                    .collect(),
                ctx,
            );
            dropdown
        });

        ctx.subscribe_to_model(&PrAgentSettings::handle(ctx), |me, _, _, ctx| {
            me.sync_from_settings(ctx);
        });

        let widgets: Vec<Box<dyn SettingsWidget<View = Self>>> = vec![
            Box::new(DefaultCheckoutWidget),
            Box::new(AutoForwardWidget::default()),
            Box::new(TextSettingWidget(TextField::PollInterval)),
            Box::new(TextSettingWidget(TextField::ReviewOtherTemplate)),
            Box::new(TextSettingWidget(TextField::WatchOwnTemplate)),
        ];

        let mut view = Self {
            page: PageType::new_uncategorized(widgets, Some(PageTitle::new("PR agent"))),
            editors,
            checkout_dropdown,
        };
        view.sync_from_settings(ctx);
        view
    }

    fn build_editor(field: TextField, ctx: &mut ViewContext<Self>) -> ViewHandle<EditorView> {
        let editor = ctx.add_typed_action_view(|ctx| match field {
            TextField::PollInterval => {
                EditorView::single_line(SingleLineEditorOptions::default(), ctx)
            }
            TextField::ReviewOtherTemplate | TextField::WatchOwnTemplate => EditorView::new(
                EditorOptions {
                    soft_wrap: true,
                    ..Default::default()
                },
                ctx,
            ),
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
            let value = field.value(PrAgentSettings::as_ref(ctx));
            if editor.as_ref(ctx).buffer_text(ctx) != value {
                editor.update(ctx, |editor, ctx| editor.set_buffer_text(&value, ctx));
            }
        }
        let checkout = PrAgentSettings::as_ref(ctx).default_checkout_mode();
        self.checkout_dropdown.update(ctx, |dropdown, ctx| {
            dropdown.set_selected_by_action(PrAgentPageAction::SetDefaultCheckout(checkout), ctx);
        });
        ctx.notify();
    }

    fn save(&mut self, field: TextField, ctx: &mut ViewContext<Self>) {
        let Some(editor) = self.editors.get(&field) else {
            return;
        };
        let text = editor.as_ref(ctx).buffer_text(ctx);
        if text == field.value(PrAgentSettings::as_ref(ctx)) {
            return;
        }
        PrAgentSettings::handle(ctx).update(ctx, |settings, ctx| match field {
            TextField::PollInterval => {
                if let Ok(secs) = text.trim().parse::<usize>() {
                    report_if_error!(settings.poll_interval_secs.set_value(secs, ctx));
                }
            }
            TextField::ReviewOtherTemplate => {
                report_if_error!(settings.review_other_template.set_value(text, ctx))
            }
            TextField::WatchOwnTemplate => {
                report_if_error!(settings.watch_own_template.set_value(text, ctx))
            }
        });
        // Restores the stored value when the input was rejected (e.g. not a number).
        self.sync_from_settings(ctx);
    }
}

impl Entity for PrAgentPageView {
    type Event = ();
}

impl TypedActionView for PrAgentPageView {
    type Action = PrAgentPageAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        PrAgentSettings::handle(ctx).update(ctx, |settings, ctx| match action {
            PrAgentPageAction::ToggleAutoForward => {
                report_if_error!(settings.auto_forward_events.toggle_and_save_value(ctx));
            }
            PrAgentPageAction::SetDefaultCheckout(mode) => {
                report_if_error!(
                    settings
                        .default_checkout
                        .set_value(mode.setting_value().to_string(), ctx)
                );
            }
        });
        ctx.notify();
    }
}

impl View for PrAgentPageView {
    fn ui_name() -> &'static str {
        "PrAgentSettingsPage"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        self.page.render(self, app)
    }
}

impl SettingsPageMeta for PrAgentPageView {
    fn section() -> SettingsSection {
        SettingsSection::PrAgent
    }

    fn should_render(&self, _ctx: &AppContext) -> bool {
        FeatureFlag::PrReviewAgent.is_enabled()
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

impl From<ViewHandle<PrAgentPageView>> for SettingsPageViewHandle {
    fn from(view_handle: ViewHandle<PrAgentPageView>) -> Self {
        SettingsPageViewHandle::PrAgent(view_handle)
    }
}

struct TextSettingWidget(TextField);

impl SettingsWidget for TextSettingWidget {
    type View = PrAgentPageView;

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
        let mut input = appearance.ui_builder().text_input(editor.clone());
        if self.0 != TextField::PollInterval {
            input = input.with_style(UiComponentStyles {
                height: Some(TEMPLATE_EDITOR_HEIGHT),
                ..Default::default()
            });
        }
        Flex::column()
            .with_child(render_body_item_label::<PrAgentPageAction>(
                self.0.label().to_string(),
                None,
                None,
                LocalOnlyIconState::Hidden,
                ToggleState::Enabled,
                appearance,
            ))
            .with_child(
                appearance
                    .ui_builder()
                    .paragraph(self.0.description().to_string())
                    .with_style(UiComponentStyles {
                        font_size: Some(12.),
                        ..Default::default()
                    })
                    .build()
                    .finish(),
            )
            .with_child(
                Container::new(input.build().finish())
                    .with_margin_top(6.)
                    .with_margin_bottom(16.)
                    .finish(),
            )
            .finish()
    }
}

#[derive(Default)]
struct AutoForwardWidget {
    switch_state: SwitchStateHandle,
}

impl SettingsWidget for AutoForwardWidget {
    type View = PrAgentPageView;

    fn search_terms(&self) -> &str {
        "pr pull request agent forward send events updates comments reviews checks automatically"
    }

    fn render(
        &self,
        _: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        render_body_item::<PrAgentPageAction>(
            "Send pull request updates to the agent".to_string(),
            None,
            LocalOnlyIconState::Hidden,
            ToggleState::Enabled,
            appearance,
            appearance
                .ui_builder()
                .switch(self.switch_state.clone())
                .check(*PrAgentSettings::as_ref(app).auto_forward_events)
                .build()
                .on_click(|ctx, _, _| {
                    ctx.dispatch_typed_action(PrAgentPageAction::ToggleAutoForward);
                })
                .finish(),
            Some(
                "New commits, review comments, reviews and check results are sent as a message \
                 once the agent is idle. The prompt decides what the agent does with them."
                    .to_string(),
            ),
        )
    }
}

struct DefaultCheckoutWidget;

impl SettingsWidget for DefaultCheckoutWidget {
    type View = PrAgentPageView;

    fn search_terms(&self) -> &str {
        "pr pull request agent default checkout worktree branch here"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        _: &AppContext,
    ) -> Box<dyn Element> {
        render_body_item::<PrAgentPageAction>(
            "Default checkout".to_string(),
            None,
            LocalOnlyIconState::Hidden,
            ToggleState::Enabled,
            appearance,
            ChildView::new(&view.checkout_dropdown).finish(),
            Some("Where a pull request is checked out for the agent.".to_string()),
        )
    }
}

//! Modal body for starting a PR agent: pull request reference, checkout mode, CLI agent, prompt
//! template and an editable preview of the prompt.

use std::path::PathBuf;
use std::rc::Rc;

use warp_core::safe_warn;
use warpui::r#async::SpawnedFutureHandle;
use warpui::elements::{
    Border, ChildView, ClippedScrollStateHandle, ClippedScrollable, ConstrainedBox, Container,
    CrossAxisAlignment, Element, Fill, Flex, MainAxisAlignment, MainAxisSize, MouseStateHandle,
    Padding, ParentElement, ScrollbarWidth, Text,
};
use warpui::keymap::FixedBinding;
use warpui::keymap::macros::*;
use warpui::ui_components::components::UiComponent;
use warpui::ui_components::radio_buttons::{
    RadioButtonItem, RadioButtonLayout, RadioButtonStateHandle,
};
use warpui::{AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle};

use super::settings::PrAgentSettings;
use super::{
    CheckoutMode, PrDetails, PrRef, PromptKind, launch_config, parse_pr_ref, pr_request,
    render_pr_prompt, run_gh, viewer_args,
};
use crate::appearance::Appearance;
use crate::editor::{
    EditorOptions, EditorView, Event as EditorEvent, SingleLineEditorOptions, TextOptions,
};
use crate::task_agent::settings::TaskAgentSettings;
use crate::task_agent::{TaskAgentRequest, plan_launch};
use crate::terminal::CLIAgent;
use crate::view_components::action_button::{ActionButton, NakedTheme, PrimaryTheme};
use crate::view_components::{Dropdown, DropdownItem};

const SECTION_GAP: f32 = 12.;
const LABEL_BOTTOM_MARGIN: f32 = 4.;
const HORIZONTAL_PADDING: f32 = 24.;
const FORM_MAX_HEIGHT: f32 = 520.;
const PROMPT_EDITOR_HEIGHT: f32 = 160.;

pub fn init(app: &mut AppContext) {
    app.register_fixed_bindings([FixedBinding::new(
        "escape",
        PrAgentModalAction::Cancel,
        id!(PrAgentModal::ui_name()),
    )]);
}

/// Everything the workspace needs to launch and watch a PR agent.
pub(crate) struct PrLaunch {
    pub request: TaskAgentRequest,
    pub details: PrDetails,
    pub viewer: Option<String>,
}

pub(crate) enum PrAgentModalEvent {
    Close,
    Launch(Box<PrLaunch>),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PrAgentModalAction {
    Cancel,
    Submit,
    SetCheckout(usize),
    SetPromptKind(usize),
    SelectCli(CLIAgent),
}

enum Lookup {
    /// The input is not a pull request reference yet.
    Idle,
    Loading(PrRef),
    Loaded(PrDetails),
    Failed {
        pr: PrRef,
        error: String,
    },
}

pub(crate) struct PrAgentModal {
    repo_root: PathBuf,
    cli: CLIAgent,
    /// Login of the `gh` user, used to pick the prompt template.
    viewer: Option<String>,
    lookup: Lookup,
    lookup_handle: Option<SpawnedFutureHandle>,
    ref_editor: ViewHandle<EditorView>,
    cli_dropdown: ViewHandle<Dropdown<PrAgentModalAction>>,
    prompt_editor: ViewHandle<EditorView>,
    /// Last rendered prompt; while the editor still holds it, the prompt follows the form.
    auto_prompt: String,
    checkout_radio_state: RadioButtonStateHandle,
    checkout_mouse_states: Vec<MouseStateHandle>,
    prompt_kind_radio_state: RadioButtonStateHandle,
    prompt_kind_mouse_states: Vec<MouseStateHandle>,
    cancel_button: ViewHandle<ActionButton>,
    submit_button: ViewHandle<ActionButton>,
    scroll_state: ClippedScrollStateHandle,
}

impl PrAgentModal {
    pub(crate) fn new(ctx: &mut ViewContext<Self>) -> Self {
        let ref_editor = ctx.add_typed_action_view(|ctx| {
            let mut editor = EditorView::single_line(SingleLineEditorOptions::default(), ctx);
            editor
                .set_placeholder_text("https://github.com/octo/repo/pull/12 or octo/repo#12", ctx);
            editor
        });
        ctx.subscribe_to_view(&ref_editor, |me, _, event, ctx| match event {
            EditorEvent::Enter => me.submit(ctx),
            EditorEvent::Escape => ctx.emit(PrAgentModalEvent::Close),
            EditorEvent::Edited(_) => me.on_ref_edited(ctx),
            _ => {}
        });

        let prompt_editor = ctx.add_typed_action_view(|ctx| {
            let appearance = Appearance::as_ref(ctx);
            let options = EditorOptions {
                soft_wrap: true,
                text: TextOptions {
                    font_size_override: Some(appearance.ui_font_size()),
                    font_family_override: Some(appearance.monospace_font_family()),
                    ..Default::default()
                },
                ..Default::default()
            };
            EditorView::new(options, ctx)
        });
        ctx.subscribe_to_view(&prompt_editor, |_, _, event, ctx| {
            if let EditorEvent::Escape = event {
                ctx.emit(PrAgentModalEvent::Close);
            }
        });

        let cli_dropdown = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = Dropdown::new(ctx);
            dropdown.set_top_bar_max_width(240.);
            dropdown.set_items(
                enum_iterator::all::<CLIAgent>()
                    .filter(|agent| !matches!(agent, CLIAgent::Unknown | CLIAgent::WarpTui))
                    .map(|agent| {
                        DropdownItem::new(
                            agent.display_name(),
                            PrAgentModalAction::SelectCli(agent),
                        )
                    })
                    .collect(),
                ctx,
            );
            dropdown
        });

        let cancel_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Cancel", NakedTheme).on_click(|ctx| {
                ctx.dispatch_typed_action(PrAgentModalAction::Cancel);
            })
        });
        let submit_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Start agent", PrimaryTheme).on_click(|ctx| {
                ctx.dispatch_typed_action(PrAgentModalAction::Submit);
            })
        });

        Self {
            repo_root: PathBuf::new(),
            cli: CLIAgent::Claude,
            viewer: None,
            lookup: Lookup::Idle,
            lookup_handle: None,
            ref_editor,
            cli_dropdown,
            prompt_editor,
            auto_prompt: String::new(),
            checkout_radio_state: Default::default(),
            checkout_mouse_states: CheckoutMode::ALL
                .iter()
                .map(|_| Default::default())
                .collect(),
            prompt_kind_radio_state: Default::default(),
            prompt_kind_mouse_states: PromptKind::ALL.iter().map(|_| Default::default()).collect(),
            cancel_button,
            submit_button,
            scroll_state: Default::default(),
        }
    }

    /// Resets the form for a pull request checked out from `repo_root`.
    pub(crate) fn on_open(&mut self, repo_root: PathBuf, ctx: &mut ViewContext<Self>) {
        self.cli = TaskAgentSettings::as_ref(ctx).default_cli(&repo_root);
        let cli = self.cli;
        self.cli_dropdown.update(ctx, |dropdown, ctx| {
            dropdown.set_selected_by_action(PrAgentModalAction::SelectCli(cli), ctx);
        });
        let checkout = PrAgentSettings::as_ref(ctx).default_checkout_mode();
        self.checkout_radio_state.set_selected_idx(
            CheckoutMode::ALL
                .iter()
                .position(|mode| *mode == checkout)
                .unwrap_or(0),
        );
        self.prompt_kind_radio_state.set_selected_idx(0);
        self.repo_root = repo_root;
        self.lookup = Lookup::Idle;
        if let Some(handle) = self.lookup_handle.take() {
            handle.abort();
        }
        self.auto_prompt = String::new();
        self.ref_editor
            .update(ctx, |editor, ctx| editor.set_buffer_text("", ctx));
        self.prompt_editor
            .update(ctx, |editor, ctx| editor.set_buffer_text("", ctx));

        if self.viewer.is_none() && !self.repo_root.as_os_str().is_empty() {
            let fetch = run_gh(ctx, self.repo_root.clone(), viewer_args());
            ctx.spawn(fetch, |me, result, ctx| match result {
                Ok(login) => {
                    me.viewer = Some(login.trim().to_string()).filter(|login| !login.is_empty());
                    me.select_prompt_kind_for_author(ctx);
                }
                Err(err) => safe_warn!(
                    safe: ("PR agent: could not read the gh user"),
                    full: ("PR agent: could not read the gh user: {err:#}")
                ),
            });
        }

        ctx.focus(&self.ref_editor);
        ctx.notify();
    }

    fn on_ref_edited(&mut self, ctx: &mut ViewContext<Self>) {
        let text = self.ref_editor.as_ref(ctx).buffer_text(ctx);
        let Some(pr) = parse_pr_ref(&text) else {
            self.lookup = Lookup::Idle;
            ctx.notify();
            return;
        };
        let current = match &self.lookup {
            Lookup::Idle => None,
            Lookup::Loading(pr) | Lookup::Failed { pr, .. } => Some(pr),
            Lookup::Loaded(details) => Some(&details.pr),
        };
        if current == Some(&pr) || self.repo_root.as_os_str().is_empty() {
            return;
        }

        self.lookup = Lookup::Loading(pr.clone());
        if let Some(handle) = self.lookup_handle.take() {
            handle.abort();
        }
        let fetch = run_gh(ctx, self.repo_root.clone(), PrDetails::gh_args(&pr));
        self.lookup_handle = Some(ctx.spawn(fetch, move |me, result, ctx| {
            me.lookup_handle = None;
            if !matches!(&me.lookup, Lookup::Loading(loading) if *loading == pr) {
                return;
            }
            me.lookup = match result
                .and_then(|json| PrDetails::parse(pr.clone(), &json).map_err(anyhow::Error::from))
            {
                Ok(details) => Lookup::Loaded(details),
                Err(err) => Lookup::Failed {
                    pr,
                    error: err.to_string().trim().to_string(),
                },
            };
            me.select_prompt_kind_for_author(ctx);
            me.refresh_prompt(true, ctx);
        }));
        ctx.notify();
    }

    /// Picks "watch my own PR" when the `gh` user wrote the pull request.
    fn select_prompt_kind_for_author(&mut self, ctx: &mut ViewContext<Self>) {
        let Lookup::Loaded(details) = &self.lookup else {
            return;
        };
        let kind = PromptKind::for_author(&details.author, self.viewer.as_deref());
        let index = PromptKind::ALL.iter().position(|k| *k == kind).unwrap_or(0);
        if self.prompt_kind_radio_state.get_selected_idx() != Some(index) {
            self.prompt_kind_radio_state.set_selected_idx(index);
            self.refresh_prompt(true, ctx);
        }
    }

    fn checkout_mode(&self) -> CheckoutMode {
        CheckoutMode::ALL[self
            .checkout_radio_state
            .get_selected_idx()
            .unwrap_or(0)
            .min(CheckoutMode::ALL.len() - 1)]
    }

    fn prompt_kind(&self) -> PromptKind {
        PromptKind::ALL[self
            .prompt_kind_radio_state
            .get_selected_idx()
            .unwrap_or(0)
            .min(PromptKind::ALL.len() - 1)]
    }

    /// The launch request for the loaded pull request with the form's choices (prompt empty).
    fn request(&self, ctx: &AppContext) -> Option<(TaskAgentRequest, PrDetails)> {
        let Lookup::Loaded(details) = &self.lookup else {
            return None;
        };
        let config = launch_config(&self.repo_root, ctx);
        let draft = TaskAgentRequest {
            cli: self.cli,
            ..TaskAgentRequest::draft(self.repo_root.clone(), ctx)
        };
        Some((
            pr_request(details, self.checkout_mode(), draft, &config),
            details.clone(),
        ))
    }

    /// Re-renders the prompt from the selected template. Unless `force`, a prompt the user
    /// edited is kept.
    fn refresh_prompt(&mut self, force: bool, ctx: &mut ViewContext<Self>) {
        let Some((request, details)) = self.request(ctx) else {
            ctx.notify();
            return;
        };
        let current = self.prompt_editor.as_ref(ctx).buffer_text(ctx);
        if !force && current != self.auto_prompt {
            return;
        }
        let plan = plan_launch(&request, &launch_config(&self.repo_root, ctx));
        let checkout_path = plan.worktree_path.unwrap_or(plan.cwd);
        let template = self.prompt_kind().template(PrAgentSettings::as_ref(ctx));
        let prompt = render_pr_prompt(
            &template,
            &details,
            &checkout_path,
            plan.branch.as_deref().unwrap_or_default(),
        );
        self.prompt_editor
            .update(ctx, |editor, ctx| editor.set_buffer_text(&prompt, ctx));
        self.auto_prompt = prompt;
        ctx.notify();
    }

    fn submit(&mut self, ctx: &mut ViewContext<Self>) {
        let Some((request, details)) = self.request(ctx) else {
            return;
        };
        let request = TaskAgentRequest {
            prompt: self.prompt_editor.as_ref(ctx).buffer_text(ctx),
            ..request
        };
        ctx.emit(PrAgentModalEvent::Launch(Box::new(PrLaunch {
            request,
            details,
            viewer: self.viewer.clone(),
        })));
    }

    fn status_text(&self) -> String {
        if self.repo_root.as_os_str().is_empty() {
            return "Open this from a terminal inside a clone of the repository.".to_string();
        }
        match &self.lookup {
            Lookup::Idle => "Paste a pull request URL or type owner/repo#123.".to_string(),
            Lookup::Loading(pr) => format!("Looking up {pr}…"),
            Lookup::Failed { pr, error } => format!("Could not load {pr}: {error}"),
            Lookup::Loaded(details) => {
                let state = match details.state.as_str() {
                    "MERGED" => " · merged",
                    "CLOSED" => " · closed",
                    _ => "",
                };
                format!(
                    "#{} {} · by {} · into {}{state}",
                    details.pr.number, details.title, details.author, details.base
                )
            }
        }
    }

    fn render_label(text: &str, appearance: &Appearance) -> Box<dyn Element> {
        let theme = appearance.theme();
        Container::new(
            Text::new_inline(
                text.to_string(),
                appearance.ui_font_family(),
                appearance.ui_font_size(),
            )
            .with_color(theme.sub_text_color(theme.background()).into())
            .finish(),
        )
        .with_margin_top(SECTION_GAP)
        .with_margin_bottom(LABEL_BOTTOM_MARGIN)
        .finish()
    }

    fn render_note(text: String, appearance: &Appearance) -> Box<dyn Element> {
        let theme = appearance.theme();
        Container::new(
            Text::new(
                text,
                appearance.ui_font_family(),
                appearance.ui_font_size() - 1.,
            )
            .with_color(theme.sub_text_color(theme.background()).into())
            .finish(),
        )
        .with_margin_top(LABEL_BOTTOM_MARGIN)
        .finish()
    }

    fn render_radio(
        mouse_states: &[MouseStateHandle],
        labels: Vec<&'static str>,
        state: &RadioButtonStateHandle,
        action: fn(usize) -> PrAgentModalAction,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        appearance
            .ui_builder()
            .radio_buttons(
                mouse_states.to_vec(),
                labels.into_iter().map(RadioButtonItem::text).collect(),
                state.clone(),
                state.get_selected_idx(),
                appearance.ui_font_size(),
                RadioButtonLayout::Row,
            )
            .on_change(Rc::new(move |ctx, _, index| {
                if let Some(index) = index {
                    ctx.dispatch_typed_action(action(index));
                }
            }))
            .build()
            .finish()
    }
}

impl Entity for PrAgentModal {
    type Event = PrAgentModalEvent;
}

impl View for PrAgentModal {
    fn ui_name() -> &'static str {
        "PrAgentModal"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let text_input = |editor: &ViewHandle<EditorView>| {
            appearance
                .ui_builder()
                .text_input(editor.clone())
                .build()
                .finish()
        };

        let mut form = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch);
        form.add_child(Self::render_label("Pull request", appearance));
        form.add_child(text_input(&self.ref_editor));
        form.add_child(Self::render_note(self.status_text(), appearance));

        form.add_child(Self::render_label("Local repository", appearance));
        form.add_child(Self::render_note(
            self.repo_root.to_string_lossy().into_owned(),
            appearance,
        ));

        form.add_child(Self::render_label("Where the agent works", appearance));
        form.add_child(Self::render_radio(
            &self.checkout_mouse_states,
            CheckoutMode::ALL.iter().map(|mode| mode.label()).collect(),
            &self.checkout_radio_state,
            PrAgentModalAction::SetCheckout,
            appearance,
        ));

        form.add_child(Self::render_label("Agent", appearance));
        form.add_child(ChildView::new(&self.cli_dropdown).finish());

        form.add_child(Self::render_label("Prompt", appearance));
        form.add_child(Self::render_radio(
            &self.prompt_kind_mouse_states,
            PromptKind::ALL.iter().map(|kind| kind.label()).collect(),
            &self.prompt_kind_radio_state,
            PrAgentModalAction::SetPromptKind,
            appearance,
        ));
        form.add_child(
            Container::new(
                ConstrainedBox::new(text_input(&self.prompt_editor))
                    .with_height(PROMPT_EDITOR_HEIGHT)
                    .finish(),
            )
            .with_margin_top(LABEL_BOTTOM_MARGIN)
            .finish(),
        );

        let scrollable = ClippedScrollable::vertical(
            self.scroll_state.clone(),
            form.finish(),
            ScrollbarWidth::Auto,
            theme.nonactive_ui_text_color().into(),
            theme.active_ui_text_color().into(),
            Fill::None,
        )
        .with_overlayed_scrollbar()
        .finish();
        let body = Container::new(
            ConstrainedBox::new(scrollable)
                .with_max_height(FORM_MAX_HEIGHT)
                .finish(),
        )
        .with_padding(
            Padding::uniform(0.)
                .with_left(HORIZONTAL_PADDING)
                .with_right(HORIZONTAL_PADDING)
                .with_bottom(16.),
        )
        .finish();

        let footer = Container::new(
            Container::new(
                Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_main_axis_alignment(MainAxisAlignment::End)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(8.)
                    .with_child(ChildView::new(&self.cancel_button).finish())
                    .with_child(ChildView::new(&self.submit_button).finish())
                    .finish(),
            )
            .with_padding(
                Padding::uniform(12.)
                    .with_left(HORIZONTAL_PADDING)
                    .with_right(HORIZONTAL_PADDING),
            )
            .finish(),
        )
        .with_border(Border::top(1.).with_border_fill(theme.outline()))
        .finish();

        Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(body)
            .with_child(footer)
            .finish()
    }
}

impl TypedActionView for PrAgentModal {
    type Action = PrAgentModalAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            PrAgentModalAction::Cancel => ctx.emit(PrAgentModalEvent::Close),
            PrAgentModalAction::Submit => self.submit(ctx),
            PrAgentModalAction::SetCheckout(index) => {
                self.checkout_radio_state.set_selected_idx(*index);
                // The checkout path in the prompt follows the mode.
                self.refresh_prompt(false, ctx);
            }
            PrAgentModalAction::SetPromptKind(index) => {
                self.prompt_kind_radio_state.set_selected_idx(*index);
                self.refresh_prompt(true, ctx);
            }
            PrAgentModalAction::SelectCli(cli) => {
                self.cli = *cli;
                ctx.notify();
            }
        }
    }
}

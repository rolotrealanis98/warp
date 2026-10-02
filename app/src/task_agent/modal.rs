//! Modal body for reviewing a [`TaskAgentRequest`] before launch, and for renaming the current
//! session from a task.

use std::path::PathBuf;
use std::rc::Rc;

use warpui::elements::{
    Border, ChildView, ClippedScrollStateHandle, ClippedScrollable, ConstrainedBox, Container,
    CrossAxisAlignment, Element, Fill, Flex, MainAxisAlignment, MainAxisSize, MouseStateHandle,
    Padding, ParentElement, ScrollbarWidth, Text,
};
use warpui::keymap::FixedBinding;
use warpui::keymap::macros::*;
use warpui::platform::Cursor;
use warpui::ui_components::components::UiComponent;
use warpui::ui_components::radio_buttons::{
    RadioButtonItem, RadioButtonLayout, RadioButtonStateHandle,
};
use warpui::ui_components::text::Span;
use warpui::{AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle};

use super::settings::TaskAgentSettings;
use super::{Checkout, TaskAgentRequest, render_prompt, resolve_branch, template_vars};
use crate::ai::persisted_workspace::PersistedWorkspace;
use crate::appearance::Appearance;
use crate::editor::{
    EditorOptions, EditorView, Event as EditorEvent, SingleLineEditorOptions, TextOptions,
};
use crate::tab_configs::branch_picker::BranchPicker;
use crate::tab_configs::repo_picker::{RepoPicker, RepoPickerEvent};
use crate::terminal::CLIAgent;
use crate::view_components::action_button::{ActionButton, NakedTheme, PrimaryTheme};
use crate::view_components::{Dropdown, DropdownItem};

const SECTION_GAP: f32 = 12.;
const LABEL_BOTTOM_MARGIN: f32 = 4.;
const HORIZONTAL_PADDING: f32 = 24.;
const FORM_MAX_HEIGHT: f32 = 520.;
const PROMPT_EDITOR_HEIGHT: f32 = 140.;
const CHECKOUT_LABELS: [&str; 3] = ["New worktree", "New branch here", "Current checkout"];

pub fn init(app: &mut AppContext) {
    app.register_fixed_bindings([FixedBinding::new(
        "escape",
        TaskAgentModalAction::Cancel,
        id!(TaskAgentModal::ui_name()),
    )]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TaskAgentModalMode {
    /// Review and start a task agent.
    Launch,
    /// Only name the current session after a task (key + title).
    Rename,
}

pub(crate) enum TaskAgentModalEvent {
    Close,
    Launch(Box<TaskAgentRequest>),
    Rename { key: Option<String>, title: String },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TaskAgentModalAction {
    Cancel,
    Submit,
    SetCheckout(usize),
    SelectCli(CLIAgent),
    ToggleRunSetup,
}

pub(crate) struct TaskAgentModal {
    mode: TaskAgentModalMode,
    /// The request being edited; the form fields are merged into it on submit.
    request: TaskAgentRequest,
    key_editor: ViewHandle<EditorView>,
    title_editor: ViewHandle<EditorView>,
    repo_picker: ViewHandle<RepoPicker>,
    base_picker: ViewHandle<BranchPicker>,
    branch_editor: ViewHandle<EditorView>,
    cli_dropdown: ViewHandle<Dropdown<TaskAgentModalAction>>,
    prompt_editor: ViewHandle<EditorView>,
    /// Last template-rendered values; a field still equal to its value was not edited by the
    /// user, so it follows key/title/repo changes.
    auto_branch: String,
    auto_prompt: String,
    checkout_radio_state: RadioButtonStateHandle,
    checkout_mouse_states: Vec<MouseStateHandle>,
    run_setup_mouse_state: MouseStateHandle,
    cancel_button: ViewHandle<ActionButton>,
    submit_button: ViewHandle<ActionButton>,
    scroll_state: ClippedScrollStateHandle,
}

impl TaskAgentModal {
    pub(crate) fn new(ctx: &mut ViewContext<Self>) -> Self {
        let key_editor = Self::build_line_editor("EXAMPLE-123 (optional)", true, ctx);
        let title_editor = Self::build_line_editor("What should the agent work on?", true, ctx);
        let branch_editor = Self::build_line_editor("feat/EXAMPLE-123-short-title", false, ctx);
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
                ctx.emit(TaskAgentModalEvent::Close);
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
                            TaskAgentModalAction::SelectCli(agent),
                        )
                    })
                    .collect(),
                ctx,
            );
            dropdown
        });

        let cancel_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Cancel", NakedTheme).on_click(|ctx| {
                ctx.dispatch_typed_action(TaskAgentModalAction::Cancel);
            })
        });
        let submit_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Start agent", PrimaryTheme).on_click(|ctx| {
                ctx.dispatch_typed_action(TaskAgentModalAction::Submit);
            })
        });

        let repo_picker = Self::build_repo_picker(None, ctx);
        let base_picker = Self::build_base_picker(None, None, ctx);
        Self {
            mode: TaskAgentModalMode::Launch,
            request: TaskAgentRequest::with_cli(PathBuf::new(), CLIAgent::Claude),
            key_editor,
            title_editor,
            repo_picker,
            base_picker,
            branch_editor,
            cli_dropdown,
            prompt_editor,
            auto_branch: String::new(),
            auto_prompt: String::new(),
            checkout_radio_state: Default::default(),
            checkout_mouse_states: CHECKOUT_LABELS.iter().map(|_| Default::default()).collect(),
            run_setup_mouse_state: Default::default(),
            cancel_button,
            submit_button,
            scroll_state: Default::default(),
        }
    }

    fn build_line_editor(
        placeholder: &'static str,
        refreshes_derived_fields: bool,
        ctx: &mut ViewContext<Self>,
    ) -> ViewHandle<EditorView> {
        let editor = ctx.add_typed_action_view(|ctx| {
            let mut editor = EditorView::single_line(SingleLineEditorOptions::default(), ctx);
            editor.set_placeholder_text(placeholder, ctx);
            editor
        });
        ctx.subscribe_to_view(&editor, move |me, _, event, ctx| match event {
            EditorEvent::Enter => me.submit(ctx),
            EditorEvent::Escape => ctx.emit(TaskAgentModalEvent::Close),
            EditorEvent::Edited(_) if refreshes_derived_fields => me.refresh_derived_fields(ctx),
            _ => {}
        });
        editor
    }

    fn build_repo_picker(
        default: Option<String>,
        ctx: &mut ViewContext<Self>,
    ) -> ViewHandle<RepoPicker> {
        let picker = ctx.add_typed_action_view(|ctx| RepoPicker::new(default, ctx));
        ctx.subscribe_to_view(&picker, |me, _, event, ctx| match event {
            RepoPickerEvent::Selected(repo) => me.set_repo(PathBuf::from(repo), ctx),
            RepoPickerEvent::RequestAddRepo => {
                let modal = ctx.handle();
                ctx.open_file_picker(
                    move |result, ctx| {
                        let Some(path) = result.ok().and_then(|paths| paths.into_iter().next())
                        else {
                            return;
                        };
                        let Some(modal) = modal.upgrade(ctx) else {
                            return;
                        };
                        let path = PathBuf::from(path);
                        PersistedWorkspace::handle(ctx).update(ctx, |workspaces, ctx| {
                            workspaces.user_added_workspace(path.clone(), ctx);
                        });
                        modal.update(ctx, |me, ctx| {
                            me.repo_picker.update(ctx, |picker, ctx| {
                                picker.refresh_and_select(path.clone(), ctx);
                            });
                            me.set_repo(path, ctx);
                        });
                    },
                    warpui::platform::FilePickerConfiguration::new().folders_only(),
                );
            }
        });
        picker
    }

    fn build_base_picker(
        repo: Option<PathBuf>,
        default_base: Option<String>,
        ctx: &mut ViewContext<Self>,
    ) -> ViewHandle<BranchPicker> {
        ctx.add_typed_action_view(move |ctx| BranchPicker::new(repo, default_base, ctx))
    }

    /// Resets the form for `request` and makes it ready to show.
    pub(crate) fn on_open(
        &mut self,
        mode: TaskAgentModalMode,
        request: TaskAgentRequest,
        ctx: &mut ViewContext<Self>,
    ) {
        self.mode = mode;
        self.submit_button.update(ctx, |button, ctx| {
            button.set_label(
                match mode {
                    TaskAgentModalMode::Launch => "Start agent",
                    TaskAgentModalMode::Rename => "Rename",
                },
                ctx,
            );
        });
        let key = request.key.clone().unwrap_or_default();
        let title = request.title.clone();
        self.key_editor
            .update(ctx, |editor, ctx| editor.set_buffer_text(&key, ctx));
        self.title_editor
            .update(ctx, |editor, ctx| editor.set_buffer_text(&title, ctx));
        self.checkout_radio_state
            .set_selected_idx(match request.checkout {
                Checkout::Worktree { .. } => 0,
                Checkout::Branch { .. } => 1,
                Checkout::Here => 2,
            });
        self.cli_dropdown.update(ctx, |dropdown, ctx| {
            dropdown.set_selected_by_action(TaskAgentModalAction::SelectCli(request.cli), ctx);
        });

        let repo = request.repo_root.clone();
        let base = match &request.checkout {
            Checkout::Worktree { base } | Checkout::Branch { base } => base.clone(),
            Checkout::Here => String::new(),
        };
        let explicit_branch = request.branch.clone();
        let explicit_prompt = request.prompt.clone();
        self.request = request;
        self.auto_branch = String::new();
        self.auto_prompt = String::new();
        self.branch_editor.update(ctx, |editor, ctx| {
            editor.set_buffer_text(explicit_branch.as_deref().unwrap_or_default(), ctx)
        });
        self.prompt_editor.update(ctx, |editor, ctx| {
            editor.set_buffer_text(&explicit_prompt, ctx)
        });

        self.repo_picker = Self::build_repo_picker(Some(repo.to_string_lossy().into_owned()), ctx);
        self.load_base_branches(repo, (!base.is_empty()).then_some(base), ctx);
        self.refresh_derived_fields(ctx);

        ctx.focus(&self.title_editor);
        ctx.notify();
    }

    fn set_repo(&mut self, repo: PathBuf, ctx: &mut ViewContext<Self>) {
        self.request.cli = TaskAgentSettings::as_ref(ctx).default_cli(&repo);
        let cli = self.request.cli;
        self.cli_dropdown.update(ctx, |dropdown, ctx| {
            dropdown.set_selected_by_action(TaskAgentModalAction::SelectCli(cli), ctx);
        });
        self.request.repo_root = repo.clone();
        self.load_base_branches(repo, None, ctx);
        self.refresh_derived_fields(ctx);
        ctx.notify();
    }

    /// Rebuilds the base-branch picker for `repo`, defaulting to `base` or, when `None`, to the
    /// remote's default branch (e.g. `origin/main`), which the launch fetches first.
    fn load_base_branches(
        &mut self,
        repo: PathBuf,
        base: Option<String>,
        ctx: &mut ViewContext<Self>,
    ) {
        if repo.as_os_str().is_empty() {
            return;
        }
        if base.is_some() {
            self.base_picker = Self::build_base_picker(Some(repo), base, ctx);
            return;
        }
        let detect_repo = repo.clone();
        ctx.spawn(
            async move { crate::util::git::detect_main_branch(&detect_repo).await },
            move |me, main_branch, ctx| {
                if me.request.repo_root != repo {
                    return;
                }
                let default_base = main_branch.ok().map(|branch| branch.trim().to_string());
                me.base_picker = Self::build_base_picker(Some(repo), default_base, ctx);
                ctx.notify();
            },
        );
    }

    /// Re-renders the branch and prompt from the templates, unless the user edited them.
    fn refresh_derived_fields(&mut self, ctx: &mut ViewContext<Self>) {
        let key = self.key_editor.as_ref(ctx).buffer_text(ctx);
        let title = self.title_editor.as_ref(ctx).buffer_text(ctx);
        self.request.key = Some(key.trim().to_string()).filter(|key| !key.is_empty());
        self.request.title = title.trim().to_string();
        if self.mode == TaskAgentModalMode::Rename {
            return;
        }

        let settings = TaskAgentSettings::as_ref(ctx);
        let config = settings.config_for(&self.request.repo_root);
        let prompt_template = settings.prompt_template.clone();
        let vars = template_vars(&self.request, config.short_title_max_chars);
        let template_request = TaskAgentRequest {
            branch: None,
            ..self.request.clone()
        };

        let current_branch = self.branch_editor.as_ref(ctx).buffer_text(ctx);
        if current_branch == self.auto_branch {
            let branch = resolve_branch(&template_request, &config, &vars);
            self.branch_editor
                .update(ctx, |editor, ctx| editor.set_buffer_text(&branch, ctx));
            self.auto_branch = branch;
        }

        let current_prompt = self.prompt_editor.as_ref(ctx).buffer_text(ctx);
        if current_prompt == self.auto_prompt {
            let branch = self.branch_editor.as_ref(ctx).buffer_text(ctx);
            let prompt = render_prompt(&prompt_template, &self.request, &branch);
            self.prompt_editor
                .update(ctx, |editor, ctx| editor.set_buffer_text(&prompt, ctx));
            self.auto_prompt = prompt;
        }
        ctx.notify();
    }

    fn checkout_index(&self) -> usize {
        self.checkout_radio_state.get_selected_idx().unwrap_or(0)
    }

    fn submit(&mut self, ctx: &mut ViewContext<Self>) {
        self.refresh_derived_fields(ctx);
        let TaskAgentRequest { key, title, .. } = self.request.clone();
        if title.is_empty() && key.is_none() {
            return;
        }
        if self.mode == TaskAgentModalMode::Rename {
            ctx.emit(TaskAgentModalEvent::Rename { key, title });
            return;
        }
        if title.is_empty() || self.request.repo_root.as_os_str().is_empty() {
            return;
        }

        let base = self
            .base_picker
            .as_ref(ctx)
            .selected_value(ctx)
            .unwrap_or_default();
        let branch = self.branch_editor.as_ref(ctx).buffer_text(ctx);
        let request = TaskAgentRequest {
            checkout: match self.checkout_index() {
                0 => Checkout::Worktree { base },
                1 => Checkout::Branch { base },
                _ => Checkout::Here,
            },
            branch: Some(branch.trim().to_string()).filter(|branch| !branch.is_empty()),
            prompt: self.prompt_editor.as_ref(ctx).buffer_text(ctx),
            ..self.request.clone()
        };
        ctx.emit(TaskAgentModalEvent::Launch(Box::new(request)));
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

    fn render_text_input(
        editor: &ViewHandle<EditorView>,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        appearance
            .ui_builder()
            .text_input(editor.clone())
            .build()
            .finish()
    }

    fn render_launch_fields(&self, form: &mut Flex, appearance: &Appearance) {
        form.add_child(Self::render_label("Repository", appearance));
        form.add_child(ChildView::new(&self.repo_picker).finish());

        form.add_child(Self::render_label("Where the agent works", appearance));
        form.add_child(
            appearance
                .ui_builder()
                .radio_buttons(
                    self.checkout_mouse_states.clone(),
                    CHECKOUT_LABELS
                        .iter()
                        .map(|label| RadioButtonItem::text(*label))
                        .collect(),
                    self.checkout_radio_state.clone(),
                    Some(self.checkout_index()),
                    appearance.ui_font_size(),
                    RadioButtonLayout::Row,
                )
                .on_change(Rc::new(|ctx, _, index| {
                    if let Some(index) = index {
                        ctx.dispatch_typed_action(TaskAgentModalAction::SetCheckout(index));
                    }
                }))
                .build()
                .finish(),
        );

        let creates_branch = self.checkout_index() < 2;
        if creates_branch {
            form.add_child(Self::render_label("Base branch", appearance));
            form.add_child(ChildView::new(&self.base_picker).finish());
            form.add_child(Self::render_label("New branch", appearance));
            form.add_child(Self::render_text_input(&self.branch_editor, appearance));
        }

        form.add_child(Self::render_label("Agent", appearance));
        form.add_child(ChildView::new(&self.cli_dropdown).finish());

        form.add_child(Self::render_label("Initial prompt", appearance));
        form.add_child(
            ConstrainedBox::new(Self::render_text_input(&self.prompt_editor, appearance))
                .with_height(PROMPT_EDITOR_HEIGHT)
                .finish(),
        );

        if creates_branch {
            form.add_child(
                Container::new(
                    appearance
                        .ui_builder()
                        .checkbox(self.run_setup_mouse_state.clone(), Some(14.))
                        .with_label(Span::new("Run setup commands", Default::default()))
                        .check(self.request.run_setup)
                        .build()
                        .with_cursor(Cursor::PointingHand)
                        .on_click(|ctx, _, _| {
                            ctx.dispatch_typed_action(TaskAgentModalAction::ToggleRunSetup)
                        })
                        .finish(),
                )
                .with_margin_top(SECTION_GAP)
                .finish(),
            );
        }
    }
}

impl Entity for TaskAgentModal {
    type Event = TaskAgentModalEvent;
}

impl View for TaskAgentModal {
    fn ui_name() -> &'static str {
        "TaskAgentModal"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();

        let mut form = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch);
        form.add_child(Self::render_label("Task key", appearance));
        form.add_child(Self::render_text_input(&self.key_editor, appearance));
        form.add_child(Self::render_label("Title", appearance));
        form.add_child(Self::render_text_input(&self.title_editor, appearance));
        if self.mode == TaskAgentModalMode::Launch {
            self.render_launch_fields(&mut form, appearance);
        }

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

impl TypedActionView for TaskAgentModal {
    type Action = TaskAgentModalAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            TaskAgentModalAction::Cancel => ctx.emit(TaskAgentModalEvent::Close),
            TaskAgentModalAction::Submit => self.submit(ctx),
            TaskAgentModalAction::SetCheckout(index) => {
                self.checkout_radio_state.set_selected_idx(*index);
                ctx.notify();
            }
            TaskAgentModalAction::SelectCli(cli) => {
                self.request.cli = *cli;
                ctx.notify();
            }
            TaskAgentModalAction::ToggleRunSetup => {
                self.request.run_setup = !self.request.run_setup;
                ctx.notify();
            }
        }
    }
}

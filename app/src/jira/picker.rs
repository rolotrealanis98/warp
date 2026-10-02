//! Body of the Jira modal: the issue picker (fuzzy over the loaded list, `jql:` for a server
//! search), plus the transition list and the comment form for the active pane's issue.

use jira_client::{JiraClient, JiraError, Transition};
use warp_core::safe_warn;
use warp_core::ui::theme::Fill as ThemeFill;
use warp_core::ui::theme::color::internal_colors;
use warp_editor::editor::NavigationKey;
use warpui::elements::{
    Border, ChildView, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Element, Fill,
    Flex, Hoverable, MainAxisAlignment, MainAxisSize, MouseStateHandle, Padding, ParentElement,
    Radius, ScrollStateHandle, Scrollable, ScrollableElement, ScrollbarWidth, Shrinkable, Text,
    UniformList, UniformListState,
};
use warpui::keymap::FixedBinding;
use warpui::keymap::macros::*;
use warpui::platform::Cursor;
use warpui::text_layout::ClipConfig;
use warpui::ui_components::chip::Chip;
use warpui::ui_components::components::{UiComponent, UiComponentStyles};
use warpui::{AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle};

use super::settings::JiraSettings;
use super::{
    ClientError, IssueAction, JiraModel, MAX_RESULTS, PickerQuery, fuzzy_filter,
    issue_search_label, looks_like_issue_key, parse_query, scoped_jql,
};
use crate::appearance::Appearance;
use crate::editor::{
    EditorOptions, EditorView, Event as EditorEvent, PropagateAndNoOpNavigationKeys,
    SingleLineEditorOptions,
};
use crate::jira::Issue;
use crate::view_components::action_button::{
    ActionButton, NakedTheme, PrimaryTheme, SecondaryTheme,
};

const HORIZONTAL_PADDING: f32 = 20.;
const LIST_HEIGHT: f32 = 380.;
const COMMENT_EDITOR_HEIGHT: f32 = 180.;
const KEY_WIDTH: f32 = 110.;
const TYPE_WIDTH: f32 = 70.;
const STATUS_WIDTH: f32 = 110.;
const ASSIGNEE_WIDTH: f32 = 130.;

pub fn init(app: &mut AppContext) {
    app.register_fixed_bindings([FixedBinding::new(
        "escape",
        JiraIssuePickerAction::Cancel,
        id!(JiraIssuePicker::ui_name()),
    )]);
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PickerMode {
    /// Pick an issue for `IssueAction`.
    Issues(IssueAction),
    /// Pick a transition for the issue `key` and apply it.
    Transitions { key: String },
    /// Write a comment on the issue `key`.
    Comment { key: String },
}

pub(crate) enum JiraIssuePickerEvent {
    Close,
    /// An issue was picked. For [`IssueAction::StartAgent`] it includes its description.
    IssueChosen {
        action: IssueAction,
        issue: Box<Issue>,
    },
    /// A write to Jira succeeded; `message` says what changed.
    Done(String),
    OpenSettings,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum JiraIssuePickerAction {
    Cancel,
    /// Choose the row at this position in the filtered list.
    Choose(usize),
    SubmitComment,
    OpenSettings,
}

#[derive(Clone, Debug, PartialEq)]
enum Status {
    Ready,
    Loading(String),
    Error(String),
    NotConfigured,
}

/// One rendered row, cloned out of the view so the list builder can own it.
#[derive(Clone)]
enum Row {
    Issue(Issue),
    Transition(Transition),
}

pub(crate) struct JiraIssuePicker {
    mode: PickerMode,
    query_editor: ViewHandle<EditorView>,
    comment_editor: ViewHandle<EditorView>,
    cancel_button: ViewHandle<ActionButton>,
    submit_button: ViewHandle<ActionButton>,
    settings_button: ViewHandle<ActionButton>,
    issues: Vec<Issue>,
    transitions: Vec<Transition>,
    /// Indices into `issues` or `transitions`, in display order after filtering.
    visible: Vec<usize>,
    selected: usize,
    row_mouse_states: Vec<MouseStateHandle>,
    list_state: UniformListState,
    scroll_state: ScrollStateHandle,
    status: Status,
    /// Bumped by every request; responses to older requests are dropped.
    epoch: usize,
    /// Whether `issues` holds a `jql:` search result rather than the default list.
    showing_jql_results: bool,
}

impl JiraIssuePicker {
    pub(crate) fn new(ctx: &mut ViewContext<Self>) -> Self {
        let query_editor = ctx.add_typed_action_view(|ctx| {
            let mut editor = EditorView::single_line(
                SingleLineEditorOptions {
                    propagate_and_no_op_vertical_navigation_keys:
                        PropagateAndNoOpNavigationKeys::Always,
                    ..Default::default()
                },
                ctx,
            );
            editor.set_placeholder_text("Filter, or jql: <query> and Enter to search Jira", ctx);
            editor
        });
        ctx.subscribe_to_view(&query_editor, |me, _, event, ctx| match event {
            EditorEvent::Edited(_) => me.on_query_edited(ctx),
            EditorEvent::Navigate(NavigationKey::Up) => me.move_selection(-1, ctx),
            EditorEvent::Navigate(NavigationKey::Down) => me.move_selection(1, ctx),
            EditorEvent::Enter => me.confirm(ctx),
            EditorEvent::Escape => ctx.emit(JiraIssuePickerEvent::Close),
            _ => {}
        });

        let comment_editor = ctx.add_typed_action_view(|ctx| {
            EditorView::new(
                EditorOptions {
                    soft_wrap: true,
                    ..Default::default()
                },
                ctx,
            )
        });
        ctx.subscribe_to_view(&comment_editor, |_, _, event, ctx| {
            if let EditorEvent::Escape = event {
                ctx.emit(JiraIssuePickerEvent::Close);
            }
        });

        let cancel_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Cancel", NakedTheme).on_click(|ctx| {
                ctx.dispatch_typed_action(JiraIssuePickerAction::Cancel);
            })
        });
        let submit_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Add comment", PrimaryTheme).on_click(|ctx| {
                ctx.dispatch_typed_action(JiraIssuePickerAction::SubmitComment);
            })
        });
        let settings_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Open Jira settings", SecondaryTheme).on_click(|ctx| {
                ctx.dispatch_typed_action(JiraIssuePickerAction::OpenSettings);
            })
        });

        Self {
            mode: PickerMode::Issues(IssueAction::StartAgent),
            query_editor,
            comment_editor,
            cancel_button,
            submit_button,
            settings_button,
            issues: Vec::new(),
            transitions: Vec::new(),
            visible: Vec::new(),
            selected: 0,
            row_mouse_states: Vec::new(),
            list_state: UniformListState::new(),
            scroll_state: Default::default(),
            status: Status::Ready,
            epoch: 0,
            showing_jql_results: false,
        }
    }

    /// Resets the body for `mode` and starts loading what it lists.
    pub(crate) fn open(&mut self, mode: PickerMode, ctx: &mut ViewContext<Self>) {
        self.mode = mode.clone();
        self.epoch += 1;
        self.issues.clear();
        self.transitions.clear();
        self.showing_jql_results = false;
        self.selected = 0;
        self.status = Status::Ready;
        self.query_editor
            .update(ctx, |editor, ctx| editor.clear_buffer(ctx));
        self.comment_editor
            .update(ctx, |editor, ctx| editor.clear_buffer(ctx));

        let client = self.client_or_status(ctx);
        match mode {
            PickerMode::Issues(_) => {
                ctx.focus(&self.query_editor);
                self.issues = JiraModel::as_ref(ctx).issues().to_vec();
                if let Some(client) = client {
                    self.load_default_issues(client, ctx);
                }
            }
            PickerMode::Transitions { key } => {
                ctx.focus(&self.query_editor);
                if let Some(client) = client {
                    self.load_transitions(client, key, ctx);
                }
            }
            PickerMode::Comment { .. } => ctx.focus(&self.comment_editor),
        }
        self.refresh_rows(ctx);
    }

    fn client_or_status(&mut self, ctx: &mut ViewContext<Self>) -> Option<JiraClient> {
        match super::client(ctx) {
            Ok(client) => Some(client),
            Err(ClientError::NotConfigured) => {
                self.status = Status::NotConfigured;
                ctx.notify();
                None
            }
            Err(err) => {
                self.status = Status::Error(err.to_string());
                ctx.notify();
                None
            }
        }
    }

    fn next_epoch(&mut self) -> usize {
        self.epoch += 1;
        self.epoch
    }

    fn load_default_issues(&mut self, client: JiraClient, ctx: &mut ViewContext<Self>) {
        let settings = JiraSettings::as_ref(ctx);
        let jql = scoped_jql(&settings.default_jql, &settings.project_keys);
        self.status = Status::Loading(if self.issues.is_empty() {
            "Loading issues…".to_string()
        } else {
            "Refreshing…".to_string()
        });
        let epoch = self.next_epoch();
        ctx.spawn(
            async move { client.search(&jql, MAX_RESULTS).await },
            move |me, result, ctx| {
                if me.epoch != epoch {
                    return;
                }
                match result {
                    Ok(issues) => {
                        JiraModel::handle(ctx)
                            .update(ctx, |model, _| model.set_issues(issues.clone()));
                        me.issues = issues;
                        me.status = Status::Ready;
                    }
                    Err(err) => me.fail("Loading the issue list", err),
                }
                me.refresh_rows(ctx);
            },
        );
    }

    fn run_jql(&mut self, jql: String, ctx: &mut ViewContext<Self>) {
        let Some(client) = self.client_or_status(ctx) else {
            return;
        };
        self.status = Status::Loading("Searching Jira…".to_string());
        let epoch = self.next_epoch();
        ctx.spawn(
            async move { client.search(&jql, MAX_RESULTS).await },
            move |me, result, ctx| {
                if me.epoch != epoch {
                    return;
                }
                match result {
                    Ok(issues) => {
                        me.issues = issues;
                        me.showing_jql_results = true;
                        me.selected = 0;
                        me.status = Status::Ready;
                    }
                    Err(err) => me.fail("JQL search", err),
                }
                me.refresh_rows(ctx);
            },
        );
        ctx.notify();
    }

    fn load_transitions(&mut self, client: JiraClient, key: String, ctx: &mut ViewContext<Self>) {
        self.status = Status::Loading(format!("Loading transitions for {key}…"));
        let epoch = self.next_epoch();
        ctx.spawn(
            async move { client.transitions(&key).await },
            move |me, result, ctx| {
                if me.epoch != epoch {
                    return;
                }
                match result {
                    Ok(transitions) => {
                        me.transitions = transitions;
                        me.status = Status::Ready;
                    }
                    Err(err) => me.fail("Loading transitions", err),
                }
                me.refresh_rows(ctx);
            },
        );
    }

    /// Fetches `key` (with its description) and emits it for `action`.
    fn fetch_and_emit(&mut self, key: String, action: IssueAction, ctx: &mut ViewContext<Self>) {
        let Some(client) = self.client_or_status(ctx) else {
            return;
        };
        self.status = Status::Loading(format!("Loading {key}…"));
        let epoch = self.next_epoch();
        ctx.spawn(
            async move { client.get_issue(&key).await },
            move |me, result, ctx| {
                if me.epoch != epoch {
                    return;
                }
                match result {
                    Ok(issue) => {
                        me.status = Status::Ready;
                        ctx.emit(JiraIssuePickerEvent::IssueChosen {
                            action,
                            issue: Box::new(issue),
                        });
                    }
                    Err(err) => me.fail("Loading the issue", err),
                }
                ctx.notify();
            },
        );
        ctx.notify();
    }

    fn apply_transition(
        &mut self,
        key: String,
        transition: Transition,
        ctx: &mut ViewContext<Self>,
    ) {
        let Some(client) = self.client_or_status(ctx) else {
            return;
        };
        let done = format!("{key} moved to {}", transition.to_status);
        self.status = Status::Loading(format!("Moving {key} to {}…", transition.to_status));
        let epoch = self.next_epoch();
        ctx.spawn(
            async move { client.transition(&key, &transition.id).await },
            move |me, result, ctx| {
                if me.epoch != epoch {
                    return;
                }
                match result {
                    Ok(()) => {
                        me.status = Status::Ready;
                        ctx.emit(JiraIssuePickerEvent::Done(done));
                    }
                    Err(err) => me.fail("Transition", err),
                }
                ctx.notify();
            },
        );
        ctx.notify();
    }

    fn submit_comment(&mut self, ctx: &mut ViewContext<Self>) {
        let PickerMode::Comment { key } = self.mode.clone() else {
            return;
        };
        if matches!(self.status, Status::Loading(_)) {
            return;
        }
        let text = self.comment_editor.as_ref(ctx).buffer_text(ctx);
        if text.trim().is_empty() {
            return;
        }
        let Some(client) = self.client_or_status(ctx) else {
            return;
        };
        let done = format!("Comment added to {key}");
        self.status = Status::Loading(format!("Adding a comment to {key}…"));
        let epoch = self.next_epoch();
        ctx.spawn(
            async move { client.add_comment(&key, &text).await },
            move |me, result, ctx| {
                if me.epoch != epoch {
                    return;
                }
                match result {
                    Ok(()) => {
                        me.status = Status::Ready;
                        ctx.emit(JiraIssuePickerEvent::Done(done));
                    }
                    Err(err) => me.fail("Adding a comment", err),
                }
                ctx.notify();
            },
        );
        ctx.notify();
    }

    fn fail(&mut self, operation: &str, err: JiraError) {
        // Jira's 400 messages can quote the query or project names; keep them out of release logs.
        let summary = match &err {
            JiraError::Rejected(_) => "Jira rejected the request".to_string(),
            other => other.to_string(),
        };
        safe_warn!(
            safe: ("[Jira] {operation} failed: {summary}"),
            full: ("[Jira] {operation} failed: {err:#}")
        );
        self.status = Status::Error(err.to_string());
    }

    fn query(&self, app: &AppContext) -> String {
        self.query_editor.as_ref(app).buffer_text(app)
    }

    fn on_query_edited(&mut self, ctx: &mut ViewContext<Self>) {
        let query = self.query(ctx);
        let leaving_jql = matches!(self.mode, PickerMode::Issues(_))
            && self.showing_jql_results
            && matches!(parse_query(&query), PickerQuery::Fuzzy(_));
        if leaving_jql {
            self.issues = JiraModel::as_ref(ctx).issues().to_vec();
            self.showing_jql_results = false;
        }
        if matches!(self.status, Status::Error(_)) {
            self.status = Status::Ready;
        }
        self.selected = 0;
        self.refresh_rows(ctx);
    }

    /// Recomputes the visible rows from the query.
    fn refresh_rows(&mut self, ctx: &mut ViewContext<Self>) {
        let query = self.query(ctx);
        self.visible = match (&self.mode, parse_query(&query)) {
            (PickerMode::Issues(_), PickerQuery::Jql(_)) => (0..self.issues.len()).collect(),
            (PickerMode::Issues(_), PickerQuery::Fuzzy(query)) => {
                let labels: Vec<String> = self.issues.iter().map(issue_search_label).collect();
                fuzzy_filter(labels.iter().map(String::as_str), query)
            }
            (PickerMode::Transitions { .. }, _) => {
                let labels: Vec<String> = self
                    .transitions
                    .iter()
                    .map(|transition| format!("{} {}", transition.name, transition.to_status))
                    .collect();
                fuzzy_filter(labels.iter().map(String::as_str), query.trim())
            }
            (PickerMode::Comment { .. }, _) => Vec::new(),
        };
        self.selected = self.selected.min(self.visible.len().saturating_sub(1));
        self.row_mouse_states
            .resize_with(self.visible.len(), Default::default);
        ctx.notify();
    }

    fn move_selection(&mut self, delta: isize, ctx: &mut ViewContext<Self>) {
        if self.visible.is_empty() {
            return;
        }
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.visible.len() - 1);
        self.list_state.scroll_to(self.selected);
        ctx.notify();
    }

    /// Enter in the search box: run a `jql:` query, open a typed issue key that is not in the
    /// list, or choose the selected row.
    fn confirm(&mut self, ctx: &mut ViewContext<Self>) {
        let query = self.query(ctx);
        match (&self.mode, parse_query(&query)) {
            (PickerMode::Issues(_), PickerQuery::Jql(jql)) => {
                if !jql.is_empty() {
                    self.run_jql(jql.to_string(), ctx);
                }
            }
            (PickerMode::Issues(action), PickerQuery::Fuzzy(text))
                if self.visible.is_empty() && looks_like_issue_key(text) =>
            {
                self.fetch_and_emit(text.to_ascii_uppercase(), *action, ctx);
            }
            _ => self.choose(self.selected, ctx),
        }
    }

    fn choose(&mut self, row: usize, ctx: &mut ViewContext<Self>) {
        let Some(&index) = self.visible.get(row) else {
            return;
        };
        match self.mode.clone() {
            PickerMode::Issues(IssueAction::StartAgent) => {
                let key = self.issues[index].key.clone();
                self.fetch_and_emit(key, IssueAction::StartAgent, ctx);
            }
            PickerMode::Issues(action) => {
                ctx.emit(JiraIssuePickerEvent::IssueChosen {
                    action,
                    issue: Box::new(self.issues[index].clone()),
                });
            }
            PickerMode::Transitions { key } => {
                // One write at a time: a second Enter while the first is in flight is ignored.
                if matches!(self.status, Status::Loading(_)) {
                    return;
                }
                let transition = self.transitions[index].clone();
                self.apply_transition(key, transition, ctx);
            }
            PickerMode::Comment { .. } => {}
        }
    }

    fn status_text(&self, app: &AppContext) -> Option<String> {
        let query = self.query(app);
        match &self.status {
            Status::Loading(message) | Status::Error(message) => Some(message.clone()),
            Status::NotConfigured => Some(ClientError::NotConfigured.to_string()),
            Status::Ready => match (&self.mode, parse_query(&query)) {
                (PickerMode::Issues(_), PickerQuery::Jql(_)) => {
                    Some("Press Enter to search Jira with this JQL.".to_string())
                }
                (PickerMode::Issues(_), PickerQuery::Fuzzy(text))
                    if self.visible.is_empty() && looks_like_issue_key(text) =>
                {
                    Some(format!(
                        "Not in the list. Press Enter to open {}.",
                        text.to_ascii_uppercase()
                    ))
                }
                (PickerMode::Issues(_), _) if self.visible.is_empty() => {
                    Some("No issues. Type jql: <query> and press Enter to search Jira.".to_string())
                }
                (PickerMode::Transitions { key }, _) if self.visible.is_empty() => {
                    Some(format!("No transitions available for {key}."))
                }
                _ => None,
            },
        }
    }

    fn render_list(&self, app: &AppContext) -> Box<dyn Element> {
        let rows: Vec<Row> = self
            .visible
            .iter()
            .filter_map(|&index| match self.mode {
                PickerMode::Issues(_) => self.issues.get(index).cloned().map(Row::Issue),
                PickerMode::Transitions { .. } => {
                    self.transitions.get(index).cloned().map(Row::Transition)
                }
                PickerMode::Comment { .. } => None,
            })
            .collect();
        let mouse_states = self.row_mouse_states.clone();
        let selected = self.selected;
        let list = UniformList::new(
            self.list_state.clone(),
            rows.len(),
            move |range, app: &AppContext| {
                range
                    .filter_map(|index| {
                        Some(render_row(
                            rows.get(index)?,
                            index,
                            index == selected,
                            mouse_states.get(index)?.clone(),
                            app,
                        ))
                    })
                    .collect::<Vec<_>>()
                    .into_iter()
            },
        )
        .finish_scrollable();
        let theme = Appearance::as_ref(app).theme();
        Scrollable::vertical(
            self.scroll_state.clone(),
            list,
            ScrollbarWidth::Auto,
            theme.nonactive_ui_detail().into(),
            theme.active_ui_detail().into(),
            Fill::None,
        )
        .with_overlayed_scrollbar()
        .finish()
    }

    fn render_footer(&self, theme_outline: ThemeFill) -> Box<dyn Element> {
        let mut buttons = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::End)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(8.)
            .with_child(ChildView::new(&self.cancel_button).finish());
        if matches!(self.mode, PickerMode::Comment { .. }) {
            buttons.add_child(ChildView::new(&self.submit_button).finish());
        }
        Container::new(
            Container::new(buttons.finish())
                .with_padding(
                    Padding::uniform(12.)
                        .with_left(HORIZONTAL_PADDING)
                        .with_right(HORIZONTAL_PADDING),
                )
                .finish(),
        )
        .with_border(Border::top(1.).with_border_fill(theme_outline))
        .finish()
    }
}

fn render_row(
    row: &Row,
    index: usize,
    is_selected: bool,
    mouse_state: MouseStateHandle,
    app: &AppContext,
) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    let theme = appearance.theme();
    let main_color = theme.main_text_color(theme.background());
    let sub_color = theme.sub_text_color(theme.background());
    let text = |value: &str, color: ThemeFill, monospace: bool| {
        let family = if monospace {
            appearance.monospace_font_family()
        } else {
            appearance.ui_font_family()
        };
        Text::new_inline(value.to_string(), family, appearance.ui_font_size())
            .with_color(color.into())
            .with_clip(ClipConfig::ellipsis())
            .finish()
    };
    let fixed = |element: Box<dyn Element>, width: f32| {
        ConstrainedBox::new(element).with_width(width).finish()
    };

    let content = match row {
        Row::Issue(issue) => {
            let status_chip = Chip::new(
                issue.status.clone(),
                UiComponentStyles {
                    border_color: Some(internal_colors::neutral_4(theme).into()),
                    border_width: Some(1.),
                    border_radius: Some(CornerRadius::with_all(Radius::Pixels(4.))),
                    font_family_id: Some(appearance.ui_font_family()),
                    font_size: Some(appearance.ui_font_size() - 1.),
                    font_color: Some(sub_color.into_solid()),
                    ..Default::default()
                },
            )
            .build()
            .finish();
            Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(10.)
                .with_child(fixed(text(&issue.key, main_color, true), KEY_WIDTH))
                .with_child(fixed(text(&issue.issue_type, sub_color, false), TYPE_WIDTH))
                .with_child(
                    ConstrainedBox::new(status_chip)
                        .with_max_width(STATUS_WIDTH)
                        .finish(),
                )
                .with_child(Shrinkable::new(1., text(&issue.summary, main_color, false)).finish())
                .with_child(fixed(
                    text(
                        issue.assignee.as_deref().unwrap_or("Unassigned"),
                        sub_color,
                        false,
                    ),
                    ASSIGNEE_WIDTH,
                ))
                .finish()
        }
        Row::Transition(transition) => Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(10.)
            .with_child(Shrinkable::new(1., text(&transition.name, main_color, false)).finish())
            .with_child(text(
                &format!("→ {}", transition.to_status),
                sub_color,
                false,
            ))
            .finish(),
    };

    let selected_background = theme.surface_overlay_1();
    Hoverable::new(mouse_state, move |_| {
        let mut container = Container::new(content)
            .with_horizontal_padding(HORIZONTAL_PADDING)
            .with_vertical_padding(6.);
        if is_selected {
            container = container.with_background(selected_background);
        }
        container.finish()
    })
    .with_cursor(Cursor::PointingHand)
    .on_click(move |ctx, _, _| {
        ctx.dispatch_typed_action(JiraIssuePickerAction::Choose(index));
    })
    .finish()
}

impl Entity for JiraIssuePicker {
    type Event = JiraIssuePickerEvent;
}

impl View for JiraIssuePicker {
    fn ui_name() -> &'static str {
        "JiraIssuePicker"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let mut body = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch);

        let editor = match self.mode {
            PickerMode::Comment { .. } => ConstrainedBox::new(
                appearance
                    .ui_builder()
                    .text_input(self.comment_editor.clone())
                    .build()
                    .finish(),
            )
            .with_height(COMMENT_EDITOR_HEIGHT)
            .finish(),
            _ => appearance
                .ui_builder()
                .text_input(self.query_editor.clone())
                .build()
                .finish(),
        };
        body.add_child(
            Container::new(editor)
                .with_horizontal_padding(HORIZONTAL_PADDING)
                .finish(),
        );

        if let Some(status) = self.status_text(app) {
            let color = if matches!(self.status, Status::Error(_)) {
                theme.ui_error_color()
            } else {
                theme.sub_text_color(theme.background()).into_solid()
            };
            body.add_child(
                Container::new(
                    appearance
                        .ui_builder()
                        .paragraph(status)
                        .with_style(UiComponentStyles {
                            font_color: Some(color),
                            ..Default::default()
                        })
                        .build()
                        .finish(),
                )
                .with_horizontal_padding(HORIZONTAL_PADDING)
                .with_vertical_padding(8.)
                .finish(),
            );
        }
        if self.status == Status::NotConfigured {
            body.add_child(
                Container::new(ChildView::new(&self.settings_button).finish())
                    .with_horizontal_padding(HORIZONTAL_PADDING)
                    .with_padding_bottom(8.)
                    .finish(),
            );
        }
        if !matches!(self.mode, PickerMode::Comment { .. }) {
            body.add_child(
                ConstrainedBox::new(self.render_list(app))
                    .with_height(LIST_HEIGHT)
                    .finish(),
            );
        }

        Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(
                Container::new(body.finish())
                    .with_padding_bottom(12.)
                    .finish(),
            )
            .with_child(self.render_footer(theme.outline()))
            .finish()
    }
}

impl TypedActionView for JiraIssuePicker {
    type Action = JiraIssuePickerAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            JiraIssuePickerAction::Cancel => ctx.emit(JiraIssuePickerEvent::Close),
            JiraIssuePickerAction::Choose(row) => {
                self.selected = *row;
                self.choose(*row, ctx);
                ctx.notify();
            }
            JiraIssuePickerAction::SubmitComment => self.submit_comment(ctx),
            JiraIssuePickerAction::OpenSettings => ctx.emit(JiraIssuePickerEvent::OpenSettings),
        }
    }
}

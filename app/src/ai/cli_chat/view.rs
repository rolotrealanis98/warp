//! Chat-style rendering of a CLI agent session. The view never owns the PTY:
//! it renders the agent's transcript and asks its terminal (through
//! [`CliChatViewEvent`]) to write prompts and keys.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use parking_lot::RwLock;
use pathfinder_color::ColorU;
use serde_json::Value;
use similar::{ChangeTag, TextDiff};
use warp_core::ui::appearance::Appearance;
use warp_core::ui::theme::Fill as ThemeFill;
use warpui::r#async::Timer;
use warpui::clipboard::ClipboardContent;
use warpui::elements::{
    Align, Border, ChildView, ClippedScrollStateHandle, ClippedScrollable, ConstrainedBox,
    Container, CornerRadius, CrossAxisAlignment, Empty, Expanded, Fill, Flex, FormattedTextElement,
    HighlightedHyperlink, Hoverable, MainAxisAlignment, MainAxisSize, MouseStateHandle,
    ParentElement, Radius, SavePosition, ScrollTarget, ScrollToPositionMode, ScrollbarWidth,
    SelectableArea, SelectionHandle, Shrinkable, Text,
};
use warpui::fonts::FamilyId;
use warpui::keymap::FixedBinding;
use warpui::platform::Cursor;
use warpui::text_layout::ClipConfig;
use warpui::units::Pixels;
use warpui::{
    AppContext, Element, Entity, EntityId, ModelHandle, SingletonEntity, TypedActionView, View,
    ViewContext, ViewHandle, id,
};

use super::model::{
    ChatItem, CliChatModel, CliChatModelEvent, Row, Thread, ToolItem, ToolKind, group_label, rows,
    turn_summary,
};
use super::{CliChatViewSettings, TOGGLE_CLI_CHAT_VIEW_BINDING};
use crate::editor::{
    EditorOptions, EditorView, EnterAction, EnterSettings, Event as EditorEvent,
    PropagateAndNoOpNavigationKeys, TextOptions,
};
use crate::terminal::CLIAgent;
use crate::terminal::cli_agent_sessions::{
    CLIAgentSessionStatus, CLIAgentSessionsModel, CLIAgentSessionsModelEvent,
};
use crate::ui_components::icons::Icon;
use crate::util::bindings::CustomAction;
use crate::view_components::action_button::{
    ActionButton, ButtonSize, KeystrokeSource, NakedTheme, SecondaryTheme,
};

/// Width of the reading column; wider panes center it.
const READING_WIDTH: f32 = 860.;
/// Tool output and diffs scroll inside a box of at most this height.
const OUTPUT_MAX_HEIGHT: f32 = 240.;
const COMPOSER_MAX_HEIGHT: f32 = 160.;
/// Characters of tool output kept for rendering.
const MAX_OUTPUT_CHARS: usize = 20_000;
const MAX_DIFF_LINES: usize = 400;
/// Turns rendered at first and added per "Show earlier turns" click.
// ponytail: renders the last N turns into one scrollable column; move to the
// viewported `List` element if full scrollback of very long sessions is needed.
const TURNS_PER_PAGE: usize = 20;
const TICK_INTERVAL: Duration = Duration::from_secs(1);

/// Keys the key strip can send to the agent's PTY.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CliKey {
    Enter,
    Escape,
    Interrupt,
    CycleMode,
    Up,
    Down,
}

impl CliKey {
    const ALL: [CliKey; 6] = [
        CliKey::Enter,
        CliKey::Escape,
        CliKey::Interrupt,
        CliKey::CycleMode,
        CliKey::Up,
        CliKey::Down,
    ];

    pub(crate) fn bytes(self) -> &'static [u8] {
        match self {
            CliKey::Enter => b"\r",
            CliKey::Escape => b"\x1b",
            CliKey::Interrupt => b"\x03",
            CliKey::CycleMode => b"\x1b[Z",
            CliKey::Up => b"\x1b[A",
            CliKey::Down => b"\x1b[B",
        }
    }

    fn label(self) -> &'static str {
        match self {
            CliKey::Enter => "Enter",
            CliKey::Escape => "Esc",
            CliKey::Interrupt => "Ctrl-C",
            CliKey::CycleMode => "Shift-Tab",
            CliKey::Up => "↑",
            CliKey::Down => "↓",
        }
    }

    fn tooltip(self) -> &'static str {
        match self {
            CliKey::Enter => "Send Enter (confirm a prompt)",
            CliKey::Escape => "Send Escape (cancel or dismiss)",
            CliKey::Interrupt => "Send Ctrl-C (interrupt)",
            CliKey::CycleMode => "Send Shift-Tab (cycle permission mode)",
            CliKey::Up => "Send Up arrow (move in a menu)",
            CliKey::Down => "Send Down arrow (move in a menu)",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum CliChatViewAction {
    /// Flip the expanded state of a card, group, or thinking block.
    Toggle(String),
    /// Expand or collapse a subagent card, tailing its transcript.
    ToggleSubagent(String),
    ShowEarlierTurns,
    JumpToTool(String),
    SendKey(CliKey),
    ShowTerminal,
    /// Focus the transcript so the copy binding applies to its selection.
    Focus,
    CopySelectedText,
}

pub(super) fn init(app: &mut AppContext) {
    app.register_fixed_bindings([FixedBinding::custom(
        CustomAction::Copy,
        CliChatViewAction::CopySelectedText,
        "Copy",
        id!(CliChatView::ui_name()) & !id!("IMEOpen"),
    )]);
}

pub(crate) enum CliChatViewEvent {
    /// Submit a prompt to the agent.
    Submit(String),
    SendKey(CliKey),
    ShowTerminal,
}

pub(crate) struct CliChatView {
    terminal_view_id: EntityId,
    agent: CLIAgent,
    model: ModelHandle<CliChatModel>,
    composer: ViewHandle<EditorView>,
    terminal_button: ViewHandle<ActionButton>,
    key_buttons: Vec<ViewHandle<ActionButton>>,
    scroll: ClippedScrollStateHandle,
    /// Largest settled scroll offset seen; while the viewport sits at it,
    /// new content keeps the transcript pinned to the bottom.
    max_seen_scroll: f32,
    /// Keys whose expanded state differs from their default.
    toggled: HashSet<String>,
    visible_turns: usize,
    ticking: bool,
    mouse_states: RefCell<HashMap<String, MouseStateHandle>>,
    output_scrolls: RefCell<HashMap<String, ClippedScrollStateHandle>>,
    selection: SelectionHandle,
    selected_text: Arc<RwLock<Option<String>>>,
}

impl CliChatView {
    pub(crate) fn new(
        terminal_view_id: EntityId,
        agent: CLIAgent,
        pane_cwd: Option<String>,
        opened_after: DateTime<Utc>,
        ctx: &mut ViewContext<Self>,
    ) -> Self {
        let model =
            ctx.add_model(|ctx| CliChatModel::new(terminal_view_id, pane_cwd, opened_after, ctx));
        ctx.subscribe_to_model(&model, |me, _, event, ctx| match event {
            CliChatModelEvent::Updated => {
                me.follow_tail();
                me.ensure_ticking(ctx);
                ctx.notify();
            }
        });
        ctx.subscribe_to_model(
            &CLIAgentSessionsModel::handle(ctx),
            move |me, _, event, ctx| {
                if event.terminal_view_id() == terminal_view_id
                    && matches!(event, CLIAgentSessionsModelEvent::StatusChanged { .. })
                {
                    me.ensure_ticking(ctx);
                    ctx.notify();
                }
            },
        );
        ctx.subscribe_to_model(&CliChatViewSettings::handle(ctx), |_, _, _, ctx| {
            ctx.notify()
        });

        let appearance = Appearance::as_ref(ctx);
        let composer_text = TextOptions::ui_text(Some(appearance.ui_font_size() + 1.), appearance);
        let placeholder = format!(
            "Message {} (Enter to send, Shift+Enter for a new line)",
            agent.display_name()
        );
        let composer = ctx.add_typed_action_view(|ctx| {
            let mut editor = EditorView::new(
                EditorOptions {
                    text: composer_text,
                    soft_wrap: true,
                    autogrow: true,
                    single_line: false,
                    propagate_and_no_op_vertical_navigation_keys:
                        PropagateAndNoOpNavigationKeys::AtBoundary,
                    enter_settings: EnterSettings {
                        enter: EnterAction::Emit,
                        shift_enter: EnterAction::InsertNewLineIfMultiLine,
                        alt_enter: EnterAction::InsertNewLineIfMultiLine,
                        ctrl_enter: EnterAction::InsertNewLineIfMultiLine,
                    },
                    ..Default::default()
                },
                ctx,
            );
            editor.set_placeholder_text(placeholder, ctx);
            editor
        });
        ctx.subscribe_to_view(&composer, |me, _, event, ctx| {
            me.handle_composer_event(event, ctx)
        });

        let terminal_button = ctx.add_typed_action_view(|ctx| {
            ActionButton::new("Terminal", SecondaryTheme)
                .with_icon(Icon::Terminal)
                .with_size(ButtonSize::Small)
                .with_tooltip("Show the terminal rendering of this session")
                .with_keybinding(KeystrokeSource::Binding(TOGGLE_CLI_CHAT_VIEW_BINDING), ctx)
                .with_compact_keybinding(true)
                .on_click(|ctx| ctx.dispatch_typed_action(CliChatViewAction::ShowTerminal))
        });
        let key_buttons = CliKey::ALL
            .into_iter()
            .map(|key| {
                ctx.add_typed_action_view(move |_| {
                    ActionButton::new(key.label(), NakedTheme)
                        .with_size(ButtonSize::XSmall)
                        .with_tooltip(key.tooltip())
                        .on_click(move |ctx| {
                            ctx.dispatch_typed_action(CliChatViewAction::SendKey(key))
                        })
                })
            })
            .collect();

        Self {
            terminal_view_id,
            agent,
            model,
            composer,
            terminal_button,
            key_buttons,
            scroll: Default::default(),
            max_seen_scroll: 0.,
            toggled: HashSet::new(),
            visible_turns: TURNS_PER_PAGE,
            ticking: false,
            mouse_states: Default::default(),
            output_scrolls: Default::default(),
            selection: Default::default(),
            selected_text: Default::default(),
        }
    }

    pub(crate) fn focus_composer(&self, ctx: &mut ViewContext<Self>) {
        ctx.focus(&self.composer);
    }

    /// Hiding keeps the view (and what the user expanded) but stops tailing the
    /// transcript; showing again catches up.
    pub(crate) fn set_visible(&mut self, visible: bool, ctx: &mut ViewContext<Self>) {
        self.model
            .update(ctx, |model, ctx| model.set_active(visible, ctx));
        if visible {
            self.ensure_ticking(ctx);
        }
    }

    #[cfg(test)]
    pub(crate) fn is_tailing(&self, app: &AppContext) -> bool {
        self.model.as_ref(app).is_active()
    }

    fn handle_composer_event(&mut self, event: &EditorEvent, ctx: &mut ViewContext<Self>) {
        match event {
            EditorEvent::Enter => {
                let text = self.composer.as_ref(ctx).buffer_text(ctx);
                if text.trim().is_empty() {
                    return;
                }
                self.composer.update(ctx, |editor, ctx| {
                    editor.clear_buffer_and_reset_undo_stack(ctx)
                });
                self.scroll_to_bottom();
                ctx.emit(CliChatViewEvent::Submit(text));
            }
            // Ctrl-C on an empty composer interrupts the agent, like in the terminal.
            EditorEvent::CtrlC {
                cleared_buffer_len: 0,
            } => ctx.emit(CliChatViewEvent::SendKey(CliKey::Interrupt)),
            _ => {}
        }
    }

    fn scroll_to_bottom(&mut self) {
        self.max_seen_scroll = 0.;
        self.scroll.scroll_to(Pixels::new(f32::MAX));
    }

    /// Keeps the transcript pinned to the bottom unless the user scrolled up.
    fn follow_tail(&mut self) {
        let current = self.scroll.scroll_start().as_f32();
        if current >= self.max_seen_scroll - 1. {
            self.scroll.scroll_to(Pixels::new(f32::MAX));
        }
        // An offset of f32::MAX has not been clamped by layout yet.
        if current < f32::MAX / 2. {
            self.max_seen_scroll = self.max_seen_scroll.max(current);
        }
    }

    /// Re-renders once a second while a turn runs so the elapsed time moves.
    fn ensure_ticking(&mut self, ctx: &mut ViewContext<Self>) {
        if self.ticking || !self.model.as_ref(ctx).is_active() || self.active_status(ctx).is_none()
        {
            return;
        }
        self.ticking = true;
        ctx.spawn(Timer::after(TICK_INTERVAL), |me, _, ctx| {
            me.ticking = false;
            ctx.notify();
            me.ensure_ticking(ctx);
        });
    }

    fn mouse_state(&self, key: &str) -> MouseStateHandle {
        self.mouse_states
            .borrow_mut()
            .entry(key.to_owned())
            .or_default()
            .clone()
    }

    fn output_scroll(&self, key: &str) -> ClippedScrollStateHandle {
        self.output_scrolls
            .borrow_mut()
            .entry(key.to_owned())
            .or_default()
            .clone()
    }

    fn is_expanded(&self, key: &str, expanded_by_default: bool) -> bool {
        expanded_by_default != self.toggled.contains(key)
    }

    fn toggle(&mut self, key: &str) {
        if !self.toggled.remove(key) {
            self.toggled.insert(key.to_owned());
        }
        // Content height changes; keep following if the user was at the bottom.
        if self.scroll.scroll_start().as_f32() >= self.max_seen_scroll - 1. {
            self.max_seen_scroll = 0.;
        }
    }

    /// The live status of the running turn, or `None` when the agent is idle.
    fn active_status(&self, app: &AppContext) -> Option<ActiveStatus> {
        let session = CLIAgentSessionsModel::as_ref(app).session(self.terminal_view_id)?;
        let thread = self.model.as_ref(app).thread();
        // A transcript-logged prompt (Copilot permission request) is pending
        // until another item follows it.
        if let Some(ChatItem::Attention { text }) = thread.items.last() {
            return Some(ActiveStatus::Blocked(
                text.lines().next().map(str::to_owned),
            ));
        }
        let turn = thread.active_turn();
        if session.supports_rich_status() {
            match &session.status {
                CLIAgentSessionStatus::Blocked { message } => {
                    return Some(ActiveStatus::Blocked(message.clone()));
                }
                CLIAgentSessionStatus::InProgress => {}
                _ => return None,
            }
        }
        turn.map(|turn| ActiveStatus::Working {
            started_at: turn.started_at,
            last_tool: turn.last_tool,
            running_subagents: turn.running_subagents,
        })
    }

    fn jump_to_tool(&mut self, tool_id: &str, ctx: &mut ViewContext<Self>) {
        let model = self.model.as_ref(ctx);
        let items = &model.thread().items;
        let group_key = rows(items).into_iter().find_map(|row| match row {
            Row::ToolGroup(range)
                if items[range.clone()]
                    .iter()
                    .any(|item| matches!(item, ChatItem::Tool(tool) if tool.id == tool_id)) =>
            {
                group_key(items, range.start)
            }
            _ => None,
        });
        if let Some(group_key) = group_key {
            self.toggled.insert(group_key);
        }
        self.scroll.scroll_to_position(ScrollTarget {
            position_id: tool_position_id(tool_id),
            mode: ScrollToPositionMode::TopIntoView,
        });
        ctx.notify();
    }
}

enum ActiveStatus {
    Working {
        started_at: Option<DateTime<Utc>>,
        last_tool: Option<String>,
        running_subagents: usize,
    },
    Blocked(Option<String>),
}

fn tool_position_id(tool_id: &str) -> String {
    format!("cli_chat_tool_{tool_id}")
}

fn tool_key(tool_id: &str) -> String {
    format!("tool:{tool_id}")
}

fn group_key(items: &[ChatItem], start: usize) -> Option<String> {
    match items.get(start)? {
        ChatItem::Tool(tool) => Some(format!("group:{}", tool.id)),
        _ => None,
    }
}

fn format_duration(duration: chrono::Duration) -> String {
    let millis = duration.num_milliseconds().max(0);
    if millis < 1_000 {
        format!("{millis}ms")
    } else if millis < 60_000 {
        format!("{:.1}s", millis as f64 / 1_000.)
    } else {
        format!("{}m {:02}s", millis / 60_000, (millis % 60_000) / 1_000)
    }
}

fn format_time(at: DateTime<Utc>) -> String {
    at.with_timezone(&Local).format("%H:%M").to_string()
}

fn capped(text: &str, max_chars: usize) -> Cow<'_, str> {
    match text.char_indices().nth(max_chars) {
        Some((cut, _)) => Cow::Owned(format!("{}\n… (truncated)", &text[..cut])),
        None => Cow::Borrowed(text),
    }
}

/// Unified line diff of an edit-like tool call, if it is one.
fn tool_diff(tool: &ToolItem) -> Option<Vec<(ChangeTag, String)>> {
    let input = &tool.input;
    let str_field = |value: &Value, key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let pairs: Vec<(String, String)> = match tool.name.as_str() {
        "Edit" => vec![(
            str_field(input, "old_string"),
            str_field(input, "new_string"),
        )],
        "MultiEdit" => input
            .get("edits")
            .and_then(Value::as_array)?
            .iter()
            .map(|edit| (str_field(edit, "old_string"), str_field(edit, "new_string")))
            .collect(),
        "Write" => vec![(String::new(), str_field(input, "content"))],
        // Copilot CLI.
        "edit" => vec![(str_field(input, "old_str"), str_field(input, "new_str"))],
        "create" => vec![(String::new(), str_field(input, "file_text"))],
        // Codex: already a diff.
        "apply_patch" => return Some(patch_lines(input.get("patch")?.as_str()?)),
        _ => return None,
    };
    let mut lines = Vec::new();
    for (old, new) in &pairs {
        if !lines.is_empty() {
            lines.push((ChangeTag::Equal, "⋯".to_owned()));
        }
        lines.extend(
            TextDiff::from_lines(old, new)
                .iter_all_changes()
                .map(|change| {
                    (
                        change.tag(),
                        change.value().trim_end_matches('\n').to_owned(),
                    )
                }),
        );
    }
    Some(lines)
}

/// Lines of a Codex `apply_patch` body; file headers stay as context lines.
fn patch_lines(patch: &str) -> Vec<(ChangeTag, String)> {
    patch
        .lines()
        .filter(|line| !matches!(*line, "*** Begin Patch" | "*** End Patch"))
        .map(|line| match line.split_at_checked(1) {
            Some(("+", rest)) => (ChangeTag::Insert, rest.to_owned()),
            Some(("-", rest)) => (ChangeTag::Delete, rest.to_owned()),
            Some((" ", rest)) => (ChangeTag::Equal, rest.to_owned()),
            _ => (
                ChangeTag::Equal,
                line.strip_prefix("*** ").unwrap_or(line).to_owned(),
            ),
        })
        .collect()
}

/// Shared values for one render pass.
struct Palette {
    text: ColorU,
    sub: ColorU,
    hint: ColorU,
    green: ColorU,
    red: ColorU,
    warning: ColorU,
    card: Fill,
    bubble: Fill,
    border: Fill,
    ui_font: FamilyId,
    mono_font: FamilyId,
    font_size: f32,
    mono_size: f32,
    collapse_thinking: bool,
    collapse_tool_output: bool,
    show_timestamps: bool,
}

impl Palette {
    fn new(app: &AppContext) -> Self {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let background = theme.background();
        let settings = CliChatViewSettings::as_ref(app);
        Self {
            text: theme.main_text_color(background).into_solid(),
            sub: theme.sub_text_color(background).into_solid(),
            hint: theme.hint_text_color(background).into_solid(),
            green: theme.ui_green_color(),
            red: theme.ui_error_color(),
            warning: theme.ui_warning_color(),
            card: theme.surface_1().into(),
            bubble: theme.surface_2().into(),
            border: theme.outline().into(),
            ui_font: appearance.ui_font_family(),
            mono_font: appearance.monospace_font_family(),
            font_size: appearance.ui_font_size() + 1.,
            mono_size: appearance.monospace_font_size() - 1.,
            collapse_thinking: *settings.collapse_thinking,
            collapse_tool_output: *settings.collapse_tool_output,
            show_timestamps: *settings.show_timestamps,
        }
    }

    fn text(&self, text: impl Into<Cow<'static, str>>, color: ColorU) -> Text {
        Text::new(text, self.ui_font, self.font_size).with_color(color)
    }

    fn small(&self, text: impl Into<Cow<'static, str>>, color: ColorU) -> Text {
        Text::new(text, self.ui_font, self.font_size - 2.).with_color(color)
    }

    fn mono(&self, text: impl Into<Cow<'static, str>>, color: ColorU) -> Text {
        Text::new(text, self.mono_font, self.mono_size).with_color(color)
    }

    fn icon(&self, icon: Icon, color: ColorU, size: f32) -> Box<dyn Element> {
        ConstrainedBox::new(icon.to_warpui_icon(ThemeFill::Solid(color)).finish())
            .with_width(size)
            .with_height(size)
            .finish()
    }
}

fn tool_icon(kind: ToolKind) -> Icon {
    match kind {
        ToolKind::Read => Icon::File,
        ToolKind::Edit => Icon::Pencil,
        ToolKind::Command => Icon::Terminal,
        ToolKind::Search => Icon::Search,
        ToolKind::Web => Icon::Globe,
        ToolKind::Mcp => Icon::Dataflow,
        ToolKind::Subagent => Icon::Agent,
        ToolKind::Other => Icon::Tool,
    }
}

impl CliChatView {
    fn clickable(
        &self,
        key: &str,
        action: CliChatViewAction,
        child: Box<dyn Element>,
    ) -> Box<dyn Element> {
        Hoverable::new(self.mouse_state(key), |_| child)
            .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
            .with_cursor(Cursor::PointingHand)
            .finish()
    }

    fn render_header(&self, thread: &Thread, palette: &Palette) -> Box<dyn Element> {
        let mut row = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_main_axis_size(MainAxisSize::Max)
            .with_spacing(8.);
        if let Some(icon) = self.agent.icon() {
            let color = self.agent.brand_color().unwrap_or(palette.text);
            row.add_child(palette.icon(icon, color, 16.));
        }
        let title = thread
            .title()
            .map(str::to_owned)
            .unwrap_or_else(|| self.agent.display_name().to_owned());
        row.add_child(
            Shrinkable::new(
                1.,
                palette
                    .text(title, palette.text)
                    .soft_wrap(false)
                    .with_clip(ClipConfig::ellipsis())
                    .finish(),
            )
            .finish(),
        );
        row.add_child(ChildView::new(&self.terminal_button).finish());
        Container::new(row.finish())
            .with_horizontal_padding(16.)
            .with_vertical_padding(8.)
            .with_border(Border::bottom(1.).with_border_fill(palette.border))
            .finish()
    }

    fn render_transcript(&self, app: &AppContext, palette: &Palette) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let model = self.model.as_ref(app);
        let thread = model.thread();
        let items = &thread.items;

        let mut column = Flex::column().with_spacing(8.);
        if items.is_empty() {
            column.add_child(
                palette
                    .text(
                        "Waiting for the session transcript. Send a prompt to start.",
                        palette.sub,
                    )
                    .finish(),
            );
        }

        let prompts: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, item)| matches!(item, ChatItem::User { .. }))
            .map(|(index, _)| index)
            .collect();
        let first_visible = prompts
            .len()
            .checked_sub(self.visible_turns)
            .and_then(|hidden| prompts.get(hidden).copied())
            .unwrap_or(0);
        if first_visible > 0 {
            column.add_child(
                self.clickable(
                    "show_earlier",
                    CliChatViewAction::ShowEarlierTurns,
                    palette
                        .small("Show earlier turns", palette.sub)
                        .with_selectable(false)
                        .finish(),
                ),
            );
        }

        for row in rows(items) {
            let start = match &row {
                Row::Item(index) => *index,
                Row::ToolGroup(range) => range.start,
            };
            if start >= first_visible
                && let Some(element) = self.render_row(&row, thread, "", true, model, palette, app)
            {
                column.add_child(element);
            }
        }

        let selected_text = self.selected_text.clone();
        let scrollable = ClippedScrollable::vertical(
            self.scroll.clone(),
            Align::new(
                ConstrainedBox::new(
                    Container::new(column.finish())
                        .with_horizontal_padding(16.)
                        .with_vertical_padding(12.)
                        .finish(),
                )
                .with_max_width(READING_WIDTH)
                .finish(),
            )
            .top_center()
            .finish(),
            ScrollbarWidth::Auto,
            appearance.theme().nonactive_ui_detail().into(),
            appearance.theme().active_ui_detail().into(),
            Fill::None,
        )
        .finish();
        SelectableArea::new(
            self.selection.clone(),
            move |args, _, _| {
                *selected_text.write() = args.selection.filter(|text| !text.is_empty());
            },
            scrollable,
        )
        .on_selection_updated(|ctx, _| ctx.dispatch_typed_action(CliChatViewAction::Focus))
        .finish()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_row(
        &self,
        row: &Row,
        thread: &Thread,
        scope: &str,
        is_main: bool,
        model: &CliChatModel,
        palette: &Palette,
        app: &AppContext,
    ) -> Option<Box<dyn Element>> {
        let items = &thread.items;
        let index = match row {
            Row::Item(index) => *index,
            Row::ToolGroup(range) => {
                return Some(self.render_group(items, range.clone(), model, palette, app));
            }
        };
        let element = match &items[index] {
            ChatItem::User { text, at } => self.render_user(text, *at, palette),
            ChatItem::Notice { text } => {
                let mut lines = text.lines();
                let mut shown = lines.by_ref().take(3).collect::<Vec<_>>().join("\n");
                if lines.next().is_some() {
                    shown.push_str("\n…");
                }
                palette.mono(shown, palette.sub).finish()
            }
            ChatItem::Attention { text } => Flex::row()
                .with_spacing(6.)
                .with_child(palette.icon(Icon::AlertTriangle, palette.warning, 14.))
                .with_child(
                    Shrinkable::new(1., palette.text(text.clone(), palette.warning).finish())
                        .finish(),
                )
                .finish(),
            ChatItem::Assistant { text, markdown, at } => {
                let body = match markdown {
                    Some(markdown) => FormattedTextElement::new_arc(
                        markdown.clone(),
                        palette.font_size,
                        palette.ui_font,
                        palette.mono_font,
                        palette.text,
                        HighlightedHyperlink::default(),
                    )
                    .with_hyperlink_font_color(
                        Appearance::as_ref(app).theme().accent().into_solid(),
                    )
                    .register_default_click_handlers(|url, _, ctx| ctx.open_url(&url.url))
                    .set_selectable(true)
                    .finish(),
                    None => palette.text(text.clone(), palette.text).finish(),
                };
                match at.filter(|_| palette.show_timestamps) {
                    Some(at) => Flex::column()
                        .with_spacing(2.)
                        .with_child(palette.small(format_time(at), palette.hint).finish())
                        .with_child(body)
                        .finish(),
                    None => body,
                }
            }
            ChatItem::Thinking { text } => {
                let key = format!("{scope}thinking:{index}");
                let expanded = self.is_expanded(&key, !palette.collapse_thinking);
                let header = self.clickable(
                    &key,
                    CliChatViewAction::Toggle(key.clone()),
                    Flex::row()
                        .with_spacing(4.)
                        .with_cross_axis_alignment(CrossAxisAlignment::Center)
                        .with_child(palette.icon(chevron(expanded), palette.sub, 12.))
                        .with_child(
                            palette
                                .small("Thinking", palette.sub)
                                .with_selectable(false)
                                .finish(),
                        )
                        .finish(),
                );
                if !expanded {
                    return Some(header);
                }
                Flex::column()
                    .with_spacing(4.)
                    .with_child(header)
                    .with_child(
                        Container::new(palette.small(text.clone(), palette.sub).finish())
                            .with_padding_left(16.)
                            .finish(),
                    )
                    .finish()
            }
            ChatItem::Tool(tool) => self.render_tool(tool, model, palette, app),
            // Subagent threads skip the end-of-turn card.
            ChatItem::TurnEnd if is_main => {
                return turn_summary(items, index)
                    .map(|summary| self.render_turn_summary(&summary, palette));
            }
            ChatItem::TurnEnd => return None,
        };
        Some(element)
    }

    fn render_user(
        &self,
        text: &str,
        at: Option<DateTime<Utc>>,
        palette: &Palette,
    ) -> Box<dyn Element> {
        let mut column = Flex::column().with_spacing(2.);
        if let Some(at) = at.filter(|_| palette.show_timestamps) {
            column.add_child(palette.small(format_time(at), palette.hint).finish());
        }
        column.add_child(palette.text(text.to_owned(), palette.text).finish());
        Container::new(column.finish())
            .with_background(palette.bubble)
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.)))
            .with_horizontal_padding(12.)
            .with_vertical_padding(8.)
            .with_margin_top(8.)
            .finish()
    }

    fn render_group(
        &self,
        items: &[ChatItem],
        range: std::ops::Range<usize>,
        model: &CliChatModel,
        palette: &Palette,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let tools: Vec<&ToolItem> = items[range.clone()]
            .iter()
            .filter_map(|item| match item {
                ChatItem::Tool(tool) => Some(tool),
                _ => None,
            })
            .collect();
        let Some(key) = group_key(items, range.start) else {
            return Empty::new().finish();
        };
        let expanded = self.is_expanded(&key, false);
        let header = self.clickable(
            &key,
            CliChatViewAction::Toggle(key.clone()),
            Flex::row()
                .with_spacing(6.)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(palette.icon(chevron(expanded), palette.sub, 12.))
                .with_child(palette.icon(Icon::Check, palette.green, 12.))
                .with_child(
                    palette
                        .small(group_label(tools.iter().copied()), palette.sub)
                        .with_selectable(false)
                        .finish(),
                )
                .finish(),
        );
        if !expanded {
            return header;
        }
        let mut column = Flex::column().with_spacing(6.).with_child(header);
        for tool in tools {
            column.add_child(self.render_tool(tool, model, palette, app));
        }
        column.finish()
    }

    fn render_tool(
        &self,
        tool: &ToolItem,
        model: &CliChatModel,
        palette: &Palette,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let key = tool_key(&tool.id);
        let is_subagent = tool.kind() == ToolKind::Subagent;
        let expanded_by_default =
            !is_subagent && (tool.is_error() || !palette.collapse_tool_output);
        let expanded = self.is_expanded(&key, expanded_by_default);
        let action = if is_subagent {
            CliChatViewAction::ToggleSubagent(tool.id.clone())
        } else {
            CliChatViewAction::Toggle(key.clone())
        };

        let mut header = Flex::row()
            .with_spacing(6.)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_main_axis_size(MainAxisSize::Max)
            .with_child(palette.icon(chevron(expanded), palette.sub, 12.))
            .with_child(palette.icon(tool_icon(tool.kind()), palette.sub, 14.));
        match tool.mcp_server_and_tool() {
            Some((server, name)) => {
                header.add_child(
                    palette
                        .text(name.to_owned(), palette.text)
                        .with_selectable(false)
                        .finish(),
                );
                header.add_child(
                    Container::new(
                        palette
                            .small(server.to_owned(), palette.sub)
                            .with_selectable(false)
                            .finish(),
                    )
                    .with_background(palette.bubble)
                    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)))
                    .with_horizontal_padding(4.)
                    .finish(),
                );
            }
            None => header.add_child(
                palette
                    .text(tool.name.clone(), palette.text)
                    .with_selectable(false)
                    .finish(),
            ),
        }
        header.add_child(
            Shrinkable::new(
                1.,
                palette
                    .mono(tool.summary(), palette.sub)
                    .with_selectable(false)
                    .soft_wrap(false)
                    .with_clip(ClipConfig::ellipsis())
                    .finish(),
            )
            .finish(),
        );
        if let Some(duration) = tool.duration() {
            header.add_child(
                palette
                    .small(format_duration(duration), palette.hint)
                    .with_selectable(false)
                    .finish(),
            );
        }
        header.add_child(match &tool.outcome {
            None => palette
                .small("running", palette.warning)
                .with_selectable(false)
                .finish(),
            Some(outcome) if outcome.is_error => palette.icon(Icon::XCircle, palette.red, 14.),
            Some(_) => palette.icon(Icon::Check, palette.green, 14.),
        });

        let mut card = Flex::column().with_spacing(6.).with_child(self.clickable(
            &key,
            action,
            header.finish(),
        ));
        if expanded {
            card.add_child(self.render_tool_body(tool, model, palette, app));
        }
        let border = if tool.is_error() {
            Fill::Solid(palette.red)
        } else {
            palette.border
        };
        SavePosition::new(
            Container::new(card.finish())
                .with_background(palette.card)
                .with_border(Border::all(1.).with_border_fill(border))
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.)))
                .with_horizontal_padding(10.)
                .with_vertical_padding(6.)
                .finish(),
            &tool_position_id(&tool.id),
        )
        .finish()
    }

    fn render_tool_body(
        &self,
        tool: &ToolItem,
        model: &CliChatModel,
        palette: &Palette,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let mut body = Flex::column().with_spacing(6.);
        if tool.kind() == ToolKind::Subagent {
            match model.subagent_thread(&tool.id) {
                Some(thread) if !thread.items.is_empty() => {
                    let mut nested = Flex::column().with_spacing(6.);
                    for row in rows(&thread.items) {
                        if let Some(element) =
                            self.render_row(&row, thread, &tool.id, false, model, palette, app)
                        {
                            nested.add_child(element);
                        }
                    }
                    body.add_child(
                        Container::new(nested.finish())
                            .with_padding_left(10.)
                            .with_border(Border::left(2.).with_border_fill(palette.border))
                            .finish(),
                    );
                }
                _ => body.add_child(
                    palette
                        .small("Loading the subagent transcript…", palette.sub)
                        .finish(),
                ),
            }
        } else if let Some(diff) = tool_diff(tool) {
            let mut lines = Flex::column();
            for (tag, line) in diff.iter().take(MAX_DIFF_LINES) {
                let (sign, color) = match tag {
                    ChangeTag::Insert => ("+", palette.green),
                    ChangeTag::Delete => ("-", palette.red),
                    ChangeTag::Equal => (" ", palette.sub),
                };
                lines.add_child(palette.mono(format!("{sign} {line}"), color).finish());
            }
            if diff.len() > MAX_DIFF_LINES {
                lines.add_child(palette.mono("… (truncated)", palette.hint).finish());
            }
            body.add_child(self.scroll_box(&format!("{}:diff", tool.id), lines.finish(), palette));
        } else if tool.kind() == ToolKind::Command {
            if let Some(command) = tool.input.get("command").and_then(Value::as_str) {
                body.add_child(palette.mono(format!("$ {command}"), palette.text).finish());
            }
        } else if tool
            .input
            .as_object()
            .is_some_and(|fields| !fields.is_empty())
        {
            let input = serde_json::to_string_pretty(&tool.input).unwrap_or_default();
            body.add_child(
                self.scroll_box(
                    &format!("{}:input", tool.id),
                    palette
                        .mono(capped(&input, MAX_OUTPUT_CHARS).into_owned(), palette.sub)
                        .finish(),
                    palette,
                ),
            );
        }
        if let Some(outcome) = &tool.outcome
            && !outcome.content.trim().is_empty()
            && tool.kind() != ToolKind::Subagent
        {
            let color = if outcome.is_error {
                palette.red
            } else {
                palette.text
            };
            body.add_child(
                self.scroll_box(
                    &format!("{}:output", tool.id),
                    palette
                        .mono(
                            capped(&outcome.content, MAX_OUTPUT_CHARS).into_owned(),
                            color,
                        )
                        .finish(),
                    palette,
                ),
            );
        }
        body.finish()
    }

    /// A height-capped block with its own scroll.
    fn scroll_box(
        &self,
        key: &str,
        child: Box<dyn Element>,
        palette: &Palette,
    ) -> Box<dyn Element> {
        ConstrainedBox::new(
            Container::new(
                ClippedScrollable::vertical(
                    self.output_scroll(key),
                    child,
                    ScrollbarWidth::Auto,
                    palette.border,
                    Fill::Solid(palette.sub),
                    Fill::None,
                )
                .finish(),
            )
            .with_background(palette.bubble)
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)))
            .with_uniform_padding(6.)
            .finish(),
        )
        .with_max_height(OUTPUT_MAX_HEIGHT)
        .finish()
    }

    fn render_turn_summary(
        &self,
        summary: &super::model::TurnSummary,
        palette: &Palette,
    ) -> Box<dyn Element> {
        let mut counts = vec![format!(
            "{} {} changed",
            summary.files.len(),
            if summary.files.len() == 1 {
                "file"
            } else {
                "files"
            }
        )];
        counts.push(format!(
            "{} {}",
            summary.commands,
            if summary.commands == 1 {
                "command"
            } else {
                "commands"
            }
        ));
        let error_color = if summary.errors.is_empty() {
            palette.sub
        } else {
            palette.red
        };
        let mut column = Flex::column().with_spacing(4.).with_child(
            Flex::row()
                .with_spacing(8.)
                .with_child(palette.small(counts.join(" · "), palette.sub).finish())
                .with_child(
                    palette
                        .small(
                            format!(
                                "{} {}",
                                summary.errors.len(),
                                if summary.errors.len() == 1 {
                                    "error"
                                } else {
                                    "errors"
                                }
                            ),
                            error_color,
                        )
                        .finish(),
                )
                .finish(),
        );
        let mut links = Flex::row().with_spacing(8.);
        for (file, tool_id) in &summary.files {
            let name = std::path::Path::new(file)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| file.clone());
            links.add_child(
                self.clickable(
                    &format!("jump:{tool_id}"),
                    CliChatViewAction::JumpToTool(tool_id.clone()),
                    palette
                        .small(name, palette.text)
                        .with_selectable(false)
                        .finish(),
                ),
            );
        }
        for tool_id in &summary.errors {
            links.add_child(
                self.clickable(
                    &format!("jump:{tool_id}"),
                    CliChatViewAction::JumpToTool(tool_id.clone()),
                    palette
                        .small("view error", palette.red)
                        .with_selectable(false)
                        .finish(),
                ),
            );
        }
        column.add_child(links.finish());
        Container::new(column.finish())
            .with_border(Border::all(1.).with_border_fill(palette.border))
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.)))
            .with_horizontal_padding(10.)
            .with_vertical_padding(6.)
            .finish()
    }

    fn render_status_strip(&self, app: &AppContext, palette: &Palette) -> Option<Box<dyn Element>> {
        let text = match self.active_status(app)? {
            ActiveStatus::Blocked(message) => {
                let message = message.unwrap_or_else(|| "Waiting for your input".to_owned());
                return Some(
                    self.strip(
                        palette
                            .small(format!("Needs you: {message}"), palette.warning)
                            .soft_wrap(false)
                            .with_clip(ClipConfig::ellipsis())
                            .finish(),
                    ),
                );
            }
            ActiveStatus::Working {
                started_at,
                last_tool,
                running_subagents,
            } => {
                let mut parts = vec!["Working".to_owned()];
                if let Some(started_at) = started_at {
                    parts.push(format_duration(Utc::now() - started_at));
                }
                if let Some(tool) = last_tool {
                    parts.push(format!("last: {tool}"));
                }
                if running_subagents > 0 {
                    parts.push(format!(
                        "{running_subagents} {}",
                        if running_subagents == 1 {
                            "subagent"
                        } else {
                            "subagents"
                        }
                    ));
                }
                parts.join(" · ")
            }
        };
        Some(
            self.strip(
                palette
                    .small(text, palette.sub)
                    .soft_wrap(false)
                    .with_clip(ClipConfig::ellipsis())
                    .finish(),
            ),
        )
    }

    fn strip(&self, child: Box<dyn Element>) -> Box<dyn Element> {
        Container::new(child)
            .with_horizontal_padding(16.)
            .with_vertical_padding(4.)
            .finish()
    }

    fn render_composer(&self, palette: &Palette) -> Box<dyn Element> {
        let editor = ConstrainedBox::new(
            Container::new(ChildView::new(&self.composer).finish())
                .with_background(palette.card)
                .with_border(Border::all(1.).with_border_fill(palette.border))
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.)))
                .with_horizontal_padding(10.)
                .with_vertical_padding(8.)
                .finish(),
        )
        .with_max_height(COMPOSER_MAX_HEIGHT)
        .finish();
        let mut keys = Flex::row()
            .with_spacing(2.)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(palette.small("Send key:", palette.hint).finish());
        for button in &self.key_buttons {
            keys.add_child(ChildView::new(button).finish());
        }
        Container::new(
            Align::new(
                ConstrainedBox::new(
                    Flex::column()
                        .with_spacing(4.)
                        .with_child(editor)
                        .with_child(
                            Flex::row()
                                .with_main_axis_alignment(MainAxisAlignment::Start)
                                .with_child(keys.finish())
                                .finish(),
                        )
                        .finish(),
                )
                .with_max_width(READING_WIDTH)
                .finish(),
            )
            .top_center()
            .finish(),
        )
        .with_horizontal_padding(16.)
        .with_padding_bottom(8.)
        .finish()
    }
}

fn chevron(expanded: bool) -> Icon {
    if expanded {
        Icon::ChevronDown
    } else {
        Icon::ChevronRight
    }
}

impl Entity for CliChatView {
    type Event = CliChatViewEvent;
}

impl View for CliChatView {
    fn ui_name() -> &'static str {
        "CliChatView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let palette = Palette::new(app);
        let appearance = Appearance::as_ref(app);
        let model = self.model.as_ref(app);
        let mut column = Flex::column()
            .with_child(self.render_header(model.thread(), &palette))
            .with_child(Expanded::new(1., self.render_transcript(app, &palette)).finish());
        if let Some(strip) = self.render_status_strip(app, &palette) {
            column.add_child(strip);
        }
        column.add_child(self.render_composer(&palette));
        Container::new(column.finish())
            .with_background(appearance.theme().background())
            .finish()
    }
}

impl TypedActionView for CliChatView {
    type Action = CliChatViewAction;

    fn handle_action(&mut self, action: &CliChatViewAction, ctx: &mut ViewContext<Self>) {
        match action {
            CliChatViewAction::Toggle(key) => {
                self.toggle(key);
                ctx.notify();
            }
            CliChatViewAction::ToggleSubagent(tool_id) => {
                self.toggle(&tool_key(tool_id));
                self.model
                    .update(ctx, |model, _| model.watch_subagent(tool_id));
                ctx.notify();
            }
            CliChatViewAction::ShowEarlierTurns => {
                self.visible_turns += TURNS_PER_PAGE;
                ctx.notify();
            }
            CliChatViewAction::JumpToTool(tool_id) => self.jump_to_tool(tool_id, ctx),
            CliChatViewAction::SendKey(key) => ctx.emit(CliChatViewEvent::SendKey(*key)),
            CliChatViewAction::ShowTerminal => ctx.emit(CliChatViewEvent::ShowTerminal),
            CliChatViewAction::Focus => ctx.focus_self(),
            CliChatViewAction::CopySelectedText => {
                if let Some(text) = self.selected_text.read().clone() {
                    ctx.clipboard().write(ClipboardContent::plain_text(text));
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;

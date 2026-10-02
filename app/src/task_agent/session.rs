//! Which task each terminal pane is working on, plus the pane-header key chip.

use std::collections::HashMap;
use std::path::PathBuf;

use warp_core::ui::theme::color::internal_colors;
use warpui::elements::{CornerRadius, Element, Hoverable, MouseStateHandle, Radius};
use warpui::platform::Cursor;
use warpui::ui_components::chip::Chip;
use warpui::ui_components::components::{UiComponent, UiComponentStyles};
use warpui::{
    AppContext, Entity, EntityId, ModelContext, SingletonEntity, UpdateView, WeakViewHandle,
};

use super::Checkout;
use crate::appearance::Appearance;
use crate::features::FeatureFlag;
use crate::terminal::TerminalView;
use crate::terminal::cli_agent_sessions::{CLIAgentSessionsModel, CLIAgentSessionsModelEvent};
use crate::workspace::WorkspaceAction;

/// The task a terminal pane's agent is working on.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TaskSession {
    pub key: Option<String>,
    pub title: String,
    pub url: Option<String>,
    pub repo_root: PathBuf,
    pub branch: Option<String>,
    pub checkout: Checkout,
}

struct Entry {
    task: TaskSession,
    key_chip_mouse_state: MouseStateHandle,
}

/// Task metadata keyed by terminal view id.
// ponytail: entries outlive closed panes (a few strings each); prune on pane close if that grows.
#[derive(Default)]
pub(crate) struct TaskSessionsModel {
    sessions: HashMap<EntityId, Entry>,
    /// Prompts to type into an agent that takes no prompt argument, once its session starts.
    pending_prompts: HashMap<EntityId, (WeakViewHandle<TerminalView>, String)>,
}

impl TaskSessionsModel {
    pub(crate) fn new(ctx: &mut ModelContext<Self>) -> Self {
        ctx.subscribe_to_model(&CLIAgentSessionsModel::handle(ctx), |me, _, event, ctx| {
            if let CLIAgentSessionsModelEvent::Started {
                terminal_view_id, ..
            }
            | CLIAgentSessionsModelEvent::SessionUpdated {
                terminal_view_id, ..
            } = event
            {
                me.deliver_pending_prompt(*terminal_view_id, ctx);
            }
        });
        Self::default()
    }

    pub(crate) fn get(&self, terminal_view_id: EntityId) -> Option<&TaskSession> {
        self.sessions
            .get(&terminal_view_id)
            .map(|entry| &entry.task)
    }

    pub(crate) fn set(&mut self, terminal_view_id: EntityId, task: TaskSession) {
        self.sessions
            .entry(terminal_view_id)
            .and_modify(|entry| entry.task = task.clone())
            .or_insert_with(|| Entry {
                task,
                key_chip_mouse_state: Default::default(),
            });
    }

    pub(crate) fn set_pending_prompt(
        &mut self,
        terminal_view: WeakViewHandle<TerminalView>,
        terminal_view_id: EntityId,
        prompt: String,
    ) {
        self.pending_prompts
            .insert(terminal_view_id, (terminal_view, prompt));
    }

    fn deliver_pending_prompt(&mut self, terminal_view_id: EntityId, ctx: &mut ModelContext<Self>) {
        let Some((view, prompt)) = self.pending_prompts.remove(&terminal_view_id) else {
            return;
        };
        let Some(view) = view.upgrade(ctx) else {
            return;
        };
        ctx.update_view(&view, |view, ctx| {
            // Bracketed paste keeps a multi-line prompt from being submitted line by line; the
            // user reviews it and presses Enter.
            let text = if view.is_cli_agent_rich_input_open(ctx) {
                prompt
            } else {
                format!("\x1b[200~{prompt}\x1b[201~")
            };
            if view
                .try_send_text_to_cli_agent_or_rich_input(text, ctx)
                .is_none()
            {
                log::warn!("Task agent prompt not delivered: no CLI agent in the pane");
            }
        });
    }

    /// The entry for a pane, when the launcher is enabled. Tolerates the model not being
    /// registered (minimal test harnesses render terminal views without it).
    fn entry(terminal_view_id: EntityId, app: &AppContext) -> Option<&Entry> {
        if !FeatureFlag::TaskAgentLauncher.is_enabled() || !app.has_singleton_model::<Self>() {
            return None;
        }
        Self::as_ref(app).sessions.get(&terminal_view_id)
    }

    /// Whether the pane has a task key to show in its header.
    pub(crate) fn has_key(terminal_view_id: EntityId, app: &AppContext) -> bool {
        Self::entry(terminal_view_id, app).is_some_and(|entry| entry.task.key.is_some())
    }

    /// The pane-header chip showing the task key; clicking it copies the key.
    pub(crate) fn render_key_chip(
        terminal_view_id: EntityId,
        app: &AppContext,
    ) -> Option<Box<dyn Element>> {
        let entry = Self::entry(terminal_view_id, app)?;
        let key = entry.task.key.clone()?;
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let style = UiComponentStyles {
            border_color: Some(internal_colors::neutral_4(theme).into()),
            border_width: Some(1.),
            border_radius: Some(CornerRadius::with_all(Radius::Pixels(4.))),
            font_family_id: Some(appearance.ui_font_family()),
            font_size: Some(appearance.ui_font_size() - 1.),
            font_color: Some(theme.sub_text_color(theme.background()).into_solid()),
            ..Default::default()
        };
        let label = key.clone();
        Some(
            Hoverable::new(entry.key_chip_mouse_state.clone(), move |_| {
                Chip::new(label.clone(), style).build().finish()
            })
            .with_cursor(Cursor::PointingHand)
            .on_click(move |ctx, _, _| {
                ctx.dispatch_typed_action(WorkspaceAction::CopyTextToClipboard(key.clone()));
            })
            .finish(),
        )
    }
}

impl Entity for TaskSessionsModel {
    type Event = ();
}

impl SingletonEntity for TaskSessionsModel {}

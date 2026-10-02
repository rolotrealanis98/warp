//! Which terminal panes run a PR agent, the polling that watches their pull requests, delivery of
//! updates to the agent, mirroring of the agent's review comments, and the pane-header chips.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use warp_core::safe_warn;
use warp_core::ui::theme::color::internal_colors;
use warpui::r#async::{SpawnedFutureHandle, Timer};
use warpui::elements::{
    Container, CornerRadius, CrossAxisAlignment, Element, Flex, Hoverable, MouseStateHandle,
    ParentElement, Radius,
};
use warpui::platform::Cursor;
use warpui::ui_components::chip::Chip;
use warpui::ui_components::components::{UiComponent, UiComponentStyles};
use warpui::{
    AppContext, Entity, EntityId, ModelContext, SingletonEntity, ViewHandle, WeakViewHandle,
};

use super::mirror::{
    ReviewComment, comments_to_mirror, parse_review_comments, review_comments_args,
};
use super::settings::PrAgentSettings;
use super::watcher::{
    ChecksState, PR_VIEW_FIELDS, PrEvent, PrSnapshot, diff_snapshots, format_events,
};
use super::{PrDetails, pr_view_args, run_gh};
use crate::appearance::Appearance;
use crate::features::FeatureFlag;
use crate::terminal::TerminalView;
use crate::terminal::cli_agent_sessions::{
    CLIAgentSessionStatus, CLIAgentSessionsModel, CLIAgentSessionsModelEvent,
};
use crate::workspace::WorkspaceAction;

/// What the workspace hands over when it starts a PR agent.
#[derive(Clone, Debug)]
pub(crate) struct PrWatchRequest {
    pub details: PrDetails,
    /// Login of the `gh` user, which the agent posts as; `None` when it could not be read.
    pub viewer: Option<String>,
    /// Repository the agent was launched from; `gh` runs here.
    pub repo_root: PathBuf,
    /// Where the pull request is checked out (the worktree, or `repo_root`).
    pub checkout_path: PathBuf,
}

struct Watch {
    request: PrWatchRequest,
    terminal: WeakViewHandle<TerminalView>,
    snapshot: Option<PrSnapshot>,
    /// Events not delivered to the agent yet (it was busy).
    pending: Vec<PrEvent>,
    /// Events since the user last opened the pull request from the header.
    unread: usize,
    /// Review comments already shown in the Code Review panel.
    mirrored: HashSet<u64>,
    is_mirroring: bool,
    /// The scheduled poll or the `gh` calls in flight; aborted with the watch.
    poll: Option<SpawnedFutureHandle>,
    chip_mouse_state: MouseStateHandle,
}

impl Drop for Watch {
    fn drop(&mut self) {
        if let Some(poll) = self.poll.take() {
            poll.abort();
        }
    }
}

/// PR agent panes keyed by terminal view id.
#[derive(Default)]
pub(crate) struct PrAgentModel {
    watches: HashMap<EntityId, Watch>,
}

impl PrAgentModel {
    pub(crate) fn new(ctx: &mut ModelContext<Self>) -> Self {
        ctx.subscribe_to_model(&CLIAgentSessionsModel::handle(ctx), |me, _, event, ctx| {
            if let CLIAgentSessionsModelEvent::StatusChanged {
                terminal_view_id,
                status,
                ..
            } = event
            {
                me.on_agent_status_changed(*terminal_view_id, status, ctx);
            }
        });
        Self::default()
    }

    /// Starts watching the pull request for the agent in `terminal`.
    pub(crate) fn watch(
        &mut self,
        terminal: &ViewHandle<TerminalView>,
        request: PrWatchRequest,
        ctx: &mut ModelContext<Self>,
    ) {
        let id = terminal.id();
        self.watches.insert(
            id,
            Watch {
                request,
                terminal: terminal.downgrade(),
                snapshot: None,
                pending: Vec::new(),
                unread: 0,
                mirrored: HashSet::new(),
                is_mirroring: false,
                poll: None,
                chip_mouse_state: Default::default(),
            },
        );
        self.poll(id, ctx);
    }

    /// Clears the unread count and returns the pull request's URL.
    pub(crate) fn mark_read(
        &mut self,
        terminal_view_id: EntityId,
        ctx: &mut ModelContext<Self>,
    ) -> Option<String> {
        let watch = self.watches.get_mut(&terminal_view_id)?;
        watch.unread = 0;
        let url = watch.request.details.url.clone();
        refresh_header(&watch.terminal, ctx);
        Some(url)
    }

    /// Fetches the pull request, then schedules the next poll. Stops once the pane is gone.
    fn poll(&mut self, id: EntityId, ctx: &mut ModelContext<Self>) {
        let Some(watch) = self.watches.get(&id) else {
            return;
        };
        if watch.terminal.upgrade(ctx).is_none() {
            self.watches.remove(&id);
            return;
        }
        let pr = watch.request.details.pr.clone();
        let cwd = watch.request.repo_root.clone();
        let view = run_gh(ctx, cwd.clone(), pr_view_args(&pr, PR_VIEW_FIELDS));
        let comments = run_gh(ctx, cwd, review_comments_args(&pr));
        let handle = ctx.spawn(
            async move {
                let (view, comments) = futures::join!(view, comments);
                let comments = parse_review_comments(&comments?)?;
                Ok::<_, anyhow::Error>(PrSnapshot::parse(&view?, &comments)?)
            },
            move |me, result, ctx| {
                match result {
                    Ok(snapshot) => me.apply_snapshot(id, snapshot, ctx),
                    Err(err) => safe_warn!(
                        safe: ("PR agent: failed to poll the pull request"),
                        full: ("PR agent: failed to poll the pull request: {err:#}")
                    ),
                }
                me.schedule_poll(id, ctx);
            },
        );
        if let Some(watch) = self.watches.get_mut(&id) {
            watch.poll = Some(handle);
        }
    }

    fn schedule_poll(&mut self, id: EntityId, ctx: &mut ModelContext<Self>) {
        if !self.watches.contains_key(&id) {
            return;
        }
        let interval = PrAgentSettings::as_ref(ctx).poll_interval();
        let handle = ctx.spawn(
            async move {
                Timer::after(interval).await;
            },
            move |me, _, ctx| me.poll(id, ctx),
        );
        if let Some(watch) = self.watches.get_mut(&id) {
            watch.poll = Some(handle);
        }
    }

    fn apply_snapshot(&mut self, id: EntityId, snapshot: PrSnapshot, ctx: &mut ModelContext<Self>) {
        let auto_forward = *PrAgentSettings::as_ref(ctx).auto_forward_events;
        let Some(watch) = self.watches.get_mut(&id) else {
            return;
        };
        if watch.snapshot.as_ref() == Some(&snapshot) {
            return;
        }
        // The first snapshot is the baseline: the initial prompt already covers it.
        let events = watch
            .snapshot
            .as_ref()
            .map(|old| diff_snapshots(old, &snapshot, watch.request.viewer.as_deref()))
            .unwrap_or_default();
        watch.unread += events.len();
        if auto_forward {
            watch.pending.extend(events);
        }
        watch.snapshot = Some(snapshot);
        refresh_header(&watch.terminal, ctx);
        self.deliver_pending(id, ctx);
    }

    /// Sends the pending events to the agent as one message, if it is waiting for input.
    fn deliver_pending(&mut self, id: EntityId, ctx: &mut ModelContext<Self>) {
        let Some(watch) = self.watches.get_mut(&id) else {
            return;
        };
        if watch.pending.is_empty() {
            return;
        }
        let Some(terminal) = watch.terminal.upgrade(ctx) else {
            return;
        };
        let text = format_events(&watch.request.details.pr, &watch.pending);
        if terminal.update(ctx, |view, ctx| view.deliver_pr_update(text, ctx)) {
            watch.pending.clear();
        }
    }

    fn on_agent_status_changed(
        &mut self,
        id: EntityId,
        status: &CLIAgentSessionStatus,
        ctx: &mut ModelContext<Self>,
    ) {
        if !self.watches.contains_key(&id) {
            return;
        }
        self.deliver_pending(id, ctx);
        // The agent's turn ended (its `stop` event): pick up review comments it posted.
        if matches!(status, CLIAgentSessionStatus::Success) {
            self.mirror_review_comments(id, ctx);
        }
    }

    /// Shows review comments the `gh` user (the agent) posted on the pull request in the Code
    /// Review panel for the checkout.
    fn mirror_review_comments(&mut self, id: EntityId, ctx: &mut ModelContext<Self>) {
        let Some(watch) = self.watches.get_mut(&id) else {
            return;
        };
        let Some(viewer) = watch.request.viewer.clone() else {
            return;
        };
        if watch.is_mirroring {
            return;
        }
        watch.is_mirroring = true;
        let pr = watch.request.details.pr.clone();
        let fetch = run_gh(
            ctx,
            watch.request.repo_root.clone(),
            review_comments_args(&pr),
        );
        ctx.spawn(
            async move { parse_review_comments(&fetch.await?).map_err(anyhow::Error::from) },
            move |me, result, ctx| match result {
                Ok(comments) => me.apply_mirror(id, &viewer, &comments, ctx),
                Err(err) => {
                    safe_warn!(
                        safe: ("PR agent: failed to load review comments"),
                        full: ("PR agent: failed to load review comments: {err:#}")
                    );
                    if let Some(watch) = me.watches.get_mut(&id) {
                        watch.is_mirroring = false;
                    }
                }
            },
        );
    }

    fn apply_mirror(
        &mut self,
        id: EntityId,
        viewer: &str,
        comments: &[ReviewComment],
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(watch) = self.watches.get_mut(&id) else {
            return;
        };
        watch.is_mirroring = false;
        let new = comments_to_mirror(comments, viewer, &watch.mirrored);
        let Some(terminal) = watch.terminal.upgrade(ctx) else {
            return;
        };
        if new.is_empty() {
            return;
        }
        watch.mirrored.extend(new.iter().map(|comment| comment.id));
        let comments: Vec<_> = new
            .into_iter()
            .map(ReviewComment::to_insert_review_comment)
            .collect();
        let checkout_path = watch.request.checkout_path.clone();
        let base = watch.request.details.base.clone();
        terminal.update(ctx, |view, ctx| {
            view.show_pr_review_comments(&checkout_path, &comments, &base, ctx);
        });
    }

    /// The watch for a pane, when the feature is enabled. Tolerates the model not being
    /// registered (minimal test harnesses render terminal views without it).
    fn watch_for(terminal_view_id: EntityId, app: &AppContext) -> Option<&Watch> {
        if !FeatureFlag::PrReviewAgent.is_enabled() || !app.has_singleton_model::<Self>() {
            return None;
        }
        Self::as_ref(app).watches.get(&terminal_view_id)
    }

    /// Whether the pane runs a PR agent, so its header shows the status chips.
    pub(crate) fn is_watching(terminal_view_id: EntityId, app: &AppContext) -> bool {
        Self::watch_for(terminal_view_id, app).is_some()
    }

    /// Pane-header chips: checks, review decision and unread updates. Clicking them opens the
    /// pull request and clears the unread count.
    pub(crate) fn render_header_chips(
        terminal_view_id: EntityId,
        app: &AppContext,
    ) -> Option<Box<dyn Element>> {
        let watch = Self::watch_for(terminal_view_id, app)?;
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let neutral = theme.sub_text_color(theme.background()).into_solid();
        let good = theme.ansi_fg_green();
        let bad = theme.ansi_fg_red();

        let mut labels = Vec::new();
        if let Some(snapshot) = &watch.snapshot {
            match snapshot.checks.state() {
                ChecksState::NoChecks => {}
                ChecksState::Pending => labels.push(("Checks pending".to_string(), neutral)),
                ChecksState::Passing => labels.push(("Checks passing".to_string(), good)),
                ChecksState::Failing => {
                    labels.push((format!("{} failing", snapshot.checks.failed.len()), bad))
                }
            }
            match snapshot.review_decision.as_deref() {
                Some("APPROVED") => labels.push(("Approved".to_string(), good)),
                Some("CHANGES_REQUESTED") => labels.push(("Changes requested".to_string(), bad)),
                Some("REVIEW_REQUIRED") => labels.push(("Review required".to_string(), neutral)),
                _ => {}
            }
        }
        if watch.unread > 0 {
            labels.push((format!("{} new", watch.unread), theme.accent().into_solid()));
        }
        if labels.is_empty() {
            return None;
        }

        let border_color = internal_colors::neutral_4(theme);
        let font_family = appearance.ui_font_family();
        let font_size = appearance.ui_font_size() - 1.;
        Some(
            Hoverable::new(watch.chip_mouse_state.clone(), move |_| {
                let mut row = Flex::row().with_cross_axis_alignment(CrossAxisAlignment::Center);
                for (label, color) in &labels {
                    let style = UiComponentStyles {
                        border_color: Some(border_color.into()),
                        border_width: Some(1.),
                        border_radius: Some(CornerRadius::with_all(Radius::Pixels(4.))),
                        font_family_id: Some(font_family),
                        font_size: Some(font_size),
                        font_color: Some(*color),
                        ..Default::default()
                    };
                    row.add_child(
                        Container::new(Chip::new(label.clone(), style).build().finish())
                            .with_margin_left(4.)
                            .finish(),
                    );
                }
                row.finish()
            })
            .with_cursor(Cursor::PointingHand)
            .on_click(move |ctx, _, _| {
                ctx.dispatch_typed_action(WorkspaceAction::OpenPrAgentPullRequest(
                    terminal_view_id,
                ));
            })
            .finish(),
        )
    }
}

/// Re-renders the pane header so the chips reflect new state.
fn refresh_header(terminal: &WeakViewHandle<TerminalView>, ctx: &mut ModelContext<PrAgentModel>) {
    let Some(terminal) = terminal.upgrade(ctx) else {
        return;
    };
    let pane_configuration = terminal.as_ref(ctx).pane_configuration().clone();
    pane_configuration.update(ctx, |pane_configuration, ctx| {
        pane_configuration.notify_header_content_changed(ctx);
    });
}

impl Entity for PrAgentModel {
    type Event = ();
}

impl SingletonEntity for PrAgentModel {}

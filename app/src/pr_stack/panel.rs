//! The PR stack left-panel section: one row per branch of the stack under the
//! focused repo, with PR status, stack-relative stats, and the actions that
//! create PRs and restack.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, anyhow};
use dunce::canonicalize;
use pathfinder_color::ColorU;
use pathfinder_geometry::vector::Vector2F;
use repo_metadata::repositories::{DetectedRepositories, RepoDetectionSource};
use repo_metadata::repository::{RepositorySubscriber, SubscriberId};
use repo_metadata::{Repository, RepositoryUpdate, RepositoryWatchMode};
use settings::Setting as _;
use warp_core::safe_warn;
use warp_core::ui::theme::color::internal_colors;
use warp_util::git::run_git_command_with_env;
use warpui::r#async::{SpawnedFutureHandle, Timer};
use warpui::clipboard::ClipboardContent;
use warpui::elements::{
    ChildAnchor, ChildView, ClippedScrollStateHandle, ClippedScrollable, Container, CornerRadius,
    CrossAxisAlignment, Element, Fill, Flex, Hoverable, MainAxisAlignment, MainAxisSize,
    MouseStateHandle, OffsetPositioning, ParentAnchor, ParentElement, ParentOffsetBounds, Radius,
    SavePosition, ScrollbarWidth, Shrinkable, Stack as StackElement, Text,
};
use warpui::fonts::{Properties, Weight};
use warpui::platform::Cursor;
use warpui::ui_components::components::UiComponent as _;
use warpui::{
    AppContext, Entity, EntityId, ModelHandle, SingletonEntity, TypedActionView, View, ViewContext,
    ViewHandle, WeakViewHandle, WindowId,
};

use super::restack::{self, RestackOutcome, conflict_handoff_message};
use super::settings::{PrStackRowOrder, PrStackSettings};
use super::stack::{
    Stack, StackBranch, StackFile, git_common_dir, infer_stack, load_stack_file, save_stack_file,
};
use super::stats::{BranchStats, ClassificationRule, Classifier, branch_stats};
use super::status::{ChecksState, PrStatus, base_name, fetch_prs};
use super::submit;
use crate::appearance::Appearance;
use crate::features::FeatureFlag;
use crate::menu::{Event as MenuEvent, Menu, MenuItem, MenuItemFields};
use crate::pane_group::{PaneGroup, WorkingDirectoriesEvent, WorkingDirectoriesModel};
use crate::task_agent::settings::TaskAgentSettings;
use crate::task_agent::{TaskSession, TaskSessionsModel, short_title};
use crate::terminal::cli_agent_sessions::{
    CLIAgentSessionStatus, CLIAgentSessionsModel, CLIAgentSessionsModelEvent,
};
use crate::terminal::view::{CliAgentRouting, TerminalView};
use crate::throttle::throttle;
use crate::ui_components::buttons::icon_button;
use crate::ui_components::icons::Icon;
use crate::util::git::{PrInfo, detect_current_branch, detect_main_branch, is_gh_auth_error};
use crate::view_components::{DismissibleToast, ToastLink};
use crate::workspace::ToastStack;

/// How long to wait for the agent to write the PR body file.
const PREPARE_TIMEOUT: Duration = Duration::from_secs(300);

/// Minimum gap between refreshes triggered by ref changes.
const REPO_CHANGE_THROTTLE: Duration = Duration::from_secs(3);

/// Stats cache keyed by (parent tip, branch tip).
type StatsCache = HashMap<(String, String), BranchStats>;

pub enum PrStackPanelEvent {
    /// Show `repo`'s changes against `base` in the code review panel.
    OpenDiff { repo: PathBuf, base: String },
}

#[derive(Clone, Debug, PartialEq)]
pub enum PrStackAction {
    Refresh,
    OpenDiff(String),
    OpenMenu {
        branch: String,
        position: Vector2F,
    },
    CreatePr(String),
    OpenPr(String),
    Sync(String),
    RestackFrom(String),
    /// Pin `child`'s parent, or clear its pin with `None`.
    Pin {
        child: String,
        parent: Option<String>,
    },
    CopyBranch(String),
}

/// Everything shown for one repo, loaded off the main thread.
struct Loaded {
    repo: PathBuf,
    top: String,
    stack: Stack,
    stats: HashMap<String, BranchStats>,
    prs: HashMap<String, PrStatus>,
    /// Why PR status is missing, if it is.
    pr_error: Option<String>,
}

enum SubmitOutcome {
    Created(PrInfo),
    /// The pre-submit sync stopped on a conflict.
    Restack(RestackOutcome),
}

struct PendingPrepare {
    branch: String,
    terminal_id: EntityId,
    timeout: SpawnedFutureHandle,
}

/// Forwards ref changes from the repository watcher.
struct RefChangeSubscriber {
    tx: async_channel::Sender<()>,
}

impl RepositorySubscriber for RefChangeSubscriber {
    fn on_scan(
        &mut self,
        _repository: &Repository,
        _ctx: &mut warpui::ModelContext<Repository>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>> {
        Box::pin(async {})
    }

    fn on_files_updated(
        &mut self,
        _repository: &Repository,
        update: &RepositoryUpdate,
        _ctx: &mut warpui::ModelContext<Repository>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>> {
        let tx = self.tx.clone();
        let changed = update.commit_updated || update.remote_ref_updated;
        Box::pin(async move {
            if changed {
                let _ = tx.send(()).await;
            }
        })
    }
}

pub struct PrStackPanel {
    pane_group: Option<WeakViewHandle<PaneGroup>>,
    repo: Option<PathBuf>,
    /// Whether the section is on screen; loads and polls only run then.
    active: bool,
    /// A refresh owed while inactive, busy, or loading; `true` when it must
    /// include remote status.
    queued: Option<bool>,
    loaded: Option<Loaded>,
    load_error: Option<String>,
    /// The running operation, which blocks other operations and refreshes.
    busy: Option<&'static str>,
    /// The last operation error.
    message: Option<String>,
    conflicts: HashSet<String>,
    stats_cache: StatsCache,
    pending_prepare: Option<PendingPrepare>,
    /// `target_tip:bottom_tip` of the last automatic restack, so a failing one
    /// is not retried until something moves.
    last_auto_restack: Option<String>,
    watched: Option<(ModelHandle<Repository>, SubscriberId)>,
    poll: Option<SpawnedFutureHandle>,
    load_handle: Option<SpawnedFutureHandle>,
    menu: ViewHandle<Menu<PrStackAction>>,
    menu_position: Option<Vector2F>,
    row_states: HashMap<String, MouseStateHandle>,
    refresh_state: MouseStateHandle,
    scroll_state: ClippedScrollStateHandle,
    view_id: EntityId,
    window_id: WindowId,
}

impl PrStackPanel {
    pub fn new(
        working_directories_model: &ModelHandle<WorkingDirectoriesModel>,
        ctx: &mut ViewContext<Self>,
    ) -> Self {
        let menu = ctx.add_typed_action_view(|_| {
            Menu::new()
                .prevent_interaction_with_other_elements()
                .with_width(240.)
        });
        ctx.subscribe_to_view(&menu, |me, _, event, ctx| {
            if let MenuEvent::Close { .. } = event {
                me.menu_position = None;
                ctx.notify();
            }
        });
        ctx.subscribe_to_model(working_directories_model, |me, _, event, ctx| {
            if let WorkingDirectoriesEvent::FocusedRepoChanged {
                pane_group_id,
                focused_repo,
                ..
            } = event
                && me.pane_group_id(ctx) == Some(*pane_group_id)
            {
                let repo = focused_repo
                    .as_ref()
                    .and_then(|repo| repo.to_local_path().map(Path::to_path_buf));
                me.set_repo(repo, ctx);
            }
        });
        ctx.subscribe_to_model(&CLIAgentSessionsModel::handle(ctx), |me, _, event, ctx| {
            me.handle_agent_event(event, ctx);
        });
        ctx.subscribe_to_model(&PrStackSettings::handle(ctx), |me, _, _, ctx| {
            me.stats_cache.clear();
            if let Some(poll) = me.poll.take() {
                poll.abort();
            }
            me.refresh(false, ctx);
        });

        Self {
            pane_group: None,
            repo: None,
            active: false,
            queued: None,
            loaded: None,
            load_error: None,
            busy: None,
            message: None,
            conflicts: HashSet::new(),
            stats_cache: HashMap::new(),
            pending_prepare: None,
            last_auto_restack: None,
            watched: None,
            poll: None,
            load_handle: None,
            menu,
            menu_position: None,
            row_states: HashMap::new(),
            refresh_state: Default::default(),
            scroll_state: Default::default(),
            view_id: ctx.view_id(),
            window_id: ctx.window_id(),
        }
    }

    fn pane_group_id(&self, app: &AppContext) -> Option<EntityId> {
        self.pane_group
            .as_ref()
            .and_then(|pane_group| pane_group.upgrade(app))
            .map(|pane_group| pane_group.id())
    }

    /// Follows the active tab: the stack shown is the one under its focused
    /// terminal's repo.
    pub fn set_pane_group(
        &mut self,
        pane_group: &ViewHandle<PaneGroup>,
        ctx: &mut ViewContext<Self>,
    ) {
        self.pane_group = Some(pane_group.downgrade());
        let repo = pane_group
            .as_ref(ctx)
            .focused_session_view(ctx)
            .and_then(|terminal| {
                terminal
                    .as_ref(ctx)
                    .current_local_repo_path()
                    .map(Path::to_path_buf)
            });
        self.set_repo(repo, ctx);
    }

    /// Called when the section is shown or hidden.
    pub fn set_active(&mut self, active: bool, ctx: &mut ViewContext<Self>) {
        if self.active == active {
            return;
        }
        self.active = active;
        if active {
            self.refresh(true, ctx);
        } else if let Some(poll) = self.poll.take() {
            poll.abort();
        }
    }

    fn set_repo(&mut self, repo: Option<PathBuf>, ctx: &mut ViewContext<Self>) {
        if self.repo == repo {
            return;
        }
        self.repo = repo;
        self.loaded = None;
        self.load_error = None;
        self.message = None;
        self.conflicts.clear();
        self.last_auto_restack = None;
        if let Some(poll) = self.poll.take() {
            poll.abort();
        }
        if let Some(load) = self.load_handle.take() {
            load.abort();
        }
        self.queued = None;
        self.watch_repo(ctx);
        self.refresh(false, ctx);
        ctx.notify();
    }

    fn watch_repo(&mut self, ctx: &mut ViewContext<Self>) {
        if let Some((repository, subscriber_id)) = self.watched.take() {
            repository.update(ctx, |repository, ctx| {
                repository.stop_watching(subscriber_id, ctx)
            });
        }
        let Some(repo) = self.repo.clone() else {
            return;
        };
        let detection = DetectedRepositories::handle(ctx).update(ctx, |model, ctx| {
            model.detect_possible_local_git_repo(
                &repo.display().to_string(),
                RepoDetectionSource::CodeReviewInitialization,
                ctx,
            )
        });
        ctx.spawn(detection, move |me, root, ctx| {
            if me.repo.as_ref() != Some(&repo) || me.watched.is_some() {
                return;
            }
            let Some(repository) = root.and_then(|root| {
                DetectedRepositories::as_ref(ctx).get_local_watched_repo_for_path(&root, ctx)
            }) else {
                return;
            };
            let (tx, rx) = async_channel::unbounded();
            let start = repository.update(ctx, |repository, ctx| {
                repository.start_watching(
                    RepositoryWatchMode::GitRepository,
                    Box::new(RefChangeSubscriber { tx }),
                    ctx,
                )
            });
            me.watched = Some((repository, start.subscriber_id));
            ctx.spawn(start.registration_future, |_, result, _| {
                if let Err(err) = result {
                    log::warn!("PR stack: could not watch repository: {err}");
                }
            });
            // The stream ends when the subscriber is dropped by stop_watching.
            ctx.spawn_stream_local(
                throttle(REPO_CHANGE_THROTTLE, rx),
                |me, _, ctx| me.refresh(false, ctx),
                |_, _| {},
            );
        });
    }

    /// Reloads the stack. `remote` also fetches origin and PR status; without
    /// it, PR status from the last load is reused. While a load is running
    /// the request is queued rather than aborting it, so ref changes made by
    /// the fetch cannot cancel the remote load that caused them.
    fn refresh(&mut self, remote: bool, ctx: &mut ViewContext<Self>) {
        if !self.active || self.busy.is_some() || self.load_handle.is_some() {
            self.queued = Some(remote || self.queued == Some(true));
            return;
        }
        let Some(repo) = self.repo.clone() else {
            return;
        };
        self.queued = None;
        let settings = PrStackSettings::as_ref(ctx);
        let targets = settings.targets.value().clone();
        let rules = settings.classification.value().clone();
        let prs = if remote {
            None
        } else {
            self.loaded
                .as_ref()
                .filter(|loaded| loaded.repo == repo)
                .map(|loaded| (loaded.prs.clone(), loaded.pr_error.clone()))
        };
        let cache = self.stats_cache.clone();
        let path_env = interactive_path(ctx);
        self.load_handle = Some(ctx.spawn(
            async move { load(repo, targets, rules, cache, prs, remote, path_env.await).await },
            move |me, result, ctx| me.on_loaded(result, remote, ctx),
        ));
        ctx.notify();
    }

    fn on_loaded(
        &mut self,
        result: Result<(Loaded, StatsCache)>,
        remote: bool,
        ctx: &mut ViewContext<Self>,
    ) {
        self.load_handle = None;
        match result {
            Ok((loaded, cache)) if self.repo.as_ref() == Some(&loaded.repo) => {
                self.stats_cache = cache;
                self.row_states
                    .retain(|branch, _| loaded.stack.index_of(branch).is_some());
                for branch in &loaded.stack.branches {
                    self.row_states.entry(branch.name.clone()).or_default();
                }
                self.load_error = None;
                self.loaded = Some(loaded);
                if remote {
                    self.maybe_auto_restack(ctx);
                }
            }
            Ok(_) => {}
            Err(err) => {
                self.loaded = None;
                self.load_error = Some(format!("{err:#}"));
            }
        }
        if let Some(remote) = self.queued.take() {
            self.refresh(remote, ctx);
        }
        self.schedule_poll(ctx);
        ctx.notify();
    }

    fn schedule_poll(&mut self, ctx: &mut ViewContext<Self>) {
        if self.poll.is_some() || !self.active || self.repo.is_none() {
            return;
        }
        let secs = *PrStackSettings::as_ref(ctx).poll_interval_secs.value();
        if secs == 0 {
            return;
        }
        self.poll = Some(
            ctx.spawn(Timer::after(Duration::from_secs(secs)), |me, _, ctx| {
                me.poll = None;
                me.refresh(true, ctx);
            }),
        );
    }

    /// Restacks after a poll finds the bottom PR merged or the target ahead.
    // ponytail: runs only while the section is open, since that is when it
    // polls; a background poller would cover closed panels.
    fn maybe_auto_restack(&mut self, ctx: &mut ViewContext<Self>) {
        if !*PrStackSettings::as_ref(ctx).auto_restack.value()
            || self.busy.is_some()
            || !self.conflicts.is_empty()
        {
            return;
        }
        let Some(loaded) = &self.loaded else {
            return;
        };
        let Some(bottom) = loaded.stack.branches.first() else {
            return;
        };
        let merged = loaded
            .prs
            .get(&bottom.name)
            .is_some_and(PrStatus::is_merged);
        if !merged && !bottom.behind {
            return;
        }
        let key = format!("{}:{}", loaded.stack.target_tip, bottom.tip);
        if self.last_auto_restack.as_ref() == Some(&key) {
            return;
        }
        self.last_auto_restack = Some(key);
        let bottom = bottom.name.clone();
        self.run_restack(bottom, false, ctx);
    }

    fn run_restack(&mut self, branch: String, only: bool, ctx: &mut ViewContext<Self>) {
        let Some(loaded) = &self.loaded else {
            return;
        };
        let Some(from) = loaded.stack.index_of(&branch) else {
            return;
        };
        let repo = loaded.repo.clone();
        let stack = loaded.stack.clone();
        let prs = loaded.prs.clone();
        self.busy = Some(if only { "Syncing…" } else { "Restacking…" });
        self.message = None;
        self.conflicts.clear();
        let path_env = interactive_path(ctx);
        ctx.spawn(
            async move {
                let path_env = path_env.await;
                restack::restack(&repo, &stack, from, only, &prs, path_env.as_deref()).await
            },
            |me, result, ctx| {
                me.busy = None;
                me.handle_restack_result(result, ctx);
                me.refresh(true, ctx);
            },
        );
        ctx.notify();
    }

    fn handle_restack_result(
        &mut self,
        result: Result<RestackOutcome>,
        ctx: &mut ViewContext<Self>,
    ) {
        match result {
            Ok(RestackOutcome::Done { retargeted }) => {
                if !retargeted.is_empty() {
                    self.toast(
                        format!("Retargeted {} to the stack target.", retargeted.join(", ")),
                        ctx,
                    );
                }
            }
            Ok(RestackOutcome::Conflict {
                branch,
                onto,
                worktree,
                remaining,
            }) => {
                self.conflicts.insert(branch.clone());
                let text = conflict_handoff_message(&branch, &onto, &worktree, &remaining);
                let message = if self
                    .send_to_agent(&worktree, Some(&branch), text.clone(), false, ctx)
                    .is_some()
                {
                    format!("Rebase conflict on {branch}; handed off to the agent pane.")
                } else {
                    ctx.clipboard().write(ClipboardContent::plain_text(text));
                    format!(
                        "Rebase conflict on {branch}. No agent pane found; the hand-off was copied to the clipboard."
                    )
                };
                self.toast(message, ctx);
            }
            Err(err) => {
                safe_warn!(
                    safe: ("PR stack: restack failed"),
                    full: ("PR stack: restack failed: {err:#}")
                );
                self.message = Some(format!("{err:#}"));
            }
        }
    }

    /// Creates the PR for `branch`: body file, else the agent's prepare
    /// command (when `allow_prepare`), else the fallback template.
    fn create_pr(&mut self, branch: String, allow_prepare: bool, ctx: &mut ViewContext<Self>) {
        let Some(loaded) = &self.loaded else {
            return;
        };
        let repo = loaded.repo.clone();
        let template = PrStackSettings::as_ref(ctx)
            .pr_body_file_template
            .value()
            .clone();
        self.busy = Some("Creating PR…");
        self.message = None;
        let lookup_branch = branch.clone();
        ctx.spawn(
            async move { read_body_file(&repo, &template, &lookup_branch).await },
            move |me, (title_body, worktree), ctx| {
                me.create_pr_with(branch, title_body, worktree, allow_prepare, ctx);
            },
        );
        ctx.notify();
    }

    fn create_pr_with(
        &mut self,
        branch: String,
        title_body: Option<(String, String)>,
        worktree: Option<PathBuf>,
        allow_prepare: bool,
        ctx: &mut ViewContext<Self>,
    ) {
        let command = PrStackSettings::as_ref(ctx)
            .pr_prepare_command
            .value()
            .trim()
            .replace("{{branch}}", &branch);
        if title_body.is_none() && allow_prepare && !command.is_empty() {
            let dir = worktree.or_else(|| self.repo.clone()).unwrap_or_default();
            if let Some(terminal_id) = self.send_to_agent(&dir, Some(&branch), command, true, ctx) {
                let timeout = ctx.spawn(Timer::after(PREPARE_TIMEOUT), |me, _, ctx| {
                    me.finish_prepare(ctx);
                });
                self.pending_prepare = Some(PendingPrepare {
                    branch,
                    terminal_id,
                    timeout,
                });
                self.busy = Some("Waiting for the agent to prepare the PR…");
                ctx.notify();
                return;
            }
        }

        let Some(loaded) = &self.loaded else {
            self.busy = None;
            return;
        };
        let repo = loaded.repo.clone();
        let stack = loaded.stack.clone();
        let prs = loaded.prs.clone();
        let task_title = self.task_title(&branch, ctx);
        let path_env = interactive_path(ctx);
        ctx.spawn(
            async move {
                let path_env = path_env.await;
                submit_branch(
                    &repo,
                    &stack,
                    &prs,
                    &branch,
                    title_body,
                    task_title,
                    path_env.as_deref(),
                )
                .await
            },
            |me, result, ctx| {
                me.busy = None;
                match result {
                    Ok(SubmitOutcome::Created(pr)) => {
                        let window_id = ctx.window_id();
                        ToastStack::handle(ctx).update(ctx, |toasts, ctx| {
                            let link = ToastLink::new("Open PR".to_string()).with_href(pr.url);
                            toasts.add_ephemeral_toast(
                                DismissibleToast::default(format!("Created PR #{}.", pr.number))
                                    .with_link(link),
                                window_id,
                                ctx,
                            );
                        });
                    }
                    Ok(SubmitOutcome::Restack(outcome)) => {
                        me.handle_restack_result(Ok(outcome), ctx)
                    }
                    Err(err) => {
                        safe_warn!(
                            safe: ("PR stack: creating a PR failed"),
                            full: ("PR stack: creating a PR failed: {err:#}")
                        );
                        me.message = Some(format!("{err:#}"));
                    }
                }
                me.refresh(true, ctx);
            },
        );
        ctx.notify();
    }

    fn finish_prepare(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(pending) = self.pending_prepare.take() else {
            return;
        };
        pending.timeout.abort();
        self.busy = None;
        self.create_pr(pending.branch, false, ctx);
    }

    fn handle_agent_event(
        &mut self,
        event: &CLIAgentSessionsModelEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        let finished = match event {
            CLIAgentSessionsModelEvent::StatusChanged {
                terminal_view_id,
                status: CLIAgentSessionStatus::Success | CLIAgentSessionStatus::Failed { .. },
                ..
            }
            | CLIAgentSessionsModelEvent::Ended {
                terminal_view_id, ..
            } => Some(*terminal_view_id),
            _ => None,
        };
        if finished.is_some() && self.pending_prepare.as_ref().map(|p| p.terminal_id) == finished {
            self.finish_prepare(ctx);
        }
    }

    /// Terminal panes in this window that the task launcher started on a
    /// branch, with their task.
    fn task_terminals(&self, ctx: &AppContext) -> Vec<(ViewHandle<TerminalView>, TaskSession)> {
        if !FeatureFlag::TaskAgentLauncher.is_enabled()
            || !ctx.has_singleton_model::<TaskSessionsModel>()
        {
            return Vec::new();
        }
        let tasks = TaskSessionsModel::as_ref(ctx);
        ctx.views_of_type::<TerminalView>(self.window_id)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|terminal| {
                let task = tasks.get(terminal.id())?.clone();
                task.branch.is_some().then_some((terminal, task))
            })
            .collect()
    }

    /// `KEY short title` of the task the launcher started on `branch`.
    fn task_title(&self, branch: &str, ctx: &AppContext) -> Option<String> {
        let (_, task) = self
            .task_terminals(ctx)
            .into_iter()
            .find(|(_, task)| task.branch.as_deref() == Some(branch))?;
        let max_chars = *TaskAgentSettings::as_ref(ctx).short_title_max_chars;
        let short = short_title(&task.title, max_chars);
        Some(match task.key {
            Some(key) => format!("{key} {short}"),
            None => short,
        })
    }

    /// Picks the CLI agent pane for `branch`: the pane the task launcher tied
    /// to it, else (for panes started by hand) the focused agent pane inside
    /// `dir`, any agent pane inside `dir`, or any agent pane inside the repo
    /// in the active tab.
    fn agent_terminal(
        &self,
        dir: &Path,
        branch: Option<&str>,
        ctx: &AppContext,
    ) -> Option<ViewHandle<TerminalView>> {
        let sessions = CLIAgentSessionsModel::as_ref(ctx);
        let has_agent =
            |terminal: &ViewHandle<TerminalView>| sessions.session(terminal.id()).is_some();
        if let Some(branch) = branch
            && let Some((terminal, _)) =
                self.task_terminals(ctx)
                    .into_iter()
                    .find(|(terminal, task)| {
                        task.branch.as_deref() == Some(branch) && has_agent(terminal)
                    })
        {
            return Some(terminal);
        }

        let pane_group = self.pane_group.as_ref()?.upgrade(ctx)?;
        let pane_group = pane_group.as_ref(ctx);
        let agents: Vec<ViewHandle<TerminalView>> = pane_group
            .terminal_views(ctx)
            .into_iter()
            .filter(has_agent)
            .collect();
        let inside = |terminal: &ViewHandle<TerminalView>, dir: &Path| {
            terminal
                .as_ref(ctx)
                .active_session_path_if_local(ctx)
                .is_some_and(|cwd| canonicalize(&cwd).unwrap_or(cwd).starts_with(dir))
        };
        let dir = canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        let repo = self
            .repo
            .as_ref()
            .map(|repo| canonicalize(repo).unwrap_or_else(|_| repo.clone()));
        pane_group
            .focused_session_view(ctx)
            .filter(|focused| {
                agents.iter().any(|a| a.id() == focused.id()) && inside(focused, &dir)
            })
            .or_else(|| agents.iter().find(|a| inside(a, &dir)).cloned())
            .or_else(|| repo.and_then(|repo| agents.iter().find(|a| inside(a, &repo)).cloned()))
    }

    /// Sends `text` to the agent pane for `branch` / `dir` and returns that pane. With
    /// `submit`, text written straight to the agent is also submitted; text
    /// that lands in the rich input waits for the user.
    fn send_to_agent(
        &self,
        dir: &Path,
        branch: Option<&str>,
        text: String,
        submit: bool,
        ctx: &mut ViewContext<Self>,
    ) -> Option<EntityId> {
        let terminal = self.agent_terminal(dir, branch, ctx)?;
        let routing = terminal.update(ctx, |terminal, ctx| {
            let routing = terminal.try_send_text_to_cli_agent_or_rich_input(text, ctx);
            if submit && matches!(routing, Some(CliAgentRouting::Pty)) {
                terminal.write_to_pty(b"\r".to_vec(), ctx);
            }
            routing
        });
        routing.map(|_| terminal.id())
    }

    fn open_diff(&mut self, branch: String, ctx: &mut ViewContext<Self>) {
        let Some(loaded) = &self.loaded else {
            return;
        };
        let Some(index) = loaded.stack.index_of(&branch) else {
            return;
        };
        let base = loaded.stack.branches[index].parent.clone();
        let repo = loaded.repo.clone();
        ctx.spawn(
            async move { restack::worktrees(&repo).await },
            move |me, worktrees, ctx| match worktrees.ok().and_then(|w| w.get(&branch).cloned()) {
                Some(repo) => ctx.emit(PrStackPanelEvent::OpenDiff { repo, base }),
                // ponytail: the code review panel diffs a working tree, so a
                // branch has to be checked out somewhere; a ref-range diff
                // pane would lift this.
                None => me.toast(
                    format!("Check out {branch} (in any worktree) to view its stack diff."),
                    ctx,
                ),
            },
        );
    }

    fn pin(&mut self, child: String, parent: Option<String>, ctx: &mut ViewContext<Self>) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        ctx.spawn(
            async move {
                let mut file = load_stack_file(&repo).await?;
                match parent {
                    Some(parent) => file.pins.insert(child, parent),
                    None => file.pins.remove(&child),
                };
                save_stack_file(&repo, &file).await
            },
            |me, result, ctx| {
                if let Err(err) = result {
                    me.message = Some(format!("{err:#}"));
                }
                me.refresh(false, ctx);
            },
        );
    }

    fn open_menu(&mut self, branch: String, position: Vector2F, ctx: &mut ViewContext<Self>) {
        let Some(loaded) = &self.loaded else {
            return;
        };
        let Some(index) = loaded.stack.index_of(&branch) else {
            return;
        };
        let row = &loaded.stack.branches[index];
        let pr = loaded.prs.get(&branch);
        let busy = self.busy.is_some();
        let item = |label: String, action: PrStackAction, disabled: bool| {
            MenuItemFields::new(label)
                .with_on_select_action(action)
                .with_disabled(disabled)
                .into_item()
        };

        let mut items = Vec::new();
        if pr.is_none_or(|pr| !pr.is_open() && !pr.is_merged()) {
            items.push(item(
                "Create PR".into(),
                PrStackAction::CreatePr(branch.clone()),
                busy,
            ));
        }
        if let Some(pr) = pr {
            items.push(item(
                format!("Open PR #{}", pr.number),
                PrStackAction::OpenPr(branch.clone()),
                false,
            ));
        }
        items.push(item(
            "Sync".into(),
            PrStackAction::Sync(branch.clone()),
            busy,
        ));
        items.push(item(
            "Restack from here".into(),
            PrStackAction::RestackFrom(branch.clone()),
            busy,
        ));
        let pins: Vec<MenuItem<PrStackAction>> = loaded.stack.branches[index + 1..]
            .iter()
            .map(|above| {
                item(
                    format!("Pin as parent of {}", above.name),
                    PrStackAction::Pin {
                        child: above.name.clone(),
                        parent: Some(branch.clone()),
                    },
                    busy,
                )
            })
            .chain(row.alternatives.iter().map(|alternative| {
                item(
                    format!("Use {alternative} as parent"),
                    PrStackAction::Pin {
                        child: branch.clone(),
                        parent: Some(alternative.clone()),
                    },
                    busy,
                )
            }))
            .chain(row.pinned.then(|| {
                item(
                    "Clear pin".into(),
                    PrStackAction::Pin {
                        child: branch.clone(),
                        parent: None,
                    },
                    busy,
                )
            }))
            .collect();
        if !pins.is_empty() {
            items.push(MenuItem::Separator);
            items.extend(pins);
        }
        items.push(MenuItem::Separator);
        items.push(item(
            "Copy branch name".into(),
            PrStackAction::CopyBranch(branch),
            false,
        ));

        self.menu
            .update(ctx, |menu, ctx| menu.set_items(items, ctx));
        self.menu_position = Some(position);
        ctx.notify();
    }

    fn toast(&self, message: String, ctx: &mut ViewContext<Self>) {
        let window_id = ctx.window_id();
        ToastStack::handle(ctx).update(ctx, |toasts, ctx| {
            toasts.add_ephemeral_toast(DismissibleToast::default(message), window_id, ctx);
        });
    }

    fn position_id(&self) -> String {
        format!("pr_stack_panel_{}", self.view_id)
    }
}

/// Future resolving to the interactive shell `PATH`, so `gh` and git hooks
/// resolve as in a terminal.
fn interactive_path(
    ctx: &mut ViewContext<PrStackPanel>,
) -> futures::future::BoxFuture<'static, Option<String>> {
    #[cfg(feature = "local_tty")]
    {
        crate::terminal::local_shell::LocalShellState::handle(ctx)
            .update(ctx, |shell, ctx| shell.get_interactive_path_env_var(ctx))
    }
    #[cfg(not(feature = "local_tty"))]
    {
        use futures::FutureExt;
        let _ = ctx;
        futures::future::ready(None).boxed()
    }
}

async fn load(
    repo: PathBuf,
    targets: HashMap<String, String>,
    rules: Vec<ClassificationRule>,
    mut cache: StatsCache,
    prs: Option<(HashMap<String, PrStatus>, Option<String>)>,
    remote: bool,
    path_env: Option<String>,
) -> Result<(Loaded, StatsCache)> {
    let path_env = path_env.as_deref();
    if remote
        && let Err(err) =
            run_git_command_with_env(&repo, &["fetch", "origin", "--quiet"], path_env).await
    {
        log::warn!("PR stack: fetch failed, using local refs: {err:#}");
    }
    let top = detect_current_branch(&repo).await?;
    if top == "HEAD" {
        return Err(anyhow!(
            "HEAD is detached. Check out a branch to see its stack."
        ));
    }
    let (mut file, file_readable) = match load_stack_file(&repo).await {
        Ok(file) => (file, true),
        Err(err) => {
            log::warn!("PR stack: ignoring unreadable stack file: {err:#}");
            (StackFile::default(), false)
        }
    };
    let common_dir = git_common_dir(&repo).await?;
    let root = common_dir.parent().unwrap_or(&repo).display().to_string();
    let target = match file.target.clone().or_else(|| targets.get(&root).cloned()) {
        Some(target) => target,
        None => detect_main_branch(&repo).await?.trim().to_string(),
    };
    let stack = infer_stack(&repo, &top, &target, &file).await?;
    if file_readable && file.record(&stack) {
        save_stack_file(&repo, &file).await?;
    }

    let classifier = Classifier::new(&rules);
    let mut stats = HashMap::new();
    for branch in &stack.branches {
        let parent_tip = stack
            .branches
            .iter()
            .find(|b| b.name == branch.parent)
            .map_or(stack.target_tip.clone(), |b| b.tip.clone());
        let key = (parent_tip, branch.tip.clone());
        let value = match cache.get(&key) {
            Some(value) => value.clone(),
            None => {
                let value = branch_stats(&repo, &branch.parent, &branch.name, &classifier).await?;
                cache.insert(key, value.clone());
                value
            }
        };
        stats.insert(branch.name.clone(), value);
    }

    let (prs, pr_error) = match prs {
        Some(prs) => prs,
        None => match fetch_prs(&repo, path_env).await {
            Ok(prs) => (prs, None),
            Err(err) => {
                safe_warn!(
                    safe: ("PR stack: could not load PR status"),
                    full: ("PR stack: could not load PR status: {err:#}")
                );
                let message = if is_gh_auth_error(&err.to_string()) {
                    "Run `gh auth login` to see PR status."
                } else {
                    "PR status unavailable (needs the GitHub CLI and a GitHub remote)."
                };
                (HashMap::new(), Some(message.to_string()))
            }
        },
    };
    Ok((
        Loaded {
            repo,
            top,
            stack,
            stats,
            prs,
            pr_error,
        },
        cache,
    ))
}

/// Reads the PR body file for `branch`; also returns the branch's worktree.
async fn read_body_file(
    repo: &Path,
    template: &str,
    branch: &str,
) -> (Option<(String, String)>, Option<PathBuf>) {
    let worktree = restack::worktrees(repo)
        .await
        .ok()
        .and_then(|worktrees| worktrees.get(branch).cloned());
    let Ok(common_dir) = git_common_dir(repo).await else {
        return (None, worktree);
    };
    let path = submit::body_file_path(
        template,
        branch,
        worktree.as_deref().unwrap_or(repo),
        &common_dir,
    );
    let title_body = async_fs::read_to_string(&path)
        .await
        .ok()
        .and_then(|contents| submit::parse_body_file(&contents));
    (title_body, worktree)
}

/// Syncs `branch` if it is behind, then pushes it and opens its PR against
/// its parent (when the parent has an open PR) or the target, and refreshes
/// the stack footer in every open PR of the stack.
async fn submit_branch(
    repo: &Path,
    stack: &Stack,
    prs: &HashMap<String, PrStatus>,
    branch: &str,
    title_body: Option<(String, String)>,
    task_title: Option<String>,
    path_env: Option<&str>,
) -> Result<SubmitOutcome> {
    let index = stack
        .index_of(branch)
        .ok_or_else(|| anyhow!("{branch} is no longer in the stack"))?;
    if stack.branches[index].behind {
        let outcome = restack::restack(repo, stack, index, true, prs, path_env).await?;
        if matches!(outcome, RestackOutcome::Conflict { .. }) {
            return Ok(SubmitOutcome::Restack(outcome));
        }
    }
    let parent = &stack.branches[index].parent;
    let base = if prs.get(parent).is_some_and(PrStatus::is_open) {
        parent.clone()
    } else {
        stack.target.clone()
    };
    let (title, body) = match title_body {
        Some(title_body) => title_body,
        None => {
            let commits = submit::commits_since(repo, parent, branch).await?;
            (
                task_title.unwrap_or_else(|| submit::fallback_title(branch, &commits)),
                submit::fallback_body(&commits),
            )
        }
    };
    let pr = submit::create_pr(repo, branch, &base, &title, &body, path_env).await?;
    let footers = async {
        let prs = fetch_prs(repo, path_env).await?;
        submit::rewrite_footers(repo, stack, &prs, path_env).await
    };
    if let Err(err) = footers.await {
        safe_warn!(
            safe: ("PR stack: could not update stack footers"),
            full: ("PR stack: could not update stack footers: {err:#}")
        );
    }
    Ok(SubmitOutcome::Created(pr))
}

impl Entity for PrStackPanel {
    type Event = PrStackPanelEvent;
}

impl TypedActionView for PrStackPanel {
    type Action = PrStackAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        let busy = self.busy.is_some();
        match action.clone() {
            PrStackAction::Refresh => self.refresh(true, ctx),
            PrStackAction::OpenDiff(branch) => self.open_diff(branch, ctx),
            PrStackAction::OpenMenu { branch, position } => self.open_menu(branch, position, ctx),
            PrStackAction::OpenPr(branch) => {
                if let Some(pr) = self.loaded.as_ref().and_then(|l| l.prs.get(&branch)) {
                    ctx.open_url(&pr.url);
                }
            }
            PrStackAction::CopyBranch(branch) => {
                ctx.clipboard().write(ClipboardContent::plain_text(branch));
            }
            _ if busy => {}
            PrStackAction::CreatePr(branch) => self.create_pr(branch, true, ctx),
            PrStackAction::Sync(branch) => self.run_restack(branch, true, ctx),
            PrStackAction::RestackFrom(branch) => self.run_restack(branch, false, ctx),
            PrStackAction::Pin { child, parent } => self.pin(child, parent, ctx),
        }
    }
}

impl View for PrStackPanel {
    fn ui_name() -> &'static str {
        "PrStackPanel"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let background = theme.background();
        let main_color = theme.main_text_color(background).into_solid();
        let sub_color = theme.sub_text_color(background).into_solid();
        let font = appearance.ui_font_family();
        let font_size = appearance.ui_font_size();
        let text = |content: String, color: ColorU| {
            Text::new_inline(content, font, font_size)
                .with_color(color)
                .finish()
        };

        let mut column = Flex::column()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch);

        let title = match &self.loaded {
            Some(loaded) => format!(
                "Stack: {} → {}",
                loaded.top,
                base_name(&loaded.stack.target)
            ),
            None => "PR stack".to_string(),
        };
        let refresh = icon_button(appearance, Icon::Refresh, false, self.refresh_state.clone())
            .build()
            .on_click(|ctx, _, _| ctx.dispatch_typed_action(PrStackAction::Refresh))
            .with_cursor(Cursor::PointingHand)
            .finish();
        column.add_child(
            Container::new(
                Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_child(
                        Shrinkable::new(
                            1.0,
                            Text::new_inline(title, font, font_size + 1.)
                                .with_color(main_color)
                                .with_style(Properties::default().weight(Weight::Semibold))
                                .finish(),
                        )
                        .finish(),
                    )
                    .with_child(refresh)
                    .finish(),
            )
            .with_horizontal_padding(12.)
            .with_vertical_padding(4.)
            .finish(),
        );

        let mut status_lines = Vec::new();
        if let Some(loaded) = &self.loaded {
            let open = loaded
                .stack
                .branches
                .iter()
                .filter(|b| loaded.prs.get(&b.name).is_some_and(PrStatus::is_open))
                .count();
            status_lines.push((
                format!(
                    "{} branches, {open} PRs open{}",
                    loaded.stack.branches.len(),
                    if self.load_handle.is_some() {
                        " · refreshing…"
                    } else {
                        ""
                    }
                ),
                sub_color,
            ));
            if let Some(pr_error) = &loaded.pr_error {
                status_lines.push((pr_error.clone(), sub_color));
            }
        }
        if let Some(busy) = self.busy {
            status_lines.push((busy.to_string(), theme.ansi_fg_yellow()));
        }
        if let Some(message) = &self.message {
            status_lines.push((message.clone(), theme.ansi_fg_red()));
        }
        for (line, color) in status_lines {
            column.add_child(
                Container::new(text(line, color))
                    .with_horizontal_padding(12.)
                    .with_margin_bottom(2.)
                    .finish(),
            );
        }

        let placeholder = |message: String| {
            Container::new(
                Text::new(message, font, font_size)
                    .with_color(sub_color)
                    .finish(),
            )
            .with_uniform_padding(12.)
            .finish()
        };
        let body = if self.repo.is_none() {
            placeholder("Focus a terminal inside a git repository to see its branch stack.".into())
        } else if let Some(error) = &self.load_error {
            placeholder(error.clone())
        } else if let Some(loaded) = &self.loaded {
            if loaded.stack.branches.is_empty() {
                placeholder(format!(
                    "{} has no commits on top of {}.",
                    loaded.top, loaded.stack.target
                ))
            } else {
                self.render_rows(loaded, appearance, app)
            }
        } else {
            placeholder("Loading…".into())
        };
        column.add_child(Shrinkable::new(1.0, body).finish());

        let position_id = self.position_id();
        let mut stack = StackElement::new()
            .with_child(SavePosition::new(column.finish(), &position_id).finish());
        if let Some(position) = self.menu_position {
            stack.add_positioned_overlay_child(
                ChildView::new(&self.menu).finish(),
                OffsetPositioning::offset_from_parent(
                    position,
                    ParentOffsetBounds::WindowByPosition,
                    ParentAnchor::TopLeft,
                    ChildAnchor::TopLeft,
                ),
            );
        }
        stack.finish()
    }
}

impl PrStackPanel {
    fn render_rows(
        &self,
        loaded: &Loaded,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let background = theme.background();
        let main_color = theme.main_text_color(background).into_solid();
        let sub_color = theme.sub_text_color(background).into_solid();
        let font = appearance.ui_font_family();
        let font_size = appearance.ui_font_size();

        let order = *PrStackSettings::as_ref(app).row_order.value();
        let mut indices: Vec<usize> = (0..loaded.stack.branches.len()).collect();
        if order == PrStackRowOrder::TopToBottom {
            indices.reverse();
        }

        let task_keys: HashMap<String, String> = self
            .task_terminals(app)
            .into_iter()
            .filter_map(|(_, task)| Some((task.branch?, task.key?)))
            .collect();
        let mut rows = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);
        for index in indices {
            let branch = &loaded.stack.branches[index];
            let pr = loaded.prs.get(&branch.name);
            let stats = loaded.stats.get(&branch.name).cloned().unwrap_or_default();
            let flags = row_flags(index, branch, pr, self.conflicts.contains(&branch.name));
            let (chip, chip_color) =
                pr_chip(pr, theme.ansi_fg_red(), theme.ansi_fg_green(), sub_color);
            let is_top = branch.name == loaded.top;
            let name = match task_keys.get(&branch.name) {
                Some(key) => format!("{key} · {}", branch.name),
                None => branch.name.clone(),
            };
            let state = self
                .row_states
                .get(&branch.name)
                .cloned()
                .unwrap_or_default();
            let hover_fill = internal_colors::fg_overlay_2(theme);
            let warning = theme.ansi_fg_yellow();
            let error = theme.ansi_fg_red();
            let position_id = self.position_id();

            let row = Hoverable::new(state, move |mouse| {
                let hovered = mouse.is_hovered();
                let mut name_text =
                    Text::new_inline(name.clone(), font, font_size).with_color(main_color);
                if is_top {
                    name_text =
                        name_text.with_style(Properties::default().weight(Weight::Semibold));
                }
                let mut content = Flex::column()
                    .with_child(
                        Flex::row()
                            .with_main_axis_size(MainAxisSize::Max)
                            .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                            .with_cross_axis_alignment(CrossAxisAlignment::Center)
                            .with_child(Shrinkable::new(1.0, name_text.finish()).finish())
                            .with_child(
                                Text::new_inline(chip, font, font_size - 1.)
                                    .with_color(chip_color)
                                    .finish(),
                            )
                            .finish(),
                    )
                    .with_child(
                        Text::new_inline(
                            if hovered {
                                full_stats(&stats)
                            } else {
                                compact_stats(&stats)
                            },
                            font,
                            font_size - 1.,
                        )
                        .with_color(sub_color)
                        .finish(),
                    );
                for (flag, is_error) in &flags {
                    content.add_child(
                        Text::new_inline(flag.clone(), font, font_size - 1.)
                            .with_color(if *is_error { error } else { warning })
                            .finish(),
                    );
                }
                let mut container = Container::new(content.finish())
                    .with_horizontal_padding(8.)
                    .with_vertical_padding(6.)
                    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)));
                if hovered {
                    container = container.with_background(hover_fill);
                }
                container.finish()
            })
            .on_click({
                let branch = branch.name.clone();
                move |ctx, _, _| ctx.dispatch_typed_action(PrStackAction::OpenDiff(branch.clone()))
            })
            .on_right_click({
                let branch = branch.name.clone();
                move |ctx, _, position| {
                    let Some(bounds) = ctx.element_position_by_id(&position_id) else {
                        return;
                    };
                    ctx.dispatch_typed_action(PrStackAction::OpenMenu {
                        branch: branch.clone(),
                        position: position - bounds.origin(),
                    });
                }
            })
            .with_cursor(Cursor::PointingHand)
            .finish();
            rows.add_child(Container::new(row).with_horizontal_margin(4.).finish());
        }

        ClippedScrollable::vertical(
            self.scroll_state.clone(),
            rows.finish(),
            ScrollbarWidth::Auto,
            theme.nonactive_ui_detail().into(),
            theme.active_ui_detail().into(),
            Fill::None,
        )
        .finish()
    }
}

/// PR chip text and color: red for failing checks or requested changes,
/// green when merged or approved with passing checks.
fn pr_chip(pr: Option<&PrStatus>, red: ColorU, green: ColorU, neutral: ColorU) -> (String, ColorU) {
    let Some(pr) = pr else {
        return ("no PR".to_string(), neutral);
    };
    let mut parts = vec![format!("#{} {}", pr.number, pr.state.to_lowercase())];
    match pr.checks {
        Some(ChecksState::Passing) => parts.push("checks ✓".into()),
        Some(ChecksState::Failing) => parts.push("checks ✗".into()),
        Some(ChecksState::Pending) => parts.push("checks …".into()),
        None => {}
    }
    match pr.review.as_deref() {
        Some("APPROVED") => parts.push("approved".into()),
        Some("CHANGES_REQUESTED") => parts.push("changes requested".into()),
        _ => {}
    }
    let color = if pr.checks == Some(ChecksState::Failing)
        || pr.review.as_deref() == Some("CHANGES_REQUESTED")
    {
        red
    } else if pr.is_merged()
        || (pr.review.as_deref() == Some("APPROVED") && pr.checks != Some(ChecksState::Pending))
    {
        green
    } else {
        neutral
    };
    (parts.join(" · "), color)
}

/// Row flags as (text, is_error).
fn row_flags(
    index: usize,
    branch: &StackBranch,
    pr: Option<&PrStatus>,
    conflict: bool,
) -> Vec<(String, bool)> {
    let mut flags = Vec::new();
    if conflict {
        flags.push(("rebase conflict, handed to the agent".to_string(), true));
    }
    if branch.behind {
        flags.push((
            if index == 0 {
                "needs sync: the target moved".to_string()
            } else {
                format!("behind {}", branch.parent)
            },
            false,
        ));
    }
    if let Some(pr) = pr.filter(|pr| pr.is_open())
        && pr.base != base_name(&branch.parent)
    {
        flags.push((format!("base mismatch: PR targets {}", pr.base), false));
    }
    if branch.ambiguous {
        flags.push((
            format!("ambiguous parent (also {})", branch.alternatives.join(", ")),
            false,
        ));
    }
    if branch.pinned {
        flags.push((format!("pinned onto {}", branch.parent), false));
    }
    flags
}

fn compact_stats(stats: &BranchStats) -> String {
    let mut parts = vec![format!("+{} −{}", stats.additions, stats.deletions)];
    for (label, value) in [
        ("code", stats.code),
        ("tests", stats.tests),
        ("docs", stats.docs),
        ("cfg", stats.config),
        ("cmt", stats.comments),
    ] {
        if value > 0 {
            parts.push(format!("{label} {value}"));
        }
    }
    parts.join(" · ")
}

fn full_stats(stats: &BranchStats) -> String {
    format!(
        "{} files · +{} −{} · code {} · comments {} · tests {} · docs {} · config {}",
        stats.files,
        stats.additions,
        stats.deletions,
        stats.code,
        stats.comments,
        stats.tests,
        stats.docs,
        stats.config
    )
}

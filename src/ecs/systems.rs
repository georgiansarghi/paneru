use bevy::app::AppExit;
use bevy::ecs::change_detection::{DetectChanges, DetectChangesMut};
use bevy::ecs::entity::Entity;
use bevy::ecs::hierarchy::{ChildOf, Children};
use bevy::ecs::message::{MessageReader, MessageWriter};
use bevy::ecs::query::{Added, Changed, Has, Or, With, Without};
use bevy::ecs::system::{
    Commands, Local, NonSend, NonSendMut, ParallelCommands, ParamSet, Populated, Query, Res,
    ResMut, Single,
};
use bevy::math::IRect;
use bevy::tasks::AsyncComputeTaskPool;
use bevy::tasks::futures_lite::future;
use bevy::time::Time;
use objc2_foundation::NSPoint;
use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;
use tracing::{Level, debug, error, info, instrument, trace, warn};

use super::{
    ActiveDisplayMarker, BProcess, ExistingMarker, FreshMarker, RepositionMarker, ResizeMarker,
    RetryFrontSwitch, SpawnWindowTrigger, Timeout, VerifyWindowPosition,
};

use crate::config::{Config, decorations::BorderRadiusOption};
use crate::ecs::layout::LayoutStrip;
use crate::ecs::params::{ActiveDisplay, Windows};
use crate::ecs::{
    ActiveWorkspaceMarker, Bounds, BruteforceWindows, FlashMessage, FocusedMarker, Initializing,
    LoopActivity, LoopDiagnostics, LoopWakeReason, LowPowerMode, MissionControlActive, Position,
    ReadDisplayProperties, RestoreWindowState, Scrolling, SendMessageTrigger, SpawnCommandsExt,
    Unmanaged, WidthRatio, WindowProperties,
};
use crate::events::{Event, WakeableEventQueue};
use crate::manager::{
    Application, Display, Process, Window, WindowManager, WindowOS, bruteforce_windows,
};
use crate::overlay::{FlashMessageManager, OverlayManager};
use crate::platform::{PlatformCallbacks, WinID};
use crate::runtime_driver::{
    DeadlineReason, RuntimeActivity, RuntimeActivityReason, RuntimeDeadlineClock, RuntimeDeadlines,
    RuntimeDirty, RuntimeDirtyReason, RuntimeDriverActive,
};

const ANIAMTE_SNAP_THRESHOLD: f32 = 5.0;
const LOOP_MAX_TIMEOUT_FRAME_ACTIVE_MS: u32 = 16;
const LOOP_MAX_TIMEOUT_LOWPOWER_MS: u32 = 500;
const LOOP_MAX_TIMEOUT_MS: u32 = 50;
const LOOP_TIMEOUT_STEP: u32 = 1;

pub(crate) const fn loop_timeout_limit_ms(activity: LoopActivity) -> u32 {
    if activity.frame_active() {
        LOOP_MAX_TIMEOUT_FRAME_ACTIVE_MS
    } else if activity.low_power {
        LOOP_MAX_TIMEOUT_LOWPOWER_MS
    } else {
        LOOP_MAX_TIMEOUT_MS
    }
}

pub(crate) const fn classify_timeout_wake(activity: LoopActivity) -> LoopWakeReason {
    if activity.flash_message {
        LoopWakeReason::FlashMessageActive
    } else if activity.repositioning || activity.resizing || activity.scrolling {
        LoopWakeReason::FrameActive
    } else {
        LoopWakeReason::TimeoutWatchdog
    }
}

pub(super) fn record_periodic_maintenance(mut diagnostics: Option<ResMut<LoopDiagnostics>>) {
    if let Some(diagnostics) = diagnostics.as_mut() {
        diagnostics.record(LoopWakeReason::PeriodicMaintenance);
    }
}

#[allow(clippy::needless_pass_by_value, clippy::too_many_arguments)]
pub(super) fn update_runtime_deadlines(
    mut deadlines: Option<ResMut<RuntimeDeadlines>>,
    clock: Option<Res<RuntimeDeadlineClock>>,
    low_power_mode: Option<Res<LowPowerMode>>,
    config: Option<Res<Config>>,
    runtime_activity: Option<Res<RuntimeActivity>>,
    strips: Query<&LayoutStrip>,
    mut diagnostics: Option<ResMut<LoopDiagnostics>>,
    repositioning: Query<(), With<RepositionMarker>>,
    resizing: Query<(), With<ResizeMarker>>,
    scrolling: Query<(), With<Scrolling>>,
    flash_messages: Query<(), With<FlashMessage>>,
    timeouts: Query<&Timeout>,
) {
    const FRAME_DEADLINE: Duration = Duration::from_millis(16);
    const NATIVE_TAB_DEADLINE: Duration = Duration::from_millis(super::NATIVE_TAB_RECONCILE_MS);
    const NATIVE_TAB_SAFETY_CHECK: Duration = Duration::from_secs(30);
    const NATIVE_TAB_ACTIVITY_GRACE: Duration = Duration::from_secs(2);
    const LOST_FOCUS_WATCHDOG: Duration = Duration::from_secs(1);
    const ORPHAN_WORKSPACE_WATCHDOG: Duration = Duration::from_secs(1);
    const REFRESH_WINDOW_SIZES: Duration = Duration::from_secs(1);
    const LOW_POWER_CHECK: Duration = Duration::from_secs(60);
    const PERIODIC_STATE_SAVE: Duration = Duration::from_secs(300);
    const PERIODIC_MAINTENANCE: Duration = Duration::from_secs(300);

    let Some(deadlines) = deadlines.as_mut() else {
        return;
    };
    let Some(clock) = clock.as_ref() else {
        return;
    };
    let now = clock.now();
    let due_reasons = deadlines.due_reasons(now);
    let mut rescheduled_reasons = Vec::new();

    for transient in [
        DeadlineReason::AnimationFrame,
        DeadlineReason::LowPowerWatchdog,
        DeadlineReason::TimeoutComponent,
    ] {
        deadlines.clear(transient);
    }

    if !repositioning.is_empty()
        || !resizing.is_empty()
        || !scrolling.is_empty()
        || !flash_messages.is_empty()
    {
        deadlines.set_after(now, DeadlineReason::AnimationFrame, FRAME_DEADLINE);
        rescheduled_reasons.push(DeadlineReason::AnimationFrame);
    }
    if low_power_mode.is_some() {
        deadlines.set_after(now, DeadlineReason::LowPowerWatchdog, LOW_POWER_CHECK);
        rescheduled_reasons.push(DeadlineReason::LowPowerWatchdog);
    }
    if let Some(remaining) = timeouts
        .iter()
        .map(|timeout| timeout.timer.remaining())
        .min()
    {
        deadlines.set_after(now, DeadlineReason::TimeoutComponent, remaining);
        rescheduled_reasons.push(DeadlineReason::TimeoutComponent);
    }

    let native_tab_plan = native_tab_deadline_plan(NativeTabDeadlineInputs {
        enabled: config
            .as_ref()
            .is_none_or(|config| config.native_tabs_enabled()),
        has_tab_groups: layout_has_native_tab_groups(&strips),
        recent_activity: runtime_activity
            .as_ref()
            .is_some_and(|activity| activity.recent(now, NATIVE_TAB_ACTIVITY_GRACE)),
        reconcile: NATIVE_TAB_DEADLINE,
        safety: NATIVE_TAB_SAFETY_CHECK,
    });
    if apply_native_tab_deadline_plan(deadlines, now, native_tab_plan) {
        rescheduled_reasons.push(DeadlineReason::NativeTabReconciliation);
    }
    if let Some(diagnostics) = diagnostics.as_mut() {
        diagnostics.record_native_tab_deadline_policy(native_tab_plan.diagnostic);
    }
    for (reason, period) in [
        (DeadlineReason::LostFocusWatchdog, LOST_FOCUS_WATCHDOG),
        (
            DeadlineReason::OrphanWorkspaceWatchdog,
            ORPHAN_WORKSPACE_WATCHDOG,
        ),
        (DeadlineReason::RefreshWindowSizes, REFRESH_WINDOW_SIZES),
        (DeadlineReason::StateSave, PERIODIC_STATE_SAVE),
        (DeadlineReason::PeriodicMaintenance, PERIODIC_MAINTENANCE),
    ] {
        if deadlines.ensure_repeating_after(now, reason, period) {
            rescheduled_reasons.push(reason);
        }
    }

    if let Some(diagnostics) = diagnostics.as_mut() {
        diagnostics.record_runtime_deadline_update(&due_reasons, &rescheduled_reasons);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NativeTabDeadlineInputs {
    enabled: bool,
    has_tab_groups: bool,
    recent_activity: bool,
    reconcile: Duration,
    safety: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NativeTabDeadlinePlan {
    cadence: Option<Duration>,
    diagnostic: &'static str,
}

fn native_tab_deadline_plan(inputs: NativeTabDeadlineInputs) -> NativeTabDeadlinePlan {
    if !inputs.enabled {
        return NativeTabDeadlinePlan {
            cadence: None,
            diagnostic: "disabled",
        };
    }

    if inputs.has_tab_groups {
        return NativeTabDeadlinePlan {
            cadence: Some(inputs.reconcile),
            diagnostic: "active_tab_group",
        };
    }

    if inputs.recent_activity {
        return NativeTabDeadlinePlan {
            cadence: Some(inputs.reconcile),
            diagnostic: "recent_activity",
        };
    }

    NativeTabDeadlinePlan {
        cadence: Some(inputs.safety),
        diagnostic: "safety_check",
    }
}

fn apply_native_tab_deadline_plan(
    deadlines: &mut RuntimeDeadlines,
    now: Duration,
    plan: NativeTabDeadlinePlan,
) -> bool {
    let Some(cadence) = plan.cadence else {
        deadlines.clear(DeadlineReason::NativeTabReconciliation);
        return false;
    };

    let target = now + cadence;
    if deadlines
        .deadline_at(DeadlineReason::NativeTabReconciliation)
        .is_none_or(|existing| existing <= now || existing > target)
    {
        deadlines.set_after(now, DeadlineReason::NativeTabReconciliation, cadence);
        return true;
    }

    false
}

fn layout_has_native_tab_groups(strips: &Query<&LayoutStrip>) -> bool {
    strips.iter().any(strip_has_native_tab_group)
}

fn strip_has_native_tab_group(strip: &LayoutStrip) -> bool {
    let mut seen = HashSet::new();
    strip.all_windows().into_iter().any(|entity| {
        if !seen.insert(entity) {
            return false;
        }
        let Some(group) = strip.tab_group(entity) else {
            return false;
        };
        seen.extend(group.iter().copied());
        true
    })
}

#[allow(clippy::needless_pass_by_value, clippy::too_many_arguments)]
pub(super) fn record_runtime_dirty(
    mut dirty: Option<ResMut<RuntimeDirty>>,
    mut runtime_activity: Option<ResMut<crate::runtime_driver::RuntimeActivity>>,
    clock: Option<Res<RuntimeDeadlineClock>>,
    mut messages: MessageReader<Event>,
    focused_changes: Query<(), Added<FocusedMarker>>,
    layout_changes: Query<(), Changed<LayoutStrip>>,
    reposition_inserted: Query<(), Added<RepositionMarker>>,
    resize_inserted: Query<(), Added<ResizeMarker>>,
    scrolling_inserted: Query<(), Added<Scrolling>>,
    flash_inserted: Query<(), Added<FlashMessage>>,
) {
    let now = clock.as_ref().map(|clock| clock.now());

    for event in messages.read() {
        match event {
            Event::Command { .. } => {
                if let Some(dirty) = dirty.as_mut() {
                    dirty.mark(RuntimeDirtyReason::CommandHandled);
                }
                mark_activity(&mut runtime_activity, now, RuntimeActivityReason::Command);
            }
            Event::MouseDown { .. }
            | Event::MouseUp { .. }
            | Event::MouseDragged { .. }
            | Event::MouseMoved { .. }
            | Event::Swipe { .. }
            | Event::VerticalSwipe { .. }
            | Event::VerticalScrollTick { .. }
            | Event::Scroll { .. }
            | Event::TouchpadDown
            | Event::TouchpadUp => {
                mark_activity(&mut runtime_activity, now, RuntimeActivityReason::Input);
            }
            Event::WindowFocused { .. } => {
                mark_activity(&mut runtime_activity, now, RuntimeActivityReason::Focus);
            }
            Event::WindowCreated { .. }
            | Event::WindowDestroyed { .. }
            | Event::WindowMoved { .. }
            | Event::WindowResized { .. }
            | Event::WindowMinimized { .. }
            | Event::WindowDeminimized { .. }
            | Event::WindowTitleChanged { .. }
            | Event::ApplicationLaunched { .. }
            | Event::ApplicationTerminated { .. }
            | Event::ApplicationFrontSwitched { .. }
            | Event::ApplicationActivated
            | Event::ApplicationDeactivated
            | Event::ApplicationVisible { .. }
            | Event::ApplicationHidden { .. }
            | Event::SpaceChanged
            | Event::DisplayChanged
            | Event::DisplayAdded { .. }
            | Event::DisplayRemoved { .. }
            | Event::DisplayMoved { .. }
            | Event::DisplayResized { .. }
            | Event::DisplayConfigured { .. } => {
                mark_activity(
                    &mut runtime_activity,
                    now,
                    RuntimeActivityReason::WindowEvent,
                );
            }
            _ => {}
        }
    }

    if !focused_changes.is_empty() {
        if let Some(dirty) = dirty.as_mut() {
            dirty.mark(RuntimeDirtyReason::FocusMarkerChanged);
        }
        mark_activity(&mut runtime_activity, now, RuntimeActivityReason::Focus);
    }
    if !layout_changes.is_empty() {
        // Layout changes are important interactive activity, but they do not by
        // themselves require another immediate Bevy update: subscriber
        // broadcasts run in the same frame, and actual animation follow-up is
        // covered by inserted animation markers below. Marking every changed
        // strip dirty can keep the settle loop alive during harmless layout
        // churn.
        mark_activity(&mut runtime_activity, now, RuntimeActivityReason::Layout);
    }
    if !reposition_inserted.is_empty()
        || !resize_inserted.is_empty()
        || !scrolling_inserted.is_empty()
        || !flash_inserted.is_empty()
    {
        if let Some(dirty) = dirty.as_mut() {
            dirty.mark(RuntimeDirtyReason::AnimationMarkerInserted);
        }
        mark_activity(&mut runtime_activity, now, RuntimeActivityReason::Animation);
    }
}

fn mark_activity(
    runtime_activity: &mut Option<ResMut<crate::runtime_driver::RuntimeActivity>>,
    now: Option<Duration>,
    reason: RuntimeActivityReason,
) {
    if let Some((activity, now)) = runtime_activity.as_mut().zip(now) {
        activity.mark(now, reason);
    }
}

/// Gathers all present displays and spawns them as entities in the Bevy world.
/// The currently active display (identified by `window_manager.active_display_id()`) is marked with `ActiveDisplayMarker`.
///
/// # Arguments
///
/// * `window_manager` - The `WindowManager` resource for querying display information.
/// * `commands` - Bevy commands to spawn entities.
#[allow(clippy::needless_pass_by_value)]
pub fn gather_displays(window_manager: Res<WindowManager>, mut commands: Commands) {
    let Ok(active_display_id) = window_manager.active_display_id() else {
        error!("Unable to get active display id!");
        return;
    };
    for (display, workspaces) in window_manager.present_displays() {
        let origin = Position(display.bounds().min);
        let entity = if display.id() == active_display_id {
            commands.spawn((display, ActiveDisplayMarker))
        } else {
            commands.spawn(display)
        }
        .id();

        commands.trigger(ReadDisplayProperties(entity));

        let Ok(active_space) = window_manager.active_display_space(active_display_id) else {
            return;
        };

        for id in workspaces {
            let active = id == active_space;
            commands.spawn_layout_strip(LayoutStrip::new(id, 0), origin.0, entity, active);
        }
    }
}

/// Adds an existing process to the window manager. This is used during initial setup for already running applications.
/// It attempts to create a new `Application` instance from the `BProcess` and attaches it as a child entity.
/// The `ExistingMarker` is then removed from the process entity.
///
/// # Arguments
///
/// * `window_manager` - The `WindowManager` resource for creating new application instances.
/// * `process_query` - A query for existing `BProcess` entities marked with `ExistingMarker`.
/// * `commands` - Bevy commands to spawn entities and manage components.
#[allow(clippy::needless_pass_by_value)]
#[instrument(level = Level::DEBUG, skip_all)]
pub(crate) fn add_existing_process(
    window_manager: Res<WindowManager>,
    processes: Populated<(Entity, &BProcess), With<ExistingMarker>>,
    mut commands: Commands,
) {
    for (entity, process) in processes {
        let Ok(app) = window_manager.new_application(&*process.0) else {
            error!("creating aplication from process '{}'", process.name());
            return;
        };
        commands.spawn((app, ExistingMarker, ChildOf(entity)));
        commands.entity(entity).try_remove::<ExistingMarker>();
    }
}

/// Adds an existing application to the window manager. This is used during initial setup.
/// It observes the application, adds its windows to the manager, and then triggers `SpawnWindowTrigger` events for newly found windows.
/// The `ExistingMarker` is removed from the application entity after processing.
///
/// # Arguments
///
/// * `window_manager` - The `WindowManager` resource for interacting with window management logic.
/// * `displays` - A query for all `Display` entities, used to gather all existing space IDs.
/// * `app_query` - A query for existing `Application` entities marked with `ExistingMarker`.
/// * `commands` - Bevy commands to spawn entities and manage components.
#[allow(clippy::needless_pass_by_value)]
#[instrument(level = Level::DEBUG, skip_all)]
pub(crate) fn add_existing_application(
    window_manager: Res<WindowManager>,
    workspaces: Query<&LayoutStrip>,
    fresh_apps: Populated<(&mut Application, Entity), With<ExistingMarker>>,
    mut commands: Commands,
) {
    let spaces = workspaces
        .into_iter()
        .map(LayoutStrip::id)
        .collect::<Vec<_>>();
    let thread_pool = AsyncComputeTaskPool::get();

    for (mut app, entity) in fresh_apps {
        let mut offscreen_windows = vec![];

        if app.observe().is_ok_and(|result| result)
            && let Ok((found_windows, offscreen)) = window_manager
                .find_existing_application_windows(&mut app, &spaces)
                .inspect_err(|err| warn!("{err}"))
        {
            offscreen_windows.extend(offscreen);
            commands.trigger(SpawnWindowTrigger(found_windows));
        }
        commands.entity(entity).try_remove::<ExistingMarker>();

        if !offscreen_windows.is_empty() {
            let pid = app.pid();
            let bruteforce_task =
                thread_pool.spawn(async move { bruteforce_windows(pid, offscreen_windows) });
            commands.spawn(BruteforceWindows(bruteforce_task));
        }
    }
}

/// Finishes the initialization process once all initial windows are loaded.
/// This system refreshes displays, assigns the `FocusedMarker` to the first window of the active space,
/// and logs the total number of managed windows.
///
/// # Arguments
///
/// * `windows` - A mutable query for all `Window` components, their `Entity`, and `Has<Unmanaged>` status.
/// * `displays` - A query for all `Display` entities, including whether they have the `ActiveDisplayMarker`.
/// * `window_manager` - The `WindowManager` resource for refreshing displays and getting active space information.
/// * `commands` - Bevy commands to insert components like `FocusedMarker`.
#[allow(clippy::needless_pass_by_value)]
#[instrument(level = Level::DEBUG, skip_all)]
pub(crate) fn finish_setup(
    process_query: Query<Entity, With<ExistingMarker>>,
    windows: Windows,
    mut bruteforce_tasks: Query<(Entity, &mut BruteforceWindows)>,
    mut workspaces: Query<(&mut LayoutStrip, Has<ActiveWorkspaceMarker>, &ChildOf)>,
    window_manager: Res<WindowManager>,
    mut commands: Commands,
) {
    if !process_query.is_empty() {
        // The other two add_* functions are still running..
        return;
    }

    // Reap the bruteforced windows.
    if !bruteforce_tasks.is_empty() {
        for (entity, mut job) in &mut bruteforce_tasks {
            if let Some(found_windows) = future::block_on(future::poll_once(&mut job.0)) {
                commands.trigger(SpawnWindowTrigger(found_windows));
                commands.entity(entity).despawn();
            }
        }
        // Wait for the next tick to finish initialization.
        return;
    }

    info!(
        "Initialization: found {:?} windows.",
        windows.iter().size_hint()
    );

    for (mut strip, active_strip, _) in &mut workspaces {
        debug!("space {}: before refresh {strip:?}", strip.id());
        let workspace_windows = window_manager
            .windows_in_workspace(strip.id())
            .inspect_err(|err| {
                warn!("failed to get windows on workspace {}: {err}", strip.id());
            })
            .ok()
            .map(|workspace_windows| {
                workspace_windows
                    .into_iter()
                    .filter_map(|window_id| windows.find_managed(window_id))
                    .filter(|(window, entity)| {
                        if window.is_minimized() {
                            commands.entity(*entity).try_insert(Unmanaged::Minimized);
                            false
                        } else {
                            true
                        }
                    })
                    .collect::<Vec<_>>()
            });
        let Some(workspace_windows) = workspace_windows else {
            continue;
        };

        // Preserve the order - do not flush existing windows.
        for entity in strip.all_windows() {
            if !workspace_windows.iter().any(|(_, e)| *e == entity) {
                strip.remove(entity);
            }
        }
        for (_, entity) in workspace_windows {
            if !strip.contains(entity) {
                strip.append(entity);
            }
        }
        debug!("space {}: after refresh {strip:?}", strip.id());

        if active_strip && let Some(entity) = strip.first().ok().and_then(|column| column.top()) {
            commands.focus_entity(entity, true);
        }
    }

    commands.remove_resource::<Initializing>();
    commands.trigger(RestoreWindowState);
}

/// Handles the event when a new application is launched. It creates a `Process` and `Application` object,
/// observes the application for events, and adds its windows to the manager.
/// This system processes `BProcess` entities marked with `FreshMarker`.
/// If the process is not yet ready, it continues observing it. If ready, it attempts to create and observe an `Application`.
/// A `Timeout` is added to the application if it takes too long to become observable.
///
/// # Arguments
///
/// * `window_manager` - The `WindowManager` resource for creating new application instances.
/// * `process_query` - A `Populated` query for `(Entity, &mut BProcess, Has<Children>)` with `With<FreshMarker>`.
/// * `commands` - Bevy commands to spawn entities and manage components.
#[allow(clippy::needless_pass_by_value)]
pub(super) fn add_launched_process(
    window_manager: Res<WindowManager>,
    fresh_processes: Populated<(Entity, &mut BProcess, Has<Children>), With<FreshMarker>>,
    mut commands: Commands,
) {
    const APP_OBSERVABLE_TIMEOUT_SEC: u64 = 5;
    let mut already_seen = HashSet::new();

    for (entity, mut process, children) in fresh_processes {
        let process = &mut *process.0;

        if !already_seen.insert(process.psn()) {
            continue;
        }

        if !process.ready() {
            continue;
        }

        if children {
            // Process already has an attached Application, so finish.
            commands.entity(entity).try_remove::<FreshMarker>();
            continue;
        }

        let Ok(mut app) = window_manager.new_application(process) else {
            error!("creating aplication from process '{}'", process.name());
            return;
        };

        if app.observe().is_ok_and(|good| good) {
            let timeout = Timeout::new(
                Duration::from_secs(APP_OBSERVABLE_TIMEOUT_SEC),
                Some(format!(
                    "{app} did not become observable in {APP_OBSERVABLE_TIMEOUT_SEC}s.",
                )),
                &mut commands,
            );
            commands.spawn((app, FreshMarker, timeout, ChildOf(entity)));
        } else {
            debug!("failed to register some observers {}", process.name());
        }
    }
}

/// Adds windows for a newly launched application.
/// This system processes `Application` entities marked with `FreshMarker`.
/// It queries the application's window list, filters out already existing windows, and triggers `SpawnWindowTrigger` events for new windows.
/// The `FreshMarker` is removed from the application entity after processing.
///
/// # Arguments
///
/// * `app_query` - A `Populated` query for `(&mut Application, Entity)` with `With<FreshMarker>`.
/// * `windows` - A query for all `Window` components, used to check for existing windows.
/// * `commands` - Bevy commands to spawn entities and manage components.
#[allow(clippy::needless_pass_by_value)]
pub(super) fn add_launched_application(
    app_query: Populated<(&mut Application, Entity, Has<Children>), With<FreshMarker>>,
    windows: Windows,
    mut commands: Commands,
) {
    // TODO: maybe refactor this with add_existing_application_windows()
    let find_window = |window_id| windows.find(window_id);

    for (app, entity, has_children) in app_query {
        let mut create_windows = app.window_list();
        // Retain the non-existing windows, so they can be created.
        create_windows.retain(|window| find_window(window.id()).is_none());

        if !create_windows.is_empty() {
            commands.entity(entity).try_remove::<FreshMarker>();
            debug!(
                "spawn! (polling path found {} new windows for {entity})",
                create_windows.len(),
            );
            commands.trigger(SpawnWindowTrigger(create_windows));
        } else if has_children {
            // Windows were already created via AXCreated notification path.
            // Remove FreshMarker so the Timeout gets cleaned up.
            debug!("removing FreshMarker from {entity}: windows already created via AXCreated");
            commands.entity(entity).try_remove::<FreshMarker>();
        }
    }
}

/// Cleans up entities which have been initializing for too long, specifically `BProcess` or `Application` entities.
/// This system removes the `Timeout` component from entities that are no longer `Fresh`.
///
/// This can be processes which are not yet observable or applications which keep failing to
/// register some of the observers.
///
/// # Arguments
///
/// * `cleanup` - A `Populated` query for `(Entity, Has<FreshMarker>, &Timeout)` components, targeting `BProcess` or `Application` entities.
/// * `commands` - Bevy commands to remove components.
#[allow(clippy::type_complexity)]
pub(super) fn fresh_marker_cleanup(
    cleanup: Populated<
        (Entity, Has<FreshMarker>, &Timeout),
        Or<(With<BProcess>, With<Application>)>,
    >,
    mut commands: Commands,
) {
    for (entity, fresh, _) in cleanup {
        if !fresh {
            // Process was ready before the timer finished.
            commands.entity(entity).try_remove::<Timeout>();
        }
    }
}

/// A Bevy system that ticks `Timeout` timers and despawns entities when their timers finish.
/// This system is responsible for cleaning up entities that have exceeded their allotted time for an operation.
///
/// # Arguments
///
/// * `timers` - A `Populated` query for `(Entity, &mut Timeout)` components.
/// * `clock` - The Bevy `Time` resource for getting the delta time.
/// * `commands` - Bevy commands to despawn entities.
#[allow(clippy::needless_pass_by_value)]
pub(super) fn timeout_ticker(
    timers: Populated<(Entity, &mut Timeout)>,
    clock: Res<Time>,
    mut commands: Commands,
) {
    for (entity, mut timeout) in timers {
        if timeout.timer.is_finished() {
            trace!("Despawning entity {entity} due to timeout.");
            if let Some(system_id) = timeout.system_id.take() {
                commands.run_system(system_id);
                commands.unregister_system(system_id);
            }
            trace!("Removing timer {entity}");
            commands.entity(entity).despawn();
        } else {
            timeout.timer.tick(clock.delta());
        }
    }
}

/// Retries querying the focused window for applications that had a transient AX error
/// during `ApplicationFrontSwitched`. Runs each frame until success or timeout.
#[allow(clippy::needless_pass_by_value)]
pub(super) fn retry_front_switch(
    retries: Populated<(Entity, &RetryFrontSwitch)>,
    applications: Query<&Application>,
    mut commands: Commands,
) {
    for (entity, retry) in retries.iter() {
        let Ok(app) = applications.get(retry.0) else {
            // Application entity no longer exists, clean up.
            if let Ok(mut entity_commands) = commands.get_entity(entity) {
                entity_commands.try_despawn();
            }
            continue;
        };
        if !app.is_frontmost() {
            // App is no longer frontmost — this retry is stale.
            debug!("Discarding stale front switch retry (app no longer frontmost).");
            if let Ok(mut entity_commands) = commands.get_entity(entity) {
                entity_commands.try_despawn();
            }
            continue;
        }
        if let Ok(focused_id) = app.focused_window_id() {
            debug!("Front switch retry succeeded for window {focused_id}.");
            commands.trigger(SendMessageTrigger(Event::WindowFocused {
                window_id: focused_id,
            }));
            if let Ok(mut entity_commands) = commands.get_entity(entity) {
                entity_commands.try_despawn();
            }
        }
        // Otherwise, let timeout_ticker handle expiry.
    }
}

/// Animates window movement.
/// This is a Bevy system that runs on `Update`. It smoothly moves windows to their target
/// positions, as indicated by the `RepositionMarker` component.
/// Animation speed is controlled by the `animation_speed` in the `Config`.
/// When a window reaches its target position, the `RepositionMarker` is removed.
///
/// # Arguments
///
/// * `windows` - A `Populated` query for `(&mut Window, Entity, &RepositionMarker)` components.
/// * `displays` - A query for all `Display` entities, used to get display bounds and menubar height.
/// * `time` - The Bevy `Time` resource for calculating delta time.
/// * `config` - The `Config` resource, used for animation speed.
/// * `commands` - Bevy commands to remove the `RepositionMarker` when animation is complete.
#[allow(clippy::needless_pass_by_value)]
#[instrument(level = Level::TRACE, skip_all)]
pub(super) fn animate_entities(
    animate: Populated<(&mut Position, Entity, &RepositionMarker)>,
    time: Res<Time>,
    config: Res<Config>,
    commands: ParallelCommands,
) {
    // Frame-rate-independent exponential smoothing (ease-out).
    // `animation_speed` is the decay rate (per second); higher = snappier.
    // t = 1 - e^(-rate*dt) is the fraction of remaining distance consumed this frame.
    let rate = config.animation_speed();
    let t = (1.0 - (-rate * time.delta_secs_f64()).exp()).clamp(0.0, 1.0) as f32;

    animate
        .into_iter()
        .for_each(|(mut position, entity, RepositionMarker(origin))| {
            let target = origin.as_vec2();
            let current = position.0.as_vec2();
            let lerped = current.lerp(target, t);

            // Snap once we're within a pixel of the target (or after one effectively-
            // complete tick), so the marker is dropped promptly.
            let finished = (target - lerped).length() <= ANIAMTE_SNAP_THRESHOLD;
            let new_pos = if finished {
                *origin
            } else {
                lerped.round().as_ivec2()
            };

            trace!(
                "entity {entity} source {} dest {origin} t {t:.3} moving to {new_pos}",
                position.0,
            );
            position.0 = new_pos;
            if finished {
                commands.command_scope(|mut command| {
                    command.entity(entity).try_remove::<RepositionMarker>();
                });
            }
        });
}

/// Animates window resizing.
/// This is a Bevy system that runs on `Update`. It resizes windows to their target
/// dimensions, as indicated by the `ResizeMarker` component.
/// When a window reaches its target size, the `ResizeMarker` is removed.
///
/// # Arguments
///
/// * `windows` - A `Populated` query for `(&mut Window, Entity, &ResizeMarker)` components.
/// * `active_display` - An `ActiveDisplay` system parameter providing immutable access to the active display.
/// * `commands` - Bevy commands to remove the `ResizeMarker` when resizing is complete.
#[allow(clippy::needless_pass_by_value)]
#[instrument(level = Level::TRACE, skip_all)]
pub(super) fn animate_resize_entities(
    animate: Populated<(&mut Bounds, Entity, &ResizeMarker)>,
    time: Res<Time>,
    config: Res<Config>,
    commands: ParallelCommands,
) {
    // Matches animate_entities: exponential ease-out, frame-rate independent.
    let rate = config.animation_speed();
    let t = (1.0 - (-rate * time.delta_secs_f64()).exp()).clamp(0.0, 1.0) as f32;

    animate
        .into_iter()
        .for_each(|(mut bounds, entity, ResizeMarker(size))| {
            let target = size.as_vec2();
            let current = bounds.0.as_vec2();
            let lerped = current.lerp(target, t);

            let finished = (target - lerped).length() <= ANIAMTE_SNAP_THRESHOLD;
            let new_size = if finished {
                *size
            } else {
                lerped.round().as_ivec2()
            };

            trace!(
                "entity {entity} source {} dest {size} t {t:.3} resizing to {new_size}",
                bounds.0,
            );
            bounds.0 = new_size;
            if finished {
                commands.command_scope(|mut command| {
                    command.entity(entity).try_remove::<ResizeMarker>();
                });
            }
        });
}

#[allow(clippy::needless_pass_by_value, clippy::too_many_arguments)]
pub(super) fn pump_events(
    mut exit: MessageWriter<AppExit>,
    mut messages: MessageWriter<Event>,
    low_power_mode: Option<Res<LowPowerMode>>,
    incoming_events: Option<NonSend<WakeableEventQueue>>,
    platform: Option<NonSendMut<Pin<Box<PlatformCallbacks>>>>,
    repositioning: Query<(), With<RepositionMarker>>,
    resizing: Query<(), With<ResizeMarker>>,
    scrolling: Query<(), With<Scrolling>>,
    flash_messages: Query<(), With<FlashMessage>>,
    custom_runtime_driver: Option<Res<RuntimeDriverActive>>,
    mut timeout: Local<u32>,
    mut diagnostics: Option<ResMut<LoopDiagnostics>>,
) {
    if custom_runtime_driver.is_some() {
        return;
    }

    let Some((ref mut platform, incoming_events)) = platform.zip(incoming_events) else {
        // No platform interface or incoming event pipe - probably executing in a unit test.
        return;
    };

    if let Some(diagnostics) = diagnostics.as_mut() {
        diagnostics.record(LoopWakeReason::CocoaEventPump);
    }
    platform.pump_cocoa_event_loop(f64::from(*timeout) / 1000.0);
    let mut received_events = Vec::new();
    let mut pending_mouse = None;
    loop {
        // Repeatedly drain the events until timeout.
        match incoming_events.recv_timeout(Duration::from_millis(1)) {
            Ok(Event::Exit) | Err(RecvTimeoutError::Disconnected) => {
                if let Some(diagnostics) = diagnostics.as_mut() {
                    diagnostics.record(LoopWakeReason::InternalEvent);
                }
                exit.write(AppExit::Success);
                break;
            }
            Ok(event) => {
                if let Some(diagnostics) = diagnostics.as_mut() {
                    diagnostics.record(LoopWakeReason::InternalEvent);
                }
                if matches!(event, Event::MouseMoved { .. }) {
                    pending_mouse = Some(event);
                } else {
                    received_events.extend(pending_mouse.take());
                    received_events.push(event);
                }
                *timeout = LOOP_TIMEOUT_STEP;
            }
            Err(RecvTimeoutError::Timeout) => {
                received_events.extend(pending_mouse.take());
                messages.write_batch(received_events);
                let activity = LoopActivity {
                    repositioning: !repositioning.is_empty(),
                    resizing: !resizing.is_empty(),
                    scrolling: !scrolling.is_empty(),
                    flash_message: !flash_messages.is_empty(),
                    low_power: low_power_mode.is_some_and(|low_power| low_power.0),
                };
                let timeout_limit = loop_timeout_limit_ms(activity);
                let next_timeout = timeout.min(timeout_limit) + LOOP_TIMEOUT_STEP;
                if let Some(diagnostics) = diagnostics.as_mut() {
                    diagnostics.record(classify_timeout_wake(activity));
                    diagnostics.record_timeout_policy(activity, next_timeout, timeout_limit);
                }
                *timeout = next_timeout;
                break;
            }
        }
    }
}

#[allow(clippy::needless_pass_by_value, clippy::type_complexity)]
#[instrument(level = Level::TRACE, skip_all)]
pub(super) fn window_resized_update_frame(
    mut messages: MessageReader<Event>,
    mut windows: Query<
        (
            &mut Window,
            Entity,
            &Position,
            &mut Bounds,
            Option<&Unmanaged>,
        ),
        Without<LayoutStrip>,
    >,
    mut workspaces: Query<(&LayoutStrip, &mut Position)>,
) {
    for event in messages.read() {
        let Event::WindowResized { window_id } = event else {
            continue;
        };

        let Some((mut window, entity, position, mut bounds, unmanaged)) = windows
            .iter_mut()
            .find(|window| window.0.id() == *window_id)
        else {
            continue;
        };
        if matches!(unmanaged, Some(Unmanaged::Minimized | Unmanaged::Hidden)) {
            continue;
        }
        let Ok(new_frame) = window.update_frame() else {
            continue;
        };
        let active_strip = workspaces
            .iter_mut()
            .find(|(strip, _)| strip.contains(entity));
        let tabbed = active_strip
            .as_ref()
            .is_some_and(|strip| strip.0.tabbed(entity));

        let old_frame = IRect::from_corners(position.0, position.0 + bounds.0);
        if old_frame.size() != new_frame.size() {
            if tabbed {
                bounds.bypass_change_detection().0 = new_frame.size();
            } else {
                bounds.0 = new_frame.size();
            }
        }

        // If the window was resized, shift LayoutStrip slightly to avoid moving right corner.
        let Some((strip, mut strip_position)) = active_strip else {
            // Floating window, don't nudge the strip.
            continue;
        };
        if tabbed {
            // Native tabs share a single layout slot. Keep the strip anchored
            // and let the tab sync/layout systems propagate the new size.
            continue;
        }

        if old_frame.min.x != new_frame.min.x {
            let shift = (old_frame.size() - new_frame.size()).with_y(0);
            // Search marke: reposition_entity - Updating position directly to reduce jitter.
            strip_position.0.x += shift.x;
        }

        // When the user drags the top edge of a stacked window, we adjust the window above to
        // accomodate.
        let diff = old_frame.min.y - new_frame.min.y;
        if diff.abs() > 0
            && let Some(above_entity) = strip.above(entity)
            && let Ok((_, _, _, mut above_bounds, _)) = windows.get_mut(above_entity)
            && above_bounds.0.y - diff > 200
        {
            above_bounds.0.y -= diff;
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
#[instrument(level = Level::TRACE, skip_all)]
pub(super) fn window_moved_update_frame(
    mut messages: MessageReader<Event>,
    mut windows: Query<
        (&mut Window, &mut Position, &Bounds, Option<&Unmanaged>),
        Without<LayoutStrip>,
    >,
) {
    for event in messages.read() {
        let Event::WindowMoved { window_id } = event else {
            continue;
        };

        let Some((mut window, mut position, bounds, unmanaged)) = windows
            .iter_mut()
            .find(|window| window.0.id() == *window_id)
        else {
            continue;
        };
        if matches!(unmanaged, Some(Unmanaged::Minimized | Unmanaged::Hidden)) {
            continue;
        }
        let Ok(new_frame) = window.update_frame() else {
            continue;
        };

        let old_frame = IRect::from_corners(position.0, position.0 + bounds.0);
        if old_frame.min != new_frame.min {
            position.0 = new_frame.min;
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn gather_initial_processes(
    receiver: Option<NonSendMut<WakeableEventQueue>>,
    mut displays: Query<&mut Display>,
    mut commands: Commands,
) {
    let Some(receiver) = receiver else {
        // Probably running in a mock environment, ignore.
        return;
    };
    let mut initial_processes: Vec<BProcess> = Vec::new();
    let mut initial_config = None;
    loop {
        match receiver.recv().expect("error reading initial processes") {
            Event::ProcessesLoaded | Event::Exit => break,
            Event::ApplicationLaunched { psn, observer } => {
                initial_processes.push(Process::new(&psn, observer.clone()).into());
            }
            Event::InitialConfig(config) => {
                // If there is a display menubar override, apply it to newly created displays.
                let height = config.menubar_height();
                for mut display in &mut displays {
                    display.set_menubar_height_override(height);
                }

                initial_config = Some(config);
            }
            event => warn!("Stray event during initial process gathering: {event:?}"),
        }
    }
    if let Some(config) = initial_config {
        commands.insert_resource(config);
    }

    while let Some(mut process) = initial_processes.pop() {
        if process.is_observable() {
            debug!("Adding existing process {}", process.name());
            commands.spawn((ExistingMarker, process));
        } else {
            debug!(
                "Existing application '{}' is not observable, ignoring it.",
                process.name(),
            );
        }
    }
}

#[derive(Default)]
pub(super) struct OverlayWindowConfigCache {
    window_id: Option<WinID>,
    focused_border_radius: Option<f64>,
    detected_border_radius: Option<f64>,
}

#[allow(clippy::needless_pass_by_value, clippy::type_complexity)]
pub(super) fn update_overlays(
    // Gating lives in the `overlay_dirty` run condition (strip change *or*
    // focus change); this query just resolves the current active workspace.
    active_workspace: Populated<(Has<Scrolling>, &LayoutStrip), With<ActiveWorkspaceMarker>>,
    windows: Windows,
    applications: Query<&Application>,
    overlay_mgr: Option<NonSendMut<OverlayManager>>,
    mission_control_active: Res<MissionControlActive>,
    config: Res<Config>,
    mut window_config_cache: Local<OverlayWindowConfigCache>,
) {
    use crate::overlay::BorderParams;
    use objc2_foundation::{NSPoint, NSRect, NSSize};

    let Some(mut overlay_mgr) = overlay_mgr else {
        return;
    };

    let dim_opacity = config.dim_inactive_opacity();
    let border_enabled = config.border_active_window();

    // Hide overlays during swipe, mission control, native fullscreen spaces,
    // or briefly after a space change (macOS space-switch animation).
    let Some((swiping, active_strip)) = active_workspace.iter().next() else {
        return;
    };

    if swiping || mission_control_active.0 || active_strip.is_fullscreen() {
        overlay_mgr.hide_all();
        return;
    }

    if dim_opacity == 0.0 && !border_enabled {
        overlay_mgr.remove_all();
        return;
    }

    // Find the focused managed window's absolute CG frame.
    // Skip floating/unmanaged windows — no overlay or border for those.
    let (focused_abs_cg, focused_window_id) = if let Some((window, _, unmanaged)) = windows
        .focused()
        .and_then(|(_, entity)| windows.get_managed(entity))
        && unmanaged.is_none()
        && !window.is_full_screen()
    {
        let frame = window.frame();
        let h_pad = window.horizontal_padding();
        let v_pad = window.vertical_padding();
        let focused_abs_cg = Some(NSRect::new(
            NSPoint::new(
                f64::from(frame.min.x + h_pad),
                f64::from(frame.min.y + v_pad),
            ),
            NSSize::new(
                f64::from(frame.width() - 2 * h_pad),
                f64::from(frame.height() - 2 * v_pad),
            ),
        ));

        (focused_abs_cg, window.id())
    } else {
        // No managed window has focus — hide the overlay rather than
        // dimming everything (e.g. during startup or when only floating
        // windows exist).
        overlay_mgr.hide_all();
        return;
    };

    let border_params = if border_enabled {
        if window_config_cache.window_id != Some(focused_window_id) || config.is_changed() {
            let Some((window, _, parent)) = windows.find_parent(focused_window_id) else {
                return;
            };
            let Ok(app) = applications.get(parent) else {
                return;
            };
            let properties = WindowProperties::new(app, window, &config);
            window_config_cache.window_id = Some(focused_window_id);
            window_config_cache.focused_border_radius = properties.border_radius();
            window_config_cache.detected_border_radius = window.border_radius();
        }

        let calculated_radius = match config.border_radius() {
            BorderRadiusOption::Auto => window_config_cache.detected_border_radius.unwrap_or(10.0),
            BorderRadiusOption::Value(value) => value.max(0.0),
        };

        Some(BorderParams {
            color: config.border_color(),
            opacity: config.border_opacity(),
            width: config.border_width(),
            radius: window_config_cache
                .focused_border_radius
                .unwrap_or(calculated_radius),
        })
    } else {
        window_config_cache.window_id = None;
        None
    };

    let dim_color = config.dim_inactive_color();
    overlay_mgr.update(
        dim_opacity,
        dim_color,
        focused_abs_cg,
        border_params.as_ref(),
    );
}

#[instrument(level = Level::TRACE, skip_all)]
pub(super) fn commit_window_position(
    mut moved_windows: Populated<(&mut Window, &Position), Changed<Position>>,
) {
    moved_windows
        .par_iter_mut()
        .for_each(|(mut window, position)| window.reposition(position.0));
}

#[allow(clippy::needless_pass_by_value)]
#[instrument(level = Level::TRACE, skip_all)]
pub(super) fn verify_window_position(
    mut windows: Populated<(Entity, &mut Window, &Position, &mut VerifyWindowPosition)>,
    mut commands: Commands,
) {
    for (entity, mut window, position, mut verification) in &mut windows {
        if window
            .update_frame()
            .is_ok_and(|frame| frame.min == position.0)
        {
            commands.entity(entity).try_remove::<VerifyWindowPosition>();
            continue;
        }

        window.reposition(position.0);
        if verification.tick() {
            commands.entity(entity).try_remove::<VerifyWindowPosition>();
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
#[instrument(level = Level::TRACE, skip_all)]
pub(super) fn commit_window_size(
    active_display: ActiveDisplay,
    mut resized_windows: Populated<(&mut Window, &Bounds, &mut WidthRatio), Changed<Bounds>>,
) {
    let display_bounds = active_display.bounds();
    resized_windows
        .par_iter_mut()
        .for_each(|(mut window, size, mut width_ratio)| {
            width_ratio.0 = f64::from(size.0.x) / f64::from(display_bounds.width());
            window.resize(size.0);
        });
}

/// Restores user-visible window state before Paneru shuts down: clears any
/// brightness dim, removes the dim/border overlay window, and centers every
/// managed window on the display its frame center falls in.
#[allow(clippy::needless_pass_by_value)]
pub(super) fn cleanup_on_exit(
    mut exit_events: MessageReader<AppExit>,
    mut all_windows: Query<&mut Window>,
    displays: Query<&Display>,
    window_manager: Res<WindowManager>,
    mut overlay_mgr: Option<NonSendMut<OverlayManager>>,
) {
    for _ in exit_events.read() {
        let ids = all_windows.iter().map(|w| w.id()).collect::<Vec<_>>();
        info!("exit cleanup: restoring {} window(s)", ids.len());
        window_manager.dim_windows(&ids, 0.0);

        if let Some(ref mut overlay_mgr) = overlay_mgr {
            overlay_mgr.remove_all();
        }

        let display_bounds = displays.iter().map(Display::bounds).collect::<Vec<_>>();
        if display_bounds.is_empty() {
            return;
        }

        for mut window in &mut all_windows {
            let frame = window.frame();
            let center = frame.center();
            let bounds = display_bounds
                .iter()
                .find(|b| {
                    center.x >= b.min.x
                        && center.x <= b.max.x
                        && center.y >= b.min.y
                        && center.y <= b.max.y
                })
                .copied()
                .unwrap_or(display_bounds[0]);

            let mut size = frame.size();
            if size.x > bounds.width() || size.y > bounds.height() {
                let new_size = bevy::math::IVec2::new(
                    size.x.min(bounds.width() * 9 / 10),
                    size.y.min(bounds.height() * 9 / 10),
                );
                window.resize(new_size);
                size = new_size;
            }

            let origin = bevy::math::IVec2::new(
                bounds.min.x + (bounds.width() - size.x) / 2,
                bounds.min.y + (bounds.height() - size.y) / 2,
            );
            info!(
                "exit cleanup: window {} -> origin {:?}, size {:?}",
                window.id(),
                origin,
                size
            );
            window.reposition(origin);
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn update_flash_messages(
    messages: Populated<(Entity, &FlashMessage, &Timeout)>,
    active_display: Single<(&Display, Entity), With<ActiveDisplayMarker>>,
    flash_mgr: Option<NonSendMut<FlashMessageManager>>,
    mut commands: Commands,
) {
    let Some(mut flash_manager) = flash_mgr else {
        return;
    };

    if messages.is_empty() {
        flash_manager.remove();
        return;
    }

    let (display, _) = *active_display;
    let bounds = display.bounds();
    let top_right = NSPoint::new(f64::from(bounds.max.x), f64::from(bounds.min.y));

    // When several FlashMessages coexist (rapid keypresses spawn a fresh
    // one per workspace switch before the previous timer expires), the
    // naïve loop would call `show()` for every one of them in arbitrary
    // order — the OSD ends up flickering between strings, and the moment
    // any one of them expires its `is_finished()` branch calls
    // `flash_manager.remove()` even though the newer ones are still
    // alive. Keep the newest (most time remaining), despawn the rest,
    // and render exactly once.
    let mut alive: Option<(Entity, &str, &Timeout)> = None;
    let mut stale: Vec<Entity> = Vec::new();
    for (entity, FlashMessage(flash), timeout) in messages {
        if timeout.timer.is_finished() {
            stale.push(entity);
            continue;
        }
        match alive {
            None => alive = Some((entity, flash, timeout)),
            Some((prev_entity, _, prev_timeout)) => {
                if timeout.timer.remaining() > prev_timeout.timer.remaining() {
                    stale.push(prev_entity);
                    alive = Some((entity, flash, timeout));
                } else {
                    stale.push(entity);
                }
            }
        }
    }

    for entity in stale {
        commands.entity(entity).despawn();
    }

    if let Some((_, flash, timeout)) = alive {
        let opacity = timeout.timer.fraction_remaining();
        flash_manager.show(flash, opacity, top_right);
    } else {
        flash_manager.remove();
    }
}

pub(crate) fn update_low_power_state(low_power_mode: Option<ResMut<LowPowerMode>>) {
    let Some(mut state) = low_power_mode else {
        return;
    };
    let process_info = objc2_foundation::NSProcessInfo::processInfo();
    state.0 = process_info.isLowPowerModeEnabled();
}

#[allow(clippy::needless_pass_by_value)]
#[instrument(level = Level::DEBUG, skip_all)]
pub(crate) fn window_creation_event(mut messages: MessageReader<Event>, mut commands: Commands) {
    for event in messages.read() {
        let Event::WindowCreated { element } = event else {
            continue;
        };

        if let Ok(window) = WindowOS::new(element)
            .inspect_err(|err| {
                trace!("not adding window {element:?}: {err}");
            })
            .map(|window| Window::new(Box::new(window)))
        {
            commands.trigger(SpawnWindowTrigger(vec![window]));
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn detect_tabbed_windows(
    created: Populated<(Entity, &Position, &Bounds, &ChildOf), Added<Window>>,
    windows: Query<(Entity, &Window, &Position, &Bounds, &ChildOf), With<Window>>,
    apps: Query<Entity, With<Application>>,
    mut workspaces: Query<(&mut LayoutStrip, Has<ActiveWorkspaceMarker>)>,
    window_manager: Res<WindowManager>,
    active_display: Single<&Display, With<ActiveDisplayMarker>>,
    mut commands: Commands,
) {
    let display_bounds = active_display.bounds();
    let Some(workspace_entities) = workspaces
        .iter()
        .find_map(|(strip, active)| active.then_some(strip.all_windows()))
    else {
        return;
    };

    for (entity, Position(position), Bounds(bounds), child) in created {
        let Ok(app_entity) = apps.get(child.parent()) else {
            continue;
        };

        // First find all the windows which have the same size and the same parent app.
        // .. and in the same workspace.
        let mut same_size = workspace_entities
            .iter()
            .filter_map(|e| windows.get(*e).ok())
            .filter(|(leader, _, _, Bounds(leader_bounds), child)| {
                *leader != entity
                    && child.parent() == app_entity
                    && leader_bounds.chebyshev_distance(*bounds) <= 1
            })
            .collect::<Vec<_>>();

        // Now check whether any of these found windows have the same position?
        let tabbed = same_size
            .iter()
            .find_map(|(leader, window, Position(leader_position), _, _)| {
                // If the window has a positional match, it's tabbed!
                (leader_position.chebyshev_distance(*position) <= 1)
                    .then_some((*leader, window.id()))
            })
            .or_else(|| {
                // Otherwise if no windows were found by position, sort all the windows by distance
                // and then pick the one which is currently offscreen.
                // This heuristic relaxes the position matching, because the window is bumped into view.
                same_size.sort_by_key(|(_, _, Position(candidate_position), _, _)| {
                    position.x.abs_diff(candidate_position.x)
                });
                same_size.into_iter().find_map(
                    |(leader, window, Position(leader_position), Bounds(leader_bounds), _)| {
                        let offscreen = !display_bounds.contains(*leader_position)
                            || !display_bounds.contains(*leader_position + leader_bounds);
                        offscreen.then_some((leader, window.id()))
                    },
                )
            });

        if let Some((leader, leader_id)) = tabbed
            && window_manager
                .windows_on_screen()
                .is_some_and(|ids| !ids.contains(&leader_id))
            && let Some((mut strip, _)) =
                workspaces.iter_mut().find(|strip| strip.0.contains(leader))
            && strip.contains(leader)
        {
            debug!("Tabbed window detected: adding {entity} to leader {leader}");
            if strip
                .convert_to_tabs(leader, entity)
                .inspect_err(|err| error!("Failed to convert to tabs: {err}"))
                .is_ok()
            {
                commands.focus_entity(entity, false);
            }
        }
    }
}

#[derive(Clone, Copy)]
struct TabReconcileCandidate {
    entity: Entity,
    app_entity: Entity,
    window_id: WinID,
    frame: IRect,
    focused: bool,
    on_screen: bool,
    strip_order: usize,
}

/// Reconciles native tab groups that were missed during the creation-time race.
///
/// Some apps expose each native tab as an AX window while only the selected tab
/// has a real CG on-screen window. The creation-time detector can miss that if
/// `CGWindowList` still reports the previous tab as on-screen. This pass is a
/// deliberately conservative second chance: it only groups same-app windows in
/// the active strip when their current frames overlap and exactly one of them is
/// on-screen.
#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    clippy::type_complexity
)]
pub(crate) fn reconcile_tabbed_windows(
    windows: Query<(Entity, &Window, &ChildOf, Has<FocusedMarker>), With<Window>>,
    mut workspaces: ParamSet<(
        Query<(Entity, &LayoutStrip, Has<ActiveWorkspaceMarker>)>,
        Query<&mut LayoutStrip>,
    )>,
    mut apps: Query<&mut Application>,
    window_manager: Res<WindowManager>,
    mut stale_tab_candidates: Local<HashMap<Entity, u8>>,
    mut visible_tab_groups: Local<HashMap<Vec<Entity>, u8>>,
    mut commands: Commands,
) {
    let Some(on_screen_ids) = window_manager.windows_on_screen() else {
        return;
    };
    let on_screen_ids = on_screen_ids.into_iter().collect::<HashSet<_>>();

    let (active_strip_entity, reconciliations, stale_entities, missing_windows, split_groups) = {
        let query = workspaces.p0();
        let mut active_strip = None;
        let mut all_candidates = Vec::new();
        let mut strip_tab_groups = Vec::new();

        for (strip_entity, strip, active) in &query {
            if active {
                active_strip = Some((strip_entity, strip));
            }
            let mut tab_seen = HashSet::new();
            let tab_groups = strip
                .all_windows()
                .into_iter()
                .filter_map(|entity| {
                    if tab_seen.contains(&entity) {
                        return None;
                    }
                    let entities = strip.tab_group(entity)?;
                    tab_seen.extend(entities.iter().copied());
                    let index = entities
                        .iter()
                        .filter_map(|entity| strip.index_of(*entity).ok())
                        .min()
                        .unwrap_or_else(|| strip.len());
                    Some((index, entities))
                })
                .collect::<Vec<_>>();
            if !tab_groups.is_empty() {
                strip_tab_groups.push((strip_entity, tab_groups));
            }
            all_candidates.extend(strip.all_windows().into_iter().enumerate().filter_map(
                |(strip_order, entity)| {
                    let (entity, window, child, focused) = windows.get(entity).ok()?;
                    Some((
                        strip_entity,
                        TabReconcileCandidate {
                            entity,
                            app_entity: child.parent(),
                            window_id: window.id(),
                            frame: window.frame(),
                            focused,
                            on_screen: on_screen_ids.contains(&window.id()),
                            strip_order,
                        },
                    ))
                },
            ));
        }

        let Some((active_strip_entity, active_strip)) = active_strip else {
            return;
        };

        let mut live_ids_by_app: HashMap<Entity, HashSet<WinID>> = HashMap::new();
        let mut focused_id_by_app: HashMap<Entity, Option<WinID>> = HashMap::new();
        let mut stale_entities = Vec::new();
        let mut seen_entities = HashSet::new();
        for (strip_entity, candidate) in &all_candidates {
            seen_entities.insert(candidate.entity);
            if candidate.focused || candidate.on_screen || cfg!(test) {
                stale_tab_candidates.remove(&candidate.entity);
                continue;
            }

            let has_live_tab_sibling_in_strip =
                all_candidates.iter().any(|(other_strip, other)| {
                    *other_strip == *strip_entity
                        && other.entity != candidate.entity
                        && other.app_entity == candidate.app_entity
                        && other.on_screen
                        && frames_match(candidate.frame, other.frame)
                });
            if has_live_tab_sibling_in_strip {
                stale_tab_candidates.remove(&candidate.entity);
                continue;
            }

            let has_live_same_app_window = all_candidates.iter().any(|(_, other)| {
                other.entity != candidate.entity
                    && other.app_entity == candidate.app_entity
                    && other.on_screen
            });
            if !has_live_same_app_window {
                stale_tab_candidates.remove(&candidate.entity);
                continue;
            }

            let focused_id = *focused_id_by_app
                .entry(candidate.app_entity)
                .or_insert_with(|| {
                    apps.get(candidate.app_entity)
                        .ok()
                        .and_then(|app| app.focused_window_id().ok())
                });
            let live_ids = live_ids_by_app
                .entry(candidate.app_entity)
                .or_insert_with(|| {
                    apps.get(candidate.app_entity)
                        .map(|app| {
                            app.window_list()
                                .into_iter()
                                .map(|window| window.id())
                                .collect::<HashSet<_>>()
                        })
                        .unwrap_or_default()
                });

            if focused_id == Some(candidate.window_id)
                || live_ids.is_empty()
                || live_ids.contains(&candidate.window_id)
            {
                stale_tab_candidates.remove(&candidate.entity);
                continue;
            }

            let count = stale_tab_candidates
                .entry(candidate.entity)
                .and_modify(|count| *count = (*count + 1).min(3))
                .or_insert(1);
            if *count >= 2 {
                stale_entities.push((
                    *strip_entity,
                    candidate.entity,
                    candidate.window_id,
                    candidate.app_entity,
                    candidate.focused,
                ));
            }
        }
        stale_tab_candidates.retain(|entity, _| seen_entities.contains(entity));

        let mut missing_windows = Vec::new();
        if !cfg!(test) {
            let represented_ids = all_candidates
                .iter()
                .map(|(_, candidate)| candidate.window_id)
                .collect::<HashSet<_>>();
            let app_entities = all_candidates
                .iter()
                .map(|(_, candidate)| candidate.app_entity)
                .collect::<HashSet<_>>();
            for app_entity in app_entities {
                let focused_id = *focused_id_by_app.entry(app_entity).or_insert_with(|| {
                    apps.get(app_entity)
                        .ok()
                        .and_then(|app| app.focused_window_id().ok())
                });
                if let Ok(app) = apps.get(app_entity) {
                    missing_windows.extend(app.window_list().into_iter().filter(|window| {
                        !represented_ids.contains(&window.id())
                            && (on_screen_ids.contains(&window.id())
                                || focused_id == Some(window.id()))
                    }));
                }
            }
        }

        let candidates = all_candidates
            .iter()
            .filter_map(|(strip_entity, candidate)| {
                (*strip_entity == active_strip_entity).then_some(*candidate)
            })
            .collect::<Vec<_>>();
        let candidates_by_entity = all_candidates
            .iter()
            .map(|(_, candidate)| (candidate.entity, *candidate))
            .collect::<HashMap<_, _>>();

        let mut split_groups = Vec::new();
        let mut visible_tab_group_keys = HashSet::new();
        for (strip_entity, tab_groups) in strip_tab_groups {
            for (index, entities) in tab_groups {
                let group = entities
                    .iter()
                    .filter_map(|entity| candidates_by_entity.get(entity).copied())
                    .collect::<Vec<_>>();
                if group.len() != entities.len() {
                    continue;
                }
                let on_screen_count = group.iter().filter(|candidate| candidate.on_screen).count();
                if on_screen_count <= 1 {
                    continue;
                }

                let key = entities.clone();
                visible_tab_group_keys.insert(key.clone());
                let count = visible_tab_groups
                    .entry(key)
                    .and_modify(|count| *count = (*count + 1).min(3))
                    .or_insert(1);
                if *count >= 2 {
                    let window_ids = group
                        .iter()
                        .map(|candidate| candidate.window_id)
                        .collect::<Vec<_>>();
                    split_groups.push((strip_entity, index, entities, window_ids));
                }
            }
        }
        visible_tab_groups.retain(|key, _| visible_tab_group_keys.contains(key));

        let mut grouped = HashSet::new();
        let mut reconciliations = Vec::new();
        for candidate in &candidates {
            if grouped.contains(&candidate.entity) {
                continue;
            }

            let mut group = candidates
                .iter()
                .copied()
                .filter(|other| {
                    other.entity != candidate.entity
                        && !grouped.contains(&other.entity)
                        && other.app_entity == candidate.app_entity
                        && frames_match(candidate.frame, other.frame)
                })
                .chain(std::iter::once(*candidate))
                .collect::<Vec<_>>();

            if group.len() < 2 {
                continue;
            }

            let on_screen_count = group.iter().filter(|candidate| candidate.on_screen).count();
            if on_screen_count != 1 {
                continue;
            }

            group.sort_by_key(|candidate| candidate.strip_order);
            let entities = ordered_tab_group(&group);
            if active_strip.tab_group(entities[0]).as_deref() == Some(entities.as_slice()) {
                grouped.extend(entities);
                continue;
            }

            let index = group
                .iter()
                .filter_map(|candidate| active_strip.index_of(candidate.entity).ok())
                .min()
                .unwrap_or_else(|| active_strip.len());
            let window_ids = group
                .iter()
                .map(|candidate| candidate.window_id)
                .collect::<Vec<_>>();
            reconciliations.push((index, entities.clone(), window_ids));
            grouped.extend(entities);
        }

        (
            active_strip_entity,
            reconciliations,
            stale_entities,
            missing_windows,
            split_groups,
        )
    };

    if !missing_windows.is_empty() {
        let window_ids = missing_windows
            .iter()
            .map(|window| window.id())
            .collect::<Vec<_>>();
        info!("Recovering missing live native-tab window(s): {window_ids:?}");
        commands.trigger(SpawnWindowTrigger(missing_windows));
    }

    if reconciliations.is_empty() && stale_entities.is_empty() && split_groups.is_empty() {
        return;
    }

    let mut query = workspaces.p1();
    for (strip_entity, entity, window_id, app_entity, focused) in stale_entities {
        info!(
            "Removing stale native-tab candidate window {window_id} entity {entity} focused={focused}"
        );
        if let Ok(mut strip) = query.get_mut(strip_entity)
            && strip.contains(entity)
        {
            strip.remove(entity);
        }
        if let Ok((_, window, _, _)) = windows.get(entity)
            && let Ok(mut app) = apps.get_mut(app_entity)
        {
            app.unobserve_window(window);
        }
        if let Ok(mut entity_commands) = commands.get_entity(entity) {
            entity_commands.try_despawn();
        }
        stale_tab_candidates.remove(&entity);
    }

    if split_groups.is_empty() && reconciliations.is_empty() {
        return;
    }

    for (strip_entity, index, entities, window_ids) in split_groups {
        info!("Splitting visible native-tab group back into windows: {window_ids:?}");
        if let Ok(mut strip) = query.get_mut(strip_entity) {
            for entity in &entities {
                strip.remove(*entity);
            }
            for (offset, entity) in entities.into_iter().enumerate() {
                strip.insert_at(index + offset, entity);
            }
        }
    }

    if reconciliations.is_empty() {
        return;
    }
    let Ok(mut strip) = query.get_mut(active_strip_entity) else {
        return;
    };
    for (index, entities, window_ids) in reconciliations {
        debug!("Reconciling native tab group: {window_ids:?}");
        strip.insert_tab_group_at(index, &entities);
    }
}

fn frames_match(lhs: IRect, rhs: IRect) -> bool {
    lhs.min.chebyshev_distance(rhs.min) <= 1 && lhs.size().chebyshev_distance(rhs.size()) <= 1
}

fn ordered_tab_group(group: &[TabReconcileCandidate]) -> Vec<Entity> {
    let mut ordered = group.to_vec();
    ordered.sort_by_key(|candidate| {
        (
            !candidate.on_screen,
            !candidate.focused,
            candidate.strip_order,
        )
    });
    ordered
        .into_iter()
        .map(|candidate| candidate.entity)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_policy_uses_frame_deadline_for_active_work() {
        for activity in [
            LoopActivity {
                repositioning: true,
                ..LoopActivity::default()
            },
            LoopActivity {
                resizing: true,
                ..LoopActivity::default()
            },
            LoopActivity {
                scrolling: true,
                ..LoopActivity::default()
            },
            LoopActivity {
                flash_message: true,
                low_power: true,
                ..LoopActivity::default()
            },
        ] {
            assert_eq!(
                loop_timeout_limit_ms(activity),
                LOOP_MAX_TIMEOUT_FRAME_ACTIVE_MS
            );
        }
    }

    #[test]
    fn timeout_policy_distinguishes_idle_and_low_power() {
        assert_eq!(
            loop_timeout_limit_ms(LoopActivity::default()),
            LOOP_MAX_TIMEOUT_MS
        );
        assert_eq!(
            loop_timeout_limit_ms(LoopActivity {
                low_power: true,
                ..LoopActivity::default()
            }),
            LOOP_MAX_TIMEOUT_LOWPOWER_MS
        );
    }

    #[test]
    fn timeout_wake_classification_prefers_specific_active_reasons() {
        assert_eq!(
            classify_timeout_wake(LoopActivity {
                repositioning: true,
                ..LoopActivity::default()
            }),
            LoopWakeReason::FrameActive
        );
        assert_eq!(
            classify_timeout_wake(LoopActivity {
                flash_message: true,
                repositioning: true,
                ..LoopActivity::default()
            }),
            LoopWakeReason::FlashMessageActive
        );
        assert_eq!(
            classify_timeout_wake(LoopActivity::default()),
            LoopWakeReason::TimeoutWatchdog
        );
    }

    #[test]
    fn native_tab_deadline_disabled_registers_no_deadline() {
        let mut deadlines = RuntimeDeadlines::default();
        let now = Duration::from_secs(10);

        apply_native_tab_deadline_plan(
            &mut deadlines,
            now,
            native_tab_deadline_plan(NativeTabDeadlineInputs {
                enabled: false,
                has_tab_groups: true,
                recent_activity: true,
                reconcile: Duration::from_millis(250),
                safety: Duration::from_secs(30),
            }),
        );

        assert_eq!(deadlines.earliest_wait(now), None);
    }

    #[test]
    fn native_tab_deadline_idle_without_groups_uses_safety_fallback() {
        let mut deadlines = RuntimeDeadlines::default();
        let now = Duration::from_secs(10);
        let plan = native_tab_deadline_plan(NativeTabDeadlineInputs {
            enabled: true,
            has_tab_groups: false,
            recent_activity: false,
            reconcile: Duration::from_millis(250),
            safety: Duration::from_secs(30),
        });

        apply_native_tab_deadline_plan(&mut deadlines, now, plan);

        assert_eq!(plan.diagnostic, "safety_check");
        assert_eq!(
            deadlines.earliest_wait(now),
            Some(crate::runtime_driver::WaitDeadline {
                duration: Duration::from_secs(30),
                reason: DeadlineReason::NativeTabReconciliation,
            })
        );
    }

    #[test]
    fn native_tab_deadline_active_group_uses_reconcile_cadence() {
        let plan = native_tab_deadline_plan(NativeTabDeadlineInputs {
            enabled: true,
            has_tab_groups: true,
            recent_activity: false,
            reconcile: Duration::from_millis(250),
            safety: Duration::from_secs(30),
        });

        assert_eq!(
            plan,
            NativeTabDeadlinePlan {
                cadence: Some(Duration::from_millis(250)),
                diagnostic: "active_tab_group",
            }
        );
    }

    #[test]
    fn native_tab_deadline_recent_activity_uses_bounded_reconcile_cadence() {
        let plan = native_tab_deadline_plan(NativeTabDeadlineInputs {
            enabled: true,
            has_tab_groups: false,
            recent_activity: true,
            reconcile: Duration::from_millis(250),
            safety: Duration::from_secs(30),
        });

        assert_eq!(
            plan,
            NativeTabDeadlinePlan {
                cadence: Some(Duration::from_millis(250)),
                diagnostic: "recent_activity",
            }
        );
    }

    #[test]
    fn native_tab_deadline_is_not_postponed_by_repeated_activity() {
        let mut deadlines = RuntimeDeadlines::default();
        let now = Duration::from_secs(10);
        let plan = native_tab_deadline_plan(NativeTabDeadlineInputs {
            enabled: true,
            has_tab_groups: false,
            recent_activity: true,
            reconcile: Duration::from_millis(250),
            safety: Duration::from_secs(30),
        });

        assert!(apply_native_tab_deadline_plan(&mut deadlines, now, plan));
        assert_eq!(
            deadlines.deadline_at(DeadlineReason::NativeTabReconciliation),
            Some(now + Duration::from_millis(250))
        );

        assert!(!apply_native_tab_deadline_plan(
            &mut deadlines,
            now + Duration::from_millis(50),
            plan,
        ));
        assert_eq!(
            deadlines.deadline_at(DeadlineReason::NativeTabReconciliation),
            Some(now + Duration::from_millis(250))
        );
    }

    #[test]
    fn native_tab_deadline_shortens_safety_deadline_on_activity() {
        let mut deadlines = RuntimeDeadlines::default();
        let now = Duration::from_secs(10);
        assert!(apply_native_tab_deadline_plan(
            &mut deadlines,
            now,
            native_tab_deadline_plan(NativeTabDeadlineInputs {
                enabled: true,
                has_tab_groups: false,
                recent_activity: false,
                reconcile: Duration::from_millis(250),
                safety: Duration::from_secs(30),
            }),
        ));

        assert!(apply_native_tab_deadline_plan(
            &mut deadlines,
            now + Duration::from_secs(1),
            native_tab_deadline_plan(NativeTabDeadlineInputs {
                enabled: true,
                has_tab_groups: false,
                recent_activity: true,
                reconcile: Duration::from_millis(250),
                safety: Duration::from_secs(30),
            }),
        ));
        assert_eq!(
            deadlines.deadline_at(DeadlineReason::NativeTabReconciliation),
            Some(now + Duration::from_secs(1) + Duration::from_millis(250))
        );
    }

    #[test]
    fn native_tab_group_detection_observes_layout_tabs() {
        let mut world = bevy::ecs::world::World::new();
        let e1 = world.spawn_empty().id();
        let e2 = world.spawn_empty().id();
        let mut strip = LayoutStrip::default();

        assert!(!strip_has_native_tab_group(&strip));

        strip.append_tab_group(&[e1, e2]);

        assert!(strip_has_native_tab_group(&strip));
    }

    #[test]
    fn always_scheduled_native_tab_reconciliation_caps_quiet_idle_at_250ms() {
        let policy = crate::runtime_driver::RuntimeDriverPolicy::new(
            crate::runtime_driver::RuntimeDriverConfig {
                frame_interval: Duration::from_millis(16),
                interactive_idle_cap: Duration::from_millis(50),
                idle_watchdog_cap: Duration::from_secs(1),
                low_power_watchdog_cap: Duration::from_millis(500),
                interactive_grace_period: Duration::from_secs(1),
            },
        );
        let now = Duration::from_secs(10);

        assert_eq!(
            policy.decide(crate::runtime_driver::RuntimeDriverState {
                now,
                next_visible_deadline: Some(crate::runtime_driver::RuntimeDeadline::new(
                    now + Duration::from_millis(250),
                    DeadlineReason::NativeTabReconciliation,
                )),
                ..crate::runtime_driver::RuntimeDriverState::default()
            }),
            crate::runtime_driver::RunnerDecision::Wait(crate::runtime_driver::WaitDeadline {
                duration: Duration::from_millis(250),
                reason: DeadlineReason::NativeTabReconciliation,
            })
        );
    }

    #[test]
    fn named_watchdog_deadline_runs_only_when_due_and_reschedules() {
        let mut deadlines = RuntimeDeadlines::default();
        let now = Duration::from_secs(10);

        assert!(deadlines.ensure_repeating_after(
            now,
            DeadlineReason::LostFocusWatchdog,
            Duration::from_secs(1),
        ));
        assert!(!deadlines.due(now, DeadlineReason::LostFocusWatchdog));
        assert!(deadlines.due(
            now + Duration::from_secs(1),
            DeadlineReason::LostFocusWatchdog
        ));
        assert!(deadlines.ensure_repeating_after(
            now + Duration::from_secs(1),
            DeadlineReason::LostFocusWatchdog,
            Duration::from_secs(1),
        ));
        assert!(!deadlines.due(
            now + Duration::from_secs(1),
            DeadlineReason::LostFocusWatchdog
        ));
    }

    #[test]
    fn critical_deadline_periods_are_registered_and_rescheduled() {
        let now = Duration::from_secs(10);
        for (reason, period) in [
            (DeadlineReason::LostFocusWatchdog, Duration::from_secs(1)),
            (
                DeadlineReason::OrphanWorkspaceWatchdog,
                Duration::from_secs(1),
            ),
            (DeadlineReason::RefreshWindowSizes, Duration::from_secs(1)),
            (DeadlineReason::StateSave, Duration::from_secs(300)),
            (
                DeadlineReason::PeriodicMaintenance,
                Duration::from_secs(300),
            ),
        ] {
            let mut deadlines = RuntimeDeadlines::default();
            assert!(deadlines.ensure_repeating_after(now, reason, period));
            assert!(!deadlines.due(now, reason));
            assert!(deadlines.due(now + period, reason));
            assert!(deadlines.ensure_repeating_after(now + period, reason, period));
            assert!(!deadlines.due(now + period, reason));
        }
    }

    #[test]
    fn diagnostics_records_due_and_rescheduled_deadlines() {
        let mut diagnostics = LoopDiagnostics::default();
        diagnostics.record_runtime_deadline_update(
            &[
                DeadlineReason::LostFocusWatchdog,
                DeadlineReason::NativeTabReconciliation,
            ],
            &[DeadlineReason::StateSave],
        );

        assert_eq!(
            diagnostics.last_due_deadline_reasons,
            vec!["lost_focus_watchdog", "native_tab_reconciliation"]
        );
        assert_eq!(
            diagnostics.last_rescheduled_deadline_reasons,
            vec!["state_save"]
        );
    }

    #[test]
    fn diagnostics_accumulates_wake_reasons_and_last_timeout_policy() {
        let mut diagnostics = LoopDiagnostics::default();
        diagnostics.record(LoopWakeReason::CocoaEventPump);
        diagnostics.record(LoopWakeReason::InternalEvent);
        diagnostics.record(LoopWakeReason::FrameActive);
        diagnostics.record_timeout_policy(
            LoopActivity {
                scrolling: true,
                ..LoopActivity::default()
            },
            4,
            LOOP_MAX_TIMEOUT_FRAME_ACTIVE_MS,
        );

        assert_eq!(diagnostics.counters.cocoa_event_pump, 1);
        assert_eq!(diagnostics.counters.internal_event, 1);
        assert_eq!(diagnostics.counters.frame_active, 1);
        assert!(diagnostics.last_activity.scrolling);
        assert_eq!(diagnostics.last_timeout_ms, 4);
        assert_eq!(
            diagnostics.last_timeout_limit_ms,
            LOOP_MAX_TIMEOUT_FRAME_ACTIVE_MS
        );
    }
}

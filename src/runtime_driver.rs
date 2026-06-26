use std::pin::Pin;
use std::sync::mpsc::TryRecvError;
use std::time::Duration;

use bevy::app::{App, AppExit, PluginsState};
use bevy::ecs::component::Component;
use bevy::ecs::query::With;
use bevy::ecs::world::World;
use bevy::tasks::tick_global_task_pools_on_main_thread;
use tracing::{debug, trace, warn};

use crate::ecs::{
    FlashMessage, LoopActivity, LoopDiagnostics, LoopWakeReason, LowPowerMode, RepositionMarker,
    ResizeMarker, Scrolling, classify_timeout_wake, loop_timeout_limit_ms,
};
use crate::events::{Event, WakeableEventQueue};
use crate::platform::PlatformCallbacks;

mod activity;
mod deadlines;
mod policy;
mod wait;

pub(crate) use activity::{
    RuntimeActivity, RuntimeActivityReason, RuntimeDirty, RuntimeDirtyReason, RuntimeDriverActive,
};
pub(crate) use deadlines::{RuntimeDeadlineClock, RuntimeDeadlines};
pub use policy::{
    ActiveRuntimeWork, DeadlineReason, RunnerDecision, RuntimeDeadline, RuntimeDriverConfig,
    RuntimeDriverPolicy, RuntimeDriverState, UpdateReason, WaitDeadline,
};
use wait::MainRunLoopWaiter;

struct RuntimeDriver {
    next_decision: RunnerDecision,
    run_loop_waiter: Option<MainRunLoopWaiter>,
}

impl RuntimeDriver {
    fn legacy_equivalent() -> Self {
        Self {
            next_decision: RunnerDecision::Wait(WaitDeadline {
                duration: MIN_OS_WAIT_DURATION,
                reason: DeadlineReason::IdleWatchdog,
            }),
            run_loop_waiter: MainRunLoopWaiter::new(),
        }
    }

    #[cfg(test)]
    const fn without_deadline_timer_for_tests() -> Self {
        Self {
            next_decision: RunnerDecision::Wait(WaitDeadline {
                duration: MIN_OS_WAIT_DURATION,
                reason: DeadlineReason::IdleWatchdog,
            }),
            run_loop_waiter: None,
        }
    }

    fn wait_and_drain_before_update(&mut self, app: &mut App) -> Option<AppExit> {
        let timeout = match self.next_decision {
            RunnerDecision::RunUpdateNow(reason) => {
                trace!(?reason, "runtime policy requested immediate update");
                return None;
            }
            RunnerDecision::Wait(wait) => clamp_os_wait_duration(wait.duration),
            RunnerDecision::Exit => return Some(AppExit::Success),
        };

        if appkit_blocking_wait_forced() {
            pump_platform(app.world_mut(), timeout);
        } else if let Some(waiter) = &mut self.run_loop_waiter {
            let outcome = waiter.wait(timeout);
            if outcome.deadline_fired {
                trace!("runner-owned deadline timer fired");
            }
            drain_platform_events_nonblocking(app.world_mut());
        } else {
            pump_platform(app.world_mut(), timeout);
        }
        if drain_external_events(app.world_mut()) {
            mark_runtime_activity(app.world_mut(), RuntimeActivityReason::ExternalEvent);
            self.note_internal_event();
        }
        None
    }

    fn note_internal_event(&mut self) {
        self.next_decision = RunnerDecision::Wait(WaitDeadline {
            duration: MIN_OS_WAIT_DURATION,
            reason: DeadlineReason::RecentInteractiveActivity,
        });
    }

    fn update_until_settled(&mut self, app: &mut App) -> Option<AppExit> {
        const MAX_DIRTY_SETTLE_UPDATES: usize = 8;

        let mut settle_updates = 0;
        loop {
            app.update();
            if let Some(exit) = app.should_exit() {
                return Some(exit);
            }

            let reasons = take_dirty_reasons(app.world_mut());
            if reasons.is_empty() {
                return None;
            }
            record_dirty_settle(app.world_mut(), &reasons);
            self.note_internal_event();

            if settle_updates >= MAX_DIRTY_SETTLE_UPDATES {
                warn!(
                    reasons = ?dirty_reason_names(&reasons),
                    "runtime dirty settle guard reached; waiting instead of spinning forever"
                );
                record_dirty_settle_guard(app.world_mut());
                return None;
            }

            debug!(
                reasons = ?dirty_reason_names(&reasons),
                "runtime dirty requested immediate follow-up update"
            );
            settle_updates += 1;
        }
    }

    fn compute_next_wait_after_update(&mut self, app: &mut App) {
        let activity = collect_loop_activity(app.world_mut());
        let timeout_limit = loop_timeout_limit_ms(activity);
        let visible_deadline = next_visible_runtime_deadline(app.world_mut());
        let next_decision =
            self.guard_repeated_immediate_deadline(Self::next_decision_after_update(
                app.world_mut(),
                activity,
                visible_deadline,
                legacy_idle_cadence_forced(),
            ));
        let next_timeout = scheduled_timeout_ms(next_decision);
        let activity_snapshot = runtime_activity_snapshot(app.world_mut());
        if let Some(mut diagnostics) = app.world_mut().get_resource_mut::<LoopDiagnostics>() {
            diagnostics.record(classify_timeout_wake(activity));
            diagnostics.record_timeout_policy(activity, next_timeout, timeout_limit);
            diagnostics.record_runner_deadline(visible_deadline);
            diagnostics.record_runtime_activity(activity_snapshot);
        }
        self.next_decision = next_decision;
    }

    fn next_decision_after_update(
        world: &mut World,
        activity: LoopActivity,
        visible_deadline: Option<WaitDeadline>,
        legacy_idle_cadence_forced: bool,
    ) -> RunnerDecision {
        production_policy(legacy_idle_cadence_forced).decide(runtime_driver_state_after_update(
            world,
            activity,
            visible_deadline,
        ))
    }

    fn guard_repeated_immediate_deadline(&self, decision: RunnerDecision) -> RunnerDecision {
        // A freshly due visible deadline should run Bevy immediately so the
        // owning system can perform or reschedule its work. If the same due
        // deadline survives that update, clamp to the minimum OS wait instead
        // of creating a zero-timeout CPU spin on stale deadline state.
        match (self.next_decision, decision) {
            (
                RunnerDecision::RunUpdateNow(UpdateReason::DeadlineElapsed(previous)),
                RunnerDecision::RunUpdateNow(UpdateReason::DeadlineElapsed(current)),
            ) if previous == current => RunnerDecision::Wait(WaitDeadline {
                duration: MIN_OS_WAIT_DURATION,
                reason: current,
            }),
            _ => decision,
        }
    }

    #[cfg(test)]
    fn next_timeout_ms(
        world: &mut World,
        activity: LoopActivity,
        visible_deadline: Option<WaitDeadline>,
    ) -> u32 {
        scheduled_timeout_ms(Self::next_decision_after_update(
            world,
            activity,
            visible_deadline,
            false,
        ))
    }
}

fn take_dirty_reasons(world: &mut World) -> Vec<RuntimeDirtyReason> {
    world
        .get_resource_mut::<RuntimeDirty>()
        .map_or_else(Vec::new, |mut dirty| dirty.take())
}

fn dirty_reason_names(reasons: &[RuntimeDirtyReason]) -> Vec<&'static str> {
    reasons.iter().map(|reason| reason.as_str()).collect()
}

fn record_dirty_settle(world: &mut World, reasons: &[RuntimeDirtyReason]) {
    let names = dirty_reason_names(reasons);
    if let Some(mut diagnostics) = world.get_resource_mut::<LoopDiagnostics>() {
        diagnostics.record_dirty_settle(&names);
    }
}

fn record_dirty_settle_guard(world: &mut World) {
    if let Some(mut diagnostics) = world.get_resource_mut::<LoopDiagnostics>() {
        diagnostics.record_dirty_settle_guard();
    }
}

pub(crate) fn mark_runtime_activity(world: &mut World, reason: RuntimeActivityReason) {
    let now = world.resource::<RuntimeDeadlineClock>().now();
    if let Some(mut activity) = world.get_resource_mut::<RuntimeActivity>() {
        activity.mark(now, reason);
    }
}

fn runtime_driver_state_after_update(
    world: &mut World,
    activity: LoopActivity,
    visible_deadline: Option<WaitDeadline>,
) -> RuntimeDriverState {
    let now = world.resource::<RuntimeDeadlineClock>().now();
    RuntimeDriverState {
        now,
        active_work: ActiveRuntimeWork {
            repositioning: activity.repositioning,
            resizing: activity.resizing,
            scrolling: activity.scrolling,
            flash_message: activity.flash_message,
        },
        last_interactive_activity: world
            .get_resource::<RuntimeActivity>()
            .and_then(RuntimeActivity::last_activity),
        next_visible_deadline: visible_deadline.map(|deadline| RuntimeDeadline {
            at: now + deadline.duration,
            reason: deadline.reason,
        }),
        low_power: activity.low_power,
        ..RuntimeDriverState::default()
    }
}

pub(crate) fn runtime_activity_snapshot(world: &mut World) -> (bool, Vec<&'static str>) {
    let now = world.resource::<RuntimeDeadlineClock>().now();
    world.get_resource::<RuntimeActivity>().map_or_else(
        || (false, Vec::new()),
        |activity| {
            (
                activity.recent(
                    now,
                    RuntimeDriverConfig::legacy_equivalent().interactive_grace_period,
                ),
                activity.reason_names(),
            )
        },
    )
}

fn next_visible_runtime_deadline(world: &mut World) -> Option<WaitDeadline> {
    let now = world.resource::<RuntimeDeadlineClock>().now();
    world.resource::<RuntimeDeadlines>().earliest_wait(now)
}

const MIN_OS_WAIT_DURATION: Duration = Duration::from_millis(1);

fn production_policy(legacy_idle_cadence_forced: bool) -> RuntimeDriverPolicy {
    RuntimeDriverPolicy::new(RuntimeDriverConfig::production(legacy_idle_cadence_forced))
}

fn scheduled_timeout_ms(decision: RunnerDecision) -> u32 {
    match decision {
        RunnerDecision::Wait(wait) => duration_to_timeout_ms(clamp_os_wait_duration(wait.duration)),
        RunnerDecision::RunUpdateNow(_) | RunnerDecision::Exit => 0,
    }
}

fn clamp_os_wait_duration(duration: Duration) -> Duration {
    duration.max(MIN_OS_WAIT_DURATION)
}

fn duration_to_timeout_ms(duration: Duration) -> u32 {
    if duration.is_zero() {
        return 0;
    }

    let whole_millis = duration.as_millis();
    let rounded_millis = if duration.subsec_nanos().is_multiple_of(1_000_000) {
        whole_millis
    } else {
        whole_millis.saturating_add(1)
    };
    rounded_millis.try_into().unwrap_or(u32::MAX)
}

/// Returns whether the custom runner should be installed for this process.
#[must_use]
pub const fn custom_runtime_driver_enabled_for_env(legacy_wait_env_present: bool) -> bool {
    !legacy_wait_env_present
}

/// Installs the custom legacy-equivalent top-level runner unless the fallback
/// legacy in-system wait environment knob is present.
pub fn install_custom_runtime_driver(app: &mut App) {
    if !custom_runtime_driver_enabled() {
        debug!("using legacy in-system runtime wait path");
        return;
    }

    debug!("using custom top-level runtime driver with legacy-equivalent timing");
    app.init_resource::<RuntimeDirty>();
    app.init_resource::<RuntimeDeadlineClock>();
    app.init_resource::<RuntimeDeadlines>();
    app.init_resource::<RuntimeActivity>();
    app.insert_resource(RuntimeDriverActive);
    app.set_runner(custom_runtime_runner);
}

fn custom_runtime_driver_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        custom_runtime_driver_enabled_for_env(
            std::env::var_os("PANERU_LEGACY_IN_SYSTEM_WAIT").is_some(),
        )
    })
}

#[must_use]
pub const fn appkit_blocking_wait_forced_for_env(env_present: bool) -> bool {
    env_present
}

fn appkit_blocking_wait_forced() -> bool {
    static FORCED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FORCED.get_or_init(|| {
        appkit_blocking_wait_forced_for_env(
            std::env::var_os("PANERU_APPKIT_BLOCKING_WAIT").is_some(),
        )
    })
}

#[must_use]
pub const fn legacy_idle_cadence_forced_for_env(env_present: bool) -> bool {
    env_present
}

fn legacy_idle_cadence_forced() -> bool {
    static FORCED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FORCED.get_or_init(|| {
        legacy_idle_cadence_forced_for_env(std::env::var_os("PANERU_LEGACY_IDLE_CADENCE").is_some())
    })
}

const fn adaptive_quiet_idle_cap_ms() -> u32 {
    1000
}

fn custom_runtime_runner(mut app: App) -> AppExit {
    prepare_plugins(&mut app);

    // Let Startup run before the custom runner drains the external queue.
    // `gather_initial_processes` intentionally reads the boot-time process and
    // config events directly from `WakeableEventQueue`; pre-draining here would
    // strand startup waiting for `ProcessesLoaded` that had already been moved
    // into Bevy messages.
    let mut driver = RuntimeDriver::legacy_equivalent();
    if let Some(exit) = driver.update_until_settled(&mut app) {
        return exit;
    }

    driver.compute_next_wait_after_update(&mut app);
    loop {
        if let Some(exit) = driver.wait_and_drain_before_update(&mut app) {
            return exit;
        }
        if let Some(exit) = driver.update_until_settled(&mut app) {
            return exit;
        }

        driver.compute_next_wait_after_update(&mut app);
    }
}

fn prepare_plugins(app: &mut App) {
    if app.plugins_state() == PluginsState::Cleaned {
        return;
    }

    while app.plugins_state() == PluginsState::Adding {
        tick_global_task_pools_on_main_thread();
    }
    app.finish();
    app.cleanup();
}

fn pump_platform(world: &mut World, timeout: Duration) {
    if let Some(mut diagnostics) = world.get_resource_mut::<LoopDiagnostics>() {
        diagnostics.record(LoopWakeReason::CocoaEventPump);
    }

    let Some(mut platform) = world.get_non_send_resource_mut::<Pin<Box<PlatformCallbacks>>>()
    else {
        return;
    };
    trace!(
        timeout_ms = timeout.as_millis(),
        "pumping Cocoa event loop from AppKit blocking fallback"
    );
    platform.pump_cocoa_event_loop(timeout.as_secs_f64());
}

fn drain_platform_events_nonblocking(world: &mut World) {
    if let Some(mut diagnostics) = world.get_resource_mut::<LoopDiagnostics>() {
        diagnostics.record(LoopWakeReason::CocoaEventPump);
    }

    let Some(mut platform) = world.get_non_send_resource_mut::<Pin<Box<PlatformCallbacks>>>()
    else {
        return;
    };
    trace!("draining Cocoa event loop without blocking");
    platform.drain_cocoa_event_loop_nonblocking();
}

fn drain_external_events(world: &mut World) -> bool {
    let Some(incoming_events) = world.get_non_send_resource::<WakeableEventQueue>() else {
        return false;
    };

    let mut received_events = Vec::new();
    let mut pending_mouse = None;
    let mut exit_requested = false;
    let mut internal_events = 0;
    loop {
        match incoming_events.try_recv() {
            Ok(Event::Exit) | Err(TryRecvError::Disconnected) => {
                internal_events += 1;
                exit_requested = true;
                break;
            }
            Ok(event) => {
                internal_events += 1;
                if matches!(event, Event::MouseMoved { .. }) {
                    pending_mouse = Some(event);
                } else {
                    received_events.extend(pending_mouse.take());
                    received_events.push(event);
                }
            }
            Err(TryRecvError::Empty) => {
                received_events.extend(pending_mouse.take());
                break;
            }
        }
    }

    for _ in 0..internal_events {
        record_internal_event(world);
    }
    if !received_events.is_empty() {
        world.write_message_batch(received_events);
    }
    if exit_requested {
        world.write_message(AppExit::Success);
    }
    internal_events > 0
}

fn record_internal_event(world: &mut World) {
    if let Some(mut diagnostics) = world.get_resource_mut::<LoopDiagnostics>() {
        diagnostics.record(LoopWakeReason::InternalEvent);
    }
}

fn collect_loop_activity(world: &mut World) -> LoopActivity {
    LoopActivity {
        repositioning: any_component::<RepositionMarker>(world),
        resizing: any_component::<ResizeMarker>(world),
        scrolling: any_component::<Scrolling>(world),
        flash_message: any_component::<FlashMessage>(world),
        low_power: world
            .get_resource::<LowPowerMode>()
            .is_some_and(|low_power| low_power.0),
    }
}

fn any_component<T: Component>(world: &mut World) -> bool {
    let mut query = world.query_filtered::<(), With<T>>();
    query.iter(world).next().is_some()
}

#[cfg(test)]
mod tests;

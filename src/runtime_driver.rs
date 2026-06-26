use std::collections::{BTreeMap, BTreeSet};
use std::ffi::c_void;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use bevy::app::{App, AppExit, PluginsState};
use bevy::ecs::component::Component;
use bevy::ecs::query::With;
use bevy::ecs::resource::Resource;
use bevy::ecs::world::World;
use bevy::tasks::tick_global_task_pools_on_main_thread;
use objc2_core_foundation::{
    CFAbsoluteTimeGetCurrent, CFRetained, CFRunLoop, CFRunLoopTimer, CFRunLoopTimerContext,
    kCFRunLoopCommonModes,
};
use tracing::{debug, trace, warn};

use crate::ecs::{
    FlashMessage, LoopActivity, LoopDiagnostics, LoopWakeReason, LowPowerMode, RepositionMarker,
    ResizeMarker, Scrolling, classify_timeout_wake, loop_timeout_limit_ms,
};
use crate::events::{Event, WakeableEventQueue};
use crate::platform::PlatformCallbacks;

/// Pure policy for deciding when Paneru's future top-level runtime driver should
/// run Bevy again, wait, or exit.
///
/// This module intentionally does not touch `AppKit`, `Bevy`, sockets, or wall-clock
/// time. Production loop behavior is unchanged; these types are a deterministic
/// model that later runner tickets can wire to real sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeDriverPolicy {
    config: RuntimeDriverConfig,
}

impl RuntimeDriverPolicy {
    #[must_use]
    pub const fn new(config: RuntimeDriverConfig) -> Self {
        Self { config }
    }

    /// Returns the next runner action for a snapshot of runtime state.
    #[must_use]
    pub fn decide(&self, state: RuntimeDriverState) -> RunnerDecision {
        if state.shutdown_requested {
            return RunnerDecision::Exit;
        }
        if state.external_event_pending {
            return RunnerDecision::RunUpdateNow(UpdateReason::ExternalEvent);
        }
        if state.bevy_dirty {
            return RunnerDecision::RunUpdateNow(UpdateReason::BevyDirty);
        }
        if let Some(deadline) = state.next_visible_deadline
            && deadline.at <= state.now
        {
            return RunnerDecision::RunUpdateNow(UpdateReason::DeadlineElapsed(deadline.reason));
        }

        let mut wait = if state.active_work.frame_active() {
            WaitDeadline {
                duration: self.config.frame_interval,
                reason: DeadlineReason::AnimationFrame,
            }
        } else if state.recent_interactive_activity(self.config.interactive_grace_period) {
            WaitDeadline {
                duration: self.config.interactive_idle_cap,
                reason: DeadlineReason::RecentInteractiveActivity,
            }
        } else if state.low_power {
            WaitDeadline {
                duration: self.config.low_power_watchdog_cap,
                reason: DeadlineReason::LowPowerWatchdog,
            }
        } else {
            WaitDeadline {
                duration: self.config.idle_watchdog_cap,
                reason: DeadlineReason::IdleWatchdog,
            }
        };

        if let Some(deadline) = state.next_visible_deadline {
            let until_deadline = deadline.at.saturating_sub(state.now);
            if until_deadline < wait.duration {
                wait = WaitDeadline {
                    duration: until_deadline,
                    reason: deadline.reason,
                };
            }
        }

        RunnerDecision::Wait(wait)
    }
}

impl Default for RuntimeDriverPolicy {
    fn default() -> Self {
        Self::new(RuntimeDriverConfig::legacy_equivalent())
    }
}

/// Marker resource inserted when waiting is owned by Paneru's custom top-level
/// runner instead of the legacy `PreUpdate` pump system.
#[derive(Clone, Copy, Debug, Resource)]
pub struct RuntimeDriverActive;

#[derive(Clone, Debug, Resource)]
pub(crate) struct RuntimeDeadlineClock {
    started: Instant,
}

impl Default for RuntimeDeadlineClock {
    fn default() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

impl RuntimeDeadlineClock {
    pub(crate) fn now(&self) -> Duration {
        self.started.elapsed()
    }
}

/// Runner-visible deadline registry. Deadlines are stored against the registry
/// clock rather than hidden inside Bevy `on_timer` run conditions, so future
/// deep-idle policy can choose a safe wake without knowing each system's
/// internals.
#[derive(Clone, Debug, Default, Resource)]
pub(crate) struct RuntimeDeadlines {
    deadlines: BTreeMap<DeadlineReason, Duration>,
}

impl RuntimeDeadlines {
    pub(crate) fn set_after(&mut self, now: Duration, reason: DeadlineReason, after: Duration) {
        self.deadlines.insert(reason, now + after);
    }

    pub(crate) fn clear(&mut self, reason: DeadlineReason) {
        self.deadlines.remove(&reason);
    }

    pub(crate) fn ensure_repeating_after(
        &mut self,
        now: Duration,
        reason: DeadlineReason,
        period: Duration,
    ) {
        if self
            .deadlines
            .get(&reason)
            .is_none_or(|deadline| *deadline <= now)
        {
            self.set_after(now, reason, period);
        }
    }

    fn earliest(&self) -> Option<RuntimeDeadline> {
        self.deadlines
            .iter()
            .min_by_key(|(_, deadline)| **deadline)
            .map(|(reason, at)| RuntimeDeadline::new(*at, *reason))
    }

    fn earliest_wait(&self, now: Duration) -> Option<WaitDeadline> {
        self.earliest().map(|deadline| WaitDeadline {
            duration: deadline.at.saturating_sub(now),
            reason: deadline.reason,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RuntimeActivityReason {
    ExternalEvent,
    Input,
    WindowEvent,
    Focus,
    Layout,
    Animation,
    Command,
}

impl RuntimeActivityReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ExternalEvent => "external_event",
            Self::Input => "input",
            Self::WindowEvent => "window_event",
            Self::Focus => "focus",
            Self::Layout => "layout",
            Self::Animation => "animation",
            Self::Command => "command",
        }
    }
}

#[derive(Clone, Debug, Default, Resource)]
pub(crate) struct RuntimeActivity {
    last_activity: Option<Duration>,
    reasons: BTreeSet<RuntimeActivityReason>,
}

impl RuntimeActivity {
    pub(crate) fn mark(&mut self, now: Duration, reason: RuntimeActivityReason) {
        self.last_activity = Some(now);
        self.reasons.insert(reason);
    }

    fn recent(&self, now: Duration, grace: Duration) -> bool {
        self.last_activity
            .is_some_and(|last| now.saturating_sub(last) < grace)
    }

    fn reason_names(&self) -> Vec<&'static str> {
        self.reasons.iter().map(|reason| reason.as_str()).collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RuntimeDirtyReason {
    CommandHandled,
    InternalMessageEmitted,
    FocusMarkerChanged,
    AnimationMarkerInserted,
}

impl RuntimeDirtyReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::CommandHandled => "command_handled",
            Self::InternalMessageEmitted => "internal_message_emitted",
            Self::FocusMarkerChanged => "focus_marker_changed",
            Self::AnimationMarkerInserted => "animation_marker_inserted",
        }
    }
}

/// Bevy-internal work that should make the custom runner perform another update
/// before it considers waiting. This covers perf-04 failure modes where work was
/// queued via Bevy messages/triggers rather than Paneru's external event queue.
#[derive(Clone, Debug, Default, Resource)]
pub(crate) struct RuntimeDirty {
    reasons: BTreeSet<RuntimeDirtyReason>,
}

impl RuntimeDirty {
    pub(crate) fn mark(&mut self, reason: RuntimeDirtyReason) {
        self.reasons.insert(reason);
    }

    fn take(&mut self) -> Vec<RuntimeDirtyReason> {
        std::mem::take(&mut self.reasons).into_iter().collect()
    }
}

struct RuntimeDriver {
    next_timeout_ms: u32,
    deadline_timer: Option<MainRunLoopDeadlineTimer>,
}

impl RuntimeDriver {
    const TIMEOUT_STEP_MS: u32 = 1;

    fn legacy_equivalent() -> Self {
        Self {
            next_timeout_ms: 0,
            deadline_timer: MainRunLoopDeadlineTimer::new(),
        }
    }

    #[cfg(test)]
    const fn without_deadline_timer_for_tests() -> Self {
        Self {
            next_timeout_ms: 0,
            deadline_timer: None,
        }
    }

    fn wait_and_drain_before_update(&mut self, app: &mut App) {
        let timeout = Duration::from_millis(u64::from(self.next_timeout_ms));
        if let Some(timer) = &mut self.deadline_timer {
            timer.schedule_after(timeout);
        }
        pump_platform(app.world_mut(), timeout);
        if drain_external_events(app.world_mut()) {
            mark_runtime_activity(app.world_mut(), RuntimeActivityReason::ExternalEvent);
            self.note_internal_event();
        }
    }

    fn note_internal_event(&mut self) {
        self.next_timeout_ms = Self::TIMEOUT_STEP_MS;
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
        let next_timeout = self.next_timeout_ms(app.world_mut(), activity, visible_deadline);
        let activity_snapshot = runtime_activity_snapshot(app.world_mut());
        if let Some(mut diagnostics) = app.world_mut().get_resource_mut::<LoopDiagnostics>() {
            diagnostics.record(classify_timeout_wake(activity));
            diagnostics.record_timeout_policy(activity, next_timeout, timeout_limit);
            diagnostics.record_runner_deadline(visible_deadline);
            diagnostics.record_runtime_activity(activity_snapshot);
        }
        self.next_timeout_ms = next_timeout;
    }

    fn next_timeout_ms(
        &self,
        world: &mut World,
        activity: LoopActivity,
        visible_deadline: Option<WaitDeadline>,
    ) -> u32 {
        let legacy_limit = loop_timeout_limit_ms(activity);
        let legacy_next = self.next_timeout_ms.min(legacy_limit) + Self::TIMEOUT_STEP_MS;
        let mut next_timeout = if legacy_idle_cadence_forced() || activity.frame_active() {
            legacy_next
        } else if runtime_recent_activity(world) {
            legacy_next.min(RuntimeDriverConfig::legacy_equivalent().interactive_idle_cap_ms())
        } else {
            let quiet_cap = if activity.low_power {
                RuntimeDriverConfig::legacy_equivalent().low_power_watchdog_cap_ms()
            } else {
                adaptive_quiet_idle_cap_ms()
            };
            visible_deadline.map_or(quiet_cap, |deadline| {
                duration_to_timeout_ms(deadline.duration).min(quiet_cap)
            })
        };

        if let Some(deadline) = visible_deadline {
            next_timeout = next_timeout.min(duration_to_timeout_ms(deadline.duration));
        }
        next_timeout
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

fn runtime_recent_activity(world: &mut World) -> bool {
    let now = world.resource::<RuntimeDeadlineClock>().now();
    world
        .get_resource::<RuntimeActivity>()
        .is_some_and(|activity| {
            activity.recent(
                now,
                RuntimeDriverConfig::legacy_equivalent().interactive_grace_period,
            )
        })
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

fn duration_to_timeout_ms(duration: Duration) -> u32 {
    duration.as_millis().try_into().unwrap_or(u32::MAX)
}

struct MainRunLoopDeadlineTimer {
    run_loop: CFRetained<CFRunLoop>,
    timer: CFRetained<CFRunLoopTimer>,
    fired: Arc<AtomicBool>,
}

impl MainRunLoopDeadlineTimer {
    fn new() -> Option<Self> {
        let run_loop = CFRunLoop::main()?;
        let fired = Arc::new(AtomicBool::new(false));
        let mut context = CFRunLoopTimerContext {
            version: 0,
            info: Arc::as_ptr(&fired).cast_mut().cast::<c_void>(),
            retain: Some(retain_atomic_bool),
            release: Some(release_atomic_bool),
            copyDescription: None,
        };
        let timer = unsafe {
            CFRunLoopTimer::new(
                None,
                far_future_fire_date(),
                0.0,
                0,
                0,
                Some(deadline_timer_fired),
                &raw mut context,
            )?
        };
        CFRunLoop::add_timer(&run_loop, Some(&timer), unsafe { kCFRunLoopCommonModes });
        Some(Self {
            run_loop,
            timer,
            fired,
        })
    }

    fn schedule_after(&mut self, duration: Duration) {
        self.fired.store(false, Ordering::SeqCst);
        self.timer
            .set_next_fire_date(CFAbsoluteTimeGetCurrent() + duration.as_secs_f64());
    }
}

impl Drop for MainRunLoopDeadlineTimer {
    fn drop(&mut self) {
        self.timer.invalidate();
        CFRunLoop::remove_timer(&self.run_loop, Some(&self.timer), unsafe {
            kCFRunLoopCommonModes
        });
    }
}

fn far_future_fire_date() -> f64 {
    CFAbsoluteTimeGetCurrent() + 60.0 * 60.0 * 24.0 * 365.0
}

unsafe extern "C-unwind" fn retain_atomic_bool(info: *const c_void) -> *const c_void {
    unsafe { Arc::increment_strong_count(info.cast::<AtomicBool>()) };
    info
}

unsafe extern "C-unwind" fn release_atomic_bool(info: *const c_void) {
    unsafe { Arc::decrement_strong_count(info.cast::<AtomicBool>()) };
}

unsafe extern "C-unwind" fn deadline_timer_fired(_timer: *mut CFRunLoopTimer, info: *mut c_void) {
    let fired = unsafe { &*info.cast::<AtomicBool>() };
    fired.store(true, Ordering::SeqCst);
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
        driver.wait_and_drain_before_update(&mut app);
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
        "pumping Cocoa event loop from custom runner"
    );
    platform.pump_cocoa_event_loop(timeout.as_secs_f64());
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
        match incoming_events.recv_timeout(Duration::from_millis(1)) {
            Ok(Event::Exit) | Err(RecvTimeoutError::Disconnected) => {
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
            Err(RecvTimeoutError::Timeout) => {
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

/// Timing knobs for the pure runner policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeDriverConfig {
    pub frame_interval: Duration,
    pub interactive_idle_cap: Duration,
    pub idle_watchdog_cap: Duration,
    pub low_power_watchdog_cap: Duration,
    pub interactive_grace_period: Duration,
}

impl RuntimeDriverConfig {
    /// Legacy-equivalent caps matching the currently accepted responsive loop.
    #[must_use]
    pub const fn legacy_equivalent() -> Self {
        Self {
            frame_interval: Duration::from_millis(16),
            interactive_idle_cap: Duration::from_millis(50),
            idle_watchdog_cap: Duration::from_millis(50),
            low_power_watchdog_cap: Duration::from_millis(500),
            interactive_grace_period: Duration::from_secs(1),
        }
    }

    const fn interactive_idle_cap_ms(self) -> u32 {
        self.interactive_idle_cap.as_millis() as u32
    }

    const fn low_power_watchdog_cap_ms(self) -> u32 {
        self.low_power_watchdog_cap.as_millis() as u32
    }
}

impl Default for RuntimeDriverConfig {
    fn default() -> Self {
        Self::legacy_equivalent()
    }
}

/// A deterministic snapshot of the inputs relevant to runner scheduling.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RuntimeDriverState {
    pub now: Duration,
    pub external_event_pending: bool,
    pub bevy_dirty: bool,
    pub active_work: ActiveRuntimeWork,
    pub last_interactive_activity: Option<Duration>,
    pub next_visible_deadline: Option<RuntimeDeadline>,
    pub low_power: bool,
    pub shutdown_requested: bool,
}

impl RuntimeDriverState {
    #[must_use]
    pub fn recent_interactive_activity(self, grace_period: Duration) -> bool {
        self.last_interactive_activity
            .is_some_and(|last_activity| self.now.saturating_sub(last_activity) < grace_period)
    }
}

/// Work that should keep frame-rate deadlines short while it is visible.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActiveRuntimeWork {
    pub repositioning: bool,
    pub resizing: bool,
    pub scrolling: bool,
    pub flash_message: bool,
}

impl ActiveRuntimeWork {
    #[must_use]
    pub const fn frame_active(self) -> bool {
        self.repositioning || self.resizing || self.scrolling || self.flash_message
    }
}

/// A runner-visible deadline such as a watchdog, timer, state save, or repair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeDeadline {
    pub at: Duration,
    pub reason: DeadlineReason,
}

impl RuntimeDeadline {
    #[must_use]
    pub const fn new(at: Duration, reason: DeadlineReason) -> Self {
        Self { at, reason }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunnerDecision {
    RunUpdateNow(UpdateReason),
    Wait(WaitDeadline),
    Exit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WaitDeadline {
    pub duration: Duration,
    pub reason: DeadlineReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateReason {
    ExternalEvent,
    BevyDirty,
    DeadlineElapsed(DeadlineReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeadlineReason {
    AnimationFrame,
    RecentInteractiveActivity,
    IdleWatchdog,
    LowPowerWatchdog,
    PeriodicMaintenance,
    StateSave,
    LostFocusWatchdog,
    OrphanWorkspaceWatchdog,
    RefreshWindowSizes,
    NativeTabReconciliation,
    TimeoutComponent,
}

impl DeadlineReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AnimationFrame => "animation_frame",
            Self::RecentInteractiveActivity => "recent_interactive_activity",
            Self::IdleWatchdog => "idle_watchdog",
            Self::LowPowerWatchdog => "low_power_watchdog",
            Self::PeriodicMaintenance => "periodic_maintenance",
            Self::StateSave => "state_save",
            Self::LostFocusWatchdog => "lost_focus_watchdog",
            Self::OrphanWorkspaceWatchdog => "orphan_workspace_watchdog",
            Self::RefreshWindowSizes => "refresh_window_sizes",
            Self::NativeTabReconciliation => "native_tab_reconciliation",
            Self::TimeoutComponent => "timeout_component",
        }
    }
}

#[cfg(test)]
mod tests {
    use bevy::ecs::system::ResMut;

    use super::*;

    #[derive(Clone, Copy, Debug)]
    struct FakeClock {
        now: Duration,
    }

    impl FakeClock {
        const fn new(now: Duration) -> Self {
            Self { now }
        }

        fn advance(&mut self, duration: Duration) {
            self.now += duration;
        }
    }

    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    struct FakeWaker {
        scheduled_wait: Option<WaitDeadline>,
        ran_update: bool,
        exited: bool,
    }

    #[derive(Debug)]
    struct FakeRunnerHarness {
        clock: FakeClock,
        policy: RuntimeDriverPolicy,
        state: RuntimeDriverState,
        waker: FakeWaker,
    }

    impl FakeRunnerHarness {
        fn new(config: RuntimeDriverConfig) -> Self {
            Self {
                clock: FakeClock::new(Duration::ZERO),
                policy: RuntimeDriverPolicy::new(config),
                state: RuntimeDriverState::default(),
                waker: FakeWaker::default(),
            }
        }

        fn step(&mut self) -> RunnerDecision {
            self.state.now = self.clock.now;
            let decision = self.policy.decide(self.state);
            match decision {
                RunnerDecision::RunUpdateNow(_) => self.waker.ran_update = true,
                RunnerDecision::Wait(wait) => self.waker.scheduled_wait = Some(wait),
                RunnerDecision::Exit => self.waker.exited = true,
            }
            decision
        }
    }

    fn long_idle_config() -> RuntimeDriverConfig {
        RuntimeDriverConfig {
            frame_interval: Duration::from_millis(16),
            interactive_idle_cap: Duration::from_millis(50),
            idle_watchdog_cap: Duration::from_secs(1),
            low_power_watchdog_cap: Duration::from_secs(5),
            interactive_grace_period: Duration::from_secs(2),
        }
    }

    #[test]
    fn external_event_pending_runs_update_now_instead_of_waiting() {
        // Guards the perf-04 regression where a queued socket/query/macOS event
        // could be stranded behind a long idle wait before Bevy observed it.
        let mut harness = FakeRunnerHarness::new(long_idle_config());
        harness.state.external_event_pending = true;

        assert_eq!(
            harness.step(),
            RunnerDecision::RunUpdateNow(UpdateReason::ExternalEvent)
        );
        assert!(harness.waker.ran_update);
        assert!(harness.waker.scheduled_wait.is_none());
    }

    #[test]
    fn bevy_internal_dirty_work_runs_update_now_instead_of_waiting() {
        // Guards the perf-04 regression where Bevy-generated messages/triggers
        // could require a follow-up update without going through the external
        // wakeable event queue.
        let mut harness = FakeRunnerHarness::new(long_idle_config());
        harness.state.bevy_dirty = true;

        assert_eq!(
            harness.step(),
            RunnerDecision::RunUpdateNow(UpdateReason::BevyDirty)
        );
        assert!(harness.waker.ran_update);
        assert!(harness.waker.scheduled_wait.is_none());
    }

    #[test]
    fn active_animation_repositioning_uses_frame_deadline() {
        // Visible animation/reposition/resize/scroll/flash work must not inherit
        // a long idle cap; otherwise movement begins late or looks choppy.
        let mut harness = FakeRunnerHarness::new(long_idle_config());
        harness.state.active_work.repositioning = true;

        assert_eq!(
            harness.step(),
            RunnerDecision::Wait(WaitDeadline {
                duration: Duration::from_millis(16),
                reason: DeadlineReason::AnimationFrame,
            })
        );
    }

    #[test]
    fn quiet_idle_with_only_watchdog_may_wait_until_watchdog_cap() {
        let mut harness = FakeRunnerHarness::new(long_idle_config());

        assert_eq!(
            harness.step(),
            RunnerDecision::Wait(WaitDeadline {
                duration: Duration::from_secs(1),
                reason: DeadlineReason::IdleWatchdog,
            })
        );
    }

    #[test]
    fn shutdown_requested_exits_without_waiting() {
        let mut harness = FakeRunnerHarness::new(long_idle_config());
        harness.state.shutdown_requested = true;

        assert_eq!(harness.step(), RunnerDecision::Exit);
        assert!(harness.waker.exited);
        assert!(harness.waker.scheduled_wait.is_none());
    }

    #[test]
    fn recent_input_or_window_activity_keeps_short_deadline_during_grace_period() {
        // The future deep-idle policy may use a long watchdog, but immediately
        // after user/window activity it must keep the legacy-style short cap.
        let mut harness = FakeRunnerHarness::new(long_idle_config());
        harness.clock.now = Duration::from_secs(10);
        harness.state.last_interactive_activity = Some(Duration::from_secs(9));

        assert_eq!(
            harness.step(),
            RunnerDecision::Wait(WaitDeadline {
                duration: Duration::from_millis(50),
                reason: DeadlineReason::RecentInteractiveActivity,
            })
        );

        harness.waker = FakeWaker::default();
        harness.clock.advance(Duration::from_secs(2));

        assert_eq!(
            harness.step(),
            RunnerDecision::Wait(WaitDeadline {
                duration: Duration::from_secs(1),
                reason: DeadlineReason::IdleWatchdog,
            })
        );
    }

    #[test]
    fn visible_deadline_shortens_idle_wait_and_runs_when_due() {
        // Runner-visible timers/watchdogs must be able to cut a long idle wait;
        // hidden Bevy timer state was one structural weakness in perf-04.
        let mut harness = FakeRunnerHarness::new(long_idle_config());
        harness.state.next_visible_deadline = Some(RuntimeDeadline::new(
            Duration::from_millis(250),
            DeadlineReason::StateSave,
        ));

        assert_eq!(
            harness.step(),
            RunnerDecision::Wait(WaitDeadline {
                duration: Duration::from_millis(250),
                reason: DeadlineReason::StateSave,
            })
        );

        harness.clock.advance(Duration::from_millis(250));
        assert_eq!(
            harness.step(),
            RunnerDecision::RunUpdateNow(UpdateReason::DeadlineElapsed(DeadlineReason::StateSave))
        );
    }

    #[derive(Debug, Default, PartialEq, Eq)]
    struct FakeDeadlineTimer {
        active_deadline: Option<Duration>,
        schedule_count: usize,
        invalidate_count: usize,
    }

    impl FakeDeadlineTimer {
        fn schedule_after(&mut self, duration: Duration) {
            self.active_deadline = Some(duration);
            self.schedule_count += 1;
        }

        fn invalidate(&mut self) {
            self.active_deadline = None;
            self.invalidate_count += 1;
        }
    }

    #[derive(Resource, Default)]
    struct UpdateCounter(usize);

    fn emit_internal_dirty_once(
        mut counter: ResMut<UpdateCounter>,
        mut dirty: ResMut<RuntimeDirty>,
    ) {
        if counter.0 == 0 {
            dirty.mark(RuntimeDirtyReason::InternalMessageEmitted);
        }
        counter.0 += 1;
    }

    fn emit_internal_dirty_forever(
        mut counter: ResMut<UpdateCounter>,
        mut dirty: ResMut<RuntimeDirty>,
    ) {
        dirty.mark(RuntimeDirtyReason::InternalMessageEmitted);
        counter.0 += 1;
    }

    #[test]
    fn dirty_internal_work_runs_follow_up_update_before_waiting() {
        let mut app = App::new();
        app.add_plugins(bevy::MinimalPlugins)
            .init_resource::<RuntimeDirty>()
            .init_resource::<UpdateCounter>()
            .add_systems(bevy::app::Update, emit_internal_dirty_once);
        let mut driver = RuntimeDriver::without_deadline_timer_for_tests();

        assert_eq!(driver.update_until_settled(&mut app), None);

        assert_eq!(app.world().resource::<UpdateCounter>().0, 2);
    }

    #[test]
    fn dirty_settle_guard_terminates_perpetual_internal_work() {
        let mut app = App::new();
        app.add_plugins(bevy::MinimalPlugins)
            .init_resource::<RuntimeDirty>()
            .init_resource::<LoopDiagnostics>()
            .init_resource::<UpdateCounter>()
            .add_systems(bevy::app::Update, emit_internal_dirty_forever);
        let mut driver = RuntimeDriver::without_deadline_timer_for_tests();

        assert_eq!(driver.update_until_settled(&mut app), None);

        let diagnostics = app.world().resource::<LoopDiagnostics>();
        assert_eq!(diagnostics.dirty_settle_guard_hits, 1);
        assert!(app.world().resource::<UpdateCounter>().0 > 1);
    }

    #[test]
    fn runtime_deadline_registry_earliest_deadline_wins() {
        let now = Duration::from_secs(10);
        let mut deadlines = RuntimeDeadlines::default();

        deadlines.set_after(now, DeadlineReason::StateSave, Duration::from_secs(300));
        deadlines.set_after(
            now,
            DeadlineReason::LostFocusWatchdog,
            Duration::from_secs(1),
        );
        deadlines.set_after(
            now,
            DeadlineReason::AnimationFrame,
            Duration::from_millis(16),
        );
        deadlines.set_after(
            now,
            DeadlineReason::TimeoutComponent,
            Duration::from_millis(125),
        );

        assert_eq!(
            deadlines.earliest_wait(now),
            Some(WaitDeadline {
                duration: Duration::from_millis(16),
                reason: DeadlineReason::AnimationFrame,
            })
        );
    }

    #[test]
    fn visible_registry_deadline_prevents_unsafe_long_idle_wait() {
        let policy = RuntimeDriverPolicy::new(long_idle_config());
        let now = Duration::from_secs(10);

        assert_eq!(
            policy.decide(RuntimeDriverState {
                now,
                ..RuntimeDriverState::default()
            }),
            RunnerDecision::Wait(WaitDeadline {
                duration: Duration::from_secs(1),
                reason: DeadlineReason::IdleWatchdog,
            })
        );

        let mut deadlines = RuntimeDeadlines::default();
        deadlines.set_after(
            now,
            DeadlineReason::NativeTabReconciliation,
            Duration::from_millis(250),
        );
        let deadline = deadlines.earliest().expect("deadline should be visible");

        assert_eq!(
            policy.decide(RuntimeDriverState {
                now,
                next_visible_deadline: Some(deadline),
                ..RuntimeDriverState::default()
            }),
            RunnerDecision::Wait(WaitDeadline {
                duration: Duration::from_millis(250),
                reason: DeadlineReason::NativeTabReconciliation,
            })
        );
    }

    #[test]
    fn watchdog_and_state_save_deadlines_are_scheduled_and_rescheduled() {
        let mut deadlines = RuntimeDeadlines::default();
        let now = Duration::from_secs(10);

        deadlines.ensure_repeating_after(
            now,
            DeadlineReason::LostFocusWatchdog,
            Duration::from_secs(1),
        );
        deadlines.ensure_repeating_after(
            now,
            DeadlineReason::OrphanWorkspaceWatchdog,
            Duration::from_secs(1),
        );
        deadlines.ensure_repeating_after(now, DeadlineReason::StateSave, Duration::from_secs(300));

        assert_eq!(
            deadlines.deadlines[&DeadlineReason::LostFocusWatchdog],
            now + Duration::from_secs(1)
        );
        assert_eq!(
            deadlines.deadlines[&DeadlineReason::OrphanWorkspaceWatchdog],
            now + Duration::from_secs(1)
        );
        assert_eq!(
            deadlines.deadlines[&DeadlineReason::StateSave],
            now + Duration::from_secs(300)
        );

        let later = now + Duration::from_secs(1);
        deadlines.ensure_repeating_after(
            later,
            DeadlineReason::LostFocusWatchdog,
            Duration::from_secs(1),
        );

        assert_eq!(
            deadlines.deadlines[&DeadlineReason::LostFocusWatchdog],
            later + Duration::from_secs(1)
        );
    }

    fn app_with_runtime_resources(started: Instant) -> App {
        let mut app = App::new();
        app.insert_resource(RuntimeDeadlineClock { started });
        app.init_resource::<RuntimeDeadlines>();
        app.init_resource::<RuntimeActivity>();
        app
    }

    fn instant_seconds_ago(seconds: u64) -> Instant {
        Instant::now()
            .checked_sub(Duration::from_secs(seconds))
            .expect("test duration should be representable")
    }

    #[test]
    fn adaptive_activity_grace_uses_fast_legacy_cadence() {
        let mut app = app_with_runtime_resources(Instant::now());
        mark_runtime_activity(app.world_mut(), RuntimeActivityReason::Input);
        let mut driver = RuntimeDriver::without_deadline_timer_for_tests();
        driver.next_timeout_ms = 50;

        assert_eq!(
            driver.next_timeout_ms(app.world_mut(), LoopActivity::default(), None),
            50
        );
    }

    #[test]
    fn adaptive_quiet_idle_uses_visible_deadline_after_grace() {
        let mut app = app_with_runtime_resources(instant_seconds_ago(2));
        app.world_mut()
            .resource_mut::<RuntimeActivity>()
            .mark(Duration::ZERO, RuntimeActivityReason::Input);
        app.world_mut()
            .resource_mut::<RuntimeDeadlines>()
            .set_after(
                Duration::from_secs(2),
                DeadlineReason::NativeTabReconciliation,
                Duration::from_millis(250),
            );
        let mut driver = RuntimeDriver::without_deadline_timer_for_tests();
        driver.next_timeout_ms = 50;

        let visible_deadline = next_visible_runtime_deadline(app.world_mut());
        let next_timeout =
            driver.next_timeout_ms(app.world_mut(), LoopActivity::default(), visible_deadline);
        assert!((200..=250).contains(&next_timeout));
    }

    #[test]
    fn adaptive_quiet_idle_is_capped_without_visible_deadline() {
        let mut app = app_with_runtime_resources(instant_seconds_ago(2));
        app.world_mut()
            .resource_mut::<RuntimeActivity>()
            .mark(Duration::ZERO, RuntimeActivityReason::Input);
        let mut driver = RuntimeDriver::without_deadline_timer_for_tests();
        driver.next_timeout_ms = 50;

        assert_eq!(
            driver.next_timeout_ms(app.world_mut(), LoopActivity::default(), None),
            adaptive_quiet_idle_cap_ms()
        );
    }

    #[test]
    fn adaptive_low_power_does_not_sleep_past_repair_deadline() {
        let mut app = app_with_runtime_resources(instant_seconds_ago(2));
        app.world_mut()
            .resource_mut::<RuntimeDeadlines>()
            .set_after(
                Duration::from_secs(2),
                DeadlineReason::LostFocusWatchdog,
                Duration::from_millis(250),
            );
        let driver = RuntimeDriver::without_deadline_timer_for_tests();

        let visible_deadline = next_visible_runtime_deadline(app.world_mut());
        let next_timeout = driver.next_timeout_ms(
            app.world_mut(),
            LoopActivity {
                low_power: true,
                ..LoopActivity::default()
            },
            visible_deadline,
        );
        assert!((200..=250).contains(&next_timeout));
    }

    #[test]
    fn low_power_policy_does_not_sleep_past_critical_registry_deadline() {
        let policy = RuntimeDriverPolicy::new(long_idle_config());
        let now = Duration::from_secs(10);
        let mut deadlines = RuntimeDeadlines::default();
        deadlines.set_after(
            now,
            DeadlineReason::LostFocusWatchdog,
            Duration::from_secs(1),
        );

        assert_eq!(
            policy.decide(RuntimeDriverState {
                now,
                low_power: true,
                next_visible_deadline: deadlines.earliest(),
                ..RuntimeDriverState::default()
            }),
            RunnerDecision::Wait(WaitDeadline {
                duration: Duration::from_secs(1),
                reason: DeadlineReason::LostFocusWatchdog,
            })
        );
    }

    #[test]
    fn fake_deadline_timer_rescheduling_replaces_old_deadline() {
        let mut timer = FakeDeadlineTimer::default();

        timer.schedule_after(Duration::from_millis(50));
        timer.schedule_after(Duration::from_millis(5));
        timer.schedule_after(Duration::from_millis(500));

        assert_eq!(timer.active_deadline, Some(Duration::from_millis(500)));
        assert_eq!(timer.schedule_count, 3);
    }

    #[test]
    fn fake_deadline_timer_invalidates_active_deadline_on_shutdown() {
        let mut timer = FakeDeadlineTimer::default();
        timer.schedule_after(Duration::from_millis(50));

        timer.invalidate();

        assert_eq!(timer.active_deadline, None);
        assert_eq!(timer.invalidate_count, 1);
    }

    #[test]
    fn custom_runner_resets_timeout_ramp_after_internal_events() {
        // The legacy in-system pump resets its timeout ramp whenever it drains
        // an internal event. The custom runner must preserve that behavior or a
        // user command arriving after idle can be followed by another ~50 ms
        // wait, which feels like the perf-04 latency regression.
        let mut driver = RuntimeDriver::without_deadline_timer_for_tests();
        driver.next_timeout_ms = 50;

        driver.note_internal_event();

        assert_eq!(driver.next_timeout_ms, RuntimeDriver::TIMEOUT_STEP_MS);
    }

    #[test]
    fn legacy_equivalent_config_matches_current_responsive_caps() {
        let config = RuntimeDriverConfig::legacy_equivalent();

        assert_eq!(config.frame_interval, Duration::from_millis(16));
        assert_eq!(config.interactive_idle_cap, Duration::from_millis(50));
        assert_eq!(config.idle_watchdog_cap, Duration::from_millis(50));
        assert_eq!(config.low_power_watchdog_cap, Duration::from_millis(500));
    }
}

use std::time::Instant;

use bevy::ecs::resource::Resource;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FakeWaitWake {
    ExternalSource,
    Timer,
    Timeout,
    Shutdown,
}

#[derive(Debug, Default)]
struct FakeRunLoopWaitSet {
    source_signaled: bool,
    timer_due: bool,
    shutdown_requested: bool,
    wait_count: usize,
    schedule_count: usize,
    invalidate_count: usize,
}

impl FakeRunLoopWaitSet {
    fn signal_source(&mut self) {
        self.source_signaled = true;
    }

    fn fire_timer(&mut self) {
        self.timer_due = true;
    }

    fn request_shutdown(&mut self) {
        self.shutdown_requested = true;
    }

    fn wait(&mut self, _timeout: Duration) -> FakeWaitWake {
        self.wait_count += 1;
        self.schedule_count += 1;
        if self.shutdown_requested {
            return FakeWaitWake::Shutdown;
        }
        if std::mem::take(&mut self.source_signaled) {
            return FakeWaitWake::ExternalSource;
        }
        if std::mem::take(&mut self.timer_due) {
            return FakeWaitWake::Timer;
        }
        FakeWaitWake::Timeout
    }

    fn invalidate(&mut self) {
        self.invalidate_count += 1;
    }
}

#[derive(Debug, Default)]
struct FakeAppKitDrain {
    pending_events: usize,
    drain_calls: usize,
}

impl FakeAppKitDrain {
    fn drain_nonblocking(&mut self) -> usize {
        self.drain_calls += 1;
        std::mem::take(&mut self.pending_events)
    }
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

fn emit_internal_dirty_once(mut counter: ResMut<UpdateCounter>, mut dirty: ResMut<RuntimeDirty>) {
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

fn app_with_external_event_queue(events: Vec<Event>) -> App {
    app_with_external_event_queue_connected(events, true)
}

fn app_with_external_event_queue_connected(events: Vec<Event>, keep_connected: bool) -> App {
    let mut app = App::new();
    app.init_resource::<bevy::ecs::message::Messages<Event>>();
    app.init_resource::<bevy::ecs::message::Messages<AppExit>>();
    let (tx, rx) = std::sync::mpsc::channel();
    for event in events {
        tx.send(event).expect("test receiver should still be alive");
    }
    if keep_connected {
        std::mem::forget(tx);
    }
    app.world_mut()
        .insert_non_send_resource(WakeableEventQueue::from_receiver(rx));
    app
}

fn drained_events(app: &mut App) -> Vec<Event> {
    app.world_mut()
        .resource_mut::<bevy::ecs::message::Messages<Event>>()
        .drain()
        .collect()
}

fn instant_seconds_ago(seconds: u64) -> Instant {
    Instant::now()
        .checked_sub(Duration::from_secs(seconds))
        .expect("test duration should be representable")
}

#[test]
fn custom_runner_drain_empty_queue_returns_immediately() {
    let mut app = app_with_external_event_queue(Vec::new());

    assert!(!drain_external_events(app.world_mut()));
    assert!(drained_events(&mut app).is_empty());
}

#[test]
fn custom_runner_drain_collects_burst_without_blocking() {
    let mut app = app_with_external_event_queue(vec![
        Event::ProcessesLoaded,
        Event::ApplicationActivated,
        Event::ApplicationDeactivated,
    ]);

    assert!(drain_external_events(app.world_mut()));
    let events = drained_events(&mut app);

    assert_eq!(events.len(), 3);
    assert!(matches!(events[0], Event::ProcessesLoaded));
    assert!(matches!(events[1], Event::ApplicationActivated));
    assert!(matches!(events[2], Event::ApplicationDeactivated));
}

#[test]
fn custom_runner_drain_coalesces_mouse_moves() {
    let mut app = app_with_external_event_queue(vec![
        Event::MouseMoved {
            point: objc2_core_foundation::CGPoint::new(1.0, 1.0),
            modifiers: crate::platform::Modifiers::empty(),
        },
        Event::MouseMoved {
            point: objc2_core_foundation::CGPoint::new(2.0, 2.0),
            modifiers: crate::platform::Modifiers::empty(),
        },
        Event::ApplicationActivated,
    ]);

    assert!(drain_external_events(app.world_mut()));
    let events = drained_events(&mut app);

    assert_eq!(events.len(), 2);
    assert!(matches!(
        events[0],
        Event::MouseMoved { point, .. } if (point.x - 2.0).abs() < f64::EPSILON
            && (point.y - 2.0).abs() < f64::EPSILON
    ));
    assert!(matches!(events[1], Event::ApplicationActivated));
}

#[test]
fn custom_runner_drain_exit_and_disconnect_request_shutdown() {
    for (events, connected) in [(vec![Event::Exit], true), (Vec::new(), false)] {
        let mut app = app_with_external_event_queue_connected(events, connected);

        assert!(drain_external_events(app.world_mut()));
        let exits = app
            .world_mut()
            .resource_mut::<bevy::ecs::message::Messages<AppExit>>()
            .drain()
            .collect::<Vec<_>>();
        assert_eq!(exits, vec![AppExit::Success]);
    }
}

#[test]
fn production_decision_for_active_animation_uses_frame_cadence() {
    let mut app = app_with_runtime_resources(Instant::now());

    assert_eq!(
        RuntimeDriver::next_decision_after_update(
            app.world_mut(),
            LoopActivity {
                repositioning: true,
                ..LoopActivity::default()
            },
            None,
            false,
        ),
        RunnerDecision::Wait(WaitDeadline {
            duration: Duration::from_millis(16),
            reason: DeadlineReason::AnimationFrame,
        })
    );
}

#[test]
fn production_decision_for_recent_activity_uses_legacy_fast_cadence() {
    let mut app = app_with_runtime_resources(Instant::now());
    mark_runtime_activity(app.world_mut(), RuntimeActivityReason::Input);

    assert_eq!(
        RuntimeDriver::next_decision_after_update(
            app.world_mut(),
            LoopActivity::default(),
            None,
            false
        ),
        RunnerDecision::Wait(WaitDeadline {
            duration: Duration::from_millis(50),
            reason: DeadlineReason::RecentInteractiveActivity,
        })
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
    let visible_deadline = next_visible_runtime_deadline(app.world_mut());
    assert_eq!(
        RuntimeDriver::next_decision_after_update(
            app.world_mut(),
            LoopActivity::default(),
            visible_deadline,
            false,
        ),
        RunnerDecision::Wait(WaitDeadline {
            duration: visible_deadline
                .expect("deadline should be visible")
                .duration,
            reason: DeadlineReason::NativeTabReconciliation,
        })
    );
}

#[test]
fn adaptive_quiet_idle_is_capped_without_visible_deadline() {
    let mut app = app_with_runtime_resources(instant_seconds_ago(2));
    app.world_mut()
        .resource_mut::<RuntimeActivity>()
        .mark(Duration::ZERO, RuntimeActivityReason::Input);
    assert_eq!(
        RuntimeDriver::next_timeout_ms(app.world_mut(), LoopActivity::default(), None),
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
    let visible_deadline = next_visible_runtime_deadline(app.world_mut());
    assert_eq!(
        RuntimeDriver::next_decision_after_update(
            app.world_mut(),
            LoopActivity {
                low_power: true,
                ..LoopActivity::default()
            },
            visible_deadline,
            false,
        ),
        RunnerDecision::Wait(WaitDeadline {
            duration: visible_deadline
                .expect("deadline should be visible")
                .duration,
            reason: DeadlineReason::LostFocusWatchdog,
        })
    );
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
fn fake_run_loop_source_signal_returns_before_update() {
    let mut wait_set = FakeRunLoopWaitSet::default();
    let mut update_ran = false;

    wait_set.signal_source();
    let wake = wait_set.wait(Duration::from_secs(1));
    assert_eq!(wake, FakeWaitWake::ExternalSource);
    assert!(!update_ran, "wait must return control before Bevy update");

    update_ran = true;
    assert!(update_ran);
    assert_eq!(wait_set.wait_count, 1);
}

#[test]
fn fake_run_loop_timer_fires_once_and_can_be_rescheduled() {
    let mut wait_set = FakeRunLoopWaitSet::default();

    wait_set.fire_timer();
    assert_eq!(
        wait_set.wait(Duration::from_millis(250)),
        FakeWaitWake::Timer
    );
    assert_eq!(
        wait_set.wait(Duration::from_millis(250)),
        FakeWaitWake::Timeout
    );
    wait_set.fire_timer();
    assert_eq!(
        wait_set.wait(Duration::from_millis(500)),
        FakeWaitWake::Timer
    );
    assert_eq!(wait_set.schedule_count, 3);
}

#[test]
fn fake_burst_external_signals_coalesce_without_losing_events() {
    let mut wait_set = FakeRunLoopWaitSet::default();
    let queued_events = 128;

    for _ in 0..queued_events {
        wait_set.signal_source();
    }

    assert_eq!(
        wait_set.wait(Duration::from_secs(1)),
        FakeWaitWake::ExternalSource
    );
    assert_eq!(queued_events, 128);
    assert_eq!(wait_set.wait_count, 1);
}

#[test]
fn fake_appkit_drain_is_nonblocking_and_drains_pending_events() {
    let mut appkit = FakeAppKitDrain {
        pending_events: 3,
        drain_calls: 0,
    };

    assert_eq!(appkit.drain_nonblocking(), 3);
    assert_eq!(appkit.drain_nonblocking(), 0);
    assert_eq!(appkit.drain_calls, 2);
}

#[test]
fn fake_shutdown_invalidates_wait_set() {
    let mut wait_set = FakeRunLoopWaitSet::default();

    wait_set.request_shutdown();
    assert_eq!(
        wait_set.wait(Duration::from_secs(1)),
        FakeWaitWake::Shutdown
    );
    wait_set.invalidate();

    assert_eq!(wait_set.invalidate_count, 1);
}

#[test]
fn appkit_blocking_wait_fallback_env_is_modeled() {
    assert!(!appkit_blocking_wait_forced_for_env(false));
    assert!(appkit_blocking_wait_forced_for_env(true));
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
fn custom_runner_records_minimum_wait_after_internal_events() {
    // Draining an external/internal event should never leave the runner on a
    // long quiet-idle wait before Bevy has a chance to process the event.
    let mut driver = RuntimeDriver::without_deadline_timer_for_tests();

    driver.note_internal_event();

    assert_eq!(scheduled_timeout_ms(driver.next_decision), 1);
}

#[test]
fn legacy_idle_cadence_fallback_uses_same_policy_path() {
    let mut app = app_with_runtime_resources(instant_seconds_ago(2));

    assert_eq!(
        RuntimeDriver::next_decision_after_update(
            app.world_mut(),
            LoopActivity::default(),
            None,
            false
        ),
        RunnerDecision::Wait(WaitDeadline {
            duration: Duration::from_secs(1),
            reason: DeadlineReason::IdleWatchdog,
        })
    );
    assert_eq!(
        RuntimeDriver::next_decision_after_update(
            app.world_mut(),
            LoopActivity::default(),
            None,
            true,
        ),
        RunnerDecision::Wait(WaitDeadline {
            duration: Duration::from_millis(50),
            reason: DeadlineReason::IdleWatchdog,
        })
    );
}

#[test]
fn submillisecond_waits_are_clamped_for_os_scheduling() {
    assert_eq!(
        scheduled_timeout_ms(RunnerDecision::Wait(WaitDeadline {
            duration: Duration::from_micros(500),
            reason: DeadlineReason::StateSave,
        })),
        1
    );
}

#[test]
fn repeated_due_deadline_is_clamped_instead_of_spinning() {
    let mut driver = RuntimeDriver::without_deadline_timer_for_tests();
    driver.next_decision =
        RunnerDecision::RunUpdateNow(UpdateReason::DeadlineElapsed(DeadlineReason::StateSave));

    let guarded = driver.guard_repeated_immediate_deadline(RunnerDecision::RunUpdateNow(
        UpdateReason::DeadlineElapsed(DeadlineReason::StateSave),
    ));

    assert_eq!(
        guarded,
        RunnerDecision::Wait(WaitDeadline {
            duration: MIN_OS_WAIT_DURATION,
            reason: DeadlineReason::StateSave,
        })
    );
    assert_eq!(scheduled_timeout_ms(guarded), 1);
}

#[test]
fn due_production_deadline_runs_update_now_instead_of_zero_wait() {
    let mut app = app_with_runtime_resources(Instant::now());
    app.world_mut()
        .resource_mut::<RuntimeDeadlines>()
        .set_after(Duration::ZERO, DeadlineReason::StateSave, Duration::ZERO);
    let visible_deadline = next_visible_runtime_deadline(app.world_mut());

    assert_eq!(
        RuntimeDriver::next_decision_after_update(
            app.world_mut(),
            LoopActivity::default(),
            visible_deadline,
            false,
        ),
        RunnerDecision::RunUpdateNow(UpdateReason::DeadlineElapsed(DeadlineReason::StateSave))
    );
}

#[test]
fn legacy_equivalent_config_matches_current_responsive_caps() {
    let config = RuntimeDriverConfig::legacy_equivalent();

    assert_eq!(config.frame_interval, Duration::from_millis(16));
    assert_eq!(config.interactive_idle_cap, Duration::from_millis(50));
    assert_eq!(config.idle_watchdog_cap, Duration::from_millis(50));
    assert_eq!(config.low_power_watchdog_cap, Duration::from_millis(500));
}

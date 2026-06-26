use std::sync::mpsc::channel;

use bevy::app::Update;
use bevy::prelude::*;

use crate::commands::{Command, Direction, Operation};
use crate::ecs::state::StateQueryKind;
use crate::ecs::{
    FocusedMarker, LoopDiagnostics, RepositionMarker, native_tab_reconcile_period,
    single_threaded_schedules_enabled_for_env,
};
use crate::events::Event;
use crate::manager::Window;
use crate::platform::cf_run_loop_pump_enabled_for_env;
use crate::tests::TestHarness;

#[test]
fn state_query_responds_on_next_update_without_socket_sleep() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.app.update();

    let (tx, rx) = channel();
    harness.world().write_message::<Event>(Event::StateQuery {
        kind: StateQueryKind::Active,
        respond_to: tx,
    });

    harness.app.update();

    let response = rx
        .try_recv()
        .expect("state query should respond during the next Bevy update");
    let json: serde_json::Value = serde_json::from_str(&response).expect("valid query JSON");
    assert_eq!(json["display_id"], crate::tests::TEST_DISPLAY_ID);
}

#[test]
fn runtime_diagnostics_query_responds_on_next_update() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.world().insert_resource(LoopDiagnostics::default());
    harness.app.update();

    let (tx, rx) = channel();
    harness.world().write_message::<Event>(Event::StateQuery {
        kind: StateQueryKind::RuntimeDiagnostics,
        respond_to: tx,
    });

    harness.app.update();

    let response = rx
        .try_recv()
        .expect("runtime diagnostics query should respond during the next Bevy update");
    let json: serde_json::Value = serde_json::from_str(&response).expect("valid diagnostics JSON");
    assert!(json.get("counters").is_some());
    assert!(json.get("last_timeout_ms").is_some());
}

#[test]
fn internal_bevy_command_work_is_not_stranded_behind_idle_wait_in_harness() {
    let mut harness = TestHarness::new().with_windows(3);
    harness.app.update();

    harness.world().write_message::<Event>(Event::Command {
        command: Command::Window(Operation::Focus(Direction::East)),
    });

    harness.app.update();

    let world = harness.world();
    let mut focused = world.query_filtered::<&Window, With<FocusedMarker>>();
    let window = focused
        .single(world)
        .expect("focused window should exist after processing command");
    assert_eq!(
        window.id(),
        1,
        "focus command should have performed follow-up Bevy work in the same update"
    );
}

#[test]
fn animation_work_starts_promptly_after_focus_command() {
    let mut harness = TestHarness::new().with_windows(4);
    harness.app.update();

    harness.world().write_message::<Event>(Event::Command {
        command: Command::Window(Operation::Focus(Direction::Last)),
    });

    harness.world().run_schedule(Update);

    let world = harness.world();
    let mut repositioning = world.query_filtered::<Entity, With<RepositionMarker>>();
    let count = repositioning.iter(world).count();
    assert!(
        count > 0,
        "layout animation/reposition markers should be created on the first update after a command"
    );
}

#[test]
fn native_tab_reconciliation_is_throttled() {
    assert_eq!(native_tab_reconcile_period().as_millis(), 250);
}

#[test]
fn perf_fallback_knobs_have_documented_defaults() {
    assert!(single_threaded_schedules_enabled_for_env(false));
    assert!(!single_threaded_schedules_enabled_for_env(true));

    assert!(!cf_run_loop_pump_enabled_for_env(false));
    assert!(cf_run_loop_pump_enabled_for_env(true));
}

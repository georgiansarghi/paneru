# perf-06: Add robustness regression tests for perf loop experiments

## Revised context after `perf-loop-spike`

perf-04's long-idle/deadline attempts are not shippable yet, so this ticket's
scope was revised to protect the conservative solution we kept:

- legacy responsive Cocoa pump / ~50 ms idle cadence;
- wakeable external event queue from perf-03;
- single-threaded Bevy schedules by default from perf-05;
- native-tab reconciliation throttled to 250 ms;
- opt-in/fallback env knobs documented.

## Goal

Add deterministic regression coverage around the risks discovered during the
runtime-loop spike, before any future custom-runner attempt.

## Implemented coverage on this branch

Added `src/tests/perf.rs` with tests for:

- state queries respond on the next Bevy update without socket sleeps;
- runtime diagnostics queries respond on the next Bevy update;
- a Bevy-internal command path performs follow-up work in the same update and is
  not dependent on the external wake queue;
- animation/reposition work is created promptly when running the command schedule;
- native-tab reconciliation remains throttled to 250 ms;
- perf fallback knobs have documented default behavior:
  - single-threaded schedules default on;
  - `PANERU_MULTI_THREADED_SCHEDULES=1` disables that default;
  - `PANERU_CF_RUN_LOOP_PUMP=1` is opt-in and disabled by default.

Existing perf-03 tests continue to cover burst external event sends and
receiver-disconnect behavior.

## Acceptance criteria

- Tests are deterministic and do not rely on wall-clock sleeps.
- Tests exercise the specific regression classes found during the spike.
- Existing test suite passes.
- `cargo clippy --all-targets --all-features -- -D warnings` passes.

## Validation performed

- `cargo test` — 162 passed.
- `cargo clippy --all-targets --all-features -- -D warnings` — passed.

## Remaining future coverage

These remain useful for a future custom runner but were not required for the
current conservative perf-05 solution:

- full command socket integration test around `/tmp/paneru.socket`;
- subscriber notification delivery under real socket IO;
- lost-focus/orphan-workspace watchdog tests driven by simulated runner time;
- graceful shutdown through the custom runner once that runner exists.

# Paneru performance profiling

This document is the repeatable baseline workflow for the perf tickets in
`plan/`. It intentionally does not require paid tooling.

## Current pre-optimization baseline

Before the event-driven loop changes, Paneru's runtime loop is polling-oriented:

- `src/ecs/systems.rs` uses a normal idle cap of `LOOP_MAX_TIMEOUT_MS = 50` ms.
- frame-active work uses `LOOP_MAX_TIMEOUT_FRAME_ACTIVE_MS = 16` ms.
- low-power mode uses `LOOP_MAX_TIMEOUT_LOWPOWER_MS = 500` ms.
- AppKit/AX run-loop sources, command socket events, config polling, and Bevy
  schedule execution are serviced from that loop.

That means an otherwise idle daemon is expected to wake regularly, roughly on a
50 ms cadence (about 20 idle wake opportunities per second) before additional
macOS/AX/config events are considered.

## Current architecture after perf-01 through perf-06

The current shippable branch intentionally keeps the responsive legacy wait
model, but reduces per-tick cost:

- **Legacy Cocoa/AppKit pump retained by default.** `pump_events` still runs at
  the start of Bevy `PreUpdate` and pumps AppKit with the established timeout
  ramp. This preserves shortcut and animation responsiveness.
- **Idle watchdog polling is intentional.** The ~50 ms idle cap is not an
  accidental busy loop; it is the current safety mechanism that prevents missed
  macOS/AppKit/AX wakeups or Bevy-internal follow-up work from becoming visible
  latency. Low-power mode keeps the previous 500 ms cap.
- **Frame-active work stays fast.** Active repositioning, resizing, scrolling,
  and flash messages keep the 16 ms frame cap.
- **External events are wakeable.** `EventSender::send` queues the event and then
  wakes the main run loop with `CFRunLoopWakeUp`; this is a prerequisite for a
  future custom runner, but not relied on as the only responsiveness mechanism.
- **Bevy schedules are single-threaded by default.** Paneru's schedules use
  `ExecutorKind::SingleThreaded` to avoid multi-thread scheduler/worker overhead
  for a mostly main-thread/macOS-bound workload.
- **Native-tab reconciliation is throttled.** The expensive AX/window-list pass
  runs every 250 ms instead of every app tick.
- **Custom top-level runner migration has started.** The default runner now owns
  the same legacy-equivalent wait cadence outside `pump_events`; it intentionally
  preserves the 16 ms / ~50 ms / 500 ms caps before any deep-idle policy is
  attempted.

The tradeoff is explicit: Paneru keeps a moderate idle wake frequency to protect
command/shortcut latency, while reducing the amount of work done on each wake.

## One-command local profile

Start or reload Paneru first, then run:

```sh
scripts/profile-idle.sh
```

Common variants:

```sh
# Write artifacts to a specific directory.
scripts/profile-idle.sh --out perf-runs/baseline

# Profile a specific running daemon.
scripts/profile-idle.sh --pid "$(pgrep -x paneru | head -n 1)"

# Use the exact CLI that Karabiner/launchd runs for query latency.
PANERU_BIN=/path/to/paneru scripts/profile-idle.sh
```

The script creates a timestamped directory under `perf-runs/` containing:

- `summary.txt` — machine/run metadata and a concise result summary.
- `ps-before.txt` / `ps-after.txt` — process CPU and thread count snapshots.
- `top.txt` — 30 second CPU/thread sample; wakeups are included when supported
  by the local macOS `top` version.
- `powermetrics.txt` — best-effort task wakeup/energy data. This may require
  `sudo` or fail on machines without permission; that is acceptable.
- `query-active-latency.txt` — latency for repeated `paneru query active` calls.
- `sample.txt` — a short stack sample for identifying idle work.

Keep raw `perf-runs/` output out of source control unless a small, anonymized
excerpt is intentionally added to explain a regression or result.

## Runtime loop diagnostics

After perf-02, the daemon exposes low-overhead wake counters:

```sh
paneru query runtime-diagnostics --json
```

Use this with the profile artifacts to see whether recent loop iterations were
servicing Cocoa, internal socket/AX/config events, frame-active work, flash
messages, or idle watchdog timeouts. Counters reset when the daemon restarts.

## Wakeable internal event queue

After perf-03, `EventSender::send` queues the event and then calls
`CFRunLoopWakeUp` on the main Cocoa run loop. Apple documents this wake function
as cross-thread safe for waking a target run loop, and Paneru only stores the
process-lifetime main run-loop pointer. This makes command socket requests,
queries, config notifications, AX callbacks, and shutdown requests able to wake
the main loop immediately instead of waiting for a polling timeout.

The ordering is intentional: event first, wake second. If the receiver is gone,
`send` returns an error and does not signal a wake. Unit tests cover successful
background-thread wake signalling, disconnected receiver behavior, and burst
send delivery.

## Rollout controls and experiments

### Single-threaded schedules

perf-05 keeps the known-responsive legacy Cocoa pump / ~50 ms idle cadence and
switches Paneru's Bevy schedules to the single-threaded executor by default. The
intent is to reduce scheduler/worker overhead without changing wake timing.

Fallback while testing:

```sh
PANERU_MULTI_THREADED_SCHEDULES=1 paneru
```

Use `scripts/profile-idle.sh` to compare idle CPU, thread activity, wakeups, and
`paneru query active` latency with and without the fallback. This experiment also
throttles the expensive native-tab reconciliation pass to 250 ms while preserving
the legacy input/animation cadence.

Current local result after manual reload/testing: responsiveness felt good,
native tabs still worked, and `scripts/profile-idle.sh --out
perf-runs/perf-05-throttled-single-thread` reported 1.8% Paneru CPU at the end
of the 30s `top` window with `paneru query active` median latency of 11.066 ms.

### Computed idle deadline / CFRunLoop pump experiment

perf-04 is in spike mode and is not considered shippable. See
[`docs/perf-loop-spike-handoff.md`](perf-loop-spike-handoff.md) for the detailed
attempt log and recommendations.

Earlier long-idle-deadline attempts caused visible shortcut and animation
latency because AppKit's `nextEventMatchingMask` wait did not reliably return
when non-NSEvent run-loop sources (CGEventTap/AX callbacks) enqueued Paneru
internal events.

The CFRunLoop pump / computed-deadline spike is now opt-in only because manual
testing found it still introduced variable latency:

```sh
PANERU_CF_RUN_LOOP_PUMP=1 paneru
```

Default runtime behavior preserves the legacy AppKit pump and idle timeout ramp
while perf-05 tests per-tick cost reductions.

### Custom top-level runner migration

perf-08 added a pure deterministic runtime-driver policy model and fake-clock
harness. perf-09 then moved ownership of the legacy wait cadence into a custom
Bevy runner without introducing deep idle sleeps. perf-10 added dedicated
`CFRunLoopSource` wake and `CFRunLoopTimer` deadline primitives. perf-11 added a
`RuntimeDirty` settle loop so Bevy-internal messages/triggers can request an
immediate follow-up update before the runner waits. perf-12 added a runner-visible
`RuntimeDeadlines` registry for frame work, native-tab reconciliation,
lost-focus/orphan/refresh watchdogs, low-power checks, periodic maintenance,
state save, and timeout components. perf-13 added conservative adaptive idle:
Paneru keeps the fast legacy cadence for a short grace period after input,
window/focus/layout/command, or animation activity, then quiet idle may sleep
until the next runner-visible deadline (capped conservatively). The custom runner
is on by default and preserves the accepted interactive caps:

- 16 ms while frame-active work is present;
- ~50 ms during normal idle / recent interactive activity;
- 500 ms in low-power mode;
- quiet idle after the grace window may wait until the next visible deadline,
  capped at 1000 ms.

Fallback while testing the runner architecture:

```sh
PANERU_LEGACY_IN_SYSTEM_WAIT=1 paneru
```

This restores the old shape where `pump_events` performs the blocking wait from
`PreUpdate`.

Fallback while testing adaptive idle only:

```sh
PANERU_LEGACY_IDLE_CADENCE=1 paneru
```

This keeps the custom runner but forces the legacy ~50 ms idle cadence.

Runtime diagnostics include dirty-settle counters, the last dirty reasons, recent
activity reasons, and the current runner-visible deadline reason/duration. These
help identify whether a command, internal message, focus/animation change, or
watchdog requested immediate follow-up work. State queries/subscriptions are
served in the same update and intentionally do not request dirty follow-up work.

### Fallback knob summary

| Knob | Default | Effect | Removal criteria |
| --- | --- | --- | --- |
| `PANERU_LEGACY_IN_SYSTEM_WAIT=1` | unset | Restores the pre-runner in-system wait path for comparison or emergency fallback. | Remove after the custom legacy-equivalent runner has shipped without responsiveness regressions. |
| `PANERU_LEGACY_IDLE_CADENCE=1` | unset | Keeps the custom runner but disables adaptive quiet idle, forcing the legacy ~50 ms cadence. | Remove after adaptive idle has shipped with acceptable latency and profile results. |
| `PANERU_MULTI_THREADED_SCHEDULES=1` | unset | Restores Bevy's multi-threaded schedule executor for comparison or emergency fallback. | Remove after single-threaded scheduling has shipped across enough macOS/Paneru usage without regressions. |
| `PANERU_CF_RUN_LOOP_PUMP=1` | unset | Enables the non-shippable CFRunLoop pump/deadline spike. | Remove or replace when a real CFRunLoop source/timer runner supersedes the spike. |

## Robustness tests added

perf-06 added deterministic tests for the risks found during the loop spike:

- state queries respond on the next Bevy update;
- runtime diagnostics queries respond on the next Bevy update;
- Bevy-internal command follow-up work is not dependent on the external wake
  queue;
- animation/reposition work is created promptly at schedule level;
- native-tab reconciliation remains throttled to 250 ms;
- fallback knobs have documented defaults.

Existing perf-03 tests cover external event burst delivery and disconnected
receiver behavior.

perf-08 through perf-14 added deterministic coverage for the custom runner work:

- pure runtime policy decisions for external events, dirty Bevy work, active
  animation, shutdown, watchdog deadlines, recent activity, and low-power caps;
- custom runner startup ordering, timeout-ramp reset after internal events, and
  dirty settle-loop guard behavior;
- `CFRunLoopSource` wake coalescing and `CFRunLoopTimer` rescheduling/invalidation
  behavior through fakes and unit tests;
- runner-visible deadline registry selection/rescheduling for frame work,
  watchdogs, state save, native-tab reconciliation, and timeout components;
- adaptive-idle transitions from active/recent activity to quiet idle without
  sleeping past visible repair deadlines;
- native-tab close focus handoff to the remaining tab, preventing stale
  Paneru/sketchybar focus after closing a Ghostty tab.

## PR / handoff summary

- Baseline tooling: `scripts/profile-idle.sh` and this document.
- Accepted conservative changes from perf-01 through perf-06: diagnostics,
  wakeable external queue, single-threaded schedules by default, native-tab
  reconciliation throttling.
- Deferred perf-04 spike: computed long-idle deadlines inside `pump_events` were
  not shippable; see [`docs/perf-loop-spike-handoff.md`](perf-loop-spike-handoff.md).
- New custom-runner work from perf-08 through perf-14: deterministic policy model,
  top-level runner, dedicated `CFRunLoopSource`/`CFRunLoopTimer` primitives,
  dirty settle-loop, runner-visible deadline registry, adaptive idle, validation,
  docs, and fallback knobs.
- Manual result on this branch: responsiveness felt good throughout reload and
  normal use; first-interaction sluggishness was fixed by resetting the timeout
  ramp after internal events; Ghostty native-tab close focus no longer reproduced
  after explicitly focusing the remaining tab; sketchybar focus stayed
  consistent; adaptive idle felt at least as responsive as the legacy cadence.
- Current profile after the final layout-dirty fix:
  `scripts/profile-idle.sh --out perf-runs/perf-14-final-after-layout-dirty-fix
  --seconds 30 --queries 20` reported Paneru CPU at 2.2% at the end of the 30s
  `top` window and `paneru query active` median latency of 8.045 ms, max 11.557
  ms. The command-line `top` on this macOS version did not expose a wakeups
  column.

## Interpreting results

For every perf ticket, compare against the same machine, build type, and usage
state where possible.

A useful report includes:

1. Paneru commit/build and macOS version from `summary.txt`.
2. Idle CPU over the 30 second `top` interval.
3. Wakeups from `top.txt`, `powermetrics.txt`, or Activity Monitor > Energy when
   the command-line tools do not expose them.
4. Thread count from `ps-after.txt` or `top.txt`.
5. `paneru query active` median and max latency.
6. The top idle stack themes from `sample.txt`.

"Good enough" for the first baseline ticket means the numbers are reproducible:
rerunning the script in the same state should produce the same order of magnitude
for idle CPU/wakeups and similar query latency. Later tickets should show lower
idle CPU/wakeups without large increases in query latency or missed repairs.

## Troubleshooting

- **No Paneru PID found:** reload/start Paneru first, or pass `--pid`.
- **Wrong CLI measured:** set `PANERU_BIN` to the binary used by the running
  service. Avoid `cargo run` for latency measurements because Cargo startup time
  dominates the result.
- **`sample` fails:** grant the terminal/developer tool permission to inspect the
  process, or rerun from a terminal with the required macOS security approval.
- **No wakeup column in `top`:** macOS versions differ. Check
  `powermetrics.txt`, run `sudo powermetrics --samplers tasks`, or use Activity
  Monitor's Energy/Wakeups columns during the same idle window.
- **High numbers while testing:** ensure no animation, gesture scroll, flash
  message, config edit, or window launch is active during the idle window.

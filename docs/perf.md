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
macOS/AX/config events are considered. Later perf tickets should reduce idle CPU
and wakeups while preserving command latency and watchdog repairs.

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

## Single-threaded schedule experiment

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

## Computed idle deadline experiment

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

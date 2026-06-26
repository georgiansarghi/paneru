# Runtime loop performance spike handoff

Branch: `perf-loop-spike`

This document is a historical handoff for the earlier perf-04 long-idle spike.
That specific attempt is **not considered shippable** because every attempt to
substantially extend idle sleep inside the old in-system wait path introduced
user-visible shortcut/animation latency.

Newer perf-08 through perf-22 work supersedes this conclusion by moving the wait
boundary into a custom top-level runner, adding a dedicated wake source/timer,
nonblocking external-event drain, Bevy-internal dirty settling, and
runner-visible deadlines. See `docs/perf.md` for the current accepted runtime
architecture and rollout checklist.

## Original problem

Paneru is responsive because its Bevy app update includes a `pump_events` system
that blocks briefly while pumping Cocoa/AppKit and then drains Paneru's internal
event queue. The legacy timeout ramps up to about 50 ms while idle, so even if a
wake signal is missed or delayed the next poll arrives quickly.

The cost is idle CPU/wakeups. The user observed roughly 3-5% CPU while Paneru
was otherwise idle.

## Work completed on this branch

### perf-01: profiling baseline tooling

Added:

- `scripts/profile-idle.sh`
- `docs/perf.md`
- `/perf-runs` gitignore entry

The script collects local idle CPU/thread/wakeup-ish data, query latency for
`paneru query active`, and a short `sample` stack. Raw profile outputs are kept
out of source control.

### perf-02: runtime loop diagnostics

Added low-overhead runtime loop counters exposed via:

```sh
paneru query runtime-diagnostics --json
```

Counters include Cocoa pump, internal event, timeout/watchdog, frame-active,
flash-message, and periodic-maintenance wake accounting. Unit tests cover the
classification and diagnostic counter logic.

### perf-03: wakeable internal event queue

Replaced the plain internal event transport with a `WakeableEventQueue` wrapper.
`EventSender::send` now:

1. queues the event on the MPSC channel;
2. wakes the main run loop via `CFRunLoopWakeUp`;
3. returns send errors without waking if the receiver is gone.

Tests cover:

- event queued before wake;
- sending from a background thread;
- disconnected receiver behavior;
- burst sends without event loss.

This appears useful as a prerequisite, but it was not sufficient by itself to
make long idle sleeps responsive.

## perf-04 attempts and results

### Attempt 1: computed deadlines with long idle sleep

Changed the runtime policy from ramping to the legacy ~50 ms idle timeout to a
computed deadline policy with long idle sleeps, initially around 1000 ms and then
100 ms/250 ms variants.

Result: **failed manual UX testing**.

Observed behavior:

- shortcuts were noticeably sluggish;
- target/focus highlights appeared before window movement;
- animations started late and felt unsmooth;
- latency varied from attempt to attempt.

Interpretation: internal state changes or macOS/AppKit events could happen, but
Bevy did not reliably run the next update quickly enough to perform layout and
animation work.

### Attempt 2: immediate next tick after internal events

After draining any internal event, forced the next timeout to 1 ms so work queued
by the current update would be processed quickly on the following update.

Result: **still sluggish**.

This suggested the delay was not only after internal queue draining. Some delay
likely occurred before the queued event reached Bevy, while the loop was blocked
inside the Cocoa/AppKit pump.

### Attempt 3: AppKit pump returns after one event

Modified `PlatformCallbacks::pump_cocoa_event_loop` so it blocked for at most one
NSEvent, then drained already-pending NSEvents without blocking again.

Result: **still sluggish**.

This suggested Paneru's important inputs are not always represented as normal
queued AppKit `NSEvent`s, or that non-NSEvent run-loop sources were still not
coordinated properly with Bevy updates.

### Attempt 4: CFRunLoopRunInMode spike

Retained as an opt-in experiment, not the default. The Cocoa pump has a spike path using:

```text
CFRunLoopRunInMode(kCFRunLoopDefaultMode, timeout, returnAfterSourceHandled=true)
```

Then it drains pending AppKit events without blocking. It can be enabled with:

```sh
PANERU_CF_RUN_LOOP_PUMP=1 paneru
```

The default path was restored to the legacy AppKit pump / 50 ms idle ramp before
starting perf-05. Idle deadline in this spike was reduced to 100 ms after 250 ms
still showed noticeable lag.

Result: **better than previous attempts but still not good enough**.

Manual observation:

- responsiveness improved versus earlier perf-04 attempts;
- there is still noticeable, variable latency;
- not acceptable compared with the original legacy loop.

## Current hypothesis

This is probably not simply "Bevy is slow". The issue appears to be coordination
between three layers:

1. macOS run-loop sources / AppKit / event tap / AX callbacks;
2. Paneru's internal event queue;
3. Bevy schedule execution.

The legacy 50 ms polling behavior masks coordination gaps. Longer sleeps expose
that some paths do not wake and drive Bevy deterministically enough for a window
manager, where shortcut-to-animation latency must feel immediate.

## Manager feedback incorporated

The failed perf-04 attempts failed for a structural reason: they kept the wait
inside a Bevy `PreUpdate` system. The branch tried to improve this shape:

```text
Bevy app.update()
  PreUpdate:
    pump_events() blocks/waits
    drain external queue
  Update/PostUpdate:
    process messages, mutate layout, animate, commit
```

Problems with this shape:

1. External wake is not the same as being Bevy-ready. `EventSender::send` can wake
   the CFRunLoop, but Bevy only processes the event after `pump_events` returns
   and schedules continue.
2. Bevy-internal work is not wakeable. Code paths like
   `commands.trigger(SendMessageTrigger(Event::WindowFocused { ... }))` write to
   Bevy's internal messages/triggers, not the external `WakeableEventQueue`. If a
   follow-up update is needed, the next update can immediately re-enter
   `pump_events` and sleep despite pending internal work.
3. The computed deadline policy only sees Paneru `Timeout` components. Bevy
   `on_timer(...)` run conditions have internal deadline state that is not
   visible to the policy.
4. `CFRunLoopWakeUp` is weaker than a dedicated run-loop source/timer whose
   callback deterministically hands control back to Paneru's scheduler.
5. Long idle sleeps expose every missed wake or pending-internal-message delay as
   shortcut/animation lag.

Revised conclusion: do not continue tuning idle constants in the existing
`pump_events` design. A real event-driven implementation should move waiting
outside Bevy into a custom top-level runner, or short-term work should focus on
reducing per-tick cost while preserving the legacy 50 ms cadence.

## Important implementation detail

Paneru uses Bevy `MinimalPlugins`, whose default schedule runner loops as fast as
possible. Paneru's effective sleep is not primarily Bevy's runner sleep; it is
inside `systems::pump_events`, which blocks while pumping Cocoa/AppKit and then
waiting briefly for internal events.

So a real fix likely needs to redesign that wait/update boundary, not just tune a
constant.

## Possible next approaches

These are suggestions for a future attempt, not completed work.

1. **Custom Bevy runner / explicit main-loop state machine**
   - Stop blocking inside a Bevy system.
   - Own the top-level loop explicitly:
     1. run one Bevy update;
     2. compute next deadline from ECS/runtime state;
     3. wait on a run-loop source/timer;
     4. wake and immediately run Bevy again.
   - This may make the wake-to-Bevy-update relationship easier to reason about.

2. **Use a real CFRunLoopSource/CFRunLoopTimer pair**
   - Instead of only `CFRunLoopWakeUp`, create a custom source or timer that is
     part of the main run loop's wait set.
   - Signal that source from `EventSender::send` and callbacks.
   - Ensure the source callback sets a flag and returns to the app runner, not
     just wakes a wait that may continue inside AppKit.

3. **Adaptive conservative policy**
   - Keep 50 ms idle polling for a grace period after any input/window event.
   - Only extend idle sleep after several seconds of quiet.
   - This may recover some idle CPU without risking interactive latency.

4. **Optimize per-tick cost instead of reducing tick frequency**
   - perf-05's single-threaded Bevy schedule experiment may reduce worker/thread
     overhead while preserving the known-good polling cadence.
   - Audit systems that run every tick and gate them behind change detection or
     explicit timers.

5. **Measure before changing again**
   - Use `scripts/profile-idle.sh` and `paneru query runtime-diagnostics --json`.
   - Compare legacy pump vs spike with the same build/machine.
   - Treat manual shortcut/animation responsiveness as a hard acceptance gate.

## Status recommendation

Do not merge perf-04 as-is. Keep perf-01 through perf-03 concepts if desired,
but only ship the runtime-loop deadline change after a design proves that
macOS/AppKit wake sources and Bevy updates are synchronized with no perceptible
latency regression.

# perf-04: Replace in-system waiting with a custom wakeable runner

## Revised status after `perf-loop-spike`

Not done / not shippable.

The first perf-04 implementation tried to make the existing pattern smarter:

```text
Bevy app.update()
  PreUpdate:
    pump_events() blocks/waits
    drain external queue
  Update/PostUpdate:
    process messages, mutate layout, animate, commit
```

Manual testing showed that extending idle sleeps inside this shape causes visible,
variable shortcut and animation latency. Do **not** try to salvage this by tuning
100 ms vs 250 ms vs 1000 ms. The issue is structural: the wait happens inside a
Bevy system at the start of a frame.

See `docs/perf-loop-spike-handoff.md` for the detailed attempt log and manager
feedback summary.

## Revised goal

Move waiting out of `pump_events`/`PreUpdate` and prove a top-level wakeable
runner can reduce idle wakeups while preserving current 50 ms-polling
responsiveness.

## Problem

The legacy loop is responsive because a 50 ms poll masks coordination gaps
between:

1. macOS/AppKit/AX/event-tap run-loop sources;
2. Paneru's external `WakeableEventQueue`;
3. Bevy's internal `Messages<Event>`, triggers, timers, and schedules.

The spike showed several hidden wake/deadline issues:

- `CFRunLoopWakeUp` wakes the run loop, but does not guarantee Bevy runs at the
  right time when the wait is buried inside a Bevy system.
- Bevy-internal messages/triggers, such as `commands.trigger(SendMessageTrigger(...))`,
  do not go through the external wakeable queue.
- Bevy `on_timer(...)` run conditions have internal deadline state that the
  Paneru timeout policy cannot currently inspect.
- Long sleeps expose any missed wake or pending internal Bevy work as visible
  shortcut/animation lag.

## Revised scope

Create a spike implementation that changes the loop ownership model instead of
only changing timeout constants.

Target architecture:

```text
loop:
  drain external events into Bevy
  run Bevy update
  if Bevy generated follow-up work, run another update immediately/soon
  compute next deadline
  wait on CFRunLoop source/timer/socket
```

Key requirements:

- Do not block in `pump_events` as a `PreUpdate` system.
- Add an explicit "Bevy dirty / immediate next update" mechanism for work
  generated inside Bevy schedules.
- Use a real `CFRunLoopSource` and/or `CFRunLoopTimer` rather than relying only
  on `CFRunLoopWakeUp`.
- Account for external events, active animations, Paneru `Timeout` components,
  Bevy timer/run-condition needs, state-save cadence, lost-focus watchdog, and
  orphan-workspace watchdog.
- Keep a fallback/feature flag/env knob for legacy polling during the spike.

## Red-Green development requirement

1. **Red:** Add a deterministic runner-policy test that fails if pending
   internal Bevy work can be followed by a long wait.
2. **Red:** Add a regression test/harness showing external command/query events
   must cause an immediate Bevy update from idle.
3. **Green:** Implement the custom runner/wake source so both pass.
4. **Manual Green:** User confirms shortcut-to-animation responsiveness is
   indistinguishable from the legacy 50 ms loop.

## Acceptance criteria

- Idle CPU and wakeups improve versus perf-01 baseline.
- Shortcut, command, query, focus, and animation latency feel indistinguishable
  from the legacy loop in manual testing.
- `paneru query active` latency remains low while idle.
- Animations and gesture scrolling remain smooth.
- Lost focus and orphan workspace recovery still run on schedule.
- Existing tests pass.
- A fallback legacy polling knob exists and is documented during rollout.

## Robustness requirements

- No lost external events.
- No stranded Bevy-internal messages/triggers behind a long wait.
- A missed macOS notification must still be repaired by a watchdog deadline.
- State save must still happen periodically.
- The loop must not sleep past an active animation frame.
- The loop must not spin when no work is present.

## Quality bar

- Do not tune idle constants as a substitute for fixing loop ownership.
- Deadline policy must be deterministic and unit-tested without real sleeping.
- Include before/after performance numbers and manual responsiveness notes.
- If the custom runner spike fails, document why and fall back to preserving the
  legacy 50 ms cadence while optimizing per-tick cost in later tickets.

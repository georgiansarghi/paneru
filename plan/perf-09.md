# perf-09: Introduce a custom top-level runner with legacy-equivalent timing

## Goal
Move waiting out of Bevy `PreUpdate` while preserving the current responsive legacy behavior exactly enough that users cannot tell the difference.

## Problem
The current shape blocks inside a Bevy system:

```text
app.update()
  PreUpdate: pump_events() waits
  Update/PostUpdate: process work
```

This makes long sleeps structurally unsafe because the runner cannot see Bevy-internal follow-up work before deciding to wait. The first production refactor must change loop ownership without also changing timing.

## Scope
- Add a `RuntimeDriver`/custom Bevy runner that owns the main loop.
- Extract external event draining from `systems::pump_events` into a callable function used by the runner.
- Make `pump_events` stop performing long blocking waits when the custom runner is enabled.
- Preserve the legacy timeout ramp/caps initially:
  - 16 ms frame-active cap
  - ~50 ms normal idle cap
  - 500 ms low-power cap
- Add an env fallback to use the old in-system wait path during the migration.
- Do **not** introduce deep idle sleeps yet.

## Red-Green development requirement
1. **Red:** Add a runner-level test using the perf-08 fake harness showing that an external event wakes/drains and runs one Bevy update before any wait.
2. **Red:** Add a test showing the custom runner uses legacy-equivalent wait caps in idle/frame-active/low-power modes.
3. **Green:** Implement the custom runner with legacy-equivalent timing.
4. **Manual Green:** User confirms shortcut/focus/layout/animation responsiveness is indistinguishable from the current build.

## Acceptance criteria
- Waiting is owned by the custom top-level runner, not by `pump_events` in `PreUpdate`, when the new runner is enabled.
- Default behavior remains responsive and uses legacy-equivalent timing.
- `paneru query active` latency remains comparable to current perf-05 measurements.
- Existing tests pass.
- Fallback knob is documented.

## Robustness requirements
- No lost external events.
- Shutdown exits cleanly.
- Bevy startup schedules still run exactly once.
- Main-thread AppKit/CoreGraphics constraints remain satisfied.

## Quality bar
- Keep the initial runner boring: no adaptive/deep idle in this ticket.
- Avoid mixing architecture movement with performance-policy changes.
- Add tracing/debug diagnostics to identify whether the legacy or custom runner is active.

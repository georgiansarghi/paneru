# perf-18: Make RuntimeDeadlines the source of truth for watchdogs and maintenance

## Goal
Replace duplicated Bevy `on_timer(...)` timing with runner-visible deadline state for critical watchdogs and maintenance systems.

## Problem
`RuntimeDeadlines` currently mirrors several Bevy timers so the runner can avoid sleeping past them, but some systems still use hidden `on_timer(...)` run-condition state. This means there are two timing systems:

```text
RuntimeDeadlines
Bevy on_timer run conditions
```

That is acceptable for transition, but production-ready deep idle should make runner-visible deadlines the source of truth for critical repair and maintenance work.

## Scope
- Introduce a clear deadline ownership pattern:
  - systems register or consume named deadlines;
  - runner wakes for the earliest deadline;
  - systems reschedule after running.
- Migrate critical timers away from hidden `on_timer(...)` where practical:
  - lost-focus recovery;
  - orphan workspace repair/reparenting;
  - refresh workspace window sizes;
  - low-power check;
  - periodic state save;
  - periodic diagnostics/maintenance;
  - native-tab reconciliation if still timer-driven.
- Keep behavior-equivalent periods unless a separate ticket changes policy.
- Add diagnostics for due, consumed, and rescheduled deadlines.

## Red-Green development requirement
1. **Red:** Add tests showing a migrated watchdog runs when its named deadline is due without relying on Bevy `on_timer`.
2. **Red:** Add tests showing a migrated watchdog does not run before its deadline.
3. **Green:** Migrate one watchdog at a time, with deterministic fake-clock tests for each.
4. **Green:** Remove or reduce hidden `on_timer(...)` usage only after equivalent tests pass.

## Required tests
- Lost-focus recovery deadline is registered, consumed, and rescheduled.
- Orphan workspace repair deadline is registered, consumed, and rescheduled.
- Refresh-window-sizes deadline is registered, consumed, and rescheduled.
- Periodic state save runs on due deadline and reschedules after success.
- Low-power check deadline does not sleep past repair deadlines.
- Native-tab reconciliation deadline behavior matches perf-16 policy.

## Acceptance criteria
- Critical watchdog timing is visible to the runner as source-of-truth state.
- Hidden Bevy timer state is not required for critical repair deadlines.
- Existing behavior periods remain conservative and documented.
- Existing tests pass.

## Robustness requirements
- Missed macOS notifications are still repaired by watchdog deadlines.
- State save still happens periodically.
- Deadlines cannot grow stale indefinitely; each due deadline must be consumed or rescheduled explicitly.
- A failed watchdog run must not permanently disable future repair attempts.

## Quality bar
- Migrate incrementally; each migrated timer gets tests.
- Prefer explicit named deadline constants and comments explaining why they exist.
- Do not remove safety watchdogs for performance reasons.

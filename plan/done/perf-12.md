# perf-12: Make runner-visible deadline registry for watchdogs and Bevy timers

## Goal
Give the custom runner a complete enough view of Paneru deadlines to sleep safely without missing watchdogs or hidden Bevy timer run conditions.

## Problem
The perf-04 spike only considered Paneru `Timeout` components. Bevy `on_timer(...)` run conditions maintain internal state that the custom policy cannot inspect. If those remain hidden, a long-idle runner can sleep past lost-focus recovery, workspace repair, state save, or other periodic maintenance.

## Scope
- Introduce a runner-visible deadline registry/resource, e.g. `RuntimeDeadlines`.
- Register named deadlines for:
  - active animation/repositioning/resizing frame deadline
  - scrolling/inertia frame deadline
  - flash message update/removal
  - lost focus recovery watchdog
  - workspace/display orphan repair watchdog
  - refresh workspace window sizes
  - low-power mode check
  - periodic state save
  - startup restore grace / Paneru `Timeout` components
- Replace or mirror critical `on_timer(...)` usages so the runner can compute the next wake accurately.
- Keep conservative fallback caps where a deadline is not yet visible.

## Red-Green development requirement
1. **Red:** Add policy tests showing that hidden timers cause the runner to choose an unsafe long wait.
2. **Green:** Add deadline registry entries until the runner chooses the earliest required deadline.
3. **Red/Green:** For each migrated watchdog, add a deterministic test proving the deadline is scheduled and consumed.

## Required tests
- Earliest deadline wins across frame, watchdog, state-save, and timeout-component deadlines.
- Lost-focus watchdog deadline is scheduled even when no external events are pending.
- Orphan-workspace repair deadline is scheduled even when no external events are pending.
- Periodic state save deadline is scheduled and rescheduled after running.
- Low-power mode can choose a different cap without sleeping past critical repair deadlines.

## Acceptance criteria
- Runner deadline policy is deterministic and unit-tested without real sleeping.
- Critical watchdogs no longer depend on hidden Bevy timer state that the runner cannot see.
- Existing tests pass.

## Robustness requirements
- Missed macOS notifications are still repaired by watchdog deadlines.
- State save still happens periodically.
- Active animation frame deadlines always beat idle/deep-idle deadlines.
- Deadline registry cannot grow stale indefinitely; systems must clear/reschedule named deadlines explicitly.

## Quality bar
- Name constants and document why each deadline exists.
- Do not remove safety watchdogs in the name of performance.
- Add diagnostics that report the chosen next deadline and reason.

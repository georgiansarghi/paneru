# perf-15: Unify production runner decisions with the deterministic policy model

## Goal
Make the production custom runner use the same deterministic policy path that unit tests exercise, eliminating drift between `RuntimeDriverPolicy::decide(...)` and `RuntimeDriver::next_timeout_ms(...)`.

## Problem
The current branch has a good pure policy model, but production timeout selection is implemented separately in `RuntimeDriver::next_timeout_ms(...)`. That means tests can pass while production behavior diverges, especially around recent activity, visible deadlines, low-power mode, and quiet-idle caps.

There is also a small spin risk: converting sub-millisecond deadline durations to milliseconds can produce `0`, which may cause a zero-timeout wait/update loop if a due deadline is not cleared or rescheduled correctly.

## Scope
- Refactor production runner decision-making to call the pure policy model, or replace the pure model with a production decision function that remains pure and directly unit-tested.
- Model all production inputs in one deterministic snapshot:
  - external event pending / just drained
  - dirty Bevy work pending
  - active frame work
  - recent runtime activity
  - next runner-visible deadline
  - low-power mode
  - shutdown requested
- Clamp wait durations used for actual OS waits to at least 1 ms unless the decision is explicitly `RunUpdateNow`.
- Keep fallback knobs unchanged.
- No intentional behavior change beyond removing policy drift and zero-timeout waits.

## Red-Green development requirement
1. **Red:** Add a test that mutates the pure policy expectations and proves production timeout selection would currently diverge.
2. **Red:** Add a test for a sub-millisecond / due visible deadline proving the runner does not schedule a `0 ms` wait.
3. **Green:** Refactor production to use the tested policy path and clamp wait durations.
4. **Green:** Existing responsiveness and diagnostics behavior remains unchanged in manual testing.

## Required tests
- Production decision for active animation returns frame cadence.
- Production decision for recent activity returns legacy fast cadence.
- Production decision for quiet idle respects the earliest visible deadline.
- Production decision for low-power mode does not sleep past repair deadlines.
- Sub-millisecond or already-due wait durations are clamped to 1 ms for OS waits, or converted to `RunUpdateNow` explicitly.
- Fallback `PANERU_LEGACY_IDLE_CADENCE=1` forces legacy idle cadence through the same policy path.

## Acceptance criteria
- There is one authoritative runner decision path used by both tests and production.
- No production wait path can schedule an accidental `0 ms` wait.
- `cargo test` and strict clippy pass.
- Manual shortcut/focus/layout responsiveness remains at least as good as perf-14.

## Robustness requirements
- The runner must not spin if a deadline is stale or due.
- The runner must not sleep past active animation or repair deadlines.
- Policy tests must not use real sleeps or wall-clock timing.

## Quality bar
- Keep policy types small, explicit, and readable.
- Prefer testing observable decisions over implementation details.
- Document why `RunUpdateNow` vs minimum `1 ms` wait is chosen for due deadlines.

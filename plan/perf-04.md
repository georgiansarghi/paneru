# perf-04: Replace fixed idle polling with computed deadlines

## Goal
Reduce idle CPU by running Paneru only when there is work or when a known deadline is due, while preserving watchdog robustness.

## Problem
The current loop uses fixed polling timeouts, waking about every 50ms while idle. This keeps Paneru responsive but causes avoidable idle CPU. A pure event-only loop would risk missed repairs and stalled commands, so the correct design is event-driven with computed deadlines.

## Scope
- Refactor loop timeout selection into a pure, tested policy.
- Deadlines must account for:
  - active animation/repositioning/resizing: ~16ms frame deadline
  - active scrolling/inertia: ~16ms frame deadline
  - flash messages: next display/update deadline
  - restore grace / timeout ticker needs
  - lost focus recovery watchdog
  - workspace/display orphan repair watchdog
  - periodic state save
  - low power mode
  - no active work: long sleep or wait until next watchdog
- Preserve safety watchdogs; do not remove fallback repair behavior.

## Red-Green development requirement
1. **Red:** Add tests for deadline policy showing current fixed-idle behavior is too eager or not deadline-aware.
2. **Green:** Implement computed deadlines and make tests pass.
3. Add regression tests for command/query wake behavior using the wakeable channel from perf-03.

## Acceptance criteria
- Idle CPU and wakeups improve versus perf-01 baseline.
- Command/query latency remains acceptable and documented.
- Animations and gesture scrolling remain smooth.
- Lost focus and orphan workspace recovery still run on schedule.
- Existing tests pass.

## Robustness requirements
- A missed macOS notification must still be repaired by a watchdog deadline.
- State save must still happen periodically.
- The loop must not sleep past an active animation frame.
- The loop must not spin when no work is present.

## Quality bar
- Deadline policy should be deterministic and unit-tested without real sleeping.
- Keep constants named and documented.
- Include before/after performance numbers in the PR description.

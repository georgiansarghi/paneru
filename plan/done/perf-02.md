# perf-02: Add runtime loop instrumentation and wake reason logging

## Goal
Make Paneru’s main loop observable enough to prove why it wakes and what work each tick performs.

## Problem
The current loop wakes periodically and runs Bevy schedules. Samples show a mix of Cocoa event-loop blocking, Bevy executor overhead, and occasional AX/event-tap work. We need structured instrumentation before altering the loop.

## Scope
- Add low-overhead counters for loop wake reasons, e.g.:
  - Cocoa event pump
  - internal event received
  - timeout/watchdog tick
  - animation/resize/scrolling active
  - flash message active
  - periodic maintenance
- Add optional debug logging or query/debug output for those counters.
- Keep instrumentation disabled or very low cost by default.
- Do not yet change the polling/deadline behavior.

## Red-Green development requirement
1. **Red:** Add a failing test or assertion showing the absence of wake reason accounting.
2. **Green:** Implement wake reason accounting and test it with deterministic unit-level coverage where possible.

## Acceptance criteria
- Tests cover at least the logic that classifies frame-active vs idle wake reasons.
- A developer can enable diagnostics and see why Paneru is waking.
- No noticeable CPU regression from instrumentation when diagnostics are disabled.
- Existing test suite passes.

## Quality bar
- Instrumentation must not rely on wall-clock sleeps in unit tests.
- Prefer small pure functions for wake/deadline classification.
- Avoid log spam in normal operation.

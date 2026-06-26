# perf-08: Build deterministic runtime-driver policy model and test harness

## Goal
Create a deterministic test foundation for the custom top-level runner before moving any production waiting logic out of Bevy systems.

## Problem
The perf-04 spike failed partly because tests covered small pieces (`EventSender` wake, deadline functions) but not the whole wake-to-Bevy-update flow. We need tests that can prove a future runner will not sleep while external events, Bevy-internal messages, animations, or deadlines require immediate work.

## Scope
- Add a pure runner-policy module/model that does **not** call AppKit, sleep, or depend on wall-clock time.
- Model at least:
  - external event queue non-empty
  - Bevy-internal dirty work pending
  - active animation/reposition/resize/scroll/flash work
  - recent interactive activity grace window
  - next visible watchdog/deadline
  - requested shutdown
- Add a fake-clock/fake-waker test harness for runner decisions.
- No production loop behavior changes in this ticket.

## Red-Green development requirement
1. **Red:** Add failing tests showing the current conceptual policy would permit a long wait while follow-up Bevy work is pending.
2. **Green:** Implement the pure policy model until those tests pass.
3. Keep tests deterministic; no sleeps, no real sockets, no AppKit.

## Required tests
- External event pending => decision is `RunUpdateNow`, not wait.
- Bevy-internal dirty flag pending => decision is `RunUpdateNow`, not wait.
- Active animation/repositioning => next deadline is ~16 ms.
- Quiet idle with only watchdog => may wait until watchdog cap.
- Shutdown requested => exits without waiting.
- Recent input/window activity keeps a short interactive deadline for the configured grace period.

## Acceptance criteria
- A maintainer can read the policy tests and understand exactly when the runner may sleep.
- No runtime behavior changes are included.
- Existing test suite passes.

## Quality bar
- Use small pure types with clear names, e.g. `RuntimeDriverState`, `RunnerDecision`, `DeadlineReason`.
- Avoid testing implementation details that would make the later runner hard to refactor.
- Include comments explaining why each test guards against a perf-04 regression.

# perf-11: Add Bevy dirty/immediate-update tracking for internal follow-up work

## Goal
Ensure the custom runner never sleeps while Bevy-internal work requires a follow-up update.

## Problem
Not all Paneru work enters through the external wakeable queue. Many paths create Bevy-internal messages/triggers, for example:

```rust
commands.trigger(SendMessageTrigger(Event::WindowFocused { ... }))
```

Those do not signal the external queue. If the next loop iteration waits before processing the follow-up, users see delayed focus/layout/animation.

## Scope
- Add a resource such as `RuntimeDirty` / `ImmediateUpdateRequested` with structured reasons.
- Mark dirty for internal work that can require another Bevy update, including at least:
  - command handled
  - `SendMessageTrigger` emitted
  - focus marker changed
  - layout strip changed
  - reposition/resize/scrolling/flash markers inserted or still active
  - state query/subscriber response pending
- Custom runner should run another update immediately or after a minimal yield when dirty is set.
- Add a max settle-iteration guard to prevent infinite immediate-update loops.

## Red-Green development requirement
1. **Red:** Add a runner-level test where a Bevy system emits internal `SendMessageTrigger(Event::WindowFocused)` and prove the runner would incorrectly wait without dirty tracking.
2. **Red:** Add an integration-style harness test for command -> focus -> layout -> reposition marker within the same runner cycle.
3. **Green:** Implement dirty tracking and settle-loop behavior until tests pass.

## Required tests
- External command event causes update, sets dirty, and runner performs follow-up update before waiting.
- Internal `SendMessageTrigger` path is not dependent on `EventSender::send`.
- Active animation markers keep the runner on frame deadlines.
- Dirty loop terminates when no more work is pending.
- Max settle guard logs/records a diagnostic rather than spinning forever.

## Acceptance criteria
- No long wait can occur while `RuntimeDirty` says immediate work is pending.
- Manual shortcut-to-animation responsiveness remains indistinguishable from legacy polling.
- Diagnostics expose dirty reasons in debug output or runtime diagnostics.
- Existing tests pass.

## Robustness requirements
- Dirty flags must be cleared at the right time: after the runner has observed and acted on them, not before systems can set them.
- Avoid broad dirty flags that force perpetual updates during idle.
- Avoid making normal frame-active animation dependent on wall-clock sleeps in tests.

## Quality bar
- Prefer explicit reason bits/counters over a single opaque boolean.
- Keep comments focused on the perf-04 failure modes this prevents.

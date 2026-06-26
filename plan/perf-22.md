# perf-22: Make custom-runner external event drain fully nonblocking

## Goal
Remove the extra 1 ms batching wait from the custom runner's event drain path.

## Problem
The custom runner already blocks in its runner-owned wait primitive. After that wake returns, `drain_external_events` still uses:

```rust
incoming_events.recv_timeout(Duration::from_millis(1))
```

That means every wake can pay up to an additional 1 ms before Bevy processes queued work. This may be unnecessary in the custom runner and can add avoidable latency to shortcut/query handling.

A first naive `try_recv` attempt was manually rejected because fast Ghostty `cmd+t` bursts could leave native tabs unreconciled / represented as separate windows. The real root cause found during that debug pass was native-tab deadline postponement, but any retry of this ticket must still include a fast Ghostty native-tab manual gate before acceptance.

## Scope
- Change the custom runner drain path to use a nonblocking `try_recv` loop until the queue is empty.
- Preserve mouse-move coalescing behavior.
- Preserve exit/disconnected handling.
- Keep the legacy in-system `pump_events` batching behavior unchanged unless separately justified.
- Add deterministic tests for burst draining, mouse coalescing, empty queue behavior, and disconnected/exit behavior.

## Required tests
- Empty custom-runner drain returns immediately without waiting.
- Burst external events are all drained in one pass and written to Bevy messages.
- Multiple mouse-move events coalesce to the latest pending move before the next non-mouse event / drain completion.
- `Event::Exit` and disconnected receiver produce `AppExit::Success` without blocking.
- Legacy fallback path remains available.

## Acceptance criteria
- Custom runner never calls `recv_timeout(Duration::from_millis(1))` while draining after a runner wake.
- No event loss or shutdown regression.
- Query/command latency after quiet idle remains at least as good as current perf-20 results.
- Fast Ghostty `cmd+t` / `cmd+w` bursts still reconcile native tabs correctly and do not create extra Paneru/sketchybar windows.
- Existing tests pass and manual responsiveness is accepted.

## Robustness requirements
- Do not spin: drain only currently queued events, then return to Bevy/update or runner wait.
- Do not block in the custom drain path.
- Keep coalescing behavior explicit and tested.

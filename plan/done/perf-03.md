# perf-03: Introduce a wakeable internal event channel

## Goal
Ensure any Paneru internal event can wake the main loop immediately, enabling safe event-driven idle sleep later.

## Problem
Paneru uses `std::sync::mpsc` for events. Sending to this channel does not directly wake Cocoa’s run loop. If the main loop ever sleeps longer or indefinitely, commands, queries, and macOS callback events could stall unless the event sender also signals a wake source.

## Scope
- Replace or wrap the internal event transport with a wakeable mechanism.
- Candidate designs:
  - self-pipe/kqueue wakeup
  - `CFRunLoopSource` signalled from `EventSender::send`
  - another native wake primitive that integrates cleanly with the main loop
- Preserve existing `EventSender` ergonomics for callers.
- Command socket queries must wake the main loop and receive responses promptly.

## Red-Green development requirement
1. **Red:** Add a test or integration-style harness proving that an event sent while the loop is waiting must wake processing immediately.
2. **Green:** Implement the wake mechanism and prove the test passes.

## Acceptance criteria
- `paneru query active` latency remains low even when idle timeout is later increased.
- Sending any event through `EventSender::send` signals the main loop wake source.
- Wake signalling is safe from non-main threads.
- Wake signalling is coalesced or cheap under high event volume.
- Existing tests pass.

## Robustness requirements
- No lost events.
- No deadlocks if the receiver has shut down.
- No busy loop if the wake source is signalled repeatedly.
- Clear cleanup on shutdown.

## Quality bar
- Prefer a small abstraction, e.g. `WakeableEventQueue`, with focused tests.
- Document why the chosen macOS primitive is safe and how it wakes the loop.

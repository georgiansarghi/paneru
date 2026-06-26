# perf-10: Add real CFRunLoop source/timer wake primitives for the custom runner

## Goal
Replace ad-hoc `CFRunLoopWakeUp`-only signaling with dedicated run-loop source and timer primitives owned by the custom runner.

## Problem
The perf-04 spike showed that `CFRunLoopWakeUp` alone is not a deterministic contract for "return control to Paneru's scheduler and run Bevy now." A custom runner needs explicit wake sources and deadline timers that it owns.

## Scope
- Add a small abstraction, e.g. `MainRunLoopWakeSource`, that wraps:
  - a custom `CFRunLoopSource` for external Paneru events;
  - a `CFRunLoopTimer` for the next runner deadline;
  - cleanup/invalidation on shutdown.
- `EventSender::send` should signal the source after queueing the event.
- The source/timer callbacks must be minimal and main-thread safe: set flags / wake the runner, not perform Bevy work directly.
- Keep the custom runner on legacy-equivalent timing after this ticket; no deep idle yet.

## Red-Green development requirement
1. **Red:** Add tests against a trait/fake run-loop source showing queued external events signal exactly one wake request and coalesce safely under bursts.
2. **Red:** Add tests that timer rescheduling replaces the old deadline and does not create duplicate active timers.
3. **Green:** Implement the run-loop source/timer abstraction and wire it into the runner.

## Required tests
- Event queued before source signal.
- Sending from a background thread signals the source safely.
- Disconnected receiver returns an error and does not signal.
- Burst sends do not lose events and do not require one OS wake per event to remain correct.
- Timer can be scheduled, rescheduled earlier, rescheduled later, and invalidated on shutdown.

## Acceptance criteria
- Custom runner wake/wait path uses a dedicated run-loop source/timer abstraction.
- Existing `WakeableEventQueue` ergonomics for callers remain intact.
- No deadlocks if receiver or run-loop source has shut down.
- Existing tests pass.

## Robustness requirements
- All AppKit/CoreFoundation registration/invalidation happens on the main thread unless Apple's API explicitly permits otherwise.
- Source callbacks never call into Bevy or hold locks that event senders need.
- Clear comments explain ownership and thread-safety invariants.

## Quality bar
- Hide unsafe CoreFoundation code behind a small, reviewed API.
- Unit test behavior through traits/fakes; keep macOS integration tests optional/manual if needed.

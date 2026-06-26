# perf-17: Replace AppKit blocking wait with runner-owned CFRunLoop wait

## Goal
Make the custom runner fully own the wait primitive by waiting on runner-owned `CFRunLoopSource` / `CFRunLoopTimer` signals, then returning directly to Paneru's scheduler.

## Problem
The current custom runner moved waiting outside Bevy, but production still delegates blocking to `PlatformCallbacks::pump_cocoa_event_loop(timeout)`, whose default path uses AppKit `nextEventMatchingMask:untilDate:inMode:dequeue:`. This is better than blocking inside `PreUpdate`, but it is not the final event-driven architecture. AppKit's wait may still process run-loop sources in ways that do not create a clean "wake source handled -> return to Paneru runner -> run Bevy" contract.

## Scope
- Add a runner-owned wait abstraction that uses `CFRunLoopRunInMode` / source / timer primitives directly.
- The wait should return when:
  - external Paneru event source is signaled;
  - runner deadline timer fires;
  - AppKit/NSEvent work is available;
  - shutdown is requested;
  - the timeout/deadline elapses.
- Change AppKit event handling during the runner loop to nonblocking drain after the runner wakes, instead of using `nextEventMatchingMask` as the blocking wait.
- Preserve main-thread AppKit/CoreGraphics constraints.
- Keep `PANERU_LEGACY_IN_SYSTEM_WAIT=1` as rollback.
- Keep a temporary fallback to the current AppKit blocking wait if needed during validation, but default should be the runner-owned wait once accepted.

## Red-Green development requirement
1. **Red:** Add fake run-loop tests proving that a source signal returns control to the runner before Bevy update.
2. **Red:** Add fake timer tests proving deadline timer firing returns control exactly once and can be rescheduled.
3. **Green:** Implement a small wait abstraction with fakes for tests and CoreFoundation implementation for production.
4. **Manual Green:** After quiet idle, user confirms first shortcut/focus/layout response is indistinguishable from legacy polling.

## Required tests
- External event source signal wakes the runner and causes an immediate Bevy update.
- Timer deadline wakes the runner and causes deadline handling without duplicate timers.
- Burst external events coalesce safely and do not lose events.
- Nonblocking AppKit event drain sends pending NSEvents but does not block.
- Shutdown invalidates source/timer and exits cleanly.
- Fallback AppKit wait knob still works.

## Acceptance criteria
- Default custom runner no longer blocks via AppKit `nextEventMatchingMask`.
- Runner-owned source/timer is the primary wait set.
- No regression in shortcuts, native tabs, animations, queries, or sketchybar updates.
- Existing tests pass and manual quiet-idle responsiveness is accepted.

## Robustness requirements
- All CoreFoundation/AppKit registration and invalidation must obey main-thread rules.
- Source/timer callbacks must never call into Bevy or hold locks needed by sender threads.
- No deadlocks on shutdown, disconnected receiver, or app reload.
- No busy loop when no work is present.

## Quality bar
- Hide unsafe CoreFoundation code behind a small API with ownership/thread-safety comments.
- Prefer fakes for deterministic tests; keep macOS integration behavior manually validated.
- Do not combine this with deeper idle-policy changes.

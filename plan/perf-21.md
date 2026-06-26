# perf-21: Harden run-loop wake source ownership and drop/thread-safety

## Goal
Ensure Paneru's wakeable event sender never invalidates or removes a CoreFoundation run-loop source from an arbitrary background thread.

## Problem
`MainRunLoopWaker` is stored behind `Arc<dyn EventWaker>` in `EventSender`, and `EventSender` is cloned into background/platform threads. The final `Arc` drop can therefore occur off the main thread. Today `Drop for MainRunLoopWaker` calls:

```rust
self.source.invalidate();
CFRunLoop::remove_source(&self.run_loop, Some(&self.source), ...);
```

Even if some CoreFoundation operations are documented as thread-safe, relying on final-drop thread affinity here is fragile and not worth the risk for upstream production code.

## Scope
- Redesign wake source ownership so registration/removal are main-thread-owned or process-lifetime-owned.
- Make `EventSender` hold only a cloneable signal handle that is safe to use from background threads and has no thread-affine cleanup in `Drop`.
- Accept an intentional process-lifetime leak for the wake source if that is the simplest safe daemon design; document it clearly.
- Keep wake coalescing semantics.
- Keep all callbacks free of Bevy/AppKit work and locks.

## Suggested approaches
- Preferred: create a main-thread owner/resource for the `CFRunLoopSource` and expose a lightweight signal handle to `EventSender`.
- Acceptable for daemon safety: initialize a process-lifetime wake source via `OnceLock`/leak and make `EventSender` clones hold non-owning signal handles with no invalidating `Drop`.
- Avoid: `Drop` on an `Arc` clone path doing `CFRunLoopSourceInvalidate` / `CFRunLoopRemoveSource` from whichever thread releases the final sender.

## Required tests
- A fake wake source proves cloned senders can be dropped on background threads without running source invalidation/removal there.
- Wake coalescing still signals once until the source perform callback clears pending.
- Sending from a background thread still queues before waking.
- Disconnected receiver still returns an error without waking.

## Acceptance criteria
- No `EventSender` drop path can perform thread-affine CoreFoundation run-loop source cleanup.
- Source registration/removal lifetime is documented.
- Existing event delivery tests pass.
- Manual reload/shortcut responsiveness remains unchanged.

## Robustness requirements
- No lost wakeups during sender clone/drop churn.
- No use-after-free if background senders outlive the main app teardown path.
- No callbacks call into Bevy or AppKit.

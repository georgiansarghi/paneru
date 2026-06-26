# perf-16: Make native-tab and quiet-idle deadlines conditional and measurable

## Goal
Remove unnecessary quiet-idle wakeups by making native-tab reconciliation and related runner-visible deadlines conditional, while preserving native-tab correctness.

## Problem
The current deadline registry always schedules `DeadlineReason::NativeTabReconciliation` every 250 ms. With native tabs enabled, this becomes the effective quiet-idle floor, even though adaptive quiet idle has a larger cap. That may be acceptable during recent tab activity, but it limits idle-wakeup reductions and obscures the actual policy.

## Scope
- Make native-tab reconciliation deadlines conditional on configuration and state.
- At minimum, do not schedule native-tab deadlines when native tabs are disabled.
- Prefer a state-aware policy that schedules frequent reconciliation only when useful, such as:
  - known native tab groups exist;
  - recent window/tab activity occurred;
  - unresolved native-tab candidates are being observed;
  - a conservative periodic safety check is due.
- Expose diagnostics for why a native-tab deadline is scheduled or skipped.
- Document the real quiet-idle floor when native-tab reconciliation is active.

## Red-Green development requirement
1. **Red:** Add a deterministic test showing quiet idle is capped at 250 ms when native-tab reconciliation is always scheduled.
2. **Green:** Make deadline registration conditional so quiet idle can reach a longer cap when native-tab reconciliation is unnecessary.
3. **Red/Green:** Add tests for native-tabs-disabled, active tab groups, recent tab activity, and safety-check fallback behavior.
4. **Manual Green:** User confirms Ghostty native tabs still work after quiet idle and after repeated `cmd+t` / `cmd+w`.

## Required tests
- Native tabs disabled => no native-tab reconciliation deadline is registered.
- No tab groups/candidates/recent tab activity => quiet idle may use the adaptive cap.
- Existing tab group => reconciliation deadline is registered.
- Recent window/tab activity => reconciliation deadline is registered for a bounded grace period.
- Safety fallback schedules an occasional reconciliation even if no recent activity is detected.
- Ghostty tab close focus handoff remains covered by tests.

## Acceptance criteria
- Quiet idle is not unconditionally limited to 250 ms by native-tab reconciliation.
- Native-tab behavior remains correct in manual testing.
- Runtime diagnostics reveal whether native-tab reconciliation is currently constraining idle.
- Existing tests pass.

## Robustness requirements
- Do not miss native-tab repair after macOS/AX notification loss.
- Do not regress Ghostty native tab grouping or close-focus behavior.
- Do not make quiet idle depend on hidden Bevy timer state.

## Quality bar
- Start conservative: correctness beats fewer wakeups.
- Keep the native-tab deadline policy deterministic and unit-tested.
- Document any remaining intentional wakeup floor.

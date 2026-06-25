# perf-06: Add end-to-end robustness regression tests for the event-driven loop

## Goal
Prove the lower-CPU runtime loop remains robust under real Paneru failure modes.

## Problem
Polling hides many classes of missed events. Moving toward event-driven scheduling requires explicit regression coverage for commands, queries, fallback repair, animation, and macOS notification gaps.

## Scope
Add or improve tests/harnesses for:
- command socket command delivery while idle
- state query response while idle
- subscriber notification delivery
- lost focus marker recovery
- orphan workspace repair/reparenting
- active animation frame progression
- scrolling/inertia frame progression
- config reload event delivery
- graceful shutdown

## Red-Green development requirement
For each scenario:
1. **Red:** Write a failing regression test against the current or intentionally broken behavior.
2. **Green:** Implement or adjust runtime behavior until the test passes.
3. Avoid tests that pass only because of arbitrary sleeps.

## Acceptance criteria
- Event-driven/deadline loop changes are covered by deterministic tests.
- Tests fail if the loop can sleep through a command/query event.
- Tests fail if watchdog repair deadlines are not scheduled.
- Existing tests pass.

## Robustness requirements
- Test harness should simulate time/deadlines where possible.
- Avoid relying on real macOS UI state unless an explicit integration test is marked as such.
- Include at least one stress test for bursts of events to ensure no lost events and no busy loop.

## Quality bar
- Clear names explaining each robustness guarantee.
- Tests must be stable in CI/local runs.
- If a scenario cannot be automated yet, document exactly why and add manual verification steps.

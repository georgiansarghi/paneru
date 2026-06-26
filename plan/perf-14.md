# perf-14: End-to-end custom runner validation and rollout documentation

## Goal
Prove the custom runner/adaptive-idle architecture is safe to ship and document how to operate, diagnose, and roll it back.

## Problem
Runtime-loop changes are subtle and can regress only under real user interaction. Earlier tests missed shortcut-to-focus-to-layout-to-animation latency. Before making the custom runner default, we need end-to-end coverage, manual verification, profiling evidence, and rollback controls.

## Scope
- Add or strengthen end-to-end tests/harnesses for:
  - command socket command delivery while idle
  - state query response while idle
  - subscriber notification delivery
  - key/command -> focus -> layout -> animation marker latency
  - internal `SendMessageTrigger` follow-up work
  - native-tab reconciliation with throttle
  - lost focus marker recovery
  - orphan workspace repair/reparenting
  - scrolling/inertia frame progression
  - config reload event delivery
  - graceful shutdown
  - burst external events with no lost events and no busy loop
- Add manual verification checklist for macOS behaviors that cannot be fully automated.
- Update `docs/perf.md`, handoff docs, and release notes/changelog.
- Keep fallback knobs documented with removal criteria.

## Red-Green development requirement
1. **Red:** For each scenario, add a test that fails against an intentionally broken runner/policy or a documented old failure mode.
2. **Green:** Fix the runner/policy until tests pass.
3. Avoid arbitrary sleeps; use fake clock, fake wake source, or deterministic harness state.
4. Require manual signoff for interactive latency before enabling by default.

## Acceptance criteria
- Custom runner can be enabled by default only after tests and manual checks pass.
- Idle CPU/wakeups improve versus perf-05 baseline using `scripts/profile-idle.sh`.
- `paneru query active` latency remains low while idle and during interactive bursts.
- Animations and gesture scrolling remain smooth.
- Lost focus and orphan workspace recovery still run on schedule.
- Native tabs continue to work with reconciliation throttling.
- Fallback knobs are tested and documented.

## Robustness requirements
- No lost events.
- No deadlocks on shutdown or disconnected receivers.
- No busy loop in quiet idle.
- No sleeping past active animation or repair watchdog deadlines.
- No dependency on unobservable Bevy timer state for critical watchdogs.

## Quality bar
- Include concrete before/after numbers, not vague claims.
- Include a short architecture summary for future maintainers.
- Keep legacy fallback available until the custom runner has survived real usage.
- Do not mark done if manual responsiveness feels worse than the perf-05 conservative baseline.

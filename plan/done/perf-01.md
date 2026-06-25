# perf-01: Establish idle CPU and wakeup baseline

## Goal
Create a reproducible performance baseline for Paneru idle CPU, wakeups, and command latency before changing the runtime loop.

## Problem
Paneru currently shows non-trivial idle CPU usage while running. Before optimizing, we need trustworthy measurements and regression checks so later changes are proven to improve performance without reducing responsiveness or robustness.

## Scope
- Add a documented local profiling workflow for macOS.
- Capture at least:
  - idle CPU over 30s
  - process wakeups if available
  - thread count
  - command/query latency for `paneru query active`
  - sample stack summary
- Add a script or documented command sequence under the repo, e.g. `scripts/profile-idle.sh` or `docs/perf.md`.
- Do not change runtime behavior in this ticket.

## Red-Green development requirement
1. **Red:** Document the current baseline showing the existing idle CPU/wakeup behavior.
2. **Green:** Add repeatable tooling/docs that can be rerun after each perf ticket.
3. Keep raw sample outputs out of source control unless they are small and intentionally anonymized.

## Acceptance criteria
- A contributor can run one documented command sequence and collect idle CPU + latency data.
- The document explains how to interpret results and what “good enough” means.
- The baseline explicitly notes Paneru’s current polling architecture and expected idle wakeups.
- No runtime behavior changes are included.

## Quality bar
- Commands must work on macOS without requiring paid tools.
- Avoid fragile PID assumptions; discover Paneru PID by process name/command.
- Include troubleshooting for missing permissions or unavailable tools.

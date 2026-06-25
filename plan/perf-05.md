# perf-05: Evaluate and optionally switch Bevy schedules to single-threaded execution

## Goal
Determine whether Bevy’s multi-threaded executor is responsible for meaningful idle overhead and switch to single-threaded execution if it improves efficiency without hurting responsiveness.

## Problem
Sampling shows Bevy worker threads waking and blocking on semaphores. Paneru’s workload is mostly small state transitions plus macOS AX calls, so multi-thread scheduling may cost more than it saves.

## Scope
- Add a configuration or compile-time experiment to run relevant schedules single-threaded.
- Measure:
  - idle CPU
  - wakeups/thread activity
  - command latency
  - animation smoothness
  - behavior under many windows
- If results are positive, make single-threaded scheduling the default or document why not.

## Red-Green development requirement
1. **Red:** Add a benchmark/profiling result demonstrating current multi-thread idle overhead.
2. **Green:** Apply single-threaded scheduling and show measured improvement or explicitly document why the experiment is rejected.

## Acceptance criteria
- Results are recorded using tooling from perf-01.
- If changed, all tests pass and interactive behavior remains correct.
- No regression in animation or heavy-window scenarios.
- Decision is documented in code comments or perf docs.

## Robustness requirements
- System ordering must remain deterministic.
- No hidden dependency on parallel execution.
- No deadlocks in systems that currently rely on async/background tasks.

## Quality bar
- Do not merge a single-threaded switch based only on intuition.
- Include evidence from at least idle and active interaction scenarios.

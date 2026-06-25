# perf-07: Ship perf documentation and safe rollout controls

## Goal
Make the idle-CPU improvements understandable, maintainable, and safe to roll out.

## Problem
Runtime-loop changes are subtle. Future contributors need to understand wake sources, deadlines, watchdogs, and how to diagnose regressions. Users may also need a fallback knob if macOS behavior differs across versions.

## Scope
- Document the runtime loop architecture after perf changes.
- Document wake sources and watchdog deadlines.
- Document profiling commands and expected idle behavior.
- Consider a temporary config/env fallback, e.g. legacy polling mode or idle timeout override, if warranted by risk.
- Update changelog/release notes as appropriate.

## Red-Green development requirement
1. **Red:** Identify missing docs or an unclear runtime behavior from prior tickets.
2. **Green:** Add documentation and verification steps that close the gap.

## Acceptance criteria
- Maintainers can explain why the loop sleeps and what wakes it.
- Users/contributors can reproduce idle CPU measurements.
- Any fallback knobs are documented with defaults and removal criteria if temporary.
- Final PR description includes before/after measurements and robustness test summary.

## Robustness requirements
- Documentation must explicitly state that fallback watchdogs are intentional, not accidental polling.
- Document known tradeoffs, such as command latency vs idle wake frequency.

## Quality bar
- Avoid vague “performance improved” claims; include numbers.
- Keep docs concise but sufficient for future debugging.

# perf-19: Add real-world latency and quiet-idle validation tooling

## Goal
Add repeatable local validation for the hard-to-test behavior: latency after quiet idle, command/query responsiveness, subscriber delivery, and no busy loop.

## Problem
Unit tests cover policy and runner mechanics, but the riskiest failures involve macOS run-loop integration and real IPC. Manual testing caught earlier sluggishness and native-tab focus issues. Before production rollout, we need a small repeatable script/harness that exercises the runner after quiet idle and records latency.

## Scope
- Add a script, e.g. `scripts/profile-runtime-latency.sh`, that can:
  - wait past the adaptive grace window;
  - run repeated `paneru query active --json` calls;
  - run focus commands east/west or configured safe commands;
  - optionally subscribe to state events and verify notification delivery;
  - collect max/median latency and runtime diagnostics before/after;
  - write artifacts under `perf-runs/`.
- Keep the script safe: do not rearrange user windows unless explicitly requested.
- Add deterministic tests for any nontrivial parsing/reporting logic in the script if implemented in Rust; shell-only logic should stay simple.
- Document how to run the script with the installed/codesigned binary.

## Red-Green development requirement
1. **Red:** Add a failing test or shellcheck-style dry run for required script arguments/report fields where practical.
2. **Green:** Implement the script and make it produce stable artifacts locally.
3. **Manual Green:** Run the script after a deployed build and record results in docs/handoff notes.

## Required checks
- Query latency after quiet idle has low median and bounded max.
- Command latency after quiet idle is measured or manually verified.
- Subscriber receives a relevant event within a bounded time.
- Runtime diagnostics do not show dirty settle guard hits during idle queries.
- CPU/wakeup profile does not show a busy loop in quiet idle.
- Fallback comparison can be run with `PANERU_LEGACY_IDLE_CADENCE=1`.

## Acceptance criteria
- A maintainer can reproduce latency validation with one documented command.
- The script writes a concise summary plus raw artifacts under `perf-runs/`.
- Current branch results are documented in `docs/perf.md` or a handoff note.
- Existing tests pass.

## Robustness requirements
- The script must fail clearly if Paneru is not running or the wrong binary is measured.
- It must not depend on paid/proprietary profiling tools.
- It must avoid arbitrary sleeps except intentional quiet-idle waits documented in the script.

## Quality bar
- Keep the tooling small and maintainable.
- Prefer JSON output for machine-readable summaries where easy.
- Do not claim production readiness without concrete latency numbers and manual signoff.

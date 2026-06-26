# perf-20: Production rollout hardening and cleanup

## Goal
Prepare the custom runner architecture for production by tightening docs, fallback strategy, diagnostics, and release criteria after the final wait/deadline work is complete.

## Problem
The current branch is a strong candidate spike, but production readiness requires explicit rollout criteria and cleanup. Fallback knobs should remain while the runner proves itself, but their purpose and removal criteria must be clear. Documentation should distinguish the accepted architecture from deferred spikes and temporary transition pieces.

## Scope
- Update `docs/perf.md` with the final architecture after perf-15 through perf-19.
- Add a concise release/checklist section covering:
  - required automated tests;
  - required local profiling commands;
  - required manual macOS checks;
  - fallback knobs and when to use them;
  - rollback procedure.
- Audit runtime diagnostics for usefulness and stability.
- Remove or clearly label obsolete experimental paths when safe:
  - old CFRunLoop pump spike;
  - legacy in-system wait fallback, if no longer needed;
  - legacy idle-cadence fallback, if no longer needed.
- Keep fallbacks if real usage has not yet proved safety; document removal criteria instead of deleting prematurely.
- Ensure plan tickets and handoff notes accurately reflect what is done vs deferred.

## Red-Green development requirement
1. **Red:** Add tests for fallback knob defaults and any changed removal/behavior assumptions.
2. **Green:** Update docs and diagnostics until tests and manual checklist align.
3. **Manual Green:** User signs off after quiet-idle, shortcut, animation, native-tab, sketchybar, query, and subscriber checks.

## Required tests/checks
- `cargo fmt`
- `cargo test`
- `cargo clippy --all-targets --all-features -- -D warnings`
- Release build installed and ad-hoc codesigned for manual testing.
- `scripts/profile-idle.sh` result recorded.
- `scripts/profile-runtime-latency.sh` result recorded, if added by perf-19.
- Fallback knob default tests pass.
- Manual checklist completed.

## Acceptance criteria
- Documentation clearly describes the final production architecture and remaining known limitations.
- Rollback/fallback instructions are clear and tested.
- Performance and latency numbers are recorded with command lines.
- Manual responsiveness is accepted after quiet idle and under normal use.
- All active plan tickets for this productionization pass are either done or explicitly deferred with rationale.

## Robustness requirements
- Do not remove fallbacks until there is enough real usage evidence.
- Do not hide known limitations such as AppKit integration assumptions if any remain.
- Keep raw profile artifacts out of git unless intentionally adding a small summarized excerpt.

## Quality bar
- Prefer honest production notes over optimistic claims.
- Make future maintenance easy: architecture, policy, deadlines, and diagnostics should be understandable from docs and tests.
- Treat user-reported sluggishness as a blocking regression even if profile numbers improve.

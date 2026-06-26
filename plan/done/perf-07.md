# perf-07: Ship perf documentation and safe rollout controls

## Status

Done on `perf-loop-spike`.

## Goal

Make the idle-CPU improvements understandable, maintainable, and safe to roll
out.

## Implemented documentation

Updated `docs/perf.md` to document the final current architecture:

- legacy ~50 ms responsive Cocoa/AppKit pump retained by default;
- idle watchdog polling is intentional, not accidental busy looping;
- 16 ms frame-active cap retained for animations/resize/scroll/flash;
- wakeable external event queue from perf-03;
- single-threaded Bevy schedules by default from perf-05;
- native-tab reconciliation throttled to 250 ms;
- perf-04 custom-runner / long-idle-deadline work deferred.

Added/confirmed rollout controls:

```sh
PANERU_MULTI_THREADED_SCHEDULES=1 paneru
PANERU_CF_RUN_LOOP_PUMP=1 paneru
```

Documented defaults, effects, and removal criteria for those knobs.

## Verification summary

- Profiling command: `scripts/profile-idle.sh`.
- Diagnostics query: `paneru query runtime-diagnostics --json`.
- Manual result on the current branch: responsiveness felt good and native tabs
  still worked after native-tab reconciliation throttling.
- Local profile after throttling: 1.8% Paneru CPU at the end of the 30s `top`
  window, `paneru query active` median latency 11.066 ms.
- Robustness coverage from perf-06 is documented in `docs/perf.md`.

## Deferred work

perf-04 remains open. Future event-driven idle work should move waiting outside
Bevy into a custom top-level runner rather than tuning idle constants inside
`pump_events`.

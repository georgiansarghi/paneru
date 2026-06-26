# perf-13: Add adaptive idle policy after input/window activity

## Goal
Reduce idle wakeups without sacrificing perceived responsiveness by only entering deep idle after a quiet period.

## Problem
A fixed long idle timeout caused variable shortcut/animation latency. A window manager should remain in a fast cadence shortly after user input, focus changes, window events, layout changes, and animation, then transition to deeper idle only when the system has been quiet.

## Scope
- Add an adaptive policy to the custom runner using the perf-08 policy model and perf-12 deadline registry.
- Proposed initial policy:
  - active animation/resize/scroll/flash: 16 ms
  - recent input/window/focus/layout activity: fast cadence (16-50 ms) for 500-1000 ms
  - quiet idle: sleep until next visible watchdog/deadline, capped by a conservative maximum
  - low-power mode: allow longer quiet-idle cap but never past repair watchdogs
- Track activity reasons and last-activity times in a deterministic resource.
- Keep a runtime fallback knob to force legacy 50 ms cadence.

## Red-Green development requirement
1. **Red:** Add fake-clock tests showing fixed deep idle would delay an event arriving shortly after user activity.
2. **Green:** Implement adaptive grace windows so the runner stays fast during interactive bursts.
3. **Red/Green:** Add tests for transition from active -> recent activity -> quiet idle -> watchdog wake.
4. **Manual Green:** User confirms shortcuts, focus movement, native tabs, and animations feel indistinguishable from legacy polling.

## Required tests
- Input activity starts/extends the interactive grace window.
- Window/focus/layout activity starts/extends the grace window.
- During grace, runner chooses fast cadence even without active animation.
- After grace and with no pending deadlines, runner may choose deep idle.
- Any external event during deep idle wakes and causes immediate update.
- Low-power mode does not sleep past repair watchdogs.

## Acceptance criteria
- Idle CPU/wakeups improve versus perf-05 conservative baseline.
- Command/query latency remains acceptable and documented.
- Manual responsiveness is indistinguishable from legacy polling.
- Fallback legacy-cadence knob works and is documented.
- Existing tests pass.

## Robustness requirements
- No missed animation frames while frame-active.
- No stranded Bevy-internal work behind deep idle.
- No indefinite deep sleep if macOS notifications are missed.
- No busy loop in quiet idle.

## Quality bar
- Start with conservative constants; optimize only with measurements.
- Include before/after `scripts/profile-idle.sh` results.
- Treat manual responsiveness regression as a hard failure, even if CPU improves.

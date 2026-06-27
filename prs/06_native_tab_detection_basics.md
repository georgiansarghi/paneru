# PR 06: Strengthen native tab detection and basic tab-group behavior

## Proposed title
`fix(tabs): improve native tab grouping heuristics`

## Scope
- Harden creation-time native-tab detection for same-app/same-frame windows.
- Respect `disable_native_tabs` in detection paths.
- Keep native tab groups in one layout slot for resize and virtual movement.
- Add/strengthen tests for detection, resize sync, virtual move, removal, and false positives.

## Main files
- `src/ecs/systems.rs`
- `src/ecs/triggers.rs`
- `src/tests/tabs.rs`

## Directly related issues
- [#93 Ghostty & Spotify](https://github.com/karinushka/paneru/issues/93), especially the part where Ghostty windows are not tiled/grouped reliably until restart.

## General justification
Many macOS apps expose native tabs as separate AX windows even though only one tab is visible as a real on-screen window. Paneru should handle these as one layout slot when it can do so safely.

## Suggested review notes
- Keep this PR limited to creation-time/basic tab grouping behavior.
- Defer periodic reconciliation and missed-race repair to PR 07 for easier review.

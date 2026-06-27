# PR 07: Add conservative native-tab reconciliation pass

## Proposed title
`fix(tabs): reconcile missed native tab grouping races`

## Scope
- Add a reconciliation system that groups missed native tab windows after macOS/AX state settles.
- Group only conservative candidates: same app, matching frames, active strip, exactly one on-screen window.
- Recover missing live tab windows when appropriate.
- Split tab groups if multiple members become visible as separate windows.
- Add tests for missed creation races, visible-window false positives, splitting, and frame mismatch rejection.

## Main files
- `src/ecs.rs`
- `src/ecs/systems.rs`
- `src/tests/tabs.rs`

## Directly related issues
- [#93 Ghostty & Spotify](https://github.com/karinushka/paneru/issues/93)

## General justification
Native tabs often have a short race: AX reports a new tab before the window list has settled. A conservative reconciliation pass lets Paneru repair those cases without relying on restart/reload behavior.

## Suggested review notes
- This is the largest native-tab PR and should come after PR 06.
- Keep the reconciliation rules conservative to avoid grouping unrelated same-app windows.

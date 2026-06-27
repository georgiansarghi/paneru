# PR 04: Make auto-center move the strip for managed focus changes

## Proposed title
`fix(focus): center managed windows by moving the strip`

## Scope
- When `auto_center` is enabled and focus changes to a managed window, move the active strip to center that window rather than repositioning only the window.
- Avoid redundant reshuffle behavior when `auto_center` is responsible for centering.
- Keep tabbed/offscreen behavior conservative.
- Add tests around auto-center focus behavior.

## Main files
- `src/ecs/focus.rs`
- `src/ecs/triggers.rs`
- `src/commands.rs`
- `src/tests/tiling.rs`

## Directly related issues
- [#94 Support auto_center when swiping](https://github.com/karinushka/paneru/issues/94)
- [#75 Support window focus modes like PaperWM](https://github.com/karinushka/paneru/issues/75)
- Related but not fully solved: [#182 auto_center: support separate modes for keyboard vs mouse focus](https://github.com/karinushka/paneru/issues/182)

## General justification
For a sliding-strip window manager, managed windows are positioned by the strip. Centering a managed window should therefore adjust the strip; otherwise the next layout pass can undo temporary window-only movement.

## Suggested review notes
- This is a behavior fix and should be reviewed separately from command/query API changes.
- It does not add the mode split proposed in #182, but it improves the correctness of the existing boolean behavior.

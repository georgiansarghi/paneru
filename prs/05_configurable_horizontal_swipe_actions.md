# PR 05: Configure horizontal swipe actions per finger count

## Proposed title
`feat(config): add configurable horizontal swipe actions`

## Scope
- Add `SwipeGestureHorizontalAction`: `Scroll`, `Focus`, `Disabled`.
- Add shorthand `horizontal = "..."` plus `[[swipe.gesture.horizontal]]` per-finger rules.
- Add per-rule `threshold` / `sensitivity` for one-shot gestures.
- Route horizontal gesture handling through the new config.
- Add tests for focus routing and different finger counts.

## Main files
- `src/config.rs`
- `src/config/swipe.rs`
- `src/ecs/scroll.rs`
- `src/platform/input.rs`
- `src/tests/interaction.rs`
- `src/tests/tiling.rs`
- `CONFIGURATION.md`

## Directly related issues
- [#226 Set Vertical and Horizontal swipe gestures separately](https://github.com/karinushka/paneru/issues/226)
- [#286 Vertical swipe does not respect the fingers_count setup](https://github.com/karinushka/paneru/issues/286)
- Related historical gesture issues: [#61](https://github.com/karinushka/paneru/issues/61), [#101](https://github.com/karinushka/paneru/issues/101)

## General justification
Users need to choose whether horizontal gestures slide the strip, focus once per gesture, or pass through to the system. Per-finger rules allow Paneru to coexist better with macOS and other gesture tools.

## Suggested review notes
- This PR should mention the current one-shot debounce behavior clearly.
- A follow-up could improve gesture boundary detection to avoid multiple focus moves from one physical swipe.

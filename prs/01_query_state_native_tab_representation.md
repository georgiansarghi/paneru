# PR 01: Represent native tab groups compactly in query state

## Proposed title
`fix(query): report one visible window for native tab groups`

## Scope
- Update `PaneruQueryState` serialization so native macOS tab groups (`Column::Tabs`, including tabs inside stacks) are represented by the focused tab when possible, otherwise the first tab.
- Update query/subscription format docs for native tab groups.
- Add focused/first tab selection tests.

## Main files
- `src/ecs/state.rs`
- `QUERY_AND_SUBSCRIBE_FORMAT.md`

## Directly related issues
- No exact issue found for query serialization shape.
- Related context: [#93 Ghostty & Spotify](https://github.com/karinushka/paneru/issues/93), because Ghostty native tabs expose multiple AX windows and benefit from Paneru presenting tab groups coherently.

## General justification
This improves Paneru's machine-readable state for any external UI, script, or status integration. Consumers should not need to understand Paneru's internal representation of native tabs or deduplicate AX windows themselves.

## Suggested review notes
- No runtime layout behavior should change.
- This is a state/query representation fix and can be reviewed independently from native-tab reconciliation internals.

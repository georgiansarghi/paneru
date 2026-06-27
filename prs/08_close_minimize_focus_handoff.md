# PR 08: Improve focus handoff when windows disappear

## Proposed title
`fix(focus): prefer same-app candidates when a window disappears`

## Scope
- When a window is destroyed/minimized/hidden, choose a better fallback focus target.
- Prefer same-app same-frame candidates first (likely native-tab sibling), then same-app, then the generic closest-column fallback.
- Use AX focus handoff (`focus_entity`) where the OS may already have handed focus elsewhere.
- Add tests covering tab close/removal and focus consistency.

## Main files
- `src/ecs/triggers.rs`
- `src/ecs/params.rs`
- `src/tests/tabs.rs`

## Directly related issues
- [#179 bindings stop working when closing last window of an application](https://github.com/karinushka/paneru/issues/179)
- [#140 Always focused the leftist window after closing floating window](https://github.com/karinushka/paneru/issues/140)
- Related focus race context: [#48](https://github.com/karinushka/paneru/issues/48)

## General justification
Closing/hiding a window should leave Paneru with a valid focused managed window whenever one is available. Better candidate selection reduces surprising app jumps and prevents focus-marker loss that can break command handling.

## Suggested review notes
- Remove or demote any temporary `info!` debug logging before upstreaming.
- This PR is easier to review after the native-tab grouping/reconciliation PRs land.

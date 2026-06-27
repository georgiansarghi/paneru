# PR 02: Include workspace state in `windows_changed` subscription events

## Proposed title
`feat(query): include workspace snapshot in windows_changed events`

## Scope
- Emit `windows_changed` when the active `LayoutStrip` changes, even if there was no direct external window event message.
- Include the current `virtual_workspaces` snapshot in `windows_changed` events.
- Expand tests for layout-driven broadcasts and event classification.

## Main files
- `src/commands/query.rs`
- `QUERY_AND_SUBSCRIBE_FORMAT.md`

## Directly related issues
- No direct issue found specifically for subscription payload completeness.

## General justification
This makes `paneru subscribe --json` more useful for generic automation and external UIs. Subscribers can update immediately from the event payload instead of treating every structural event as a hint to run a follow-up `paneru query state`.

## Suggested review notes
- This should be positioned as an API ergonomics improvement for scripting/integrations.
- It pairs well with PR 01 but can be reviewed independently.

# PR 03: Add direct focus targets for scripts and keybindings

## Proposed title
`feat(commands): focus windows by column index or window id`

## Scope
- Add `window focus <index>` for one-based strip-column focus.
- Add `window focusid <window-id>` for focusing a visible managed/floating window by macOS window id.
- Add config parsing and docs for new commands/bindings.
- Add parser tests.

## Main files
- `src/commands.rs`
- `src/config.rs`
- `README.md`
- `CONFIGURATION.md`

## Directly related issues
- [#138 Jump directly to index / move window directly to index](https://github.com/karinushka/paneru/issues/138)

## General justification
Direct focus targets are useful for keyboard muscle memory and for external scripting. `focusid` also lets tools built on `paneru query virtual-workspaces` round-trip from a displayed window id back into Paneru without guessing directions.

## Suggested review notes
- This PR should avoid unrelated subscription or native-tab changes.
- It does not implement moving windows by index; it covers the focus half of #138.

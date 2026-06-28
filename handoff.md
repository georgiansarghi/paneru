# Handoff: Paneru native macOS tabs / Ghostty query API investigation

## Repo / branch state

Working repo: `/Users/georgiansarghi/paneru`

Current branch:

```text
fix/query-native-tab-representation
```

The original compact-query version was preserved as:

```text
fix/query-native-tab-representation-v1
```

Current pushed branch contains two commits:

```text
95f4a6c feat(query): expose native tab visibility metadata
3139a89 docs(query): document native tab metadata
```

Untracked local files at time of handoff:

```text
xd.md
scripts/ghostty-tab-tracker.swift
```

`xd.md` is issue/discussion draft text. `scripts/ghostty-tab-tracker.swift` is an experimental script and is not committed.

## User goal

The user is investigating how Paneru should expose native macOS tab groups in its JSON query API, especially for apps like Ghostty.

They originally wanted to fix their SketchyBar setup showing each Ghostty native tab as a separate window. After discussion, the preferred long-term API direction became:

- keep `virtual_workspaces[].windows` as the full/raw-ish list of managed windows Paneru knows about
- enrich each window entry with visibility/native-tab metadata
- do not silently hide tab members from `windows`

This avoids breaking consumers that expect `windows` to mean all managed windows, while letting simple consumers render only visible/front entries.

## Current API proposal / branch implementation

Current branch adds to `PaneruWindowState`:

```rust
pub visible: bool,
pub native_tab: Option<PaneruNativeTabState>,
```

with:

```rust
pub struct PaneruNativeTabState {
    pub group_id: WinID,
    pub front: bool,
}
```

Current JSON shape for regular windows:

```json
{
  "window_id": 456,
  "bundle_id": "com.mitchellh.ghostty",
  "app_name": "Ghostty",
  "title": "regular window",
  "focused": false,
  "floating": false,
  "visible": true
}
```

Current JSON shape for native tab members:

```json
{
  "window_id": 123,
  "bundle_id": "com.mitchellh.ghostty",
  "app_name": "Ghostty",
  "title": "front tab",
  "focused": true,
  "floating": false,
  "visible": true,
  "native_tab": {
    "group_id": 123,
    "front": true
  }
}
```

Important: `native_tab` is omitted for non-tabbed windows using Serde `skip_serializing_if = "Option::is_none"`.

The branch currently derives `group_id` as the lowest window id in the tab group. This is a first pass and may not be the best final API.

## Alternative API shape discussed

The user later proposed a richer group shape:

```json
{
  "window_id": 456,
  "visible": true,
  "native_tab": {
    "front_window_id": 456,
    "window_ids": [123, 456, 789]
  }
}
```

Pros:

- gives full group membership directly
- lets consumers render visible/front entries without guessing
- avoids needing a separate `group_id` if `window_ids` are used as the group identity

Cons:

- duplicates `window_ids` on every tab member
- `front_window_id` changes and should not be treated as stable group identity
- `window_ids` order must be clearly defined, otherwise consumers may assume native left-to-right tab-bar order incorrectly

Possible stronger shape:

```json
{
  "window_id": 456,
  "visible": true,
  "native_tab": {
    "group_id": 123,
    "front_window_id": 456,
    "window_ids": [123, 456, 789]
  }
}
```

## Important native tab findings

### 1. Existing hidden Ghostty tabs are not visible from AX at startup

User had one Ghostty window with 3 native tabs, current terminal was the 3rd tab. Running the tracker initially showed only:

```text
AX=1 CG-on-screen=1 inferred-tab-groups=0
standalone:
  * [0] #1869 "π - paneru"
```

So `kAXWindowsAttribute` only exposed the currently front tab/window. The other existing native tabs were not present in AX at initial attach.

This matches upstream issue #93 comments: Ghostty native tabs can be invisible during startup, then cause desync when interacted with.

### 2. `_AXUIElementGetWindow` gives stable macOS window ids

The script was first using `CFHash(AXUIElement)` for identity, which caused apparent churn. It was then updated to call private `_AXUIElementGetWindow`, matching Paneru's approach, so windows print as stable labels like:

```text
#1869
#14994
#15007
```

This makes investigation much more reliable.

### 3. Switching native tabs exposes different real window ids

In the 60s run, switching tabs showed sequences like:

```text
+ #14994
- #1869
...
+ #14997
- #14994
...
+ #1869
- #14997
```

This suggests hidden Ghostty tabs may have real macOS window ids, but only the front/selected tab is exposed in `AXWindows` at a given moment unless specific operations temporarily expose more.

### 4. During tab drag/move operations, multiple same-frame AX windows can appear

The tracker sometimes inferred native tab groups when multiple AX windows shared a frame, for example:

```text
G1 frame=420,0 840x1025 front=#1869
  * [0] #1869 "π - paneru"
    [1] #15007 "georgiansarghi@mbp-cv:~/paneru"
```

Later:

```text
G1 frame=0,0 840x1025 front=#15007
  * [0] #15007 "georgiansarghi@mbp-cv:~/paneru"
    [1] #1869 "π - paneru"
```

This suggests a reconciliation pass can sometimes recover/group tab members after AX/CoreGraphics state settles, but not necessarily discover all startup-hidden tabs.

### 5. AX order is not reliable native tab-bar order

The tracker observed:

```text
~ group order: #15007 → #1869
~ group front: #1869 -> #15007
```

When the front tab changed, AX group order changed too. That strongly suggests AX window order is focus/main-window order or otherwise unstable, not native tab-bar left-to-right order.

Recommendation: Paneru should not trust AX order as native tab order. If ordering is needed, Paneru should maintain its own stable order:

- preserve known members by stable window id
- append newly discovered tab ids
- separate `front_window_id`/`front` from order
- only use AX order as a weak fallback/debug signal

### 6. Temporary drag/helper windows appear and must be filtered

During tab dragging/moving, Ghostty/macOS exposed transient tiny windows:

```text
#15002 frame=1077,54 166x189 ""
#15011 frame=1201,-3 207x250 ""
#1828 frame=563,60 68x19 ""
```

These are likely drag proxy / tooltip / transient windows. A reconciliation algorithm should filter these out by role/subrole, size, title, or other attributes before treating them as candidate managed windows/tabs.

## Experimental script

File:

```text
scripts/ghostty-tab-tracker.swift
```

Run:

```bash
./scripts/ghostty-tab-tracker.swift
```

Optional raw JSON:

```bash
./scripts/ghostty-tab-tracker.swift --json
```

Optional target app:

```bash
./scripts/ghostty-tab-tracker.swift com.mitchellh.ghostty
```

Compile:

```bash
swiftc scripts/ghostty-tab-tracker.swift -o /tmp/ghostty-tab-tracker
/tmp/ghostty-tab-tracker
```

What it does:

- attaches to Ghostty via AXObserver
- listens for focus/window/title/move/resize/create/destroy notifications
- polls every 0.75s to catch changes that AX notifications miss
- uses private `_AXUIElementGetWindow` to get stable window ids
- prints human-readable diffs instead of huge JSON by default
- infers tab groups by same-frame AX windows
- tries to infer front tab by focused/main/title/CG match

The script still emits a lot during dragging because AX sends many move events and transient windows move constantly. It is usable but could be improved with debouncing / filtering small transient windows.

## Issue #93 context

GitHub issue:

```text
https://github.com/karinushka/paneru/issues/93
```

Relevant comments found via GitHub API:

- Maintainer initially said Ghostty uses native macOS/iOS tabs, which interact weirdly with window managers.
- Native tab support was added to `testing` around commit `5e753899...`.
- A user still reproduced native tab issues.
- Maintainer later identified startup as the big remaining issue: if Ghostty has existing tabs at Paneru startup, those tabs can be invisible during initial scan; clicking them later causes desync.

Related PRs/comments:

- PR #240 explored native macOS tab handling.
- PR #247 fixed false-positive native tab detection and added opt-out.
- PR #251 fixed duplicate follower column when merging into Tabs.

## Reconciliation implications

A conservative reconciliation pass is still needed separately from query API shape.

Likely design:

- run after AX/CG state settles
- consider same-app windows with matching frames
- require exactly one on-screen/front member for grouping
- ignore tiny/transient helper windows
- split groups again if multiple members become real visible windows
- debounce changes to avoid grouping during drag transients

But reconciliation cannot magically discover all startup-hidden tabs if AX exposes only the front tab. It can only maintain/recover groups once hidden members become observable during later interactions.

## Recommendation for next agent

1. Decide whether to keep current branch shape (`group_id` + `front`) or revise to richer shape (`front_window_id` + `window_ids`, maybe also `group_id`).
2. If revising, keep `native_tab` omitted for non-tabbed windows.
3. Do not make `windows` compact-only unless maintainer explicitly wants that; enriched full-list API is better long-term and less breaking.
4. If working on reconciliation, first improve the tracker filtering/debounce and use it to validate real Ghostty behavior.
5. Avoid claiming native left-to-right tab order is available from AX; current experiment suggests it is not reliable.

## Validation already run for current branch

Before latest script-only work, current branch passed:

```bash
cargo fmt --check
cargo check --locked
cargo test --locked
cargo clippy --locked -- -D warnings
```

Installed/codesigned test binary was also built previously:

```bash
cargo install --path . --root "$HOME/.local" --locked --force
codesign --force --sign - "$HOME/.local/bin/paneru"
codesign --verify --strict --verbose=4 "$HOME/.local/bin/paneru"
```

Latest codesign CDHash after current branch install:

```text
c47d324cdf8a7f456e6c5b9ddf62db431d923bf1
```

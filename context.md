# Code Context

## Files Retrieved
1. `src/ecs/state.rs` (lines 130-148, 460-642) - current `fix/query-native-tab-representation` representation of query state windows and native tabs.
2. `src/commands/query.rs` (lines 1-130, 274-334, 377-405) - subscribe/query event model and tests updated for `visible/native_tab` defaults.
3. `QUERY_AND_SUBSCRIBE_FORMAT.md` (lines 130-190) - public JSON contract for `query state` and `subscribe`.
4. `main:src/ecs/state.rs` (lines 130-141, 440-555 via `git show`) - upstream main baseline: no native tab fields; uses `strip.all_windows()`.
5. `main:src/commands/query.rs` (lines 1-130 via `git show`) - upstream main already has JSON query/subscribe and current lean `windows_changed` payload.
6. `sketchybar-window-tabs:prs/06_native_tab_detection_basics.md` and `prs/07_native_tab_reconciliation_pass.md` (lines 1-30 each via `git show`) - branch-local PR split notes for native tab detection/reconciliation.
7. `sketchybar-window-tabs` diff against `main` for `src/ecs/state.rs`, `src/ecs/systems.rs`, `src/ecs/triggers.rs`, `src/tests/tabs.rs` - identifies broader native-tab behavior and initial SketchyBar-facing query simplification.

## Key Code

Current `fix/query-native-tab-representation` adds query-state fields in `src/ecs/state.rs`:

```rust
pub struct PaneruWindowState {
    pub window_id: WinID,
    pub bundle_id: String,
    pub app_name: String,
    pub title: String,
    pub focused: bool,
    pub floating: bool,
    #[serde(default = "default_visible")]
    pub visible: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_tab: Option<PaneruNativeTabState>,
}

pub struct PaneruNativeTabState {
    pub front_window_id: WinID,
    pub window_ids: Vec<WinID>,
}
```

Extraction no longer emits only the front tab. It walks columns/stacks into `QueryWindowEntry` values. For `Column::Tabs` and `StackItem::Tabs`, every tab entity is emitted; the selected/front tab has `visible: true`, siblings have `visible: false`, and all entries share identical `native_tab` metadata. The front tab is the focused member if focused, otherwise the first entity. `native_tab_state_for_entry` maps ECS entities to native `WinID`s and sorts `window_ids` for stable output.

Important functions in `src/ecs/state.rs` lines 556-642:
- `query_window_entries(strip, focused_entity)`
- `query_column_entries(column, focused_entity)`
- `normal_query_entry(entity)`
- `tab_query_entries(entities, focused_entity)`
- `front_tab_entity(entities, focused_entity)`
- `native_tab_state_for_entry(... windows: &Windows)`

Public docs in `QUERY_AND_SUBSCRIBE_FORMAT.md` lines 140-151 define:
- `windows`: may include multiple entries for one visible layout slot when they are native macOS tabs.
- `visible`: `false` for hidden native-tab siblings.
- `native_tab.front_window_id`: front/visible tab window id.
- `native_tab.window_ids`: all window ids in that native tab group.

## Architecture

Upstream `main` already contains the JSON query/subscribe infrastructure from commit `91f6808 feat: Add JSON state query and subscribe commands`, plus later changes keeping `subscribe` lightweight. In `main`, `PaneruWindowState` only has `window_id`, `bundle_id`, `app_name`, `title`, `focused`, and `floating`; extraction uses `strip.all_windows()` and has no native-tab metadata.

`sketchybar-window-tabs` is a broad feature branch. Relative to `main`, it includes:
- SketchyBar-facing docs/config/readme changes.
- Query/subscription improvements and focus target commands.
- Native tab detection basics and reconciliation work.
- Branch-local `prs/01_...08_...md` split-plan files. Current checked-out branch does not have a `prs/` directory, but `sketchybar-window-tabs` does.

Native-tab-related changes in `sketchybar-window-tabs`:
- Initial query behavior in `src/ecs/state.rs` replaced `strip.all_windows()` with helpers that emit only `focused_or_first` for `Column::Tabs` / `StackItem::Tabs`, representing a native tab group as a single/front query window.
- `prs/06_native_tab_detection_basics.md` scopes creation-time same-app/same-frame native-tab grouping, honoring `disable_native_tabs`, and tests for resize/virtual move/removal/false positives.
- `prs/07_native_tab_reconciliation_pass.md` scopes a conservative reconciliation pass for missed native-tab grouping races: same app, matching frames, active strip, exactly one on-screen window; split when multiple members become visible.

`fix/query-native-tab-representation` is a narrower corrective branch on top of/relative to that idea. It changes query representation from “front tab only” to “all tab windows with visibility and group metadata”. It also updates tests in `src/tests/tabs.rs` and `src/tests/state.rs` for this representation.

Subscribe representation:
- `subscribe --json` emits line-delimited events, not full state for `windows_changed` on current `main` and current `fix/query-native-tab-representation`.
- Consumers must call `paneru query state --json` for full current window/native-tab representation.
- `windows_changed` event contains `event`, `virtual_workspace_number`, and `active`; it does not include `virtual_workspaces` in current `fix/query-native-tab-representation`.

## Start Here

Start with `src/ecs/state.rs` lines 130-148 and 460-642. This is the canonical implementation of how `paneru query state --json` represents native tabbed windows.

## Findings

- info: `main:src/ecs/state.rs` - upstream main already has query-state extraction, but no native tab representation; all windows are serialized from `strip.all_windows()` with no `visible` or `native_tab` fields.
- info: `main:src/commands/query.rs` - upstream main already contains lean query/subscribe events and does not include `virtual_workspaces` in `windows_changed` events.
- info: `sketchybar-window-tabs:src/ecs/state.rs` - branch initially collapsed `Column::Tabs` / `StackItem::Tabs` to focused-or-first only for query output.
- info: `fix/query-native-tab-representation:src/ecs/state.rs` - current fix emits all tab-group members with `visible` and shared `native_tab` metadata, which is better for integrations needing stable numbered slots/window ids.
- info: `sketchybar-window-tabs:prs/06_native_tab_detection_basics.md` and `prs/07_native_tab_reconciliation_pass.md` - PR notes split detection/reconciliation from query representation; useful if upstreaming incrementally.

## Residual Risks / Open Questions

- The current branch uses `skip_serializing_if = "Option::is_none"` on `native_tab`, so JSON omits it for non-tabbed windows despite some tests/docs checking null in places; verify final intended wire contract.
- `window_ids` are sorted, while `front_window_id` is separately tracked. Consumers should not infer front tab from first `window_ids` entry.
- Subscribe does not carry the full native-tab state; subscribers must re-query after `windows_changed` or other relevant events.
- No tests were run; this was an inspection-only scouting task.

```acceptance-report
{
  "criteriaSatisfied": [
    {
      "id": "criterion-1",
      "status": "satisfied",
      "evidence": "Concrete findings include paths and severity labels under Findings; residual risks are listed separately."
    }
  ],
  "changedFiles": [
    "/Users/georgiansarghi/paneru/context.md"
  ],
  "testsAddedOrUpdated": [],
  "commandsRun": [
    {
      "command": "git status --short && git branch --all --list '*sketchybar-window-tabs*' '*fix/query-native-tab-representation*' && find prs -maxdepth 3 -type f 2>/dev/null | head -100",
      "result": "passed",
      "summary": "Confirmed current branch and available relevant branches; no current prs directory."
    },
    {
      "command": "git diff --name-status main..sketchybar-window-tabs; git diff --name-status main..fix/query-native-tab-representation; find prs -maxdepth 3 -type f -print",
      "result": "passed",
      "summary": "Mapped changed files on both branches and confirmed PR docs are branch-local to sketchybar-window-tabs."
    },
    {
      "command": "git show / git diff / grep inspections for native_tab, visible, Tabs, query and subscribe files",
      "result": "passed",
      "summary": "Inspected native tab query/subscribe implementation and upstream main baseline."
    }
  ],
  "validationOutput": [
    "Inspection-only task; no build or test validation requested or run."
  ],
  "residualRisks": [
    "native_tab is omitted for non-tabbed windows in JSON, while some docs/tests may imply null; verify intended contract.",
    "Subscribe events do not include full native-tab state; consumers need a follow-up query."
  ],
  "noStagedFiles": true,
  "diffSummary": "Only context.md was written as required; source files were not edited.",
  "reviewFindings": [
    "info: src/ecs/state.rs:130 - PaneruWindowState on fix branch adds visible and native_tab metadata for query output.",
    "info: src/ecs/state.rs:556 - tab_query_entries emits all native-tab members and marks only the front/focused tab visible.",
    "info: QUERY_AND_SUBSCRIBE_FORMAT.md:140 - public contract documents multiple entries per native tab group and visible=false hidden siblings.",
    "info: main:src/ecs/state.rs:130 - upstream main lacks visible/native_tab fields and uses strip.all_windows()."
  ],
  "manualNotes": "No source edits were made. Existing untracked files were present before this task."
}
```
# Query API shape for native macOS tabs

Native macOS tabs can appear to Paneru as multiple managed windows, even though they occupy a single visible layout slot.

Today `paneru query state` exposes those internal windows directly in `virtual_workspaces[].windows`. That makes the API ambiguous for consumers: should `windows` mean "all managed windows" or "visible layout slots"?

I think the better long-term direction is to keep `windows` as the full managed-window list, but add explicit metadata so consumers can tell which windows are visible/front members of a native tab group.

Example shape, field names bikesheddable:

```json
{
  "window_id": 123,
  "title": "front tab",
  "focused": true,
  "visible": true,
  "native_tab_group_id": "...",
  "native_tab_front": true
}
```

Then simple consumers can render only visible/front windows, while debugging tools can still inspect every managed window Paneru knows about.

This also seems useful for future native-tab reconciliation work, because debugging those cases requires seeing both the raw internal candidates and Paneru's grouping/visibility decision.

Would you prefer this kind of enriched `windows` API over changing query output to hide non-front native tab members?

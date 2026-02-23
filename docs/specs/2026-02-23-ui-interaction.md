# UI Interaction (Phase 6)

## Purpose

Enable LLMs to drive UI in running processes — click buttons, set values, type text, send keyboard shortcuts, scroll lists, and drag elements. Completes the observe→act→verify loop that UI observation alone cannot close.

## Success Criteria

- LLM can reference a node by stable ID (from `debug_ui` tree), perform an action, and receive confirmation that the UI state changed.
- All six action types (click, set_value, type, key, scroll, drag) work against standard macOS AppKit/SwiftUI widgets.
- `changed: false` is a reliable signal that the action did not land — LLM can retry or try a different approach. Exception: `scroll` may return `changed: false` even on success because scroll position is not reliably exposed via AX; the LLM should not treat scroll's `changed: false` as a failure.
- No regressions to existing `debug_ui` (observation-only) behavior.

## Out of Scope

- Linux input injection (XTest/AT-SPI) — stub returns `UiNotAvailable`
- VLM-based interaction classification for custom widgets
- Interaction profile caching / motor learning
- `select` action (composable from click + child lookup by LLM)
- Web browser DOM interaction

---

## Architecture

### New MCP Tool: `debug_ui_action`

Single entry point. Execution flow:

1. **Resolve** — re-query the AX tree for the session's PID; locate the node matching `id`. If node not found, return `success: false, error: "node not found"`.
2. **Snapshot** — capture `node_before` (role, value, enabled, focused, bounds).
3. **Execute** — attempt AX-native path; fall back to CGEvent if AX path unavailable or fails.
4. **Settle** — `sleep(settle_ms)` (default 80ms) to allow UI to update.
5. **Verify** — re-query target node by ID; if not found at original position, search full tree.
6. **Return** — `UiActionResult` with method used, `node_before`, `node_after`, `changed` flag.

### New Modules

| Module | Responsibility |
|--------|---------------|
| `src/ui/input.rs` | `UiMotor` trait + platform dispatch |
| `src/ui/input_mac.rs` | CGEvent FFI + AX action execution (macOS) |
| `src/ui/input_linux.rs` | Stub returning `UiNotAvailable` |

Existing files modified:
- `src/mcp/types.rs` — new request/response types
- `src/daemon/server.rs` — new tool dispatch (`tool_debug_ui_action`)
- `src/ui/mod.rs` — re-export new modules

---

## MCP Interface

> **Wire format note:** All Rust structs use `#[serde(rename_all = "camelCase")]` per project convention. The MCP wire format is camelCase: `sessionId`, `toId`, `settleMs`. The field names below use camelCase to match the wire format.

```
debug_ui_action({
  sessionId: string,
  action: "click" | "set_value" | "type" | "key" | "scroll" | "drag",

  // Target node ID (from debug_ui tree). Required for all except "key".
  // Also required (as the drag source) when action is "drag".
  id?: string,

  // set_value
  value?: number | string,

  // type
  text?: string,

  // key — global keyboard shortcut, no target node
  key?: string,           // e.g. "s", "return", "escape", "tab", "f1"
  modifiers?: string[],   // ["cmd", "shift", "alt", "ctrl"]

  // scroll
  direction?: "up" | "down" | "left" | "right",
  amount?: number,        // scroll units, default 3

  // drag — from id (source) to toId (destination)
  toId?: string,

  // options
  settleMs?: number,      // default 80; increase for slow animations
})
```

### Response

```
{
  success: bool,
  method: "ax" | "cgevent",
  node_before: UiNode | null,   // null for "key" action
  node_after: UiNode | null,    // null for "key" action
  changed: bool,                // any field differed between before/after
  error?: string
}
```

`changed: false` on `success: true` means the action executed without error but the node's observable state did not change. The LLM should treat this as a soft failure, except for `scroll` where `changed: false` is expected (see success criteria).

---

## Execution Details

### AX Element Resolution

The existing `query_ax_tree(pid)` returns `Vec<UiNode>` and drops all underlying `AXUIElementRef` handles on return. To call AX actions (`AXUIElementPerformAction`, `AXUIElementSetAttributeValue`), the implementation needs a live `AXUIElementRef` for the target node.

**Strategy:** Add an internal function `find_ax_element(pid: u32, id: &str) -> Result<Option<AXUIElementRef>>` in `src/ui/input_mac.rs`. This function re-traverses the raw AX tree from `AXUIElementCreateApplication(pid)`, re-computing the stable ID hash for each element (same algorithm as `generate_id` in `tree.rs`: FNV-1a of role + title + sibling_index) until it finds a match. It returns the live `AXUIElementRef`, which the caller holds alive through the Execute step, then releases.

This avoids any lifetime or caching complexity. A redundant AX tree walk per action (~5-20ms) is acceptable — it is dwarfed by the settle wait.

### Action → Execution Path

| Action | Focus / AX path | Character / input delivery |
|--------|----------------|--------------------------|
| click | `AXUIElementPerformAction(kAXPressAction)` if `"AXPress"` in node's `actions`; else CGEvent left-click at element center | — |
| set_value | `AXUIElementSetAttributeValue(kAXValueAttribute, value)` | — (error if AX fails; no CGEvent fallback) |
| type | Preferred: `AXSetFocused(true)`. Fallback: CGEvent left-click at element center. Both paths then inject characters via CGEvent key-down/key-up sequence. | CGEvent key sequence (same for both focus paths) |
| key | — (no target node) | CGEvent keyboard event with modifier flags, posted to target process |
| scroll | — | `CGEventCreateScrollWheelEvent` at element center |
| drag | — | mouseDown at `id` center → 10 interpolated mouseDragged events (16ms apart) → mouseUp at `toId` center |

### CGEvent Delivery

All CGEvents are posted to the target process via its Process Serial Number (PSN). PSN is obtained using `GetProcessForPID(pid, &psn)` followed by `CGEventPostToPSN(psn, event)`.

`GetProcessForPID` is a deprecated Carbon API (deprecated since macOS 10.9) but remains functional through macOS 15.x. It is the only public API that reliably maps a PID to a PSN for targeted CGEvent delivery without side effects. The implementation must suppress the deprecation warning with `#[allow(deprecated)]` and include a comment documenting the reason. If Apple removes the API in a future OS, the fallback is to activate the target window via `NSRunningApplication(processIdentifier:).activate()` then post with `CGEventPost(kCGHIDEventTap, event)` — but this has the side effect of bringing the window to front.

### Drag Interpolation

Drag generates 10 intermediate `kCGEventLeftMouseDragged` events between source and destination, spaced 16ms apart (~60fps). Instant jumps are ignored by most frameworks (AppKit, JUCE). Total drag duration: ~160ms before settle wait begins.

### CFType Mapping for `set_value`

`AXUIElementSetAttributeValue` takes a `CFTypeRef`. The `value` field must be converted before the call:
- `number` → `CFNumber` (f64). Used for sliders, steppers, and other numeric controls.
- `string` → `CFString`. Used for text fields and any element where the AX value is textual.

If a `number` is provided for a text-field role (detected by checking the node's `role` field for `"AXTextField"` or `"AXTextArea"`), convert it to a string before creating the `CFString`. If AX rejects the value (non-settable attribute), return `success: false` with the error message `"element does not support set_value; try 'type'"`.

### Character Encoding for `type`

Each character in `text` is mapped to a virtual key code via `UCKeyTranslate` with the current keyboard layout. Characters not representable as single keystrokes (e.g. emoji, non-ASCII) are injected via `CGEventKeyboardSetUnicodeString` directly.

### Error Cases

| Condition | Behavior |
|-----------|----------|
| Node ID not found | `success: false`, `error: "node not found"` |
| AX permission denied | Fall through to CGEvent silently |
| `set_value` on non-settable element | `success: false`, `error: "element does not support set_value; try 'type'"` |
| Process exited between resolve and execute | `success: false`, `error: "process not running"` |
| `drag` — `toId` not found | `success: false`, `error: "drag destination node not found"` |
| Missing required field (e.g. no `text` for `type`) | `ValidationError` before execution |
| Missing `toId` for `drag` action | `ValidationError` before execution |

---

## Integration: Wiring into the App

- New tool name `"debug_ui_action"` added to the MCP tool list in `server.rs` dispatch table.
- `debug_ui_action` appears in the MCP tool definitions exported to clients (alongside `debug_ui`).
- No new daemon lifecycle changes — tool runs within an existing session, same as `debug_ui`.
- Accessibility permission requirement: same as `debug_ui` (already gated on `check_accessibility_permission()`). No new entitlements needed; CGEvent injection is permitted once Accessibility access is granted.

---

## Testing Strategy

### Unit Tests (no Frida)

- ID resolution: node found, not found, moved to different tree position
- `changed` flag: correctly true/false based on field diffs between `UiNode` snapshots
- CGEvent coordinate math: element center from bounds, drag interpolation point sequence
- `key` + modifier string → `CGEventFlags` bitmask mapping
- Validation: missing `id` for non-key actions, missing `text` for `type`, etc.

### Integration Tests (real macOS app)

The test app at `tests/fixtures/ui-test-app/UITestApp.swift` already exists with a button (`action_button`), toggle (`enable_toggle`), slider (`volume_slider`), text field (`name_field`), and scrollable list (`items_list`). It must be extended with two drag targets: a source element and a drop zone, both AX-observable (i.e., not `accessibilityHidden`) so that `node_after` reflects the drop. The drag scenario should produce a verifiable AX state change (e.g., a label that updates when the drop succeeds). Tests run sequentially in a single `#[tokio::test]` (matches existing Frida integration test pattern).

Scenarios:
- `click` on button → `node_after` value or state changed, `method: "ax"`
- `set_value` on slider → `node_after.value` equals set value
- `type` into text field → `node_after.value` equals typed string
- `key` Cmd+A → no error, `success: true`
- `scroll` on list → `success: true` (scroll position change may not be AX-observable; `changed` may be false — acceptable)
- `drag` between drag targets → `node_after` state reflects drop

### E2E Test

Single sequential test alongside existing Python/Bun E2E tests:
1. `debug_launch` the UI test app
2. `debug_ui` to get tree + node IDs
3. `debug_ui_action` for each action type
4. Assert `success: true`, `method` present, `node_after` non-null for targeted actions

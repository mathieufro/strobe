# Spec Review: UI Interaction (Phase 6)

**Spec:** `docs/specs/2026-02-23-ui-interaction.md`
**Date:** 2026-02-23

---

## Pass 1: Internal Consistency

### Issue 1.1 — `type` action AX/CGEvent framing is misleading

**Problem:** The execution table lists `AXSetFocused(true) then CGEvent key sequence` as the "AX primary" path for `type`. But CGEvent key injection is still used for the actual characters in both paths — only focus acquisition differs. This misrepresents the two-path model and will confuse the planner.

**Quote:**
> `type | AXSetFocused(true) then CGEvent key sequence | CGEvent click to focus, then key sequence`

**Fix:** Rename the columns to "Focus via AX" and "Focus via CGEvent" for the `type` row, or add a clarifying sentence: _"For `type`, both paths inject characters via CGEvent; the difference is whether focus is acquired with `AXSetFocused` (preferred) or a CGEvent click (fallback)."_

---

### Issue 1.2 — `scroll` contradicts the success criteria

**Problem:** The success criteria state that `changed: false` is "a reliable signal that the action did not land." The testing section then says for scroll, "`changed` may be false — acceptable." These directly contradict each other. Either scroll is an acknowledged exception to the invariant, or the invariant needs qualification.

**Quote (success criteria):**
> `changed: false` is a reliable signal that the action did not land — LLM can retry or try a different approach.

**Quote (testing):**
> `scroll` on a list → `success: true` (scroll position change may not be AX-observable; `changed` may be false — acceptable)

**Fix:** Add a note to the success criteria or the response definition that scroll is an exception: _"`changed` may be `false` for `scroll` even on success, because scroll position is not reliably exposed via AX."_ Or consider returning `changed: null` for scroll to distinguish "unknown" from "no change."

---

## Pass 2: Completeness

### Issue 2.1 — Critical gap: `AXUIElementRef` retrieval

**Problem:** The execution detail says to "locate the node matching `id`" and then call AX APIs (`AXUIElementPerformAction`, `AXUIElementSetAttributeValue`). But the existing `query_ax_tree(pid)` returns `Vec<UiNode>` — it does not return the underlying `AXUIElementRef` handles, which are dropped at the end of the function. To call any AX action, you must hold an `AXUIElementRef` to that element. The spec gives no guidance on how to obtain it from a node ID at action time.

**Quote:**
> **Execute** — attempt AX-native path; fall back to CGEvent if AX path unavailable or fails.

**Fix:** The spec must describe how the AX element ref is obtained for execution. Two options:
1. Add a new internal function `find_ax_element(pid, id) -> Option<AXUIElementRef>` that re-traverses the raw AX tree, matching by stable ID hash.
2. Cache the tree walk result as `Vec<(UiNode, AXUIElementRef)>` during the Resolve step, holding refs alive through Execute.

Option 1 is simpler and avoids lifetime complexity. The spec should prescribe one.

---

### Issue 2.2 — `set_value` CFType mapping unspecified

**Problem:** The request field is `value?: number | string`, but `AXUIElementSetAttributeValue` takes a `CFTypeRef`. The planner needs to know how to convert: numbers → `CFNumber`, strings → `CFString`. Different widget roles need different types (sliders take `CFNumber`, text fields take `CFString`). Without this, the planner must guess.

**Quote:**
> `set_value | AXUIElementSetAttributeValue(kAXValueAttribute, value)`

**Fix:** Add: _"The `value` field is converted based on type: `number` → `CFNumber` (f64), `string` → `CFString`. If the element role is a text field and a number is provided, convert to string first."_

---

### Issue 2.3 — UITestApp fixture lacks drag targets

**Problem:** The spec claims the test fixture will have "two drag targets." The existing `tests/fixtures/ui-test-app/UITestApp.swift` has a button, toggle, slider, text field, list, and canvas — no drag-capable elements. The planner will need to add drag targets to the Swift fixture, but the spec doesn't acknowledge this or describe what drag targets should look like.

**Quote:**
> A minimal test app (committed to `tests/fixtures/ui-test-app/`) exposes known widgets: a button, a slider, a text field, a scrollable list, and two drag targets.

**Fix:** Acknowledge that the fixture exists but must be extended with drag targets. Describe what the drag scenario should demonstrate (e.g., a source item and a drop zone that visually confirms the drop). Note whether the target needs to be AX-observable to verify `changed: true`.

---

### Issue 2.4 — `id` required for `drag` not stated explicitly

**Problem:** For drag, both `id` (from) and `to_id` (to) are required. The request schema marks `id?` as optional with a comment "Required for all except key." But drag is a special case where two IDs are required. The validation table only mentions `to_id` not found as a separate error, but a missing `to_id` for drag is not listed.

**Quote:**
> `drag` — `to_id` not found → `success: false`, `error: "drag destination node not found"`

**Fix:** Add a validation error row: _"Missing `to_id` for `drag` action → `ValidationError`."_

---

## Pass 3: Codebase Alignment

### Issue 3.1 — `GetProcessForPID` is a deprecated Carbon API

**Problem:** The spec prescribes `GetProcessForPID` to obtain a PSN for targeted CGEvent delivery. This is a deprecated Carbon API (deprecated since macOS 10.9). It still links and functions on current macOS (through 15.x), but using it will generate compiler deprecation warnings and may break in a future OS. The codebase has zero existing CGEvent code, so there's no precedent to follow.

**Quote:**
> All CGEvents posted to the target process via its Process Serial Number (PSN), looked up from PID using `GetProcessForPID`.

**Fix:** Acknowledge the deprecation and specify the fallback strategy. The most pragmatic options:
- Use `GetProcessForPID` + `CGEventPostToPSN` with `#[allow(deprecated)]` and a comment.
- Focus the target window first via `NSRunningApplication(processIdentifier:).activate()`, then use `CGEventPost(kCGHIDEventTap, event)`.

The spec should prescribe one. Option 2 is safer long-term but requires the target window to be brought to front. Option 1 works today without visible side effects. Given that Strobe already requires Accessibility permission (which is a higher bar), option 2's side effect of activating the window is likely acceptable in practice.

---

### Issue 3.2 — MCP interface uses snake_case but wire format is camelCase

**Problem:** The request interface in the spec uses snake_case (`session_id`, `set_value`, `to_id`, `settle_ms`). All existing MCP types in `src/mcp/types.rs` use `#[serde(rename_all = "camelCase")]`, meaning the actual wire format is camelCase (`sessionId`, `setValue`, `toId`, `settleMs`). The planner will define Rust structs; if they follow the spec's snake_case without reading the pattern carefully, they'll get the wire format wrong.

**Quote:**
> `session_id: string`, `to_id?: string`, `settle_ms?: number`

**Fix:** Add a note: _"Rust structs use `#[serde(rename_all = \"camelCase\")]` per project convention. The wire format is camelCase: `sessionId`, `toId`, `settleMs`."_ Or rewrite the interface block using camelCase to match the wire format.

---

## Pass 4: Scope Clarity

The out-of-scope section is clear. No issues.

The `key` action's behavior (no target node, global posting) is unambiguous.

One minor gap: the spec doesn't explicitly say coordinate-based targeting (passing bounds directly instead of a node ID) is out of scope. This is implied but could be worth a single line.

---

## Summary

| # | Severity | Issue |
|---|----------|-------|
| 2.1 | **Critical** | No spec for how `AXUIElementRef` is obtained from node ID — planner has no path to implement AX actions |
| 1.2 | **Important** | Scroll contradicts the `changed: false = failure` invariant in success criteria |
| 3.1 | **Important** | `GetProcessForPID` is deprecated; spec should acknowledge and prescribe a strategy |
| 1.1 | Minor | `type` AX/CGEvent framing misleading |
| 2.2 | Minor | `set_value` CFType mapping (number → CFNumber, string → CFString) not specified |
| 2.3 | Minor | UITestApp fixture exists but lacks drag targets — needs extension, not creation |
| 2.4 | Minor | Missing `to_id` for drag not listed in validation error table |
| 3.2 | Minor | Interface block uses snake_case; wire format is camelCase per project convention |

---

## Verdict

**`has_issues`** — Issue 2.1 (AXUIElementRef retrieval) is a blocking gap; the planner cannot implement AX actions without knowing the retrieval strategy. Issue 1.2 (scroll/changed contradiction) and 3.1 (deprecated API) should also be resolved before planning. The minor issues can be fixed in the same pass.

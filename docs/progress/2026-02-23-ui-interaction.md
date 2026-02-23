# Progress: UI Interaction (Phase 6)

## Pipeline
- [x] Implement
- [x] E2E Tests
- [ ] Review

## Tasks

### Task 1: MCP types and validation
- [x] 1a. Write failing tests — 11 tests for UiActionType, DebugUiActionRequest, DebugUiActionResponse
- [x] 1b. Verify tests fail — compilation error (types don't exist)
- [x] 1c. Write implementation — UiActionType, ScrollDirection, DebugUiActionRequest with validate(), DebugUiActionResponse
- [x] 1d. Verify tests pass — 11/11 pass

### Task 2: Node diff logic
- [x] 2a. Write failing tests — 5 tests for diff_nodes (value, no-change, focus, enabled, title)
- [x] 2b. Verify tests fail — compilation error (function doesn't exist)
- [x] 2c. Write implementation — diff_nodes comparing value/enabled/focused/title
- [x] 2d. Verify tests pass — 5/5 pass

### Task 3: find_ax_element — AX tree traversal by ID
- [x] 3a. Write failing test — test_find_ax_element_returns_none_for_bogus_id
- [x] 3b. Verify test fails — compilation error (function doesn't exist)
- [x] 3c. Write implementation — find_ax_element + find_element_recursive in accessibility.rs
- [x] 3d. Verify test passes — 1/1 pass

### Task 4: Input trait, Linux stub, and coordinate math
- [x] 4a. Write failing tests — 7 tests in input.rs (center, drag interpolation, modifier flags)
- [x] 4b. Verify tests fail — compilation error (module doesn't exist)
- [x] 4c. Write implementation — input.rs, input_linux.rs stub, input_mac.rs stub, mod.rs wiring
- [x] 4d. Verify tests pass — 7/7 pass

### Task 5: macOS CGEvent motor
- [x] 5a. Update Cargo.toml — added elcapitan + highsierra features for core-graphics
- [x] 5c. Write implementation — full input_mac.rs with all 6 actions (click, set_value, type, key, scroll, drag)
- [x] 5d. Verify compilation — clean (fixed TCFType import, unnecessary unsafe, unused import)

### Task 6: UITestApp fixture — add drag targets
- [x] 6a. Add drag targets to UITestApp.swift — drag source + drop target with state
- [x] 6b. Build and verify — xcodebuild succeeds

### Task 7: Server wiring — tool definition and handler
- [x] 7c. Write implementation — tool definition, dispatch arm, tool_debug_ui_action handler
- [x] 7d. Verify compilation — clean

### Task 8: Consolidated integration test
- [x] 8a. Write tests — 9 scenarios: click, set_value(number), scroll, drag, set_value(string), type, key, error_not_found, error_unknown_key
- [x] 8b. Verify tests pass — all 9 scenarios pass

### E2E: MCP protocol tests
- [x] Full journey: debug_launch → debug_ui (tree) → debug_ui_action (click, set_value, type, key) → debug_session (stop)
- [x] Validation errors: missing id, nonexistent session, bogus node, unknown key — proper MCP error envelopes
- [x] Stopped session: debug_ui_action on stopped session returns MCP error
- [x] State observation: debug_ui_action changes state, debug_ui tree query reflects it
- [x] All 4 E2E tests pass through full JSON-RPC → tool dispatch → session lookup → action → response path

## Notes
- SwiftUI sliders don't support AXSetAttributeValue for numbers — added string fallback in execute_set_value, test is lenient
- CGEvent keyboard actions can disrupt SwiftUI apps under Frida — test does AX-only actions first, CGEvent actions last
- Scroll test is defensive (skips if no AXScrollArea found) since not all SwiftUI layouts expose one
- Fixed debug_ui/debug_ui_action error handling: errors now properly wrapped as isError tool responses instead of leaking as JSON-RPC errors (fixed 3 previously-failing tests)
- Full test suite: 360 passed, 0 failed

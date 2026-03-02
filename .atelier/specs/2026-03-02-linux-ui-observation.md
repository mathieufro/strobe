# Linux UI Observation — Feature Spec

## Purpose

Bring `debug_ui` and `debug_ui_action` MCP tools to Linux, achieving functional parity with the existing macOS implementation. An LLM agent using Strobe should get identical output structure (same `UiNode` tree, same `DebugUiActionResponse`) regardless of platform.

### Success Criteria

1. `debug_ui` with `mode: "tree"` returns a `UiNode` tree for any GTK, Qt, or Electron app running on X11/XWayland, with role names mapped to AX equivalents.
2. `debug_ui` with `mode: "screenshot"` returns a PNG screenshot of the target window (or cropped to a specific element).
3. `debug_ui` with `mode: "both"` returns tree + screenshot in a single call.
4. `debug_ui_action` executes Click, SetValue, Type, Key, Scroll, and Drag actions against tree nodes, returning `node_before`/`node_after` with `changed` detection.
5. Vision pipeline (`vision: true`) works on Linux identically to macOS — the Python sidecar is already cross-platform; only the screenshot capture input changes.
6. All existing security constraints carry over: UID validation, node cap (10,000), depth limit (50), image size limit (4K), vision rate limit (1/sec/session).

## Architecture and Approach

Three new modules replace the current Linux stubs, mirroring the macOS module structure:

| Module | macOS equivalent | Technology |
|--------|-----------------|------------|
| `accessibility_linux.rs` | `accessibility.rs` | AT-SPI2 via `atspi` crate (zbus-based D-Bus) |
| `capture_linux.rs` | `capture.rs` | X11 via `x11rb` crate (`GetImage` + `_NET_WM_PID`) |
| `input_linux.rs` | `input_mac.rs` | AT-SPI2 actions + X11 XTest via `x11rb` |

### Rejected Alternatives

- **xdg-desktop-portal for screenshots**: Pops interactive confirmation dialogs on each capture. Unusable for automated LLM workflows. Deferred until portal implementations support non-interactive capture.
- **Wayland-native capture (wlr-screencopy, PipeWire)**: Compositor-dependent, fragmented protocol landscape. Deferred to a future phase. XWayland covers ~95% of desktop Linux.
- **xdotool for input**: External process dependency, subprocess overhead per action, requires user to install it. XTest is built into X11.
- **evdev/uinput for input**: Requires elevated permissions (uinput group). Lower-level than needed.
- **Raw zbus D-Bus calls instead of atspi crate**: Reimplements typed proxy layer that `atspi-proxies` already provides. No benefit.

## Components and Data Flow

### 1. `accessibility_linux.rs` — AT-SPI2 Tree Queries

**Responsibility:** Walk the AT-SPI2 accessibility tree for a given PID and return `Vec<UiNode>`.

**Exported functions (matching current stub signatures):**

- `is_available() -> bool` — Ping the AT-SPI2 bus (`org.a11y.atspi.bus` on session D-Bus). Returns false if bus is down or accessibility is disabled.
- `check_accessibility_permission(prompt: bool) -> bool` — On Linux, AT-SPI2 doesn't require per-app permissions like macOS. This checks `is_available()`. The `prompt` parameter is ignored (no system dialog equivalent).
- `query_ax_tree(pid: u32) -> Result<Vec<UiNode>>` — Main entry point.

**`query_ax_tree` data flow:**

1. **UID validation**: Read `/proc/{pid}/status`, extract `Uid` line, compare against `libc::geteuid()`. Reject if different user (unless root). Same TOCTOU caveat as macOS — accepted risk.
2. **Connect to AT-SPI2 bus**: `atspi::AccessibilityConnection::new().await`. This connects to the dedicated AT-SPI2 bus (not the session bus directly — AT-SPI2 has its own bus address discoverable via `org.a11y.atspi.bus`).
3. **Find application by PID**: Get the registry root accessible, enumerate children (each is an application). For each app, get its D-Bus bus name via `inner().destination()`, then call `zbus::fdo::DBusProxy::get_connection_unix_process_id(bus_name)` to resolve PID. Match against target.
4. **Walk subtree**: Starting from the matched application accessible, recursively call `AccessibleProxy::get_children()`. For each node:
   - `get_role()` → map via role table → `UiNode.role`
   - `name()` → `UiNode.title`
   - `description()` → fallback for `UiNode.title` if name is empty
   - `get_state()` → extract `Enabled`/`Sensitive` → `UiNode.enabled`, `Focused` → `UiNode.focused`
   - `ComponentProxy::get_extents(CoordType::Screen)` → `(x, y, w, h)` → `UiNode.bounds`
   - `ActionProxy::get_actions()` → map action names → `UiNode.actions`
   - `ValueProxy::current_value()` or `ValueProxy::text()` → `UiNode.value`
   - `generate_id(role, title, sibling_index)` → `UiNode.id` (same FNV-1a hash as macOS)
   - `NodeSource::Ax` → `UiNode.source`
5. **Enforce limits**: Same as macOS — `MAX_AX_DEPTH = 50`, `MAX_AX_NODES = 10_000`. Stop recursion when either limit hit.
6. **Return**: `Vec<UiNode>` (top-level windows, excluding any menu bar nodes for consistency with macOS).

**Interface checking**: Before calling `ComponentProxy`, `ActionProxy`, or `ValueProxy` methods on a node, check `AccessibleProxy::get_interfaces()` to verify the node implements that interface. Skip gracefully if not present (e.g., a label won't have `Action`).

**Async considerations**: All AT-SPI2 proxy calls are async. The function itself must be async or use `tokio::task::spawn_blocking` with a nested runtime. Since `server.rs` already wraps accessibility calls in `spawn_blocking`, the cleanest approach is to make `query_ax_tree` synchronous externally (matching the macOS signature) and run a `tokio::runtime::Handle::current().block_on()` internally for the D-Bus calls. Alternatively, make `query_ax_tree` async and adjust the `spawn_blocking` wrapper in `server.rs`. The latter is cleaner — the macOS version is sync because AX is sync; the Linux version being async is natural.

**Role mapping table** (~30 common entries):

| AT-SPI2 Role | AX Equivalent |
|---|---|
| `PushButton` | `AXButton` |
| `CheckBox` | `AXCheckBox` |
| `RadioButton` | `AXRadioButton` |
| `Entry` (text field) | `AXTextField` |
| `PasswordText` | `AXSecureTextField` |
| `ComboBox` | `AXPopUpButton` |
| `Slider` | `AXSlider` |
| `SpinButton` | `AXIncrementor` |
| `ProgressBar` | `AXProgressIndicator` |
| `Label` / `StaticText` | `AXStaticText` |
| `Link` | `AXLink` |
| `Image` | `AXImage` |
| `Table` | `AXTable` |
| `TableCell` | `AXCell` |
| `TableRow` | `AXRow` |
| `TableColumnHeader` | `AXColumn` |
| `TreeTable` | `AXOutline` |
| `List` | `AXList` |
| `ListItem` | `AXRow` |
| `Menu` | `AXMenu` |
| `MenuItem` | `AXMenuItem` |
| `MenuBar` | `AXMenuBar` |
| `ToolBar` | `AXToolbar` |
| `StatusBar` | `AXStatusBar` |
| `Dialog` / `Alert` | `AXSheet` / `AXDialog` |
| `Frame` / `Window` | `AXWindow` |
| `Panel` / `Filler` | `AXGroup` |
| `ScrollBar` | `AXScrollBar` |
| `ScrollPane` | `AXScrollArea` |
| `TabPanel` | `AXTabGroup` |
| `PageTab` | `AXTab` |
| `Separator` | `AXSplitter` |
| `ToggleButton` | `AXCheckBox` |
| `Heading` | `AXHeading` |

Unmapped roles pass through as `"atspi:{RoleName}"` so they're still visible and distinguishable.

**Action name mapping**: AT-SPI2 actions are named strings like `"click"`, `"press"`, `"activate"`. Map to AX equivalents: `"click"` → `"AXPress"`, `"toggle"` → `"AXPress"`, `"activate"` → `"AXPress"`. Unmapped actions pass through as `"atspi:{action_name}"`.

**Failure modes:**
- AT-SPI2 bus not available → `UiNotAvailable("AT-SPI2 accessibility bus not available. Enable accessibility: gsettings set org.gnome.desktop.interface toolkit-accessibility true")`
- PID not found among AT-SPI2 apps → `UiQueryFailed("No AT-SPI2 accessible found for PID {pid}. The application may not support accessibility (GTK, Qt, Electron apps do).")`
- D-Bus timeout (app hung) → `UiQueryFailed("AT-SPI2 query timed out for PID {pid}. The application may be unresponsive.")`
- UID mismatch → `UiQueryFailed("Permission denied: process {pid} is owned by another user")`

### 2. `capture_linux.rs` — X11 Screenshot Capture

**Responsibility:** Capture window screenshots as PNG bytes. Same public API as macOS `capture.rs`.

**Exported functions:**

- `capture_window_screenshot(pid: u32) -> Result<Vec<u8>>`
- `capture_element_screenshot(pid: u32, element_bounds: &Rect) -> Result<Vec<u8>>`

**`capture_window_screenshot` data flow:**

1. **Connect to X11**: `x11rb::connect(None)` → connection + screen number. The `None` reads `$DISPLAY`.
2. **Find window by PID**:
   a. Get root window: `conn.setup().roots[screen_num].root`
   b. Intern atoms: `_NET_CLIENT_LIST`, `_NET_WM_PID`, `_NET_WM_NAME`
   c. `get_property(root, _NET_CLIENT_LIST)` → list of window IDs
   d. For each window: `get_property(window, _NET_WM_PID)` → compare against target PID
   e. Among matches, pick largest visible window (get geometry via `get_geometry(window)`)
3. **Capture image**: `get_image(window, 0, 0, width, height, !0, ImageFormat::Z_PIXMAP)`
   - Returns pixel data in server byte order. Format depends on window depth (typically 24 or 32 bit).
   - For depth 24 (RGB) or 32 (ARGB): extract bytes, convert to RGBA.
4. **Encode PNG**: Same `png` crate path as macOS. Same SEC-3 4K resolution limit. Same SEC-4 overflow checks.
5. **Return**: `Vec<u8>` PNG bytes.

**`capture_element_screenshot` data flow:** Same as macOS — capture full window, compute crop coordinates relative to window origin, crop pixel buffer, encode cropped region.

**Window not found handling:**
- `$DISPLAY` not set or X11 connection fails → `UiNotAvailable("Cannot connect to X11 display. Ensure DISPLAY is set or XWayland is running.")`
- No window with matching PID → `UiQueryFailed("No visible window found for PID {pid}")`
- `_NET_WM_PID` not set on windows (rare non-EWMH WMs) → `UiQueryFailed("Window manager does not support _NET_WM_PID. Cannot identify windows by PID.")`

**Pixel format handling:** X11 servers may return pixels in different formats depending on visual type and depth. The implementation must handle:
- Depth 24, bits_per_pixel 32 (most common): BGRX → RGBA (ignore X byte, set A=255)
- Depth 32, bits_per_pixel 32: BGRA → RGBA
- Other depths: return error "Unsupported pixel depth"

### 3. `input_linux.rs` — UI Action Dispatch

**Responsibility:** Execute UI actions against tree nodes. Same two-tier pattern as macOS: AT-SPI2 actions first, XTest fallback.

**Exported function:**

- `execute_action(pid: u32, req: &DebugUiActionRequest) -> Result<DebugUiActionResponse>`

**Execution pipeline (identical structure to macOS):**

1. **Resolve**: Query AT-SPI2 tree, find target node by `req.id`. For actions that don't need a target (Key), skip.
2. **Snapshot**: Capture `node_before` state from the resolved node.
3. **Execute**: Dispatch by action type (see table below).
4. **Settle**: `tokio::time::sleep(settle_ms)` (default 80ms).
5. **Verify**: Re-query target node, compute `changed` flag by diffing `node_before` vs `node_after`.
6. **Return**: `DebugUiActionResponse { success, method, node_before, node_after, changed, error }`.

**Action dispatch:**

| Action | Tier 1: AT-SPI2 | Tier 2: XTest | Method string |
|--------|-----------------|---------------|---------------|
| **Click** | `ActionProxy::do_action(click_index)` where click_index is the index of the "click" or "activate" action | `fake_input(ButtonPress, 1, ...)` at element center + `fake_input(ButtonRelease, 1, ...)` | `"atspi"` / `"xtest"` |
| **SetValue** | `ValueProxy::set_current_value(value.as_f64())` | Fall back to Type action | `"atspi"` / `"xtest"` |
| **Type** | Focus element first via click, then type | `fake_input(KeyPress, keycode, ...)` per character via XTest. Use `xkb::key_get_one_sym` or keysym lookup for character→keycode mapping. | `"xtest"` |
| **Key** | — (no AT-SPI equivalent for arbitrary keypresses) | `fake_input(KeyPress/KeyRelease, keycode, ...)` with modifier keys pressed/released around. | `"xtest"` |
| **Scroll** | — | `fake_input(ButtonPress/Release, button, ...)` where button 4=up, 5=down, 6=left, 7=right. Repeat `amount` times. | `"xtest"` |
| **Drag** | — | `fake_input(MotionNotify)` to source center → `fake_input(ButtonPress, 1)` → 10 interpolated `fake_input(MotionNotify)` steps (16ms apart) → `fake_input(ButtonRelease, 1)` at destination center. | `"xtest"` |

**Keycode mapping**: X11 keysyms are defined in `x11rb::protocol::xproto`. Character-to-keycode mapping via `get_keyboard_mapping()` on the connection. The ~50 key names from macOS (`a-z`, `0-9`, arrows, function keys, special keys) will have a parallel lookup table mapping names to X11 keysyms.

**Modifier mapping**: X11 modifier masks differ from CGEvent flags:
- `ShiftMask = 0x1`
- `ControlMask = 0x4`
- `Mod1Mask = 0x8` (Alt)
- `Mod4Mask = 0x40` (Super/Meta, equivalent of Command)

These are used when setting modifier state for XTest fake key events.

**AT-SPI2 element lookup by ID**: Same `generate_id` logic as tree building. Walk the AT-SPI2 tree looking for the node whose computed ID matches `req.id`. Once found, create proxies on that node's object reference to execute actions.

**Failure modes:**
- Node not found → `success: false, error: "node not found"`
- No click action available, XTest also fails → `success: false, error: "Element does not support click action"`
- X11 not available (for XTest fallback) → `success: false, error: "X11 display not available for input synthesis"`
- Unknown key name → `success: false, error: "Unknown key: {name}"`

### 4. Integration Changes

**`src/ui/mod.rs`:**
- Add `#[cfg(target_os = "linux")] pub mod capture_linux;`
- Add `#[cfg(target_os = "linux")] pub use capture_linux as capture;`
- Existing `accessibility_linux` and `input_linux` module declarations stay, just the file contents change.

**`src/daemon/server.rs`:**
- Vision sidecar: Remove `#[cfg(target_os = "macos")]` gate. Make `vision_sidecar` field unconditional. The Python sidecar has no platform dependency.
- `tool_debug_ui`: Replace `#[cfg(not(target_os = "macos"))]` error blocks with `#[cfg(target_os = "linux")]` blocks that call `crate::ui::accessibility::query_ax_tree()` and `crate::ui::capture::capture_window_screenshot()`. Since Linux accessibility is async, the `spawn_blocking` wrapper may need adjustment — either use `block_in_place` or restructure to call async directly.
- `tool_debug_ui_action`: The existing dispatch via `input.rs::execute_ui_action()` already routes to `input_linux::execute_action()`. No change needed beyond the implementation.

**`Cargo.toml`:**
```toml
[target.'cfg(target_os = "linux")'.dependencies]
atspi = { version = "0.29", features = ["proxies", "connection", "zbus"] }
x11rb = { version = "0.13", features = ["xtest", "shm"] }
png = "0.17"
```

Note: `png` currently lives under `[target.'cfg(target_os = "macos")'.dependencies]`. Move it to shared `[dependencies]` since both platforms need it, or duplicate under Linux target.

**`src/ui/input.rs`:**
- The modifier constants (`MOD_SHIFT`, etc.) are currently CGEvent-specific. Either:
  - Make them platform-conditional with `#[cfg]`
  - Or keep them as abstract IDs and let each platform's input module interpret them
- The `modifier_string_to_flags` function can remain cross-platform if the constants are made abstract. The actual bitfield values only matter inside `input_mac.rs` and `input_linux.rs`.

## Error Handling

### System-Level Failures

| Failure | Detection | Response |
|---------|-----------|----------|
| AT-SPI2 bus down | Connection error on `AccessibilityConnection::new()` | `UiNotAvailable` with `gsettings` guidance |
| X11 display unavailable | `x11rb::connect()` error | `UiNotAvailable` with `$DISPLAY` guidance |
| D-Bus timeout (hung app) | zbus timeout (25s default) | `UiQueryFailed` with timeout message |
| App exits mid-query | D-Bus peer disconnected error | `UiQueryFailed("Process exited during query")` |
| X11 GetImage fails | Protocol error from server | `UiQueryFailed("Screenshot capture failed")` |
| XTest not available | Extension query fails | `UiQueryFailed("XTest extension not available")` |

### Degraded Operation

If AT-SPI2 is available but X11 is not (pure Wayland, no XWayland):
- `debug_ui mode: "tree"` works (AT-SPI2 is D-Bus, display-agnostic)
- `debug_ui mode: "screenshot"` fails with clear error
- `debug_ui_action` with AT-SPI2 actions (Click via do_action, SetValue) works
- `debug_ui_action` with XTest fallback (Type, Key, Scroll, Drag) fails with clear error

This partial-capability mode is preferable to all-or-nothing.

## Edge Cases

1. **App registered on AT-SPI2 but no windows on X11**: Tree query succeeds, screenshot fails. Return tree with a note that screenshot is unavailable.
2. **Multiple windows for same PID**: Pick largest visible (same heuristic as macOS). This covers the common case of main window + toolbars/dialogs.
3. **Electron apps**: Electron exposes AT-SPI2 when launched with `--force-renderer-accessibility` or when a screen reader is detected. Without this flag, the AT-SPI2 tree may be empty. Error message should mention this.
4. **Qt apps with different AT-SPI2 bridge**: Qt uses `qt-at-spi` bridge. Some older Qt versions have incomplete AT-SPI2 support. Tree may be shallow. Not a Strobe bug — faithfully report what's available.
5. **GTK4 vs GTK3**: Both expose AT-SPI2 but with different role granularity. GTK4 may expose fewer nodes. Not a Strobe issue.
6. **Xwayland coordinate mismatch**: XWayland windows report X11 coordinates. AT-SPI2 reports screen coordinates (which may differ with scaling). Element screenshots may have offset errors with HiDPI. Detect scale factor from X11 (`Xft.dpi` resource or RandR) and adjust.
7. **Window not mapped (minimized)**: `_NET_WM_PID` query finds the window but `GetImage` on an unmapped window returns error. Check window map state before capture.
8. **AT-SPI2 tree changes between snapshot and verify**: Race condition same as macOS. `node_after` may reflect an intermediate state. Accepted — the `changed` flag is best-effort.

## Testing Strategy

### Unit Tests (no display/AT-SPI2 needed)
- Role mapping table: verify all entries bidirectionally
- Action name mapping: verify common actions
- Key name → keysym lookup: verify all ~50 standard key names
- Modifier string → X11 mask: verify all modifier combinations
- Pixel format conversion: BGRX→RGBA, BGRA→RGBA for known byte sequences
- PNG encoding: verify output is valid PNG with correct dimensions

### Integration Tests (require X11 + AT-SPI2 + test app)
- Launch a GTK test app (or use an existing one), verify tree structure
- Verify tree contains expected roles, names, bounds
- Verify screenshot produces valid PNG with non-zero dimensions
- Verify Click action on a button triggers state change
- Verify Type action in a text field changes value
- Verify Key action with modifiers
- Verify error messages for missing PID, unavailable bus

### Platform Gating
- All Linux-specific tests gated with `#[cfg(target_os = "linux")]`
- CI needs a headless X11 environment (Xvfb) with AT-SPI2 bus (dbus-daemon + at-spi2-registryd)

## Out of Scope

- **Pure Wayland capture**: Deferred. Requires compositor-specific protocols. XWayland covers most cases.
- **Wayland input synthesis**: Deferred. Requires libei or compositor support.
- **Custom AT-SPI2 role definitions**: Some apps define custom roles. These will show as `"atspi:UnknownRole"`.
- **AT-SPI2 event streaming**: Real-time accessibility events (focus changes, property updates). Current design is poll-based (query on demand), same as macOS.
- **Flatpak/Snap sandboxed apps**: AT-SPI2 access through portals may require additional D-Bus configuration. Document as known limitation.

## Dependencies

### New Crate Dependencies (Linux only)

| Crate | Version | Purpose |
|-------|---------|---------|
| `atspi` | 0.29 | AT-SPI2 accessibility tree queries |
| `x11rb` | 0.13 | X11 protocol (screenshot + XTest input) |
| `png` | 0.17 | PNG encoding (move from macOS-only to shared) |

### System Dependencies (runtime)

| Dependency | Purpose | Typically present |
|------------|---------|-------------------|
| AT-SPI2 registry (`at-spi2-registryd`) | Accessibility bus | Yes on GNOME/KDE desktops |
| D-Bus session bus | AT-SPI2 transport | Yes on all modern Linux |
| X11 server or XWayland | Screenshots + XTest input | Yes on most desktops |
| XTest extension | Input synthesis | Yes (standard X11 extension) |

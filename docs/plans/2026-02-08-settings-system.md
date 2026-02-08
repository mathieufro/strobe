# Settings System Implementation Plan

**Spec:** `docs/specs/2026-02-08-settings-system.md`
**Goal:** Replace scattered hardcoded constants and env vars with a three-layer file-based settings system (defaults → user global → project-local).
**Architecture:** New `src/config.rs` module provides `resolve(project_root) → StrobeSettings`. Each tool call resolves settings fresh from disk. MCP tool params for `eventLimit` and `timeout` are removed in favor of settings files.
**Tech Stack:** Rust, serde_json for settings file parsing, existing `dirs` crate for `~/.strobe` path.
**Commit strategy:** Single commit at the end.

## Workstreams

- **Stream A (config module):** Tasks 1, 2 — new `src/config.rs` with tests
- **Stream B (MCP cleanup):** Task 3 — remove `eventLimit`/`timeout` from types, update tool schemas
- **Serial:** Tasks 4, 5 — integrate config into server.rs and session_manager.rs (depends on A and B)

---

### Task 1: Create `src/config.rs` with settings struct and resolver

**Files:**
- Create: `src/config.rs`
- Modify: `src/lib.rs:1` (add `pub mod config;`)

**Step 1: Write the failing test**

In `src/config.rs`, write the module with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_defaults_when_no_files_exist() {
        let settings = resolve_with_paths(None, None);
        assert_eq!(settings.events_max_per_session, 200_000);
        assert_eq!(settings.test_status_retry_ms, 5_000);
    }
}
```

**Step 2: Run test — verify it fails**
Run: `cargo test config::tests::test_defaults_when_no_files_exist`
Expected: FAIL (module doesn't exist yet)

**Step 3: Write minimal implementation**

```rust
use serde::Deserialize;
use std::path::Path;

pub const MAX_EVENT_LIMIT: usize = 10_000_000;

/// All configurable settings with their defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct StrobeSettings {
    pub events_max_per_session: usize,
    pub test_status_retry_ms: u64,
}

impl Default for StrobeSettings {
    fn default() -> Self {
        Self {
            events_max_per_session: 200_000,
            test_status_retry_ms: 5_000,
        }
    }
}

/// Raw JSON representation — all fields optional for partial overrides.
#[derive(Debug, Deserialize, Default)]
struct SettingsFile {
    #[serde(rename = "events.maxPerSession")]
    events_max_per_session: Option<usize>,
    #[serde(rename = "test.statusRetryMs")]
    test_status_retry_ms: Option<u64>,
}

/// Resolve settings: defaults → user global → project-local.
pub fn resolve(project_root: Option<&Path>) -> StrobeSettings {
    let global_path = dirs::home_dir()
        .map(|h| h.join(".strobe/settings.json"));
    let project_path = project_root
        .map(|r| r.join(".strobe/settings.json"));
    resolve_with_paths(
        global_path.as_deref(),
        project_path.as_deref(),
    )
}

/// Testable resolver that accepts explicit file paths (no home dir dependency).
fn resolve_with_paths(
    global_path: Option<&Path>,
    project_path: Option<&Path>,
) -> StrobeSettings {
    let mut settings = StrobeSettings::default();

    if let Some(path) = global_path {
        apply_file(&mut settings, path);
    }
    if let Some(path) = project_path {
        apply_file(&mut settings, path);
    }

    settings
}

fn apply_file(settings: &mut StrobeSettings, path: &Path) {
    let Ok(content) = std::fs::read_to_string(path) else { return };
    let Ok(file) = serde_json::from_str::<SettingsFile>(&content) else {
        tracing::warn!("Invalid settings file, ignoring: {}", path.display());
        return;
    };
    if let Some(v) = file.events_max_per_session {
        if v > 0 && v <= MAX_EVENT_LIMIT {
            settings.events_max_per_session = v;
        } else {
            tracing::warn!(
                "events.maxPerSession ({}) out of range (1..{}), using default",
                v, MAX_EVENT_LIMIT
            );
        }
    }
    if let Some(v) = file.test_status_retry_ms {
        if v >= 500 && v <= 60_000 {
            settings.test_status_retry_ms = v;
        } else {
            tracing::warn!(
                "test.statusRetryMs ({}) out of range (500..60000), using default",
                v
            );
        }
    }
}
```

Add to `src/lib.rs`:
```rust
pub mod config;
```

**Step 4: Run test — verify it passes**
Run: `cargo test config::tests::test_defaults_when_no_files_exist`
Expected: PASS

**Checkpoint:** `config::resolve()` exists and returns defaults.

---

### Task 2: Add full test coverage for config module

**Files:**
- Modify: `src/config.rs` (add tests to existing `#[cfg(test)]` block)

**Step 1: Write all remaining tests**

Add these tests to the `tests` module in `src/config.rs`:

```rust
#[test]
fn test_global_overrides_defaults() {
    let dir = tempdir().unwrap();
    let global = dir.path().join("global.json");
    std::fs::write(&global, r#"{"events.maxPerSession": 500000}"#).unwrap();

    let settings = resolve_with_paths(Some(&global), None);
    assert_eq!(settings.events_max_per_session, 500_000);
    assert_eq!(settings.test_status_retry_ms, 5_000); // unchanged
}

#[test]
fn test_project_overrides_global() {
    let dir = tempdir().unwrap();
    let global = dir.path().join("global.json");
    let project = dir.path().join("project.json");
    std::fs::write(&global, r#"{"events.maxPerSession": 500000, "test.statusRetryMs": 3000}"#).unwrap();
    std::fs::write(&project, r#"{"events.maxPerSession": 1000000}"#).unwrap();

    let settings = resolve_with_paths(Some(&global), Some(&project));
    assert_eq!(settings.events_max_per_session, 1_000_000); // project wins
    assert_eq!(settings.test_status_retry_ms, 3_000); // global applies (project didn't set)
}

#[test]
fn test_invalid_json_ignored() {
    let dir = tempdir().unwrap();
    let bad_file = dir.path().join("bad.json");
    std::fs::write(&bad_file, "not json {{{").unwrap();

    let settings = resolve_with_paths(Some(&bad_file), None);
    assert_eq!(settings, StrobeSettings::default());
}

#[test]
fn test_missing_file_ignored() {
    let settings = resolve_with_paths(
        Some(Path::new("/nonexistent/settings.json")),
        None,
    );
    assert_eq!(settings, StrobeSettings::default());
}

#[test]
fn test_unknown_keys_ignored() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("settings.json");
    std::fs::write(&file, r#"{"events.maxPerSession": 300000, "unknown.key": true}"#).unwrap();

    let settings = resolve_with_paths(Some(&file), None);
    assert_eq!(settings.events_max_per_session, 300_000);
}

#[test]
fn test_out_of_range_events_uses_default() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("settings.json");
    // Zero is out of range
    std::fs::write(&file, r#"{"events.maxPerSession": 0}"#).unwrap();
    let settings = resolve_with_paths(Some(&file), None);
    assert_eq!(settings.events_max_per_session, 200_000);

    // Over 10M is out of range
    std::fs::write(&file, r#"{"events.maxPerSession": 99999999}"#).unwrap();
    let settings = resolve_with_paths(Some(&file), None);
    assert_eq!(settings.events_max_per_session, 200_000);
}

#[test]
fn test_out_of_range_retry_uses_default() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("settings.json");
    // 100ms is below minimum
    std::fs::write(&file, r#"{"test.statusRetryMs": 100}"#).unwrap();
    let settings = resolve_with_paths(Some(&file), None);
    assert_eq!(settings.test_status_retry_ms, 5_000);

    // 120000ms is above maximum
    std::fs::write(&file, r#"{"test.statusRetryMs": 120000}"#).unwrap();
    let settings = resolve_with_paths(Some(&file), None);
    assert_eq!(settings.test_status_retry_ms, 5_000);
}

#[test]
fn test_partial_override_preserves_other_defaults() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("settings.json");
    std::fs::write(&file, r#"{"test.statusRetryMs": 2000}"#).unwrap();

    let settings = resolve_with_paths(Some(&file), None);
    assert_eq!(settings.events_max_per_session, 200_000); // default preserved
    assert_eq!(settings.test_status_retry_ms, 2_000); // overridden
}
```

**Step 2: Run tests — verify they pass**
Run: `cargo test config::tests`
Expected: all 8 tests PASS

**Checkpoint:** Config module fully tested — layering, validation, error handling all covered.

---

### Task 3: Clean up MCP types and tool schemas

**Files:**
- Modify: `src/mcp/types.rs`
- Modify: `src/daemon/server.rs` (tool schema definitions and debug_trace description)

**Step 1: Modify `src/mcp/types.rs`**

Remove from `DebugTraceRequest` (lines 45-47):
```rust
    /// Maximum events to keep for this session (default: 200,000)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_limit: Option<usize>,
```

Add to `DebugTraceRequest` (after `serialization_depth`):
```rust
    /// Project root for settings resolution
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_root: Option<String>,
```

Remove from `DebugTestRequest` (lines 326-327):
```rust
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
```

Remove `MAX_EVENT_LIMIT` constant (line 118) — it moves to `config.rs`.

Remove the `event_limit` validation block from `DebugTraceRequest::validate()` (lines 126-133):
```rust
        // Validate event_limit
        if let Some(limit) = self.event_limit {
            if limit > MAX_EVENT_LIMIT {
                return Err(crate::Error::ValidationError(
                    format!("event_limit ({}) exceeds maximum of {}", limit, MAX_EVENT_LIMIT)
                ));
            }
        }
```

**Step 2: Modify tool schemas in `src/daemon/server.rs`**

In `handle_tools_list()`, update the `debug_trace` tool:

Remove lines 414-415 from the description:
```
Validation Limits (enforced):
- eventLimit: max 10,000,000 events per session
```

Remove from `debug_trace` input_schema `properties` (line 423):
```json
"eventLimit": { "type": "integer", "description": "Maximum events to keep for this session (default: 200,000). Oldest events are deleted when limit is reached. Use higher limits (500k-1M) for audio/DSP debugging." },
```

Add to `debug_trace` input_schema `properties`:
```json
"projectRoot": { "type": "string", "description": "Root directory for user code detection" },
```

In `debug_test` input_schema, remove (line 566):
```json
"timeout": { "type": "integer", "description": "Hard timeout in ms (default varies by level)" }
```

Update line 317 in `debugging_instructions()` from:
```
- Default 200k events/session (FIFO). Adjust via `eventLimit`. Use 500k for audio/DSP; avoid 1M+.
```
to:
```
- Default 200k events/session (FIFO). Configure via .strobe/settings.json. Use 500k for audio/DSP; avoid 1M+.
```

**Step 3: Run build — verify it compiles**
Run: `cargo check`
Expected: Compilation errors from server.rs referencing removed fields (expected — fixed in Task 4)

**Checkpoint:** MCP types cleaned up. Build will break until Task 4 integrates the config module.

---

### Task 4: Integrate config into server.rs

**Files:**
- Modify: `src/daemon/server.rs`

This task has multiple touch points. Apply all changes, then verify.

**Step 1: Update `tool_debug_trace` (pending patterns branch, ~line 817)**

Replace:
```rust
                    event_limit: crate::daemon::session_manager::DEFAULT_MAX_EVENTS_PER_SESSION,
```
with:
```rust
                    event_limit: crate::config::StrobeSettings::default().events_max_per_session,
```

**Step 2: Update `tool_debug_trace` (runtime branch, ~lines 856-867)**

Remove the entire `eventLimit` handling block:
```rust
                // Update event limit if provided
                const MAX_EVENT_LIMIT: usize = 10_000_000; // 10M hard cap

                if let Some(limit) = req.event_limit {
                    if limit == 0 || limit > MAX_EVENT_LIMIT {
                        return Err(crate::Error::Frida(format!(
                            "Event limit must be between 1 and {}",
                            MAX_EVENT_LIMIT
                        )));
                    }
                    self.session_manager.set_event_limit(session_id, limit);
                }
```

Replace with settings-based event limit resolution. The event limit is now set once at session creation time (Task 5), so here we just read the current value:
```rust
                // Resolve event limit from settings
                let project_root_path = req.project_root.as_deref()
                    .or_else(|| {
                        // Fall back to session's stored project_root
                        self.session_manager.get_session(session_id).ok()
                            .flatten()
                            .map(|s| s.project_root.as_str().to_string())
                            .as_deref()
                            .map(|_| ()) // Can't return borrowed, use workaround below
                    });
```

Actually, simpler approach — look up project_root from the session DB if not in the request:

```rust
                // Resolve settings from project root
                let project_root_str = req.project_root.clone().or_else(|| {
                    self.session_manager.get_session(session_id).ok()
                        .flatten()
                        .map(|s| s.project_root)
                });
                let settings = crate::config::resolve(
                    project_root_str.as_deref().map(std::path::Path::new)
                );
                self.session_manager.set_event_limit(session_id, settings.events_max_per_session);
```

**Step 3: Update `tool_debug_test_status` (~line 1508)**

Replace:
```rust
                let retry_ms = if warnings.is_empty() { 5_000 } else { 2_000 };
```
with:
```rust
                let settings = crate::config::resolve(
                    Some(std::path::Path::new(&test_run.project_root))
                );
                let retry_ms = if warnings.is_empty() {
                    settings.test_status_retry_ms
                } else {
                    settings.test_status_retry_ms.min(2_000)
                };
```

**Step 4: Update `tool_debug_test` — remove timeout passthrough (~line 1370)**

In the `runner.run()` call, change `req_clone.timeout` to `None`:
```rust
            let run_result = runner.run(
                &project_root,
                req_clone.framework.as_deref(),
                req_clone.level,
                req_clone.test.as_deref(),
                req_clone.command.as_deref(),
                &env,
                None,  // timeout — was req_clone.timeout, now uses adapter defaults
                &session_manager,
                ...
```

**Step 5: Run build — verify it compiles**
Run: `cargo check`
Expected: May still have errors from session_manager.rs referencing removed constant (fixed in Task 5)

**Checkpoint:** Server uses settings for retry_ms and event limits.

---

### Task 5: Remove env var support from session_manager.rs

**Files:**
- Modify: `src/daemon/session_manager.rs`

**Step 1: Remove old constant and env var reader**

Delete lines 12-29 (the doc comment, `DEFAULT_MAX_EVENTS_PER_SESSION`, and `get_max_events_per_session()`):
```rust
pub const DEFAULT_MAX_EVENTS_PER_SESSION: usize = 200_000;

fn get_max_events_per_session() -> usize {
    std::env::var("STROBE_MAX_EVENTS_PER_SESSION")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MAX_EVENTS_PER_SESSION)
}
```

**Step 2: Update `create_session` (line 131)**

Replace:
```rust
        self.event_limits.write().unwrap().insert(id.to_string(), get_max_events_per_session());
```
with:
```rust
        let settings = crate::config::resolve(Some(std::path::Path::new(project_root)));
        self.event_limits.write().unwrap().insert(id.to_string(), settings.events_max_per_session);
```

**Step 3: Update `get_event_limit` fallback (line 237)**

Replace:
```rust
            .unwrap_or(DEFAULT_MAX_EVENTS_PER_SESSION)
```
with:
```rust
            .unwrap_or(crate::config::StrobeSettings::default().events_max_per_session)
```

**Step 4: Update DB writer task fallback values (lines 295, 310, 342)**

Replace all three occurrences of `DEFAULT_MAX_EVENTS_PER_SESSION` in `spawn_with_frida`:
```rust
let mut cached_limit = DEFAULT_MAX_EVENTS_PER_SESSION;
```
→
```rust
let mut cached_limit = crate::config::StrobeSettings::default().events_max_per_session;
```

And the two `.unwrap_or(DEFAULT_MAX_EVENTS_PER_SESSION)` → `.unwrap_or(crate::config::StrobeSettings::default().events_max_per_session)`.

**Step 5: Run full build and tests**
Run: `cargo build && cargo test`
Expected: PASS — all code compiles, existing tests pass, new config tests pass.

**Checkpoint:** env var fully removed. Settings system is the sole configuration path. All tests pass.

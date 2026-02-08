# Settings System

**Date:** 2026-02-08
**Status:** Draft
**Goal:** Centralized, file-based configuration with per-project overrides. Replace scattered hardcoded constants and env vars with a standard settings.json pattern.

---

## Problem

Strobe has no centralized configuration system. Settings are scattered across modules as hardcoded constants with one env var override (`STROBE_MAX_EVENTS_PER_SESSION`):

| Setting | Location | Current mechanism |
|---------|----------|-------------------|
| Event limit per session | `session_manager.rs` | Env var + constant (200,000) |
| Test status retry delay | `server.rs:1508` | Hardcoded (5,000ms / 2,000ms) |
| Idle timeout | `server.rs` | Constant (30 min) |
| Max hooks per call | `spawner.rs` | Constant (100) |
| Stuck detector intervals | `stuck_detector.rs` | Hardcoded (2s, 6s, etc.) |

This means:
- **No per-project customization** — audio projects need higher event limits, fast test suites want shorter poll intervals
- **No discoverability** — users must read source code to know what's tunable
- **Multiple frontends** (MCP CLI, VSCode extension) have no shared config mechanism

---

## Solution

A three-layer settings system with JSON files:

```
Built-in defaults (Rust)
  ↓ overridden by
~/.strobe/settings.json (user global)
  ↓ overridden by
<projectRoot>/.strobe/settings.json (project-local)
```

### Design Decisions

**File-based, not MCP params.** Settings that tune daemon behavior belong in config files, not as MCP tool parameters. This keeps the MCP tool schema focused on the task at hand (what to launch, what to trace, what to query) and avoids polluting every tool call with config knobs. As a consequence, `eventLimit` is removed from `DebugTraceRequest` and `timeout` is removed from `DebugTestRequest`.

**No env var overrides.** Three layers only. CI environments can place a `.strobe/settings.json` in the project directory. Removing the `STROBE_MAX_EVENTS_PER_SESSION` env var eliminates a fourth resolution path.

**Project root from tool calls.** The daemon discovers the project root from `projectRoot` on `debug_launch`, `debug_test`, and `debug_trace` (new). MCP `roots/list` was considered but [Claude Code doesn't implement it](https://github.com/anthropics/claude-code/issues/3315) and it wouldn't handle cross-project debugging (e.g., using Strobe from workspace A to debug a binary in project B).

**Resolved per tool call.** Settings are re-read from disk on every tool call — no caching, no file watchers. Two small JSON file reads per call is negligible (<1ms) and guarantees the user always sees their latest edits reflected immediately.

**Shallow merge per key.** Each key present in a higher-priority file completely replaces the lower-priority value. No deep merging of nested objects.

---

## Settings Schema

### Initial settings (v1)

```jsonc
{
  // Maximum events stored per debug session before FIFO eviction kicks in.
  // Higher values enable longer traces but increase DB size and query time.
  // Guidelines: 200k (~56MB), 500k (~140MB, audio/DSP), 1M+ (avoid).
  "events.maxPerSession": 200000,

  // Base delay (ms) the daemon suggests between debug_test_status polls.
  // When stuck-detector warnings are active, the daemon uses
  // min(this value, 2000ms) for faster feedback.
  "test.statusRetryMs": 5000
}
```

### Naming Convention

Dotted namespace: `<category>.<setting>`. Flat keys in JSON (not nested objects) for simplicity and unambiguous merge semantics.

```jsonc
// ✓ Good — flat dotted keys
{
  "events.maxPerSession": 500000,
  "test.statusRetryMs": 3000
}

// ✗ Not this — nested objects
{
  "events": { "maxPerSession": 500000 },
  "test": { "statusRetryMs": 3000 }
}
```

### Future settings (not implemented now)

These are candidates for later addition. Listed here for schema namespace planning only:

| Key | Default | Purpose |
|-----|---------|---------|
| `daemon.idleTimeoutMinutes` | 30 | Auto-shutdown after inactivity |
| `test.stuckDetector.enabled` | true | Enable/disable stuck detection |
| `trace.serializationDepth` | 3 | Default argument serialization depth |

---

## File Layout

### User global: `~/.strobe/settings.json`

Lives alongside existing daemon files:

```
~/.strobe/
├── daemon.lock
├── strobe.sock
├── strobe.pid
├── strobe.db
├── daemon.log
└── settings.json          ← NEW
```

### Project-local: `<projectRoot>/.strobe/settings.json`

```
my-audio-project/
├── .strobe/
│   └── settings.json      ← project overrides
├── src/
└── Cargo.toml
```

Projects should add `.strobe/` to `.gitignore` (or commit it — team choice). Strobe does not create this directory or file automatically.

---

## Implementation

### New module: `src/config.rs`

```rust
use serde::Deserialize;
use std::path::Path;

/// All configurable settings with their defaults.
#[derive(Debug, Clone)]
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
#[serde(default)]
struct SettingsFile {
    #[serde(rename = "events.maxPerSession")]
    events_max_per_session: Option<usize>,
    #[serde(rename = "test.statusRetryMs")]
    test_status_retry_ms: Option<u64>,
}

/// Resolve settings: defaults → user global → project-local.
pub fn resolve(project_root: Option<&Path>) -> StrobeSettings {
    let mut settings = StrobeSettings::default();

    // Layer 1: user global
    let global_path = dirs::home_dir()
        .map(|h| h.join(".strobe/settings.json"));
    if let Some(path) = global_path {
        apply_file(&mut settings, &path);
    }

    // Layer 2: project-local
    if let Some(root) = project_root {
        apply_file(&mut settings, &root.join(".strobe/settings.json"));
    }

    settings
}

fn apply_file(settings: &mut StrobeSettings, path: &Path) {
    let Ok(content) = std::fs::read_to_string(path) else { return };
    let Ok(file) = serde_json::from_str::<SettingsFile>(&content) else {
        tracing::warn!("Invalid settings file: {}", path.display());
        return;
    };
    if let Some(v) = file.events_max_per_session {
        settings.events_max_per_session = v;
    }
    if let Some(v) = file.test_status_retry_ms {
        settings.test_status_retry_ms = v;
    }
}
```

### Changes to existing code

**`src/mcp/types.rs`**
- Remove `event_limit` field from `DebugTraceRequest`
- Remove `timeout` field from `DebugTestRequest`
- Remove validation logic for `event_limit` in `DebugTraceRequest::validate()`
- Keep `event_limit` in `DebugTraceResponse` (informational — shows the resolved value)

**`src/daemon/server.rs`**
- Import `config::resolve`
- In `handle_debug_trace`: call `config::resolve(project_root)` to get `events_max_per_session`
- In `handle_debug_test_status`: call `config::resolve(project_root)` to get `test_status_retry_ms`, use `min(retry_ms, 2000)` when warnings are active
- Remove hardcoded `5_000` / `2_000` retry constants
- Add `project_root: Option<String>` to `DebugTraceRequest` (new field)

**`src/daemon/session_manager.rs`**
- Remove `DEFAULT_MAX_EVENTS_PER_SESSION` constant
- Remove `get_max_events_per_session()` function (env var reader)
- Accept `events_max_per_session` as a parameter where sessions are created

**`src/mcp/types.rs` (DebugTraceRequest)**
- Add optional `project_root: Option<String>` field for settings resolution

---

## Validation

Settings files with unknown keys are silently ignored (forward compatibility). Settings files with invalid JSON log a warning and are skipped (fall through to lower layer).

Value validation for known keys:
- `events.maxPerSession`: must be > 0 and ≤ 10,000,000 (existing `MAX_EVENT_LIMIT`)
- `test.statusRetryMs`: must be ≥ 500 and ≤ 60,000

Invalid values log a warning and use the default.

---

## Testing

1. **Unit tests in `src/config.rs`:**
   - Default values when no files exist
   - User-global file overrides defaults
   - Project file overrides user-global
   - Partial files (only some keys) merge correctly
   - Invalid JSON warns and falls through
   - Unknown keys are ignored
   - Out-of-range values warn and use defaults

2. **Integration test:**
   - Create temp project with `.strobe/settings.json`
   - Run `debug_test` → verify `retry_in_ms` in status response reflects the setting
   - Change setting file → next tool call picks up the change

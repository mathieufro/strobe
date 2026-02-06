# Review: MCP & Daemon Lifecycle Robustness

**Plan:** `docs/plans/2026-02-06-lifecycle-robustness.md`
**Reviewed:** 2026-02-06
**Commits:** cfcc6d0..5d5920b
**Branch:** feature/lifecycle-robustness

## Summary

| Category | Critical | Important | Minor |
|----------|----------|-----------|-------|
| Security | 1 | 1 | 1 |
| Correctness | 1 | 1 | 1 |
| Tests | 0 | 4 | 1 |
| Code Quality | 1 | 1 | 0 |
| **Total** | **3** | **7** | **3** |

**Ready to merge:** No (3 critical issues)

## Blocking Issues

1. **Initialize set before validation** — malformed `initialize` request sets `initialized=true` even on failure
2. **Missing `PathBuf` import** — linux builds will fail (`create_test_binary` uses unqualified `PathBuf`)
3. **Lock file errno not checked** — permission errors silently fall through to 5s timeout

## Issues

### Issue 1: Initialize set before validation
**Severity:** Critical
**Category:** Security / Correctness
**Location:** `src/daemon/server.rs:176-181`
**Requirement:** "Enforce MCP protocol: initialize must be called first"
**Problem:** `*initialized = true` is set before `handle_initialize` validates params. A client can send `{"method":"initialize","params":"garbage"}` — parsing fails, but `initialized` is already `true`, bypassing enforcement for all subsequent calls.
**Suggested fix:**
```rust
// Replace:
"initialize" => {
    *initialized = true;
    self.handle_initialize(&request.params).await
}

// With:
"initialize" => {
    let result = self.handle_initialize(&request.params).await;
    if result.is_ok() {
        *initialized = true;
    }
    result
}
```

---

### Issue 2: Missing `PathBuf` import breaks Linux builds
**Severity:** Critical
**Category:** Code Quality
**Location:** `tests/integration.rs:1,6`
**Requirement:** Tests must compile on all platforms
**Problem:** We replaced `use std::path::PathBuf` with `use std::collections::{HashMap, HashSet}`, but `create_test_binary` (line 6, `#[cfg(target_os = "linux")]`) still uses unqualified `PathBuf`. Compiles on macOS because the function is cfg-gated out, but Linux CI will fail.
**Suggested fix:**
```rust
use std::collections::{HashMap, HashSet};
#[cfg(target_os = "linux")]
use std::path::PathBuf;
use tempfile::tempdir;
```

---

### Issue 3: Lock file flock errno not checked
**Severity:** Critical
**Category:** Security
**Location:** `src/mcp/proxy.rs:29-31`
**Requirement:** "Use a lock file to prevent multiple proxies from starting daemons simultaneously"
**Problem:** When `flock()` returns -1, the code treats ALL failures as "another process holds the lock." If the failure is EACCES or EBADF, no daemon will be started and `wait_for_daemon` will time out after 5s with a misleading "Daemon failed to start" error.
**Suggested fix:**
```rust
let lock_result = unsafe {
    libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB)
};

if lock_result == 0 {
    start_daemon(&strobe_dir)?;
} else {
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() != Some(libc::EWOULDBLOCK) {
        return Err(crate::Error::Io(err));
    }
    // EWOULDBLOCK: another proxy holds the lock, fall through to wait
}
```

---

### Issue 4: Graceful shutdown deletes sessions before DB writer flushes
**Severity:** Important
**Category:** Correctness
**Location:** `src/daemon/server.rs:94-108`
**Requirement:** "Give DB writer tasks time to flush remaining events"
**Problem:** `stop_session()` calls `delete_session()` which `DELETE FROM sessions` and `DELETE FROM events`. But the background DB writer task (session_manager.rs:201-226) may still have batched events. The 50ms sleep happens AFTER deletion, so late-arriving events will fail foreign key checks. Events are silently lost.
**Suggested fix:** Sleep before deleting, or change `stop_session` to update status rather than delete. The latter is preferable since it preserves session history for post-mortem.

---

### Issue 5: No test for initialize enforcement
**Severity:** Important
**Category:** Tests
**Location:** `tests/integration.rs:395-427`
**Requirement:** Plan Task 7 Step 1: "Test initialize enforcement"
**Problem:** `test_mcp_initialize_response_has_instructions` only tests serialization of the response struct. It does NOT test that the daemon rejects calls before `initialize`. The core enforcement logic at server.rs:165-173 has zero test coverage.

---

### Issue 6: No test for disconnect cleanup
**Severity:** Important
**Category:** Tests
**Requirement:** Plan Task 3: "Disconnecting a client stops its running sessions and clears its pending patterns"
**Problem:** The `handle_disconnect` method (server.rs:339-361) — the main deliverable of Task 3 — has no test. This is critical lifecycle behavior that prevents resource leaks when clients crash.

---

### Issue 7: No test for graceful shutdown
**Severity:** Important
**Category:** Tests
**Requirement:** Plan Task 4: "Idle timeout cleanly stops all Frida sessions, flushes events"
**Problem:** `graceful_shutdown()` (server.rs:91-108) has no test. The `test_session_cleanup_on_stop` test only exercises the DB method `get_running_sessions()`, not the actual shutdown flow.

---

### Issue 8: No test for proxy lock file / race-safe startup
**Severity:** Important
**Category:** Tests
**Requirement:** Plan Task 5: "Multiple simultaneous proxy starts are serialized via lock file"
**Problem:** The lock file logic, `wait_for_daemon`, and connection-based readiness checks in proxy.rs have no test coverage.

---

### Issue 9: `get_running_sessions` accessed via db() bypass
**Severity:** Important
**Category:** Code Quality
**Location:** `src/daemon/server.rs:95`
**Problem:** `self.session_manager.db().get_running_sessions()` bypasses the SessionManager abstraction. All other session operations go through SessionManager wrappers (e.g., `get_session`, `stop_session`). This is inconsistent.
**Suggested fix:** Add `pub fn get_running_sessions(&self) -> Result<Vec<Session>> { self.db.get_running_sessions() }` to SessionManager.

---

### Issue 10: Resource exhaustion — no per-connection session limit
**Severity:** Important
**Category:** Security
**Location:** `src/daemon/server.rs:363-409`
**Problem:** A client can call `debug_launch` with different binaries indefinitely, accumulating sessions without limit. Auto-cleanup only handles the same binary path. Long-lived connections can exhaust memory/DB/Frida resources.
**Suggested fix:** Add `MAX_SESSIONS_PER_CONNECTION` check at the start of `tool_debug_launch`.

---

### Issue 11: Auto-cleanup leaves stale reference in connection_sessions
**Severity:** Minor
**Category:** Correctness
**Location:** `src/daemon/server.rs:414-420`
**Problem:** When auto-stopping an existing session for the same binary, the old session ID is deleted from DB but not removed from the original connection's `connection_sessions` entry. On disconnect, `get_session` returns None so no harm, but it's a stale reference.
**Suggested fix:** After auto-stop, remove the old session from all connection_sessions entries.

---

### Issue 12: Pending patterns consumed in arbitrary order
**Severity:** Minor
**Category:** Correctness
**Location:** `src/daemon/server.rs:455-462`
**Problem:** `HashSet::into_iter().collect()` produces patterns in non-deterministic order. If hook installation sequence matters or debug output lists patterns by installation order, this is confusing.
**Suggested fix:** Sort the Vec after collecting, or use `BTreeSet` instead of `HashSet`.

---

### Issue 13: Test connection to daemon causes log noise
**Severity:** Minor
**Category:** Tests
**Location:** `src/mcp/proxy.rs:112-124`
**Problem:** `is_daemon_running` and `wait_for_daemon` open test connections that immediately drop, causing "Client connected/disconnected" log entries on the daemon. No functional harm, but noisy in daemon.log.

## Approved

- [x] Task 1: Per-connection state tracking — implemented correctly
- [x] Task 2: Per-connection pending patterns — scoped correctly, consumed on launch
- [x] Task 3: Connection disconnect cleanup — implemented correctly
- [x] Task 4: Graceful daemon shutdown — implemented (flush timing concern noted)
- [x] Task 5: Proxy race-safe startup — lock file + connection readiness (errno handling needs fix)
- [x] Task 6: Daemon log file — stderr redirected to ~/.strobe/daemon.log
- [ ] Task 7: Tests — 3/3 tests present but coverage gaps for enforcement, disconnect, shutdown, proxy

## Recommendations

- Consider changing `stop_session` to update status rather than DELETE, so sessions can be inspected post-mortem and the flush-before-delete race goes away
- Set `~/.strobe` directory permissions to 0700 on creation for defense in depth
- The pattern isolation test exercises HashMap logic, not actual daemon code paths — consider a higher-level integration test when a test harness for the daemon exists

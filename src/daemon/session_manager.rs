use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::RwLock;
use std::time::Instant;
use chrono::{Utc, Timelike};
use tokio::sync::mpsc;
use crate::db::{Database, Session, SessionStatus, Event};
use crate::dwarf::{DwarfParser, DwarfHandle};
use crate::frida_collector::{FridaSpawner, HookResult};
use crate::Result;

/// Map TypeKind to the string the agent expects.
fn type_kind_to_agent_str(tk: &crate::dwarf::TypeKind) -> &'static str {
    match tk {
        crate::dwarf::TypeKind::Integer { signed } => {
            if *signed { "int" } else { "uint" }
        }
        crate::dwarf::TypeKind::Float => "float",
        crate::dwarf::TypeKind::Pointer => "pointer",
        crate::dwarf::TypeKind::Unknown => "uint",
    }
}

fn hex_to_bytes(hex: &str) -> std::result::Result<Vec<u8>, String> {
    if hex.len() % 2 != 0 {
        return Err(format!("Hex string must have even length, got {}", hex.len()));
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&hex[i..i + 2], 16)
                .map_err(|e| format!("Invalid hex at offset {}: {}", i, e))
        })
        .collect()
}

/// Acquire a read lock, recovering from poisoned state.
fn read_lock<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|e| e.into_inner())
}

/// Acquire a write lock, recovering from poisoned state.
fn write_lock<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(|e| e.into_inner())
}

#[derive(Clone)]
pub struct ActiveWatchState {
    pub label: String,
    pub address: u64,
    pub size: u8,
    pub type_kind_str: String,
    pub deref_depth: u8,
    pub deref_offset: u64,
    pub type_name: Option<String>,
    pub on_patterns: Option<Vec<String>>,
    pub is_expr: bool,
    pub expr: Option<String>,
}

/// Check if a process is alive. Returns true if the process exists,
/// even if we lack permission to signal it (EPERM).
fn is_process_alive(pid: u32) -> bool {
    let result = unsafe { libc::kill(pid as i32, 0) };
    if result == 0 {
        return true; // Process exists and we can signal it
    }
    // Check errno: EPERM means alive but no permission, ESRCH means dead
    let err = std::io::Error::last_os_error();
    matches!(err.raw_os_error(), Some(libc::EPERM))
}

pub struct SessionManager {
    db: Database,
    /// Active trace patterns per session
    patterns: Arc<RwLock<HashMap<String, Vec<String>>>>,
    /// Cached DWARF handles per binary (background-parsed)
    dwarf_cache: Arc<RwLock<HashMap<String, DwarfHandle>>>,
    /// Hooked function count per session
    hook_counts: Arc<RwLock<HashMap<String, u32>>>,
    /// Active watches per session
    watches: Arc<RwLock<HashMap<String, Vec<ActiveWatchState>>>>,
    /// Per-session event limits (for dynamic configuration)
    event_limits: Arc<RwLock<HashMap<String, usize>>>,
    /// Frida spawner for managing instrumented processes (lazily initialized)
    frida_spawner: Arc<tokio::sync::RwLock<Option<FridaSpawner>>>,
    /// Child PIDs per session (parent PID is in the Session struct)
    child_pids: Arc<RwLock<HashMap<String, Vec<u32>>>>,
    /// Cancellation tokens for database writer tasks per session
    writer_cancel_tokens: Arc<RwLock<HashMap<String, tokio::sync::watch::Sender<bool>>>>,
    /// Breakpoints per session
    breakpoints: Arc<RwLock<HashMap<String, HashMap<String, Breakpoint>>>>,
    /// Logpoints per session
    logpoints: Arc<RwLock<HashMap<String, HashMap<String, Logpoint>>>>,
    /// Paused threads per session
    paused_threads: Arc<RwLock<HashMap<String, HashMap<u64, PauseInfo>>>>,
}

impl SessionManager {
    pub fn new(db_path: &Path) -> Result<Self> {
        let db = Database::open(db_path)?;

        // Clean up any sessions left as 'running' from a previous daemon instance
        db.cleanup_stale_sessions()?;

        Ok(Self {
            db,
            patterns: Arc::new(RwLock::new(HashMap::new())),
            dwarf_cache: Arc::new(RwLock::new(HashMap::new())),
            hook_counts: Arc::new(RwLock::new(HashMap::new())),
            watches: Arc::new(RwLock::new(HashMap::new())),
            event_limits: Arc::new(RwLock::new(HashMap::new())),
            frida_spawner: Arc::new(tokio::sync::RwLock::new(None)),
            child_pids: Arc::new(RwLock::new(HashMap::new())),
            writer_cancel_tokens: Arc::new(RwLock::new(HashMap::new())),
            breakpoints: Arc::new(RwLock::new(HashMap::new())),
            logpoints: Arc::new(RwLock::new(HashMap::new())),
            paused_threads: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    pub fn generate_session_id(&self, binary_name: &str) -> String {
        let now = Utc::now();
        let base_id = format!(
            "{}-{}-{:02}h{:02}",
            binary_name,
            now.format("%Y-%m-%d"),
            now.hour(),
            now.minute()
        );

        // Check for collision
        let mut id = base_id.clone();
        let mut suffix = 2;

        while self.db.get_session(&id).ok().flatten().is_some() {
            id = format!("{}-{}", base_id, suffix);
            suffix += 1;
        }

        id
    }

    pub fn create_session(
        &self,
        id: &str,
        binary_path: &str,
        project_root: &str,
        pid: u32,
    ) -> Result<Session> {
        // Clean up stale sessions on the same binary (dead process still marked Running)
        if let Some(existing) = self.db.get_session_by_binary(binary_path)? {
            if existing.status == SessionStatus::Running {
                let pid_alive = is_process_alive(existing.pid);
                if !pid_alive {
                    tracing::warn!("Session {} has dead PID {}, marking as stopped", existing.id, existing.pid);
                    self.db.update_session_status(&existing.id, SessionStatus::Stopped)?;
                }
                // Multiple concurrent sessions on the same binary are allowed
                // (e.g. parallel agents debugging the same project)
            }
        }

        let session = self.db.create_session(id, binary_path, project_root, pid)?;

        // Initialize pattern storage, watches, and event limit
        write_lock(&self.patterns).insert(id.to_string(), Vec::new());
        write_lock(&self.hook_counts).insert(id.to_string(), 0);
        write_lock(&self.watches).insert(id.to_string(), Vec::new());
        let settings = crate::config::resolve(Some(std::path::Path::new(project_root)));
        write_lock(&self.event_limits).insert(id.to_string(), settings.events_max_per_session);

        Ok(session)
    }

    pub fn get_session(&self, id: &str) -> Result<Option<Session>> {
        self.db.get_session(id)
    }

    pub fn get_running_sessions(&self) -> Result<Vec<Session>> {
        self.db.get_running_sessions()
    }

    pub fn stop_session(&self, id: &str) -> Result<u64> {
        let count = self.db.count_session_events(id)?;

        // Signal database writer task to flush and exit
        if let Some(cancel_tx) = write_lock(&self.writer_cancel_tokens).remove(id) {
            let _ = cancel_tx.send(true);
        }

        self.db.delete_session(id)?;

        // Clean up in-memory state
        write_lock(&self.patterns).remove(id);
        write_lock(&self.hook_counts).remove(id);
        write_lock(&self.watches).remove(id);
        write_lock(&self.event_limits).remove(id);
        write_lock(&self.child_pids).remove(id);

        Ok(count)
    }

    /// Stop a session but retain its DB rows for later inspection.
    /// Cleans up in-memory state and flushes the writer, but does NOT delete from DB.
    pub fn stop_session_retain(&self, id: &str) -> Result<u64> {
        let count = self.db.count_session_events(id)?;

        // Signal database writer task to flush and exit
        if let Some(cancel_tx) = write_lock(&self.writer_cancel_tokens).remove(id) {
            let _ = cancel_tx.send(true);
        }

        // Mark session as stopped (but keep it in the DB)
        self.db.mark_session_stopped(id)?;

        // Clean up in-memory state
        write_lock(&self.patterns).remove(id);
        write_lock(&self.hook_counts).remove(id);
        write_lock(&self.watches).remove(id);
        write_lock(&self.event_limits).remove(id);
        write_lock(&self.child_pids).remove(id);

        Ok(count)
    }

    pub fn add_child_pid(&self, session_id: &str, pid: u32) {
        write_lock(&self.child_pids)
            .entry(session_id.to_string())
            .or_default()
            .push(pid);
    }

    pub fn get_all_pids(&self, session_id: &str) -> Vec<u32> {
        let mut pids = vec![];
        if let Ok(Some(session)) = self.get_session(session_id) {
            pids.push(session.pid);
        }
        if let Some(children) = read_lock(&self.child_pids).get(session_id) {
            pids.extend(children);
        }
        pids
    }

    pub fn add_patterns(&self, session_id: &str, patterns: &[String]) -> Result<()> {
        let mut all_patterns = write_lock(&self.patterns);
        let session_patterns = all_patterns
            .entry(session_id.to_string())
            .or_default();

        for pattern in patterns {
            if !session_patterns.contains(pattern) {
                session_patterns.push(pattern.clone());
            }
        }

        Ok(())
    }

    pub fn remove_patterns(&self, session_id: &str, patterns: &[String]) -> Result<()> {
        let mut all_patterns = write_lock(&self.patterns);
        if let Some(session_patterns) = all_patterns.get_mut(session_id) {
            session_patterns.retain(|p| !patterns.contains(p));
        }
        Ok(())
    }

    pub fn get_patterns(&self, session_id: &str) -> Vec<String> {
        read_lock(&self.patterns)
            .get(session_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn set_hook_count(&self, session_id: &str, count: u32) {
        write_lock(&self.hook_counts)
            .insert(session_id.to_string(), count);
    }

    pub fn get_hook_count(&self, session_id: &str) -> u32 {
        read_lock(&self.hook_counts)
            .get(session_id)
            .copied()
            .unwrap_or(0)
    }

    pub fn set_event_limit(&self, session_id: &str, limit: usize) {
        self.event_limits
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id.to_string(), limit);
    }

    pub fn get_event_limit(&self, session_id: &str) -> usize {
        read_lock(&self.event_limits)
            .get(session_id)
            .copied()
            .unwrap_or(crate::config::StrobeSettings::default().events_max_per_session)
    }

    /// Get or start a background DWARF parse. Returns a handle immediately.
    /// If the binary was already parsed (or is being parsed), returns the cached handle.
    /// Failed parses are evicted from cache so that retries (e.g. after dsymutil) work.
    pub fn get_or_start_dwarf_parse(&self, binary_path: &str) -> DwarfHandle {
        // Include mtime in cache key so rebuilds invalidate the cache
        let mtime = std::fs::metadata(binary_path)
            .and_then(|m| m.modified())
            .ok();
        let cache_key = match mtime {
            Some(t) => format!("{}@{}", binary_path, t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()),
            None => binary_path.to_string(),
        };

        // Fast path: read lock only
        {
            let cache = read_lock(&self.dwarf_cache);
            if let Some(handle) = cache.get(&cache_key) {
                if !handle.is_failed() {
                    return handle.clone();
                }
            }
        }

        // Slow path: write lock with double-check
        let mut cache = write_lock(&self.dwarf_cache);
        if let Some(handle) = cache.get(&cache_key) {
            if !handle.is_failed() {
                return handle.clone();
            }
        }

        let handle = DwarfHandle::spawn_parse(binary_path);
        cache.insert(cache_key, handle.clone());
        handle
    }

    pub fn db(&self) -> &Database {
        &self.db
    }

    /// Spawn a process with Frida attached.
    /// DWARF parsing happens in the background — launch is fast (~1s).
    pub async fn spawn_with_frida(
        &self,
        session_id: &str,
        command: &str,
        args: &[String],
        cwd: Option<&str>,
        project_root: &str,
        env: Option<&std::collections::HashMap<String, String>>,
        defer_resume: bool,
    ) -> Result<u32> {
        // Extract image base cheaply (<10ms) — only reads __TEXT segment address
        let image_base = DwarfParser::extract_image_base(Path::new(command)).unwrap_or(0);

        // Start background DWARF parse (or get cached handle)
        let dwarf_handle = self.get_or_start_dwarf_parse(command);

        // Create event channel
        let (tx, mut rx) = mpsc::channel::<Event>(10000);

        // Spawn database writer task with automatic event limit enforcement
        let db = self.db.clone();
        let event_limits = Arc::clone(&self.event_limits);
        let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
        write_lock(&self.writer_cancel_tokens).insert(session_id.to_string(), cancel_tx);

        tokio::spawn(async move {
            let mut batch = Vec::with_capacity(100);
            let mut cached_limit = crate::config::StrobeSettings::default().events_max_per_session;
            let mut batches_since_refresh = 0u32;

            let flush_batch = |batch: &mut Vec<Event>, cached_limit: &mut usize, batches_since_refresh: &mut u32| {
                if batch.is_empty() { return; }
                if *batches_since_refresh >= 10 {
                    let session_id = &batch[0].session_id;
                    *cached_limit = read_lock(&event_limits)
                        .get(session_id)
                        .copied()
                        .unwrap_or(crate::config::StrobeSettings::default().events_max_per_session);
                    *batches_since_refresh = 0;
                }
                *batches_since_refresh += 1;
                match db.insert_events_with_limit(batch, *cached_limit) {
                    Ok(stats) => {
                        if stats.events_deleted > 0 {
                            tracing::warn!(
                                "Event limit cleanup: deleted {} old events from {} session(s) to stay within {} event limit",
                                stats.events_deleted, stats.sessions_cleaned.len(), cached_limit
                            );
                        }
                    }
                    Err(e) => tracing::error!("Failed to insert events: {}", e),
                }
                batch.clear();
            };

            loop {
                tokio::select! {
                    Some(event) = rx.recv() => {
                        batch.push(event);
                        if batch.len() >= 100 {
                            flush_batch(&mut batch, &mut cached_limit, &mut batches_since_refresh);
                        }
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {
                        flush_batch(&mut batch, &mut cached_limit, &mut batches_since_refresh);
                    }
                    _ = cancel_rx.changed() => {
                        flush_batch(&mut batch, &mut cached_limit, &mut batches_since_refresh);
                        break;
                    }
                }
            }
        });

        // Create pause notification channel for breakpoint support
        let (pause_tx, mut pause_rx) = mpsc::channel::<crate::frida_collector::PauseNotification>(100);
        let paused_threads = Arc::clone(&self.paused_threads);
        let sid = session_id.to_string();

        // Spawn receiver task that bridges pause notifications to SessionManager state
        tokio::spawn(async move {
            while let Some(notification) = pause_rx.recv().await {
                let info = PauseInfo {
                    breakpoint_id: notification.breakpoint_id,
                    func_name: notification.func_name,
                    file: notification.file,
                    line: notification.line,
                    paused_at: Instant::now(),
                    return_address: notification.return_address,
                };
                write_lock(&paused_threads)
                    .entry(sid.clone())
                    .or_insert_with(HashMap::new)
                    .insert(notification.thread_id, info);
            }
        });

        // Spawn process (lazily initialize FridaSpawner)
        let mut guard = self.frida_spawner.write().await;
        let spawner = guard.get_or_insert_with(FridaSpawner::new);
        spawner.spawn(
            session_id,
            command,
            args,
            cwd,
            project_root,
            env,
            dwarf_handle,
            image_base,
            tx,
            defer_resume,
            Some(pause_tx),
        ).await
    }

    /// Resume a process that was spawned with defer_resume=true.
    pub async fn resume_process(&self, pid: u32) -> Result<()> {
        let guard = self.frida_spawner.read().await;
        match guard.as_ref() {
            Some(spawner) => spawner.resume(pid).await,
            None => Err(crate::Error::Frida("No Frida spawner initialized".to_string())),
        }
    }

    /// Update Frida trace patterns
    pub async fn update_frida_patterns(
        &self,
        session_id: &str,
        add: Option<&[String]>,
        remove: Option<&[String]>,
        serialization_depth: Option<u32>,
    ) -> Result<HookResult> {
        let mut guard = self.frida_spawner.write().await;
        let spawner = match guard.as_mut() {
            Some(s) => s,
            None => return Ok(HookResult { installed: 0, matched: 0, warnings: vec![] }),
        };

        if let Some(patterns) = add {
            return spawner.add_patterns(session_id, patterns, serialization_depth).await;
        }

        if let Some(patterns) = remove {
            let remaining = spawner.remove_patterns(session_id, patterns).await?;
            return Ok(HookResult { installed: remaining, matched: 0, warnings: vec![] });
        }

        Ok(HookResult { installed: 0, matched: 0, warnings: vec![] })
    }

    /// Update Frida watches
    pub async fn update_frida_watches(
        &self,
        session_id: &str,
        watches: Vec<crate::frida_collector::WatchTarget>,
        expr_watches: Vec<crate::frida_collector::ExprWatchTarget>,
    ) -> Result<()> {
        let mut guard = self.frida_spawner.write().await;
        let spawner = match guard.as_mut() {
            Some(s) => s,
            None => return Ok(()),
        };

        spawner.set_watches(session_id, watches, expr_watches).await
    }

    /// Send a raw read_memory command to the Frida agent and return the response.
    async fn send_read_memory(
        &self,
        session_id: &str,
        recipes_json: String,
    ) -> Result<serde_json::Value> {
        let guard = self.frida_spawner.read().await;
        let spawner = guard.as_ref()
            .ok_or_else(|| crate::Error::Frida("No Frida spawner available".to_string()))?;

        spawner.read_memory(session_id, recipes_json).await
    }

    /// Execute a debug_read request end-to-end: validate, resolve DWARF, build recipes,
    /// send to agent, format response. This is the full pipeline used by the MCP tool.
    pub async fn execute_debug_read(
        &self,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        use crate::mcp::*;

        let req: DebugReadRequest = serde_json::from_value(args.clone())?;
        req.validate()?;

        // Verify session exists and is running
        let session = self.get_session(&req.session_id)?
            .ok_or_else(|| crate::Error::SessionNotFound(req.session_id.clone()))?;
        if session.status != crate::db::SessionStatus::Running {
            return Err(crate::Error::ReadFailed(
                "Process exited — session still queryable but reads unavailable".to_string()
            ));
        }

        let depth = req.depth.unwrap_or(1);

        // Build read recipes from targets
        let mut recipes: Vec<serde_json::Value> = Vec::new();
        let mut response_results: Vec<ReadResult> = Vec::new();

        // Get DWARF parser for variable resolution
        let dwarf = self.get_dwarf(&req.session_id).await?;

        for target in &req.targets {
            if let Some(ref var_name) = target.variable {
                let dwarf_ref = match dwarf.as_ref() {
                    Some(d) => d,
                    None => {
                        response_results.push(ReadResult {
                            target: var_name.clone(),
                            error: Some("No debug symbols available".to_string()),
                            ..Default::default()
                        });
                        continue;
                    }
                };

                match dwarf_ref.resolve_read_target(var_name, depth) {
                    Ok((recipe, struct_fields)) => {
                        let type_kind_str = type_kind_to_agent_str(&recipe.type_kind);

                        let mut recipe_json = serde_json::json!({
                            "label": var_name,
                            "address": format!("0x{:x}", recipe.base_address),
                            "size": recipe.final_size,
                            "typeKind": type_kind_str,
                            "derefDepth": recipe.deref_chain.len().min(1),
                            "derefOffset": recipe.deref_chain.first().copied().unwrap_or(0),
                        });

                        if let Some(fields) = struct_fields {
                            recipe_json["struct"] = serde_json::json!(true);
                            let fields_json: Vec<serde_json::Value> = fields.iter().map(|f| {
                                serde_json::json!({
                                    "name": f.name,
                                    "offset": f.offset,
                                    "size": f.size,
                                    "typeKind": type_kind_to_agent_str(&f.type_kind),
                                    "typeName": f.type_name,
                                    "isTruncatedStruct": f.is_truncated_struct,
                                })
                            }).collect();
                            recipe_json["fields"] = serde_json::json!(fields_json);
                        }

                        recipes.push(recipe_json);
                    }
                    Err(e) => {
                        response_results.push(ReadResult {
                            target: var_name.clone(),
                            error: Some(e.to_string()),
                            ..Default::default()
                        });
                    }
                }
            } else if let Some(ref addr) = target.address {
                let size = target.size.unwrap_or(4);
                let type_hint = target.type_hint.clone().unwrap_or_else(|| "bytes".to_string());

                recipes.push(serde_json::json!({
                    "label": addr,
                    "address": addr,
                    "size": size,
                    "typeKind": type_hint,
                    "derefDepth": 0,
                    "derefOffset": 0,
                    "noSlide": true,
                }));
            }
        }

        if recipes.is_empty() && !response_results.is_empty() {
            return Ok(serde_json::to_value(DebugReadResponse {
                results: response_results,
            })?);
        }

        // Build message for agent
        let mut msg = serde_json::json!({
            "type": "read_memory",
            "recipes": recipes,
        });

        // Include imageBase so the agent can compute ASLR slide even if no hooks are installed
        if let Some(ref d) = dwarf {
            msg["imageBase"] = serde_json::json!(format!("0x{:x}", d.image_base));
        }

        if let Some(ref poll) = req.poll {
            msg["poll"] = serde_json::json!({
                "intervalMs": poll.interval_ms,
                "durationMs": poll.duration_ms,
            });
        }

        let msg_str = serde_json::to_string(&msg)?;
        let agent_response = self.send_read_memory(&req.session_id, msg_str).await?;

        // Handle poll mode
        if req.poll.is_some() {
            let poll = req.poll.as_ref().unwrap();
            let expected = poll.duration_ms / poll.interval_ms;
            let response = DebugReadPollResponse {
                polling: true,
                variable_count: recipes.len(),
                interval_ms: poll.interval_ms,
                duration_ms: poll.duration_ms,
                expected_samples: expected,
                event_type: "variable_snapshot".to_string(),
                hint: "Use debug_query({ eventType: 'variable_snapshot' }) to see results".to_string(),
            };
            return Ok(serde_json::to_value(response)?);
        }

        // Handle one-shot response — merge agent results with any pre-computed errors
        if let Some(results) = agent_response.get("results").and_then(|v| v.as_array()) {
            for result in results {
                let label = result.get("label").and_then(|v| v.as_str()).unwrap_or("?");
                let mut read_result = ReadResult {
                    target: label.to_string(),
                    ..Default::default()
                };

                if let Some(err) = result.get("error").and_then(|v| v.as_str()) {
                    read_result.error = Some(err.to_string());
                } else if let Some(fields) = result.get("fields") {
                    read_result.fields = Some(fields.clone());
                } else if let Some(value) = result.get("value") {
                    if result.get("isBytes").and_then(|v| v.as_bool()).unwrap_or(false) {
                        if let Some(hex) = value.as_str() {
                            match hex_to_bytes(hex) {
                                Ok(bytes) => {
                                    let dir = "/tmp/strobe/reads";
                                    let _ = std::fs::create_dir_all(dir);
                                    let filename = format!("{}-{}.bin", req.session_id, chrono::Utc::now().timestamp());
                                    let filepath = format!("{}/{}", dir, filename);
                                    if let Err(e) = std::fs::write(&filepath, &bytes) {
                                        read_result.error = Some(format!("Failed to write bytes file: {}", e));
                                    } else {
                                        read_result.file = Some(filepath);
                                        let preview_bytes = &bytes[..bytes.len().min(32)];
                                        read_result.preview = Some(
                                            preview_bytes.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(" ")
                                        );
                                    }
                                }
                                Err(e) => {
                                    read_result.error = Some(format!("Failed to decode bytes: {}", e));
                                }
                            }
                        }
                    } else {
                        read_result.value = Some(value.clone());
                    }
                }

                response_results.push(read_result);
            }
        }

        Ok(serde_json::to_value(DebugReadResponse {
            results: response_results,
        })?)
    }

    /// Stop Frida session
    pub async fn stop_frida(&self, session_id: &str) -> Result<()> {
        let mut guard = self.frida_spawner.write().await;
        match guard.as_mut() {
            Some(spawner) => spawner.stop(session_id).await,
            None => Ok(()), // No spawner — nothing to stop
        }
    }

    /// Set active watches for a session
    pub fn set_watches(&self, session_id: &str, watches: Vec<ActiveWatchState>) {
        self.watches
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id.to_string(), watches);
    }

    /// Remove watches by label, returning the remaining watches
    pub fn remove_watches(&self, session_id: &str, labels: &[String]) -> Vec<ActiveWatchState> {
        let mut watches_map = write_lock(&self.watches);
        if let Some(watches) = watches_map.get_mut(session_id) {
            watches.retain(|w| !labels.contains(&w.label));
            watches.clone()
        } else {
            vec![]
        }
    }

    /// Get active watches for a session
    pub fn get_watches(&self, session_id: &str) -> Vec<ActiveWatchState> {
        read_lock(&self.watches)
            .get(session_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Get DWARF parser for a session's binary
    pub async fn get_dwarf(&self, session_id: &str) -> Result<Option<Arc<DwarfParser>>> {
        let session = match self.get_session(session_id)? {
            Some(s) => s,
            None => return Ok(None),
        };

        let mut handle = self.get_or_start_dwarf_parse(&session.binary_path);
        match handle.get().await {
            Ok(parser) => Ok(Some(parser)),
            Err(e) => Err(e),
        }
    }

    /// Resolve local variables for a crash event and update it in the DB.
    pub async fn resolve_crash_locals(&self, session_id: &str, event_id: &str) -> Result<()> {
        // Get the crash event
        let events = self.db.query_events(session_id, |q| {
            q.event_type(crate::db::EventType::Crash).limit(1)
        })?;
        let event = match events.first() {
            Some(e) => e,
            None => return Ok(()),
        };

        // Get DWARF parser
        let dwarf = match self.get_dwarf(session_id).await? {
            Some(d) => d,
            None => return Ok(()),
        };

        // Get crash PC from backtrace (first frame) or fault address
        let crash_pc_str = event.backtrace.as_ref()
            .and_then(|bt| bt.as_array())
            .and_then(|frames| frames.first())
            .and_then(|f| f.get("address"))
            .and_then(|a| a.as_str())
            .or(event.fault_address.as_deref());

        let crash_pc = crash_pc_str
            .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok());

        if let Some(pc) = crash_pc {
            if let Ok(locals_info) = dwarf.parse_locals_at_pc(pc) {
                let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" };

                // Extract frame_memory and frame_base from the crash event's text field
                // (stored by parse_event as JSON with frameMemory/frameBase keys)
                let (frame_memory, frame_base) = event.text.as_ref()
                    .and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok())
                    .map(|v| {
                        let fm = v.get("frameMemory").and_then(|f| f.as_str()).map(|s| s.to_string());
                        let fb = v.get("frameBase").and_then(|f| f.as_str()).map(|s| s.to_string());
                        (fm, fb)
                    })
                    .unwrap_or((None, None));

                let locals = crate::dwarf::resolve_crash_locals(
                    &locals_info,
                    event.registers.as_ref().unwrap_or(&serde_json::Value::Null),
                    frame_memory.as_deref(),
                    frame_base.as_deref(),
                    arch,
                );
                if !locals.is_empty() {
                    self.db.update_event_locals(event_id, &serde_json::Value::Array(locals))?;
                }
            }
        }
        Ok(())
    }

    // ========== Phase 2: Active debugging (async API) ==========

    /// Set a breakpoint at a function or source line
    pub async fn set_breakpoint_async(
        &self,
        session_id: &str,
        id: Option<String>,
        function: Option<String>,
        file: Option<String>,
        line: Option<u32>,
        condition: Option<String>,
        hit_count: Option<u32>,
    ) -> Result<crate::mcp::BreakpointInfo> {
        // Validate session exists
        let session = self.db.get_session(session_id)?
            .ok_or_else(|| crate::Error::SessionNotFound(session_id.to_string()))?;

        // Get DWARF parser for address resolution
        let mut dwarf_handle = self.get_or_start_dwarf_parse(&session.binary_path);
        let dwarf = dwarf_handle.get().await?;

        let breakpoint_id = id.unwrap_or_else(|| format!("bp-{}", uuid::Uuid::new_v4().to_string()));

        // Save function name for later use (before it's moved into the match)
        let function_name_for_target = function.clone();

        // Resolve target to address
        let (address, resolved_function, resolved_file, resolved_line) = if let Some(func_pattern) = function {
            // Function breakpoint: resolve via DWARF function table
            let matches = dwarf.find_by_pattern(&func_pattern);
            if matches.is_empty() {
                return Err(crate::Error::ValidationError(
                    format!("No function matching pattern '{}'", func_pattern)
                ));
            }
            let func = &matches[0];
            (
                func.low_pc,
                Some(func.name.clone()),
                func.source_file.clone(),
                func.line_number.map(|l| l as u32),
            )
        } else if let (Some(file_path), Some(line_num)) = (file, line) {
            // Line breakpoint: resolve via DWARF line table
            let result = dwarf.resolve_line(&file_path, line_num)
                .ok_or_else(|| {
                    let nearest = dwarf.find_nearest_lines(&file_path, line_num, 5);
                    crate::Error::NoCodeAtLine {
                        file: file_path.clone(),
                        line: line_num,
                        nearest_lines: nearest,
                    }
                })?;
            (result.0, None, Some(file_path), Some(result.1))
        } else {
            return Err(crate::Error::ValidationError(
                "Breakpoint must specify either function or file+line".to_string()
            ));
        };

        let runtime_address = address;

        // Send setBreakpoint message to agent
        let mut spawner_guard = self.frida_spawner.write().await;
        let spawner = spawner_guard.as_mut()
            .ok_or_else(|| crate::Error::Internal("Frida spawner not initialized".to_string()))?;

        let message = serde_json::json!({
            "type": "setBreakpoint",
            "address": format!("0x{:x}", runtime_address),
            "id": breakpoint_id,
            "condition": condition,
            "hitCount": hit_count.unwrap_or(0),
            "funcName": resolved_function,
            "file": resolved_file,
            "line": resolved_line,
        });

        spawner.set_breakpoint(session_id, message).await?;

        // Store breakpoint in session state
        let bp = Breakpoint {
            id: breakpoint_id.clone(),
            target: if let Some(f) = function_name_for_target {
                BreakpointTarget::Function(f)
            } else {
                BreakpointTarget::Line {
                    file: resolved_file.clone().unwrap(),
                    line: resolved_line.unwrap(),
                }
            },
            address: runtime_address,
            condition,
            hit_count: hit_count.unwrap_or(0),
            hits: 0,
        };

        self.add_breakpoint(session_id, bp);

        Ok(crate::mcp::BreakpointInfo {
            id: breakpoint_id,
            function: resolved_function,
            file: resolved_file,
            line: resolved_line,
            address: format!("0x{:x}", runtime_address),
        })
    }

    /// Continue execution after a breakpoint pause
    pub async fn debug_continue_async(
        &self,
        session_id: &str,
        action: Option<String>,
    ) -> Result<crate::mcp::DebugContinueResponse> {
        // Get all paused threads for this session
        let paused = self.get_all_paused_threads(session_id);

        if paused.is_empty() {
            return Err(crate::Error::ValidationError(
                "No paused threads in this session".to_string()
            ));
        }

        let action = action.unwrap_or_else(|| "continue".to_string());

        // Get session info for DWARF access
        let session = self.db.get_session(session_id)?
            .ok_or_else(|| crate::Error::SessionNotFound(session_id.to_string()))?;

        // For stepping actions, we need DWARF info
        let one_shot_addresses = if action != "continue" {
            let mut dwarf_handle = self.get_or_start_dwarf_parse(&session.binary_path);
            let dwarf = dwarf_handle.get().await?;

            // Get the first paused thread (stepping is single-threaded)
            let (_thread_id, pause_info) = paused.iter().next()
                .ok_or_else(|| crate::Error::ValidationError("No paused thread".to_string()))?;

            // Get the breakpoint to find current address
            let bp = self.get_breakpoint(session_id, &pause_info.breakpoint_id);
            let current_address = bp.map(|b| b.address).unwrap_or(0);

            match action.as_str() {
                "step-over" => {
                    let mut addresses = Vec::new();

                    // Find next line in same function
                    if let Some((next_addr, _file, _line)) = dwarf.next_line_in_function(current_address) {
                        addresses.push(next_addr);
                        tracing::debug!("step-over: next line at 0x{:x}", next_addr);
                    } else {
                        tracing::warn!("step-over: no next line for 0x{:x}", current_address);
                    }

                    // Also hook return address as fallback (end of function)
                    if let Some(ret_addr) = pause_info.return_address {
                        if !addresses.contains(&ret_addr) {
                            addresses.push(ret_addr);
                            tracing::debug!("step-over: return address fallback at 0x{:x}", ret_addr);
                        }
                    }

                    addresses
                }
                "step-into" => {
                    // Same as step-over for now (full step-into requires call target resolution)
                    let mut addresses = Vec::new();

                    if let Some((next_addr, _file, _line)) = dwarf.next_line_in_function(current_address) {
                        addresses.push(next_addr);
                        tracing::debug!("step-into: next line at 0x{:x}", next_addr);
                    } else {
                        tracing::warn!("step-into: no next line for 0x{:x}", current_address);
                    }

                    addresses
                }
                "step-out" => {
                    // Set one-shot at return address
                    let mut addresses = Vec::new();

                    if let Some(ret_addr) = pause_info.return_address {
                        addresses.push(ret_addr);
                        tracing::debug!("step-out: hooking return address 0x{:x}", ret_addr);
                    } else {
                        tracing::warn!("step-out: no return address available, resuming normally");
                    }

                    addresses
                }
                _ => {
                    return Err(crate::Error::ValidationError(
                        format!("Unknown action: '{}'. Valid: continue, step-over, step-into, step-out", action)
                    ));
                }
            }
        } else {
            Vec::new()
        };

        // Send resume message to each paused thread
        let mut spawner_guard = self.frida_spawner.write().await;
        let spawner = spawner_guard.as_mut()
            .ok_or_else(|| crate::Error::Internal("Frida spawner not initialized".to_string()))?;

        for (thread_id, _pause_info) in paused {
            spawner.resume_thread_with_step(session_id, thread_id, one_shot_addresses.clone()).await?;
            self.remove_paused_thread(session_id, thread_id);
        }

        Ok(crate::mcp::DebugContinueResponse {
            status: "running".to_string(),
            breakpoint_id: None,
            file: None,
            line: None,
            function: None,
        })
    }

    /// Set a logpoint at a function or source line (non-blocking breakpoint)
    pub async fn set_logpoint_async(
        &self,
        session_id: &str,
        id: Option<String>,
        function: Option<String>,
        file: Option<String>,
        line: Option<u32>,
        message: String,
        condition: Option<String>,
    ) -> Result<crate::mcp::LogpointInfo> {
        let session = self.db.get_session(session_id)?
            .ok_or_else(|| crate::Error::SessionNotFound(session_id.to_string()))?;

        let mut dwarf_handle = self.get_or_start_dwarf_parse(&session.binary_path);
        let dwarf = dwarf_handle.get().await?;

        let logpoint_id = id.unwrap_or_else(|| format!("lp-{}", uuid::Uuid::new_v4().to_string()));
        let function_name_for_target = function.clone();

        let (address, resolved_function, resolved_file, resolved_line) = if let Some(func_pattern) = function {
            let matches = dwarf.find_by_pattern(&func_pattern);
            if matches.is_empty() {
                return Err(crate::Error::ValidationError(
                    format!("No function matching pattern '{}'", func_pattern)
                ));
            }
            let func = &matches[0];
            (
                func.low_pc,
                Some(func.name.clone()),
                func.source_file.clone(),
                func.line_number.map(|l| l as u32),
            )
        } else if let (Some(file_path), Some(line_num)) = (file, line) {
            let result = dwarf.resolve_line(&file_path, line_num)
                .ok_or_else(|| {
                    let nearest = dwarf.find_nearest_lines(&file_path, line_num, 5);
                    crate::Error::NoCodeAtLine {
                        file: file_path.clone(),
                        line: line_num,
                        nearest_lines: nearest,
                    }
                })?;
            (result.0, None, Some(file_path), Some(result.1))
        } else {
            return Err(crate::Error::ValidationError(
                "Logpoint must specify either function or file+line".to_string()
            ));
        };

        let runtime_address = address;

        // Send setLogpoint message to agent
        let mut spawner_guard = self.frida_spawner.write().await;
        let spawner = spawner_guard.as_mut()
            .ok_or_else(|| crate::Error::Internal("Frida spawner not initialized".to_string()))?;

        let msg = serde_json::json!({
            "type": "setLogpoint",
            "address": format!("0x{:x}", runtime_address),
            "id": logpoint_id,
            "message": message,
            "condition": condition,
            "funcName": resolved_function,
            "file": resolved_file,
            "line": resolved_line,
        });

        spawner.set_breakpoint(session_id, msg).await?;

        // Store logpoint in session state
        let lp = Logpoint {
            id: logpoint_id.clone(),
            target: if let Some(f) = function_name_for_target {
                BreakpointTarget::Function(f)
            } else {
                BreakpointTarget::Line {
                    file: resolved_file.clone().unwrap(),
                    line: resolved_line.unwrap(),
                }
            },
            address: runtime_address,
            message: message.clone(),
            condition,
        };

        self.add_logpoint(session_id, lp);

        Ok(crate::mcp::LogpointInfo {
            id: logpoint_id,
            message,
            function: resolved_function,
            file: resolved_file,
            line: resolved_line,
            address: format!("0x{:x}", runtime_address),
        })
    }

    // ========== Phase 2: Breakpoint management (sync helpers) ==========

    pub fn add_breakpoint(&self, session_id: &str, breakpoint: Breakpoint) {
        let mut guard = write_lock(&self.breakpoints);
        guard.entry(session_id.to_string())
            .or_insert_with(HashMap::new)
            .insert(breakpoint.id.clone(), breakpoint);
    }

    pub fn remove_breakpoint(&self, session_id: &str, breakpoint_id: &str) {
        let mut guard = write_lock(&self.breakpoints);
        if let Some(session_bps) = guard.get_mut(session_id) {
            session_bps.remove(breakpoint_id);
        }
    }

    pub fn get_breakpoints(&self, session_id: &str) -> Vec<Breakpoint> {
        let guard = read_lock(&self.breakpoints);
        guard.get(session_id)
            .map(|bps| bps.values().cloned().collect())
            .unwrap_or_default()
    }

    pub fn get_breakpoint(&self, session_id: &str, breakpoint_id: &str) -> Option<Breakpoint> {
        let guard = read_lock(&self.breakpoints);
        guard.get(session_id)
            .and_then(|bps| bps.get(breakpoint_id))
            .cloned()
    }

    // Logpoint management
    pub fn add_logpoint(&self, session_id: &str, logpoint: Logpoint) {
        let mut guard = write_lock(&self.logpoints);
        guard.entry(session_id.to_string())
            .or_insert_with(HashMap::new)
            .insert(logpoint.id.clone(), logpoint);
    }

    pub fn remove_logpoint(&self, session_id: &str, logpoint_id: &str) {
        let mut guard = write_lock(&self.logpoints);
        if let Some(session_lps) = guard.get_mut(session_id) {
            session_lps.remove(logpoint_id);
        }
    }

    pub fn get_logpoints(&self, session_id: &str) -> Vec<Logpoint> {
        let guard = read_lock(&self.logpoints);
        guard.get(session_id)
            .map(|lps| lps.values().cloned().collect())
            .unwrap_or_default()
    }

    // Pause state management
    pub fn add_paused_thread(&self, session_id: &str, thread_id: u64, info: PauseInfo) {
        let mut guard = write_lock(&self.paused_threads);
        guard.entry(session_id.to_string())
            .or_insert_with(HashMap::new)
            .insert(thread_id, info);
    }

    pub fn remove_paused_thread(&self, session_id: &str, thread_id: u64) {
        let mut guard = write_lock(&self.paused_threads);
        if let Some(session_threads) = guard.get_mut(session_id) {
            session_threads.remove(&thread_id);
        }
    }

    pub fn is_thread_paused(&self, session_id: &str, thread_id: u64) -> bool {
        let guard = read_lock(&self.paused_threads);
        guard.get(session_id)
            .and_then(|threads| threads.get(&thread_id))
            .is_some()
    }

    pub fn get_pause_info(&self, session_id: &str, thread_id: u64) -> Option<PauseInfo> {
        let guard = read_lock(&self.paused_threads);
        guard.get(session_id)
            .and_then(|threads| threads.get(&thread_id))
            .cloned()
    }

    pub fn get_all_paused_threads(&self, session_id: &str) -> HashMap<u64, PauseInfo> {
        let guard = read_lock(&self.paused_threads);
        guard.get(session_id)
            .cloned()
            .unwrap_or_default()
    }
}

// ========== Phase 2: Breakpoint types ==========

#[derive(Debug, Clone)]
pub struct Breakpoint {
    pub id: String,
    pub target: BreakpointTarget,
    pub address: u64,
    pub condition: Option<String>,
    pub hit_count: u32,
    pub hits: u32,
}

#[derive(Debug, Clone)]
pub enum BreakpointTarget {
    Function(String),
    Line { file: String, line: u32 },
}

#[derive(Debug, Clone)]
pub struct Logpoint {
    pub id: String,
    pub target: BreakpointTarget, // Reuse same target enum
    pub address: u64,
    pub message: String,
    pub condition: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PauseInfo {
    pub breakpoint_id: String,
    pub func_name: Option<String>,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub paused_at: Instant,
    pub return_address: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_breakpoint_state_management() {
        let temp_dir = std::env::temp_dir();
        let db_path = temp_dir.join("strobe_test_bp.db");
        let _ = std::fs::remove_file(&db_path); // Clean start

        let sm = SessionManager::new(&db_path).unwrap();

        let session_id = "test-bp";
        let bp = Breakpoint {
            id: "bp1".to_string(),
            target: BreakpointTarget::Line {
                file: "main.cpp".to_string(),
                line: 42,
            },
            address: 0x1000,
            condition: None,
            hit_count: 0,
            hits: 0,
        };

        // Add breakpoint
        sm.add_breakpoint(session_id, bp.clone());

        // Retrieve breakpoint
        let breakpoints = sm.get_breakpoints(session_id);
        assert_eq!(breakpoints.len(), 1);
        assert_eq!(breakpoints[0].id, "bp1");

        // Remove breakpoint
        sm.remove_breakpoint(session_id, "bp1");
        let breakpoints = sm.get_breakpoints(session_id);
        assert_eq!(breakpoints.len(), 0);

        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn test_pause_state_management() {
        let temp_dir = std::env::temp_dir();
        let db_path = temp_dir.join("strobe_test_pause.db");
        let _ = std::fs::remove_file(&db_path);

        let sm = SessionManager::new(&db_path).unwrap();

        let session_id = "test-pause";
        let thread_id = 1234u64;
        let pause_info = PauseInfo {
            breakpoint_id: "bp1".to_string(),
            func_name: Some("foo".to_string()),
            file: Some("main.cpp".to_string()),
            line: Some(42),
            paused_at: Instant::now(),
            return_address: Some(0x1234),
        };

        // Add paused thread
        sm.add_paused_thread(session_id, thread_id, pause_info.clone());

        // Check if paused
        assert!(sm.is_thread_paused(session_id, thread_id));

        // Get pause info
        let info = sm.get_pause_info(session_id, thread_id);
        assert!(info.is_some());
        assert_eq!(info.unwrap().breakpoint_id, "bp1");

        // Resume thread
        sm.remove_paused_thread(session_id, thread_id);
        assert!(!sm.is_thread_paused(session_id, thread_id));

        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn test_logpoint_state_management() {
        let temp_dir = std::env::temp_dir();
        let db_path = temp_dir.join("strobe_test_lp.db");
        let _ = std::fs::remove_file(&db_path);

        let sm = SessionManager::new(&db_path).unwrap();

        let session_id = "test-lp";
        let lp = Logpoint {
            id: "lp1".to_string(),
            target: BreakpointTarget::Function("foo".to_string()),
            address: 0x2000,
            message: "hit: {args[0]}".to_string(),
            condition: None,
        };

        // Add logpoint
        sm.add_logpoint(session_id, lp);

        // Retrieve
        let logpoints = sm.get_logpoints(session_id);
        assert_eq!(logpoints.len(), 1);
        assert_eq!(logpoints[0].id, "lp1");
        assert_eq!(logpoints[0].message, "hit: {args[0]}");

        // Remove
        sm.remove_logpoint(session_id, "lp1");
        let logpoints = sm.get_logpoints(session_id);
        assert_eq!(logpoints.len(), 0);

        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn test_pause_with_return_address() {
        let temp_dir = std::env::temp_dir();
        let db_path = temp_dir.join("strobe_test_pause_ret.db");
        let _ = std::fs::remove_file(&db_path);

        let sm = SessionManager::new(&db_path).unwrap();

        let session_id = "test-ret";
        let pause_info = PauseInfo {
            breakpoint_id: "bp1".to_string(),
            func_name: Some("inner_func".to_string()),
            file: Some("lib.cpp".to_string()),
            line: Some(100),
            paused_at: Instant::now(),
            return_address: Some(0xdeadbeef),
        };

        sm.add_paused_thread(session_id, 99, pause_info);

        let info = sm.get_pause_info(session_id, 99).unwrap();
        assert_eq!(info.return_address, Some(0xdeadbeef));
        assert_eq!(info.func_name, Some("inner_func".to_string()));

        let all_paused = sm.get_all_paused_threads(session_id);
        assert_eq!(all_paused.len(), 1);
        assert!(all_paused.contains_key(&99));

        let _ = std::fs::remove_file(&db_path);
    }
}

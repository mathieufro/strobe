use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::RwLock;
use chrono::{Utc, Timelike};
use tokio::sync::mpsc;
use crate::db::{Database, Session, SessionStatus, Event};
use crate::dwarf::{DwarfParser, DwarfHandle};
use crate::frida_collector::{FridaSpawner, HookResult};
use crate::Result;

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
        ).await
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
            spawner.remove_patterns(session_id, patterns).await?;
        }

        Ok(HookResult { installed: 0, matched: 0, warnings: vec![] })
    }

    /// Update Frida watches
    pub async fn update_frida_watches(
        &self,
        session_id: &str,
        watches: Vec<crate::frida_collector::WatchTarget>,
    ) -> Result<()> {
        let mut guard = self.frida_spawner.write().await;
        let spawner = match guard.as_mut() {
            Some(s) => s,
            None => return Ok(()),
        };

        spawner.set_watches(session_id, watches).await
    }

    /// Send a read_memory command to the Frida agent and return the response.
    pub async fn read_memory(
        &self,
        session_id: &str,
        recipes_json: String,
    ) -> Result<serde_json::Value> {
        let guard = self.frida_spawner.read().await;
        let spawner = guard.as_ref()
            .ok_or_else(|| crate::Error::Frida("No Frida spawner available".to_string()))?;

        spawner.read_memory(session_id, recipes_json).await
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
}

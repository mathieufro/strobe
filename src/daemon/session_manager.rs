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

/// Maximum events to keep per session. Oldest events are deleted when limit is reached.
/// This prevents unbounded database growth and maintains system stability.
///
/// Default: 200,000 events (~2 seconds of 48kHz audio tracing, or several minutes of normal tracing)
/// Can be overridden via STROBE_MAX_EVENTS_PER_SESSION environment variable.
///
/// Performance characteristics (from stress testing):
/// - 200k: Query <10ms, Cleanup ~94ms, DB ~56MB
/// - 500k: Query ~28ms, Cleanup ~200ms, DB ~140MB (use for extended audio debugging)
/// - 1M+: Queries become slow (>300ms), cleanup expensive (>700ms)
pub const DEFAULT_MAX_EVENTS_PER_SESSION: usize = 200_000;

fn get_max_events_per_session() -> usize {
    std::env::var("STROBE_MAX_EVENTS_PER_SESSION")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MAX_EVENTS_PER_SESSION)
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
        // Check for existing active session on this binary
        if let Some(existing) = self.db.get_session_by_binary(binary_path)? {
            if existing.status == SessionStatus::Running {
                // Verify the old process is actually alive before blocking
                let pid_alive = unsafe { libc::kill(existing.pid as i32, 0) } == 0;
                if pid_alive {
                    return Err(crate::Error::SessionExists);
                }
                // Stale session — process is dead, clean it up
                tracing::warn!("Session {} has dead PID {}, marking as stopped", existing.id, existing.pid);
                self.db.update_session_status(&existing.id, SessionStatus::Stopped)?;
            }
        }

        let session = self.db.create_session(id, binary_path, project_root, pid)?;

        // Initialize pattern storage, watches, and event limit
        self.patterns.write().unwrap().insert(id.to_string(), Vec::new());
        self.hook_counts.write().unwrap().insert(id.to_string(), 0);
        self.watches.write().unwrap().insert(id.to_string(), Vec::new());
        self.event_limits.write().unwrap().insert(id.to_string(), get_max_events_per_session());

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
        self.db.delete_session(id)?;

        // Clean up in-memory state
        self.patterns.write().unwrap().remove(id);
        self.hook_counts.write().unwrap().remove(id);
        self.watches.write().unwrap().remove(id);
        self.event_limits.write().unwrap().remove(id);

        Ok(count)
    }

    pub fn add_patterns(&self, session_id: &str, patterns: &[String]) -> Result<()> {
        let mut all_patterns = self.patterns.write().unwrap();
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
        let mut all_patterns = self.patterns.write().unwrap();
        if let Some(session_patterns) = all_patterns.get_mut(session_id) {
            session_patterns.retain(|p| !patterns.contains(p));
        }
        Ok(())
    }

    pub fn get_patterns(&self, session_id: &str) -> Vec<String> {
        self.patterns
            .read()
            .unwrap()
            .get(session_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn set_hook_count(&self, session_id: &str, count: u32) {
        self.hook_counts
            .write()
            .unwrap()
            .insert(session_id.to_string(), count);
    }

    pub fn get_hook_count(&self, session_id: &str) -> u32 {
        self.hook_counts
            .read()
            .unwrap()
            .get(session_id)
            .copied()
            .unwrap_or(0)
    }

    pub fn set_event_limit(&self, session_id: &str, limit: usize) {
        self.event_limits
            .write()
            .unwrap()
            .insert(session_id.to_string(), limit);
    }

    pub fn get_event_limit(&self, session_id: &str) -> usize {
        self.event_limits
            .read()
            .unwrap()
            .get(session_id)
            .copied()
            .unwrap_or(DEFAULT_MAX_EVENTS_PER_SESSION)
    }

    /// Get or start a background DWARF parse. Returns a handle immediately.
    /// If the binary was already parsed (or is being parsed), returns the cached handle.
    pub fn get_or_start_dwarf_parse(&self, binary_path: &str) -> DwarfHandle {
        // Check cache first
        {
            let cache = self.dwarf_cache.read().unwrap();
            if let Some(handle) = cache.get(binary_path) {
                return handle.clone();
            }
        }

        // Start background parse and cache the handle
        let handle = DwarfHandle::spawn_parse(binary_path);
        self.dwarf_cache
            .write()
            .unwrap()
            .insert(binary_path.to_string(), handle.clone());

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
        tokio::spawn(async move {
            let mut batch = Vec::with_capacity(100);
            let mut cached_limit = DEFAULT_MAX_EVENTS_PER_SESSION;
            let mut batches_since_refresh = 0;

            loop {
                tokio::select! {
                    Some(event) = rx.recv() => {
                        batch.push(event);

                        if batch.len() >= 100 {
                            // Refresh cached limit every 10 batches to reduce lock contention
                            if batches_since_refresh >= 10 {
                                let session_id = &batch[0].session_id;
                                cached_limit = event_limits.read().unwrap()
                                    .get(session_id)
                                    .copied()
                                    .unwrap_or(DEFAULT_MAX_EVENTS_PER_SESSION);
                                batches_since_refresh = 0;
                            }
                            batches_since_refresh += 1;
                            let max_events = cached_limit;

                            match db.insert_events_with_limit(&batch, max_events) {
                                Ok(stats) => {
                                    if stats.events_deleted > 0 {
                                        tracing::warn!(
                                            "Event limit cleanup: deleted {} old events from {} session(s) to stay within {} event limit",
                                            stats.events_deleted,
                                            stats.sessions_cleaned.len(),
                                            max_events
                                        );
                                    }
                                }
                                Err(e) => {
                                    tracing::error!("Failed to insert events: {}", e);
                                }
                            }
                            batch.clear();
                        }
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {
                        if !batch.is_empty() {
                            // Refresh cached limit every 10 batches to reduce lock contention
                            if batches_since_refresh >= 10 {
                                let session_id = &batch[0].session_id;
                                cached_limit = event_limits.read().unwrap()
                                    .get(session_id)
                                    .copied()
                                    .unwrap_or(DEFAULT_MAX_EVENTS_PER_SESSION);
                                batches_since_refresh = 0;
                            }
                            batches_since_refresh += 1;
                            let max_events = cached_limit;

                            match db.insert_events_with_limit(&batch, max_events) {
                                Ok(stats) => {
                                    if stats.events_deleted > 0 {
                                        tracing::warn!(
                                            "Event limit cleanup: deleted {} old events from {} session(s) to stay within {} event limit",
                                            stats.events_deleted,
                                            stats.sessions_cleaned.len(),
                                            max_events
                                        );
                                    }
                                }
                                Err(e) => {
                                    tracing::error!("Failed to insert events: {}", e);
                                }
                            }
                            batch.clear();
                        }
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
            .unwrap()
            .insert(session_id.to_string(), watches);
    }

    /// Remove watches by label, returning the remaining watches
    pub fn remove_watches(&self, session_id: &str, labels: &[String]) -> Vec<ActiveWatchState> {
        let mut watches_map = self.watches.write().unwrap();
        if let Some(watches) = watches_map.get_mut(session_id) {
            watches.retain(|w| !labels.contains(&w.label));
            watches.clone()
        } else {
            vec![]
        }
    }

    /// Get active watches for a session
    pub fn get_watches(&self, session_id: &str) -> Vec<ActiveWatchState> {
        self.watches
            .read()
            .unwrap()
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
}

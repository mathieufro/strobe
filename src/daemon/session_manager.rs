use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};
use chrono::{Utc, Timelike};
use tokio::sync::mpsc;
use crate::db::{Database, Session, SessionStatus, Event};
use crate::dwarf::DwarfParser;
use crate::frida_collector::FridaSpawner;
use crate::Result;

pub struct SessionManager {
    db: Database,
    /// Active trace patterns per session
    patterns: Arc<RwLock<HashMap<String, Vec<String>>>>,
    /// Cached DWARF parsers per binary
    dwarf_cache: Arc<RwLock<HashMap<String, Arc<DwarfParser>>>>,
    /// Hooked function count per session
    hook_counts: Arc<RwLock<HashMap<String, u32>>>,
    /// Frida spawner for managing instrumented processes
    frida_spawner: Arc<tokio::sync::RwLock<FridaSpawner>>,
}

impl SessionManager {
    pub fn new(db_path: &Path) -> Result<Self> {
        let db = Database::open(db_path)?;

        Ok(Self {
            db,
            patterns: Arc::new(RwLock::new(HashMap::new())),
            dwarf_cache: Arc::new(RwLock::new(HashMap::new())),
            hook_counts: Arc::new(RwLock::new(HashMap::new())),
            frida_spawner: Arc::new(tokio::sync::RwLock::new(FridaSpawner::new())),
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
                return Err(crate::Error::SessionExists);
            }
        }

        let session = self.db.create_session(id, binary_path, project_root, pid)?;

        // Initialize pattern storage
        self.patterns.write().unwrap().insert(id.to_string(), Vec::new());
        self.hook_counts.write().unwrap().insert(id.to_string(), 0);

        Ok(session)
    }

    pub fn get_session(&self, id: &str) -> Result<Option<Session>> {
        self.db.get_session(id)
    }

    pub fn stop_session(&self, id: &str) -> Result<u64> {
        let count = self.db.count_session_events(id)?;
        self.db.delete_session(id)?;

        // Clean up in-memory state
        self.patterns.write().unwrap().remove(id);
        self.hook_counts.write().unwrap().remove(id);

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

    pub fn get_or_parse_dwarf(&self, binary_path: &str) -> Result<Arc<DwarfParser>> {
        // Check cache first
        {
            let cache = self.dwarf_cache.read().unwrap();
            if let Some(parser) = cache.get(binary_path) {
                return Ok(Arc::clone(parser));
            }
        }

        // Parse and cache
        let parser = Arc::new(DwarfParser::parse(Path::new(binary_path))?);
        self.dwarf_cache
            .write()
            .unwrap()
            .insert(binary_path.to_string(), Arc::clone(&parser));

        Ok(parser)
    }

    pub fn db(&self) -> &Database {
        &self.db
    }

    /// Spawn a process with Frida attached
    pub async fn spawn_with_frida(
        &self,
        session_id: &str,
        command: &str,
        args: &[String],
        cwd: Option<&str>,
        project_root: &str,
        env: Option<&std::collections::HashMap<String, String>>,
        initial_patterns: &[String],
    ) -> Result<u32> {
        // Create event channel
        let (tx, mut rx) = mpsc::channel::<Event>(10000);

        // Spawn database writer task
        let db = self.db.clone();
        let _session_id_clone = session_id.to_string();
        tokio::spawn(async move {
            let mut batch = Vec::with_capacity(100);

            loop {
                tokio::select! {
                    Some(event) = rx.recv() => {
                        batch.push(event);

                        if batch.len() >= 100 {
                            if let Err(e) = db.insert_events_batch(&batch) {
                                tracing::error!("Failed to insert events: {}", e);
                            }
                            batch.clear();
                        }
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {
                        if !batch.is_empty() {
                            if let Err(e) = db.insert_events_batch(&batch) {
                                tracing::error!("Failed to insert events: {}", e);
                            }
                            batch.clear();
                        }
                    }
                }
            }
        });

        // Spawn process with initial patterns
        let mut spawner = self.frida_spawner.write().await;
        spawner.spawn(
            session_id,
            command,
            args,
            cwd,
            project_root,
            env,
            initial_patterns,
            tx,
        ).await
    }

    /// Update Frida trace patterns
    pub async fn update_frida_patterns(
        &self,
        session_id: &str,
        add: Option<&[String]>,
        remove: Option<&[String]>,
    ) -> Result<u32> {
        let mut spawner = self.frida_spawner.write().await;

        if let Some(patterns) = add {
            return spawner.add_patterns(session_id, patterns).await;
        }

        if let Some(patterns) = remove {
            spawner.remove_patterns(session_id, patterns).await?;
        }

        Ok(0)
    }

    /// Stop Frida session
    pub async fn stop_frida(&self, session_id: &str) -> Result<()> {
        let mut spawner = self.frida_spawner.write().await;
        spawner.stop(session_id).await
    }
}

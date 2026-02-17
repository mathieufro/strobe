mod server;
mod session_manager;

pub use server::Daemon;
pub use session_manager::{SessionManager, ActiveWatchState, PauseInfo};

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_session_manager_create() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let manager = SessionManager::new(&db_path).unwrap();

        let session_id = manager.generate_session_id("myapp");
        assert!(session_id.starts_with("myapp-"));
    }

    #[tokio::test]
    async fn test_session_id_collision_handling() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let manager = SessionManager::new(&db_path).unwrap();

        // Create first session
        let id1 = manager.generate_session_id("myapp");
        manager.create_session(&id1, "/bin/myapp", "/home/user", 1234).unwrap();

        // Second session should get -2 suffix if same timestamp
        // (In practice timestamps differ, but the logic should handle it)
        let id2 = manager.generate_session_id("myapp");

        // IDs should be different
        assert_ne!(id1, id2);
    }
}

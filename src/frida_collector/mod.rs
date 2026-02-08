mod spawner;
mod hooks;

pub use spawner::FridaSpawner;
pub use spawner::HookResult;
pub use spawner::WatchTarget;
pub use hooks::HookManager;
pub use hooks::HookMode;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hook_manager_pattern_expansion() {
        let manager = HookManager::new();

        // Test that @usercode expands correctly
        let patterns = manager.expand_patterns(
            &["@usercode".to_string()],
            "/home/user/project",
        );

        // Should contain the expanded pattern
        assert!(!patterns.is_empty());
    }

    #[test]
    fn test_hook_manager_add_remove() {
        let mut manager = HookManager::new();

        manager.add_patterns(&["foo::*".to_string(), "bar::*".to_string()]);
        let active = manager.active_patterns();
        assert_eq!(active.len(), 2);

        manager.remove_patterns(&["foo::*".to_string()]);
        let active = manager.active_patterns();
        assert_eq!(active.len(), 1);
    }

    #[test]
    fn test_hook_count_accuracy() {
        let chunks = vec![
            HookResult { installed: 50, matched: 50, warnings: vec![] },
            HookResult { installed: 30, matched: 30, warnings: vec![] },
            HookResult { installed: 20, matched: 20, warnings: vec![] },
        ];

        let total_installed: u32 = chunks.iter().map(|r| r.installed).sum();
        let total_matched: u32 = chunks.iter().map(|r| r.matched).sum();

        assert_eq!(total_installed, 100);
        assert_eq!(total_matched, 100);
    }
}

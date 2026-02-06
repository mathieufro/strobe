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
}

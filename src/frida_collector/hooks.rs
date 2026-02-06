use std::collections::HashSet;
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HookMode {
    Full,   // enter + leave, no sampling
    Light,  // enter only, adaptive sampling
}

pub struct HookManager {
    active_patterns: HashSet<String>,
}

impl HookManager {
    pub fn new() -> Self {
        Self {
            active_patterns: HashSet::new(),
        }
    }

    pub fn expand_patterns(&self, patterns: &[String], project_root: &str) -> Vec<String> {
        patterns
            .iter()
            .map(|p| {
                if p == "@usercode" {
                    // Expand to match all functions in project root
                    format!("{}/**", project_root)
                } else {
                    p.clone()
                }
            })
            .collect()
    }

    pub fn add_patterns(&mut self, patterns: &[String]) {
        for p in patterns {
            self.active_patterns.insert(p.clone());
        }
    }

    pub fn remove_patterns(&mut self, patterns: &[String]) {
        for p in patterns {
            self.active_patterns.remove(p);
        }
    }

    pub fn active_patterns(&self) -> Vec<String> {
        self.active_patterns.iter().cloned().collect()
    }

    /// Classify a pattern's hook mode based on syntax.
    /// - Deep globs (**) -> Light
    /// - File patterns (@file:) -> Light
    /// - @usercode -> Light
    /// - Everything else (exact, single-glob) -> Full
    pub fn classify_pattern(pattern: &str) -> HookMode {
        if pattern.contains("**") {
            return HookMode::Light;
        }
        if pattern.starts_with("@file:") {
            return HookMode::Light;
        }
        if pattern == "@usercode" {
            return HookMode::Light;
        }
        HookMode::Full
    }

    /// Override: if a broad pattern resolved to very few functions (<=10), upgrade to Full.
    pub fn classify_with_count(pattern: &str, match_count: usize) -> HookMode {
        let mode = Self::classify_pattern(pattern);
        if mode == HookMode::Light && match_count <= 10 {
            return HookMode::Full;
        }
        mode
    }
}

impl Default for HookManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_exact_is_full() {
        assert_eq!(HookManager::classify_pattern("foo::bar"), HookMode::Full);
    }

    #[test]
    fn test_classify_single_glob_is_full() {
        assert_eq!(HookManager::classify_pattern("foo::*"), HookMode::Full);
    }

    #[test]
    fn test_classify_deep_glob_is_light() {
        assert_eq!(HookManager::classify_pattern("foo::**"), HookMode::Light);
    }

    #[test]
    fn test_classify_file_pattern_is_light() {
        assert_eq!(HookManager::classify_pattern("@file:layout_manager"), HookMode::Light);
    }

    #[test]
    fn test_classify_usercode_is_light() {
        assert_eq!(HookManager::classify_pattern("@usercode"), HookMode::Light);
    }

    #[test]
    fn test_classify_with_count_upgrades_small_match() {
        assert_eq!(HookManager::classify_with_count("@file:tiny", 5), HookMode::Full);
        assert_eq!(HookManager::classify_with_count("@file:tiny", 10), HookMode::Full);
    }

    #[test]
    fn test_classify_with_count_keeps_large_match_light() {
        assert_eq!(HookManager::classify_with_count("@file:big", 11), HookMode::Light);
        assert_eq!(HookManager::classify_with_count("@file:big", 100), HookMode::Light);
    }

    #[test]
    fn test_classify_with_count_full_stays_full() {
        assert_eq!(HookManager::classify_with_count("foo::bar", 1), HookMode::Full);
    }
}

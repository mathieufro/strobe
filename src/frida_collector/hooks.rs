use std::collections::HashSet;

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
}

impl Default for HookManager {
    fn default() -> Self {
        Self::new()
    }
}

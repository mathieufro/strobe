use serde::Deserialize;
use std::path::Path;

pub const MAX_EVENT_LIMIT: usize = 10_000_000;

/// All configurable settings with their defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct StrobeSettings {
    pub events_max_per_session: usize,
    pub test_status_retry_ms: u64,
}

impl Default for StrobeSettings {
    fn default() -> Self {
        Self {
            events_max_per_session: 200_000,
            test_status_retry_ms: 5_000,
        }
    }
}

/// Raw JSON representation — all fields optional for partial overrides.
#[derive(Debug, Deserialize, Default)]
struct SettingsFile {
    #[serde(rename = "events.maxPerSession")]
    events_max_per_session: Option<usize>,
    #[serde(rename = "test.statusRetryMs")]
    test_status_retry_ms: Option<u64>,
}

/// Resolve settings: defaults → user global → project-local.
pub fn resolve(project_root: Option<&Path>) -> StrobeSettings {
    let global_path = dirs::home_dir()
        .map(|h| h.join(".strobe/settings.json"));
    let project_path = project_root
        .map(|r| r.join(".strobe/settings.json"));
    resolve_with_paths(
        global_path.as_deref(),
        project_path.as_deref(),
    )
}

/// Testable resolver that accepts explicit file paths (no home dir dependency).
fn resolve_with_paths(
    global_path: Option<&Path>,
    project_path: Option<&Path>,
) -> StrobeSettings {
    let mut settings = StrobeSettings::default();

    if let Some(path) = global_path {
        apply_file(&mut settings, path);
    }
    if let Some(path) = project_path {
        apply_file(&mut settings, path);
    }

    settings
}

fn apply_file(settings: &mut StrobeSettings, path: &Path) {
    let Ok(content) = std::fs::read_to_string(path) else { return };
    let Ok(file) = serde_json::from_str::<SettingsFile>(&content) else {
        tracing::warn!("Invalid settings file, ignoring: {}", path.display());
        return;
    };
    if let Some(v) = file.events_max_per_session {
        if v > 0 && v <= MAX_EVENT_LIMIT {
            settings.events_max_per_session = v;
        } else {
            tracing::warn!(
                "events.maxPerSession ({}) out of range (1..{}), using default",
                v, MAX_EVENT_LIMIT
            );
        }
    }
    if let Some(v) = file.test_status_retry_ms {
        if v >= 500 && v <= 60_000 {
            settings.test_status_retry_ms = v;
        } else {
            tracing::warn!(
                "test.statusRetryMs ({}) out of range (500..60000), using default",
                v
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_defaults_when_no_files_exist() {
        let settings = resolve_with_paths(None, None);
        assert_eq!(settings.events_max_per_session, 200_000);
        assert_eq!(settings.test_status_retry_ms, 5_000);
    }

    #[test]
    fn test_global_overrides_defaults() {
        let dir = tempdir().unwrap();
        let global = dir.path().join("global.json");
        std::fs::write(&global, r#"{"events.maxPerSession": 500000}"#).unwrap();

        let settings = resolve_with_paths(Some(&global), None);
        assert_eq!(settings.events_max_per_session, 500_000);
        assert_eq!(settings.test_status_retry_ms, 5_000); // unchanged
    }

    #[test]
    fn test_project_overrides_global() {
        let dir = tempdir().unwrap();
        let global = dir.path().join("global.json");
        let project = dir.path().join("project.json");
        std::fs::write(&global, r#"{"events.maxPerSession": 500000, "test.statusRetryMs": 3000}"#).unwrap();
        std::fs::write(&project, r#"{"events.maxPerSession": 1000000}"#).unwrap();

        let settings = resolve_with_paths(Some(&global), Some(&project));
        assert_eq!(settings.events_max_per_session, 1_000_000); // project wins
        assert_eq!(settings.test_status_retry_ms, 3_000); // global applies (project didn't set)
    }

    #[test]
    fn test_invalid_json_ignored() {
        let dir = tempdir().unwrap();
        let bad_file = dir.path().join("bad.json");
        std::fs::write(&bad_file, "not json {{{").unwrap();

        let settings = resolve_with_paths(Some(&bad_file), None);
        assert_eq!(settings, StrobeSettings::default());
    }

    #[test]
    fn test_missing_file_ignored() {
        let settings = resolve_with_paths(
            Some(Path::new("/nonexistent/settings.json")),
            None,
        );
        assert_eq!(settings, StrobeSettings::default());
    }

    #[test]
    fn test_unknown_keys_ignored() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("settings.json");
        std::fs::write(&file, r#"{"events.maxPerSession": 300000, "unknown.key": true}"#).unwrap();

        let settings = resolve_with_paths(Some(&file), None);
        assert_eq!(settings.events_max_per_session, 300_000);
    }

    #[test]
    fn test_out_of_range_events_uses_default() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("settings.json");
        // Zero is out of range
        std::fs::write(&file, r#"{"events.maxPerSession": 0}"#).unwrap();
        let settings = resolve_with_paths(Some(&file), None);
        assert_eq!(settings.events_max_per_session, 200_000);

        // Over 10M is out of range
        std::fs::write(&file, r#"{"events.maxPerSession": 99999999}"#).unwrap();
        let settings = resolve_with_paths(Some(&file), None);
        assert_eq!(settings.events_max_per_session, 200_000);
    }

    #[test]
    fn test_out_of_range_retry_uses_default() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("settings.json");
        // 100ms is below minimum
        std::fs::write(&file, r#"{"test.statusRetryMs": 100}"#).unwrap();
        let settings = resolve_with_paths(Some(&file), None);
        assert_eq!(settings.test_status_retry_ms, 5_000);

        // 120000ms is above maximum
        std::fs::write(&file, r#"{"test.statusRetryMs": 120000}"#).unwrap();
        let settings = resolve_with_paths(Some(&file), None);
        assert_eq!(settings.test_status_retry_ms, 5_000);
    }

    #[test]
    fn test_partial_override_preserves_other_defaults() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("settings.json");
        std::fs::write(&file, r#"{"test.statusRetryMs": 2000}"#).unwrap();

        let settings = resolve_with_paths(Some(&file), None);
        assert_eq!(settings.events_max_per_session, 200_000); // default preserved
        assert_eq!(settings.test_status_retry_ms, 2_000); // overridden
    }
}

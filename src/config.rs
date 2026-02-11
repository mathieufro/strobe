use serde::Deserialize;
use std::path::Path;

pub const MAX_EVENT_LIMIT: usize = 10_000_000;

/// All configurable settings with their defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct StrobeSettings {
    pub events_max_per_session: usize,
    pub test_status_retry_ms: u64,
    pub vision_enabled: bool,
    pub vision_confidence_threshold: f32,
    pub vision_iou_merge_threshold: f32,
    pub vision_sidecar_idle_timeout_seconds: u64,
}

impl Default for StrobeSettings {
    fn default() -> Self {
        Self {
            events_max_per_session: 200_000,
            test_status_retry_ms: 5_000,
            vision_enabled: false,
            vision_confidence_threshold: 0.3,
            vision_iou_merge_threshold: 0.5,
            vision_sidecar_idle_timeout_seconds: 300,
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
    #[serde(rename = "vision.enabled")]
    vision_enabled: Option<bool>,
    #[serde(rename = "vision.confidenceThreshold")]
    vision_confidence_threshold: Option<f32>,
    #[serde(rename = "vision.iouMergeThreshold")]
    vision_iou_merge_threshold: Option<f32>,
    #[serde(rename = "vision.sidecarIdleTimeoutSeconds")]
    vision_sidecar_idle_timeout_seconds: Option<u64>,
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
    if let Some(v) = file.vision_enabled {
        settings.vision_enabled = v;
    }
    if let Some(v) = file.vision_confidence_threshold {
        if v > 0.0 && v <= 1.0 {
            settings.vision_confidence_threshold = v;
        } else {
            tracing::warn!(
                "vision.confidenceThreshold ({}) out of range (0.0..1.0), using default",
                v
            );
        }
    }
    if let Some(v) = file.vision_iou_merge_threshold {
        if v > 0.0 && v <= 1.0 {
            settings.vision_iou_merge_threshold = v;
        } else {
            tracing::warn!(
                "vision.iouMergeThreshold ({}) out of range (0.0..1.0), using default",
                v
            );
        }
    }
    if let Some(v) = file.vision_sidecar_idle_timeout_seconds {
        if v >= 30 && v <= 3600 {
            settings.vision_sidecar_idle_timeout_seconds = v;
        } else {
            tracing::warn!(
                "vision.sidecarIdleTimeoutSeconds ({}) out of range (30..3600), using default",
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
        // Below minimum (500)
        std::fs::write(&file, r#"{"test.statusRetryMs": 100}"#).unwrap();
        let settings = resolve_with_paths(Some(&file), None);
        assert_eq!(settings.test_status_retry_ms, 5_000);

        // Above maximum (60000)
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
        assert_eq!(settings.test_status_retry_ms, 2_000);
        assert_eq!(settings.events_max_per_session, 200_000); // unchanged
    }

    #[test]
    fn test_vision_defaults() {
        let settings = StrobeSettings::default();
        assert_eq!(settings.vision_enabled, false);
        assert_eq!(settings.vision_confidence_threshold, 0.3);
        assert_eq!(settings.vision_iou_merge_threshold, 0.5);
        assert_eq!(settings.vision_sidecar_idle_timeout_seconds, 300);
    }

    #[test]
    fn test_vision_config_overrides() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("settings.json");
        std::fs::write(&file, r#"{
            "vision.enabled": true,
            "vision.confidenceThreshold": 0.5,
            "vision.iouMergeThreshold": 0.7,
            "vision.sidecarIdleTimeoutSeconds": 600
        }"#).unwrap();

        let settings = resolve_with_paths(Some(&file), None);
        assert_eq!(settings.vision_enabled, true);
        assert_eq!(settings.vision_confidence_threshold, 0.5);
        assert_eq!(settings.vision_iou_merge_threshold, 0.7);
        assert_eq!(settings.vision_sidecar_idle_timeout_seconds, 600);
    }

    #[test]
    fn test_vision_threshold_out_of_range() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("settings.json");

        // Confidence threshold too high
        std::fs::write(&file, r#"{"vision.confidenceThreshold": 1.5}"#).unwrap();
        let settings = resolve_with_paths(Some(&file), None);
        assert_eq!(settings.vision_confidence_threshold, 0.3); // default

        // IOU threshold too low
        std::fs::write(&file, r#"{"vision.iouMergeThreshold": 0.0}"#).unwrap();
        let settings = resolve_with_paths(Some(&file), None);
        assert_eq!(settings.vision_iou_merge_threshold, 0.5); // default
    }

    #[test]
    fn test_vision_idle_timeout_out_of_range() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("settings.json");

        // Too short
        std::fs::write(&file, r#"{"vision.sidecarIdleTimeoutSeconds": 10}"#).unwrap();
        let settings = resolve_with_paths(Some(&file), None);
        assert_eq!(settings.vision_sidecar_idle_timeout_seconds, 300); // default

        // Too long
        std::fs::write(&file, r#"{"vision.sidecarIdleTimeoutSeconds": 5000}"#).unwrap();
        let settings = resolve_with_paths(Some(&file), None);
        assert_eq!(settings.vision_sidecar_idle_timeout_seconds, 300); // default
    }

}

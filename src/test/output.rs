use std::path::PathBuf;
use crate::test::adapter::TestResult;

/// Write full test details to a temp file. Returns the file path.
pub fn write_details(
    framework: &str,
    result: &TestResult,
    raw_stdout: &str,
    raw_stderr: &str,
) -> crate::Result<String> {
    let dir = PathBuf::from("/tmp/strobe/tests");
    std::fs::create_dir_all(&dir)?;

    let session_id = uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("unknown").to_string();
    let date = chrono::Utc::now().format("%Y-%m-%d");
    let filename = format!("{}-{}.json", session_id, date);
    let path = dir.join(&filename);

    let details = serde_json::json!({
        "framework": framework,
        "summary": result.summary,
        "tests": result.all_tests,
        "failures": result.failures,
        "stuck": result.stuck,
        "rawStdout": raw_stdout,
        "rawStderr": raw_stderr,
    });

    std::fs::write(&path, serde_json::to_string_pretty(&details)?)?;

    Ok(path.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::adapter::*;

    #[test]
    fn test_write_details_file() {
        let result = TestResult {
            summary: TestSummary {
                passed: 1, failed: 0, skipped: 0, stuck: None, duration_ms: 100,
            },
            failures: vec![],
            stuck: vec![],
            all_tests: vec![TestDetail {
                name: "test_foo".to_string(),
                status: TestStatus::Pass,
                duration_ms: 100,
                stdout: None,
                stderr: None,
                message: None,
            }],
        };

        let path = write_details("cargo", &result, "", "").unwrap();
        assert!(std::path::Path::new(&path).exists());

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("test_foo"));

        let _ = std::fs::remove_file(&path);
    }
}

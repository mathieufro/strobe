use std::collections::HashMap;
use std::path::Path;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TestLevel {
    Unit,
    Integration,
    E2e,
}

#[derive(Debug, Clone)]
pub struct TestCommand {
    pub program: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestSummary {
    pub passed: u32,
    pub failed: u32,
    pub skipped: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stuck: Option<u32>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestFailure {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rerun: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub suggested_traces: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StuckTest {
    pub name: String,
    pub elapsed_ms: u64,
    pub diagnosis: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub threads: Vec<ThreadStack>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub suggested_traces: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadStack {
    pub name: String,
    pub stack: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub summary: TestSummary,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub failures: Vec<TestFailure>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub stuck: Vec<StuckTest>,
    /// Per-test detail for the details file
    #[serde(skip)]
    pub all_tests: Vec<TestDetail>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TestStatus {
    Pass,
    Fail,
    Skip,
    Stuck,
}

impl TestStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TestStatus::Pass => "pass",
            TestStatus::Fail => "fail",
            TestStatus::Skip => "skip",
            TestStatus::Stuck => "stuck",
        }
    }
}

impl std::fmt::Display for TestStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestDetail {
    pub name: String,
    pub status: TestStatus,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectInfo {
    pub language: String,
    pub build_system: String,
    pub test_files: u32,
}

pub trait TestAdapter: Send + Sync {
    /// Scan projectRoot for signals. Returns 0-100 confidence. Highest wins.
    fn detect(&self, project_root: &Path, command: Option<&str>) -> u8;

    /// Human-readable name: "cargo", "catch2"
    fn name(&self) -> &str;

    /// Build command for running tests at a given level. None = all.
    fn suite_command(
        &self,
        project_root: &Path,
        level: Option<TestLevel>,
        env: &HashMap<String, String>,
    ) -> crate::Result<TestCommand>;

    /// Build command for running a single test by name.
    fn single_test_command(
        &self,
        project_root: &Path,
        test_name: &str,
    ) -> crate::Result<TestCommand>;

    /// Parse raw stdout + stderr into structured results.
    fn parse_output(
        &self,
        stdout: &str,
        stderr: &str,
        exit_code: i32,
    ) -> TestResult;

    /// Given a failure, suggest trace patterns for instrumented rerun.
    fn suggest_traces(&self, failure: &TestFailure) -> Vec<String>;

    /// Capture thread stacks for stuck detection. Language-aware.
    /// Default implementation uses OS-level native stack capture.
    fn capture_stacks(&self, pid: u32) -> Vec<ThreadStack> {
        super::stacks::capture_native_stacks(pid)
    }

    /// Build command for a user-provided binary path. Default: error.
    /// Override for binary-based adapters (Catch2, GTest).
    fn command_for_binary(
        &self,
        _cmd: &str,
        _level: Option<TestLevel>,
    ) -> crate::Result<TestCommand> {
        Err(crate::Error::ValidationError(
            format!("{} does not support direct binary execution", self.name())
        ))
    }

    /// Build command for running a single test on a user-provided binary.
    fn single_test_for_binary(
        &self,
        _cmd: &str,
        _test_name: &str,
    ) -> crate::Result<TestCommand> {
        Err(crate::Error::ValidationError(
            format!("{} does not support direct binary execution", self.name())
        ))
    }

    /// Safety-net timeout — per-test tracking via stuck detector is the primary mechanism.
    /// This only fires if something goes catastrophically wrong.
    fn default_timeout(&self, _level: Option<TestLevel>) -> u64 {
        600_000 // 10 minutes
    }
}

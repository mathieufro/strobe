//! NDJSON event stream for live test progress.
//!
//! Each event is one JSON object on its own line, flushed/synced immediately so
//! a concurrent reader (`tail -f`) sees events as they're emitted. The schema is
//! `serde(tag = "type", rename_all = "snake_case")`, so each line carries a
//! `"type": "..."` discriminator plus per-variant fields.

use serde::Serialize;
use std::fs::File;
use std::io::Write;
use std::path::Path;

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TestEvent {
    RunStarted {
        run_id: String,
        project_root: String,
        suite: String,
        framework: String,
        mode: String,
        started_at_ms: u64,
    },
    TestStarted {
        test_id: String,
        file: String,
        line: u32,
    },
    TestPassed {
        test_id: String,
        file: String,
        line: u32,
        duration_ms: u64,
        artifact_dir: String,
    },
    TestFailed {
        test_id: String,
        file: String,
        line: u32,
        duration_ms: u64,
        error: FailureDetail,
        artifact_dir: String,
    },
    TestStalled {
        test_id: String,
        file: String,
        elapsed_ms: u64,
        median_ms: u64,
        threshold_ms: u64,
        artifact_dir: String,
    },
    TestSkipped {
        test_id: String,
        file: String,
        reason: String,
    },
    RunComplete {
        run_id: String,
        exit_code: i32,
        total: u32,
        passed: u32,
        failed: u32,
        stalled: u32,
        skipped: u32,
        duration_ms: u64,
    },
}

#[derive(Serialize)]
pub struct FailureDetail {
    pub message: String,
    pub kind: String,
    pub stack_frames: Vec<StackFrame>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
    pub code_context: Vec<String>,
}

#[derive(Serialize)]
pub struct StackFrame {
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub function: String,
}

pub struct EventStream {
    file: File,
}

impl EventStream {
    pub fn create(path: &Path) -> std::io::Result<Self> {
        Ok(Self {
            file: File::create(path)?,
        })
    }

    pub fn emit(&mut self, event: TestEvent) -> std::io::Result<()> {
        let mut json = serde_json::to_string(&event)?;
        json.push('\n');
        self.file.write_all(json.as_bytes())?;
        // sync_data (cheaper than sync_all) keeps the data visible to a concurrent
        // `tail -f` immediately, without forcing metadata fsync.
        self.file.sync_data()?;
        Ok(())
    }
}

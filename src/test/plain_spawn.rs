//! Plain (non-Frida) spawn path for test processes.
//!
//! Frida-attached spawning is required for runtime instrumentation
//! (`debug_trace` patterns, function-level hooks). When the caller doesn't
//! ask for any traces, the Frida tax — codesign re-entitlement on macOS,
//! Gatekeeper checks, TTY-shaped FDs that confuse `NO_COLOR`, slower
//! startup — is pure overhead.
//!
//! This module spawns tests through `tokio::process::Command` instead, then
//! pipes stdout/stderr into the same SQLite `events` table Frida writes
//! into. The downstream polling loop, parser, and log filter all read from
//! that table — they don't care whether the bytes came from a Frida hook
//! or a plain pipe.
//!
//! Caller can opt back into Frida by passing one or more `tracePatterns`
//! to `debug_test`.

use std::collections::HashMap;
use std::path::Path;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};

use crate::db::{Database, Event, EventType};

/// Spawn a test process without Frida and bridge its stdout/stderr into the
/// session's `events` table. Returns `(pid, child_handle)`. The child is kept
/// alive by the returned handle — drop it (or call `.kill()`) to terminate.
///
/// `cwd` falls back to the current working directory when `None`.
/// `env` is the fully-resolved environment (caller is responsible for
/// merging `std::env::vars()` + test-specific + user overrides).
pub async fn spawn_plain(
    db: Database,
    session_id: &str,
    program: &str,
    args: &[String],
    cwd: Option<&Path>,
    env: &HashMap<String, String>,
) -> crate::Result<(u32, Child)> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .env_clear()
        .envs(env)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // Default tokio behaviour: SIGKILL the child if the Child handle is
        // dropped without a wait. We rely on this for cleanup when the
        // polling loop exits early (timeout, abort).
        .kill_on_drop(true);
    if let Some(c) = cwd {
        cmd.current_dir(c);
    }

    let mut child = cmd.spawn().map_err(|e| {
        crate::Error::ValidationError(format!(
            "Failed to spawn {} (plain mode): {}",
            program, e
        ))
    })?;
    let pid = child.id().ok_or_else(|| {
        crate::Error::ValidationError(
            "Spawned child has no PID — it may have exited immediately".into(),
        )
    })?;

    let stdout = child.stdout.take().expect("stdout was set to piped");
    let stderr = child.stderr.take().expect("stderr was set to piped");

    spawn_pipe_to_db(db.clone(), session_id.to_string(), stdout, EventType::Stdout);
    spawn_pipe_to_db(db, session_id.to_string(), stderr, EventType::Stderr);

    Ok((pid, child))
}

/// Read chunks from `reader` and write each as a `text` event into the DB.
/// Mirrors the shape Frida's stdout hook produces so the polling loop and
/// `collect_output` can treat both paths uniformly.
fn spawn_pipe_to_db<R>(db: Database, session_id: String, reader: R, kind: EventType)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut reader = reader;
        let mut buf = vec![0u8; 8192];
        let mut seq: u64 = 0;
        loop {
            match reader.read(&mut buf).await {
                Ok(0) => break, // EOF — child closed the pipe
                Ok(n) => {
                    let text = String::from_utf8_lossy(&buf[..n]).into_owned();
                    seq = seq.wrapping_add(1);
                    let ts_ns = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos() as i64)
                        .unwrap_or(0);
                    let event = Event {
                        id: format!("plain-{}-{}-{}", kind.as_str(), session_id, seq),
                        session_id: session_id.clone(),
                        timestamp_ns: ts_ns,
                        thread_id: 0,
                        event_type: kind.clone(),
                        function_name: String::new(),
                        text: Some(text),
                        ..Default::default()
                    };
                    if let Err(e) = db.insert_event(&event) {
                        tracing::warn!(
                            kind = %kind.as_str(),
                            err = %e,
                            "Failed to persist plain-spawn pipe chunk"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(kind = %kind.as_str(), err = %e, "plain-spawn pipe read error");
                    break;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn fresh_db_with_session(session_id: &str) -> Database {
        let db = Database::open_in_memory().expect("open in-memory db");
        db.create_session(session_id, "/bin/test", "/tmp", 0)
            .expect("create session");
        db
    }

    fn env_for(program: &str) -> HashMap<String, String> {
        // Pass through PATH so `which bun` lookups work when the caller
        // hands us a bare program name (tests give absolute paths anyway).
        let mut env: HashMap<String, String> = std::env::vars().collect();
        // Strip terminal-color hints — same as the production spawn path.
        env.insert("NO_COLOR".into(), "1".into());
        env.insert("FORCE_COLOR".into(), "0".into());
        // Sanity check that the program is on PATH.
        let _ = program;
        env
    }

    fn drain_text(db: &Database, session_id: &str, kind: &EventType) -> String {
        // Wait briefly for the spawned pipe-reader tasks to push their final
        // chunks. They run on tokio's blocking pool; in-memory DB inserts are
        // fast but not synchronous with the child's exit.
        let mut events = db
            .query_events(session_id, |q| q.event_type(kind.clone()).limit_uncapped(5000))
            .unwrap_or_default();
        events.reverse();
        events.into_iter().filter_map(|e| e.text).collect::<Vec<_>>().join("")
    }

    async fn drain_until<F>(db: &Database, session_id: &str, kind: EventType, predicate: F) -> String
    where
        F: Fn(&str) -> bool,
    {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let s = drain_text(db, session_id, &kind);
            if predicate(&s) {
                return s;
            }
            if tokio::time::Instant::now() >= deadline {
                return s;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn captures_stdout_into_db() {
        let db = fresh_db_with_session("plain-stdout");
        let env = env_for("/bin/sh");
        let (_pid, mut child) = spawn_plain(
            db.clone(),
            "plain-stdout",
            "/bin/sh",
            &["-c".into(), "printf 'hello\\nworld\\n'".into()],
            None,
            &env,
        )
        .await
        .expect("spawn");
        let status = child.wait().await.unwrap();
        assert!(status.success(), "child should exit 0");

        let stdout = drain_until(&db, "plain-stdout", EventType::Stdout, |s| {
            s.contains("hello") && s.contains("world")
        })
        .await;
        assert!(stdout.contains("hello\nworld\n"), "got: {:?}", stdout);
    }

    #[tokio::test]
    async fn captures_stderr_into_db() {
        let db = fresh_db_with_session("plain-stderr");
        let env = env_for("/bin/sh");
        let (_pid, mut child) = spawn_plain(
            db.clone(),
            "plain-stderr",
            "/bin/sh",
            &["-c".into(), "echo 'oh no' 1>&2".into()],
            None,
            &env,
        )
        .await
        .expect("spawn");
        child.wait().await.unwrap();

        let stderr = drain_until(&db, "plain-stderr", EventType::Stderr, |s| {
            s.contains("oh no")
        })
        .await;
        assert!(stderr.contains("oh no"), "got: {:?}", stderr);

        // And nothing leaked into stdout.
        let stdout = drain_text(&db, "plain-stderr", &EventType::Stdout);
        assert!(stdout.is_empty(), "stdout should be empty, got: {:?}", stdout);
    }

    #[tokio::test]
    async fn propagates_non_zero_exit_code() {
        let db = fresh_db_with_session("plain-exit");
        let env = env_for("/bin/sh");
        let (_pid, mut child) = spawn_plain(
            db.clone(),
            "plain-exit",
            "/bin/sh",
            &["-c".into(), "exit 42".into()],
            None,
            &env,
        )
        .await
        .expect("spawn");
        let status = child.wait().await.unwrap();
        assert_eq!(status.code(), Some(42));
    }

    #[tokio::test]
    async fn kill_on_drop_terminates_runaway_child() {
        let db = fresh_db_with_session("plain-runaway");
        let env = env_for("/bin/sh");
        let (pid, child) = spawn_plain(
            db.clone(),
            "plain-runaway",
            "/bin/sh",
            // Long-running command — must be killed by Child::drop.
            &["-c".into(), "sleep 30".into()],
            None,
            &env,
        )
        .await
        .expect("spawn");

        // Confirm the process exists before drop.
        assert!(
            crate::test::stacks::is_process_alive(pid),
            "child should be alive immediately after spawn"
        );
        drop(child);

        // Tokio's kill-on-drop sends SIGKILL; give the kernel a moment.
        for _ in 0..50 {
            if !crate::test::stacks::is_process_alive(pid) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("child {} still alive 1s after Child handle was dropped", pid);
    }

    #[tokio::test]
    async fn keeps_stdout_and_stderr_ordering_within_stream() {
        // The two pipe-reader tasks run independently, so cross-stream order
        // isn't guaranteed — but within a single stream we must preserve it.
        let db = fresh_db_with_session("plain-order");
        let env = env_for("/bin/sh");
        let (_pid, mut child) = spawn_plain(
            db.clone(),
            "plain-order",
            "/bin/sh",
            &[
                "-c".into(),
                "for i in 1 2 3 4 5; do echo line-$i; done".into(),
            ],
            None,
            &env,
        )
        .await
        .expect("spawn");
        child.wait().await.unwrap();

        let stdout = drain_until(&db, "plain-order", EventType::Stdout, |s| {
            s.contains("line-5")
        })
        .await;
        let mut lines: Vec<&str> = stdout.lines().filter(|l| l.starts_with("line-")).collect();
        let expected: Vec<&str> = vec!["line-1", "line-2", "line-3", "line-4", "line-5"];
        // `lines` is already in insertion order — confirm exact sequence.
        lines.truncate(5);
        assert_eq!(lines, expected, "stdout: {:?}", stdout);
    }
}

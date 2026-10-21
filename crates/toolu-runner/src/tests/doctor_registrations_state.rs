//! Tests for the `doctor` rows read from files a running job writes: run state
//! from a lock body (live, reused, dead and mid-write PIDs) and the last-job
//! row from journals cut from the committed real capture. The credentials /
//! JIT / config rows live in `doctor_registrations.rs`.

use std::path::Path;
use std::time::Duration;

use config::lockfile::{LockBody, is_pid_alive, pid_start_time};

use super::{RunState, holder_alive, last_job_row, run_row, run_state};
use crate::doctor_report::{Level, Row};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const CONFIG: &str = "/h/runners/o/r/config.toml";
const CANONICAL: &str = include_str!("../../tests/fixtures/journal/canonical.jsonl");

/// A PID that belonged to a real process which has exited and been reaped.
fn dead_pid() -> Result<u32, Box<dyn std::error::Error>> {
  for _ in 0..5 {
    let mut child = std::process::Command::new("true").spawn()?;
    let pid = child.id();
    child.wait()?;
    if !is_pid_alive(pid) {
      return Ok(pid);
    }
  }
  Err("every reaped PID was reused at once".into())
}

fn lock_body(pid: u32, started_at: String) -> LockBody {
  LockBody {
    pid,
    started_at,
    config_path: CONFIG.to_owned(),
  }
}

/// RFC 3339 for `secs` since the Unix epoch.
fn rfc3339_at(secs: i64) -> Result<String, Box<dyn std::error::Error>> {
  Ok(
    chrono::DateTime::from_timestamp(secs, 0)
      .ok_or("timestamp out of range")?
      .to_rfc3339(),
  )
}

/// The captured journal without the lines of the given event `kind`.
fn canonical_without(kind: &str) -> String {
  let marker = format!("\"type\":\"{kind}\"");
  let mut kept = CANONICAL
    .lines()
    .filter(|line| !line.contains(&marker))
    .collect::<Vec<_>>()
    .join("\n");
  kept.push('\n');
  kept
}

#[test]
fn a_live_holder_is_active_and_a_stopped_persistent_runner_fails() {
  let active = RunState::Active {
    pid: 4242,
    since: "2026-10-19T10:00:00Z".to_owned(),
  };
  for ephemeral in [false, true] {
    assert_eq!(
      run_row(&active, ephemeral, Path::new(CONFIG)),
      Row::new(
        Level::Ok,
        "run",
        "active (pid 4242, since 2026-10-19T10:00:00Z)"
      ),
      "ephemeral={ephemeral}"
    );
  }

  assert_eq!(
    run_row(&RunState::Stopped, false, Path::new(CONFIG)),
    Row::new(Level::Fail, "run", "not running")
      .with_fix("toolu-runner install-service --config /h/runners/o/r/config.toml")
  );
  assert_eq!(
    run_row(&RunState::Stopped, true, Path::new(CONFIG)),
    Row::new(Level::Warn, "run", "not running")
      .with_fix("toolu-runner run --config /h/runners/o/r/config.toml")
  );

  let unreadable = RunState::Unreadable("expected value".to_owned());
  assert_eq!(
    run_row(&unreadable, false, Path::new(CONFIG)),
    Row::new(
      Level::Fail,
      "run",
      "lock body unreadable (expected value); treated as not running"
    )
    .with_fix("toolu-runner install-service --config /h/runners/o/r/config.toml")
  );
  assert_eq!(
    run_row(&unreadable, true, Path::new(CONFIG)),
    Row::new(
      Level::Warn,
      "run",
      "lock body unreadable (expected value); treated as not running"
    )
    .with_fix("toolu-runner run --config /h/runners/o/r/config.toml")
  );
}

#[tokio::test]
async fn run_state_reads_the_lock_body_without_locking() -> TestResult {
  let dir = tempfile::tempdir()?;
  let lock = dir.path().join(".lock");
  assert_eq!(run_state(&lock).await, RunState::Stopped);

  let now = chrono::Utc::now().to_rfc3339();
  let me = lock_body(std::process::id(), now.clone());
  std::fs::write(&lock, serde_json::to_string(&me)?)?;
  assert_eq!(
    run_state(&lock).await,
    RunState::Active {
      pid: std::process::id(),
      since: now
    }
  );

  // A body that stays empty past the settle delay is a real truncated file.
  std::fs::write(&lock, "")?;
  assert_eq!(
    run_state(&lock).await,
    RunState::Unreadable("EOF while parsing a value at line 1 column 0".to_owned())
  );
  Ok(())
}

#[tokio::test]
async fn an_empty_lock_body_is_re_read_once_the_writer_finishes() -> TestResult {
  let dir = tempfile::tempdir()?;
  let lock = dir.path().join(".lock");
  std::fs::write(&lock, "")?;

  let now = chrono::Utc::now().to_rfc3339();
  let body = serde_json::to_string(&lock_body(std::process::id(), now.clone()))?;
  let writer_target = lock.clone();
  let writer = tokio::spawn(async move {
    tokio::time::sleep(Duration::from_millis(20)).await;
    std::fs::write(writer_target, body)
  });

  let state = run_state(&lock).await;
  writer.await??;
  assert_eq!(
    state,
    RunState::Active {
      pid: std::process::id(),
      since: now
    }
  );
  Ok(())
}

#[tokio::test]
async fn run_state_distinguishes_dead_reused_garbage_and_unreadable_bodies() -> TestResult {
  let dir = tempfile::tempdir()?;
  let lock = dir.path().join(".lock");
  let now = chrono::Utc::now();

  // The holder exited: its PID names no process.
  let gone = lock_body(dead_pid()?, now.to_rfc3339());
  std::fs::write(&lock, serde_json::to_string(&gone)?)?;
  assert_eq!(run_state(&lock).await, RunState::Stopped);

  // The PID is alive, but it started after the body was written: reused.
  let reused = lock_body(
    std::process::id(),
    (now - chrono::Duration::hours(2)).to_rfc3339(),
  );
  std::fs::write(&lock, serde_json::to_string(&reused)?)?;
  assert_eq!(run_state(&lock).await, RunState::Stopped);

  // Not a lock body at all.
  std::fs::write(&lock, "this is not json")?;
  assert_eq!(
    run_state(&lock).await,
    RunState::Unreadable("expected ident at line 1 column 2".to_owned())
  );

  // Unreadable for a reason other than absence: the path is a directory.
  let as_dir = dir.path().join("lock-dir");
  std::fs::create_dir(&as_dir)?;
  let io = std::fs::read_to_string(&as_dir)
    .err()
    .ok_or("reading a directory succeeded")?;
  assert_eq!(
    run_state(&as_dir).await,
    RunState::Unreadable(io.to_string())
  );
  Ok(())
}

#[test]
fn holder_alive_needs_a_live_pid_that_started_no_later_than_the_body() -> TestResult {
  let me = std::process::id();
  let now = chrono::Utc::now();

  assert!(holder_alive(&lock_body(me, now.to_rfc3339())));
  // PID now names a process that started after the body was written.
  assert!(!holder_alive(&lock_body(
    me,
    (now - chrono::Duration::hours(2)).to_rfc3339()
  )));
  // A body written long after the process started is fine.
  assert!(holder_alive(&lock_body(
    me,
    (now + chrono::Duration::days(1)).to_rfc3339()
  )));
  // An unparseable `started_at` falls back to liveness alone.
  assert!(holder_alive(&lock_body(me, "garbage".to_owned())));
  // No process holds the PID.
  assert!(!holder_alive(&lock_body(dead_pid()?, now.to_rfc3339())));
  assert!(!holder_alive(&lock_body(0, now.to_rfc3339())));
  Ok(())
}

#[test]
fn the_start_time_slack_is_exactly_sixty_seconds() -> TestResult {
  let me = std::process::id();
  let started = i64::try_from(pid_start_time(me).ok_or("this process has no start time")?)?;
  // Written 60 s before the process started: still within the slack.
  assert!(holder_alive(&lock_body(me, rfc3339_at(started - 60)?)));
  // 61 s before: the PID was reused.
  assert!(!holder_alive(&lock_body(me, rfc3339_at(started - 61)?)));
  Ok(())
}

#[test]
fn the_last_job_row_names_the_conclusion_job_and_start() -> TestResult {
  let dir = tempfile::tempdir()?;
  let jobs = dir.path().join("_diag").join("jobs");
  assert_eq!(
    last_job_row(&jobs),
    Row::new(Level::Info, "last job", "no jobs yet")
  );

  std::fs::create_dir_all(&jobs)?;
  assert_eq!(
    last_job_row(&jobs),
    Row::new(Level::Info, "last job", "no jobs yet"),
    "an existing, empty jobs dir"
  );

  std::fs::write(jobs.join("2026-10-11T02-01-30Z-0e9d8c7b.jsonl"), CANONICAL)?;
  assert_eq!(
    last_job_row(&jobs),
    Row::new(
      Level::Info,
      "last job",
      "success — build at 2026-10-11T02:01:30.612Z"
    )
  );
  Ok(())
}

#[test]
fn a_journal_without_a_completion_is_running_or_interrupted() -> TestResult {
  let dir = tempfile::tempdir()?;
  std::fs::write(
    dir.path().join("2026-10-11T02-01-30Z-0e9d8c7b.jsonl"),
    canonical_without("job_completed"),
  )?;
  assert_eq!(
    last_job_row(dir.path()),
    Row::new(
      Level::Info,
      "last job",
      "running or interrupted — build at 2026-10-11T02:01:30.612Z"
    )
  );
  Ok(())
}

#[test]
fn a_journal_without_a_job_name_falls_back_to_the_job_id() -> TestResult {
  let dir = tempfile::tempdir()?;
  std::fs::write(
    dir.path().join("2026-10-11T02-01-30Z-0e9d8c7b.jsonl"),
    canonical_without("job_started"),
  )?;
  assert_eq!(
    last_job_row(dir.path()),
    Row::new(
      Level::Info,
      "last job",
      "success — 0e9d8c7b-3333-4444-8555-666677778888 at 2026-10-11T02:01:30.612Z"
    )
  );
  Ok(())
}

#[test]
fn the_newest_journal_by_name_wins_and_headless_ones_are_skipped() -> TestResult {
  let dir = tempfile::tempdir()?;
  std::fs::write(
    dir.path().join("2026-10-11T02-01-30Z-aaaa.jsonl"),
    CANONICAL,
  )?;
  std::fs::write(
    dir.path().join("2026-10-12T09-00-00Z-bbbb.jsonl"),
    canonical_without("job_completed"),
  )?;
  assert_eq!(
    last_job_row(dir.path()).detail,
    "running or interrupted — build at 2026-10-11T02:01:30.612Z"
  );

  // A journal that never reached `job_acquired` has no job to report.
  let headless = tempfile::tempdir()?;
  let first = CANONICAL.lines().next().ok_or("empty capture")?;
  std::fs::write(
    headless.path().join("2026-10-12T09-00-00Z-cccc.jsonl"),
    format!("{first}\n"),
  )?;
  assert_eq!(
    last_job_row(headless.path()),
    Row::new(Level::Info, "last job", "no jobs yet")
  );
  Ok(())
}

#[test]
fn an_unreadable_jobs_dir_warns_with_the_io_error() -> TestResult {
  let dir = tempfile::tempdir()?;
  let not_a_dir = dir.path().join("jobs");
  std::fs::write(&not_a_dir, "a regular file where the jobs dir should be")?;
  let io = std::fs::read_dir(&not_a_dir)
    .err()
    .ok_or("reading a file as a directory succeeded")?;
  assert_eq!(
    last_job_row(&not_a_dir),
    Row::new(
      Level::Warn,
      "last job",
      format!("cannot read {}: {io}", not_a_dir.display())
    )
  );
  Ok(())
}

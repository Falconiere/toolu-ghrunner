//! Tests for the `doctor` registration rows: credentials-mode classification,
//! run state from a lock body, and the last-job row read from the committed
//! real journal capture.

use std::path::Path;

use config::lockfile::LockBody;

use super::{RunState, creds_mode_row, last_job_row, run_row, run_state};
use crate::doctor_report::Level;

const CONFIG: &str = "/h/runners/o/r/config.toml";

#[test]
fn owner_only_credentials_pass_and_wider_modes_warn_with_chmod() {
  let path = Path::new("/h/runners/o/r/credentials.json");
  let tight = creds_mode_row(path, 0o100_600);
  assert_eq!(
    (tight.level, tight.detail.as_str()),
    (Level::Ok, "present (0600)")
  );
  for mode in [0o100_644, 0o100_640, 0o100_604] {
    let loose = creds_mode_row(path, mode);
    assert_eq!(loose.level, Level::Warn, "{mode:o}");
    assert!(
      loose.detail.contains("beyond the owner"),
      "{}",
      loose.detail
    );
    assert_eq!(
      loose.fix.as_deref(),
      Some("chmod 600 /h/runners/o/r/credentials.json")
    );
  }
}

#[test]
fn a_live_holder_is_active_and_a_stopped_persistent_runner_fails() {
  let active = RunState::Active {
    pid: 4242,
    since: "2026-10-19T10:00:00Z".to_owned(),
  };
  let row = run_row(&active, false, Path::new(CONFIG));
  assert_eq!(
    (row.level, row.detail.as_str()),
    (Level::Ok, "active (pid 4242, since 2026-10-19T10:00:00Z)")
  );

  let stopped = run_row(&RunState::Stopped, false, Path::new(CONFIG));
  assert_eq!(
    (stopped.level, stopped.detail.as_str()),
    (Level::Fail, "not running")
  );
  assert_eq!(
    stopped.fix.as_deref(),
    Some("toolu-runner install-service --config /h/runners/o/r/config.toml")
  );

  let ephemeral = run_row(&RunState::Stopped, true, Path::new(CONFIG));
  assert_eq!(ephemeral.level, Level::Warn);
  assert_eq!(
    ephemeral.fix.as_deref(),
    Some("toolu-runner run --config /h/runners/o/r/config.toml")
  );

  let unreadable = run_row(
    &RunState::Unreadable("expected value".to_owned()),
    false,
    Path::new(CONFIG),
  );
  assert_eq!(unreadable.level, Level::Fail);
  assert!(
    unreadable.detail.contains("lock body unreadable"),
    "{}",
    unreadable.detail
  );
}

#[test]
fn run_state_reads_the_lock_body_without_locking() -> Result<(), Box<dyn std::error::Error>> {
  let dir = tempfile::tempdir()?;
  let lock = dir.path().join(".lock");
  assert_eq!(run_state(&lock), RunState::Stopped);

  let me = LockBody {
    pid: std::process::id(),
    started_at: "2026-10-19T10:00:00Z".to_owned(),
    config_path: CONFIG.to_owned(),
  };
  std::fs::write(&lock, serde_json::to_string(&me)?)?;
  assert_eq!(
    run_state(&lock),
    RunState::Active {
      pid: std::process::id(),
      since: "2026-10-19T10:00:00Z".to_owned()
    }
  );

  std::fs::write(&lock, "")?;
  assert!(matches!(run_state(&lock), RunState::Unreadable(_)));
  Ok(())
}

#[test]
fn the_newest_real_journal_names_its_conclusion_and_job() -> Result<(), Box<dyn std::error::Error>>
{
  let dir = tempfile::tempdir()?;
  let jobs = dir.path().join("_diag").join("jobs");
  assert_eq!(last_job_row(&jobs).detail, "no jobs yet");

  std::fs::create_dir_all(&jobs)?;
  std::fs::write(
    jobs.join("2026-10-11T02-01-30Z-0e9d8c7b.jsonl"),
    include_str!("../../tests/fixtures/journal/canonical.jsonl"),
  )?;
  let row = last_job_row(&jobs);
  assert_eq!(row.level, Level::Info);
  assert_eq!(row.detail, "success — build at 2026-10-11T02:01:30.612Z");
  Ok(())
}

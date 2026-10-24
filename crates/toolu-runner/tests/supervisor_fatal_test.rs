//! A fatal error in one registration under `run --all` stops only that
//! loop (#208), through the real binary against loopback peers.
//!
//! - AC-4: a re-mint GitHub rejects (the captured 401) stops its loop with
//!   the fatal message, recorded in `.stopped`; the other registration keeps
//!   polling; `doctor` FAILs it and `status` names it; SIGHUP restarts it.
//! - AC-5: a session conflict past its retry budget (the live 409 capture)
//!   stops only its loop, and the supervisor still exits 0 on SIGTERM.

use std::path::Path;

use config::stop_marker;

use crate::supervisor_fixtures::{
  Scope, TOKEN, TestResult, conflict_broker, count, idle_broker, job_broker_rejecting_remint,
  log_of, runner, signal, spawn_run_all, stop, wait_for, wait_until, write_registration,
};

/// The registration dir of `config`.
fn dir_of(config: &Path) -> &Path {
  config.parent().unwrap_or(config)
}

/// AC-4.
#[tokio::test]
async fn a_rejected_remint_stops_only_its_registration() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  let rejected = job_broker_rejecting_remint("true").await?;
  let idle = idle_broker().await?;
  let config_r = write_registration(
    home.path(),
    &rejected,
    Scope::Repo("octo", "rejected"),
    false,
  )?;
  write_registration(home.path(), &idle, Scope::Repo("octo", "idle"), false)?;

  let mut child = spawn_run_all(home.path(), true)?;
  wait_until("the rejected registration's .stopped", || {
    stop_marker::path(dir_of(&config_r)).exists()
  })
  .await?;
  let marker = stop_marker::read(dir_of(&config_r))?.ok_or("no marker")?;
  for needle in [
    "re-mint rejected",
    "TOOLU_RUNNER_TOKEN",
    "toolu-runner login",
  ] {
    assert!(
      marker.error.contains(needle),
      "{needle:?} in {:?}",
      marker.error
    );
  }
  assert!(
    !marker.error.contains(TOKEN),
    "the bearer never lands in .stopped"
  );
  let log = log_of(home.path(), "supervisor");
  assert!(log.contains("registration stopped"), "{log}");

  // The other registration is untouched and the process is still up.
  let before = count(&idle, "GET", "/message").await?;
  wait_for(&idle, "GET", "/message", before + 2).await?;
  assert_eq!(count(&idle, "POST", "/session").await?, 1);
  assert!(child.try_wait()?.is_none(), "the supervisor keeps running");

  let doctor = runner(home.path(), false)
    .args(["doctor", "--offline"])
    .output()?;
  let report = String::from_utf8_lossy(&doctor.stdout);
  assert_eq!(doctor.status.code(), Some(1), "{report}");
  let (_, block) = report
    .split_once("octo/rejected (persistent")
    .ok_or_else(|| format!("no block for the registration:\n{report}"))?;
  let run_line = block
    .lines()
    .find(|line| line.contains("run") && line.contains("stopped by the service"))
    .ok_or_else(|| format!("no stopped run row:\n{report}"))?;
  assert!(
    run_line.contains("FAIL") && run_line.contains("re-mint rejected"),
    "{run_line}"
  );
  assert!(
    block.contains(&format!("kill -HUP {}", child.id())),
    "the fix names the reload:\n{report}"
  );

  let status = runner(home.path(), false)
    .args(["status", "--config"])
    .arg(&config_r)
    .output()?;
  let status_out = String::from_utf8_lossy(&status.stdout);
  assert!(
    status_out.lines().any(|l| l.starts_with("run:")
      && l.contains("stopped by the service")
      && l.contains("re-mint rejected")),
    "{status_out}"
  );

  // A reload restarts it: the lock is retaken and the record cleared.
  let sessions = count(&rejected, "POST", "/session").await?;
  signal(&child, "-HUP")?;
  wait_for(&rejected, "POST", "/session", sessions + 1).await?;
  wait_until("the .stopped record to be cleared", || {
    !stop_marker::path(dir_of(&config_r)).exists()
  })
  .await?;
  stop(child, home.path(), "supervisor").await
}

/// AC-5.
#[tokio::test]
async fn a_session_conflict_stops_only_its_registration_and_never_exits_5() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  let conflicted = conflict_broker().await?;
  let idle = idle_broker().await?;
  let config_c = write_registration(
    home.path(),
    &conflicted,
    Scope::Repo("octo", "conflict"),
    false,
  )?;
  write_registration(home.path(), &idle, Scope::Repo("octo", "idle"), false)?;

  let child = spawn_run_all(home.path(), false)?;
  wait_until("the conflicted registration's .stopped", || {
    stop_marker::path(dir_of(&config_c)).exists()
  })
  .await?;
  let marker = stop_marker::read(dir_of(&config_c))?.ok_or("no marker")?;
  assert!(
    marker
      .error
      .contains("another runner session is active for this registration"),
    "{:?}",
    marker.error
  );
  assert_eq!(
    count(&conflicted, "POST", "/session").await?,
    8,
    "the bounded retry ran"
  );
  let before = count(&idle, "GET", "/message").await?;
  wait_for(&idle, "GET", "/message", before + 2).await?;
  stop(child, home.path(), "supervisor").await
}

/// A corrupt `config.toml` whose bad line holds a proxy password (unquoted,
/// so the TOML error would quote it): the loop stops, but neither `.stopped`
/// nor `doctor` / `status` ever show the password.
#[tokio::test]
async fn a_corrupt_config_never_leaks_its_source_line() -> TestResult<()> {
  const PASSWORD: &str = "pa55-208-secret";
  let home = tempfile::tempdir()?;
  let broken = idle_broker().await?;
  let idle = idle_broker().await?;
  let config = write_registration(home.path(), &broken, Scope::Repo("octo", "broken"), false)?;
  write_registration(home.path(), &idle, Scope::Repo("octo", "idle"), false)?;
  let text = format!(
    "{}\n[network]\nproxy = http://user:{PASSWORD}@proxy.corp:3128\n",
    std::fs::read_to_string(&config)?
  );
  std::fs::write(&config, text)?;

  let child = spawn_run_all(home.path(), false)?;
  wait_until("the broken registration's .stopped", || {
    stop_marker::path(dir_of(&config)).exists()
  })
  .await?;
  let marker = stop_marker::read(dir_of(&config))?.ok_or("no marker")?;
  assert!(!marker.error.contains(PASSWORD), "{:?}", marker.error);
  assert!(marker.error.contains("config.toml"), "{:?}", marker.error);
  wait_for(&idle, "GET", "/message", 1).await?;

  let doctor = runner(home.path(), false)
    .args(["doctor", "--offline"])
    .output()?;
  let status = runner(home.path(), false)
    .args(["status", "--config"])
    .arg(&config)
    .output()?;
  for out in [
    &doctor.stdout,
    &doctor.stderr,
    &status.stdout,
    &status.stderr,
  ] {
    assert!(!String::from_utf8_lossy(out).contains(PASSWORD));
  }
  assert!(!log_of(home.path(), "supervisor").contains(PASSWORD));
  stop(child, home.path(), "supervisor").await
}

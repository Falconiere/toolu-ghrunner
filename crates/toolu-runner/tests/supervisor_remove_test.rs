//! `remove` while `run --all` supervises the registration (#208, AC-9),
//! through the real binary against loopback peers. The machine unit file is
//! written by a real `install-service --no-activate`; only its deactivation
//! goes through a PATH-stub `systemctl` / `launchctl` (see the #208 design
//! for why a real user manager cannot be used here).
//!
//! - Removing one of two registrations drains only its loop: it is
//!   unregistered and deleted, the other keeps polling, the unit stays.
//! - Removing the last one also deactivates and deletes the machine unit.
//! - A registration running a job is not removed mid-job: `remove` exits 2,
//!   the job finishes `succeeded`, and a later `remove` completes.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Output;

use crate::install_service_test::{nonroot_shims, real_uid, shim_env, write_shim};
use crate::supervisor_fixtures::{
  RUNNER_ID, Scope, TestResult, count, idle_broker, job_broker, reported_conclusion, runner,
  spawn_run_all, stop, wait_for, wait_until, write_registration,
};

const MACOS: bool = cfg!(target_os = "macos");
const RECORD_OK: &str = "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$SHIM_LOG\"\nexit 0\n";

/// The machine unit file under `home`.
fn machine_dest(home: &Path) -> PathBuf {
  if MACOS {
    home.join("Library/LaunchAgents/sh.toolu.runner.plist")
  } else {
    home.join(".config/systemd/user/toolu-runner.service")
  }
}

/// Write the machine unit with the real CLI (nothing activated).
fn install_machine_unit(home: &Path) -> TestResult<()> {
  let mut cmd = runner(home, false);
  if !MACOS {
    cmd.env("PATH", nonroot_shims(home)?);
  }
  let out = cmd.args(["install-service", "--no-activate"]).output()?;
  if !out.status.success() {
    return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
  }
  Ok(())
}

/// `remove --config <config>` with the bearer and the stub supervisor CLI;
/// returns the output and the stub's argv log.
fn remove(home: &Path, config: &Path, extra_env: &[(&str, &str)]) -> TestResult<(Output, String)> {
  let (bin, log, path_env) = shim_env(home)?;
  if !MACOS {
    nonroot_shims(home)?;
  }
  write_shim(
    &bin,
    if MACOS { "launchctl" } else { "systemctl" },
    RECORD_OK,
  )?;
  let mut cmd = runner(home, true);
  cmd
    .args(["remove", "--config"])
    .arg(config)
    .env("PATH", &path_env)
    .env("SHIM_LOG", &log);
  for (key, value) in extra_env {
    cmd.env(key, value);
  }
  let out = cmd.output()?;
  Ok((out, std::fs::read_to_string(&log).unwrap_or_default()))
}

fn text(bytes: &[u8]) -> String {
  String::from_utf8_lossy(bytes).into_owned()
}

/// The runner API path of a fixture registration's DELETE.
fn unregister_path(repo: &str) -> String {
  format!("/api/v3/repos/octo/{repo}/actions/runners/{RUNNER_ID}")
}

#[tokio::test]
async fn removing_one_of_two_registrations_stops_only_its_loop() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  let (a, b) = (idle_broker().await?, idle_broker().await?);
  let config_a = write_registration(home.path(), &a, Scope::Repo("octo", "a"), false)?;
  write_registration(home.path(), &b, Scope::Repo("octo", "b"), false)?;
  install_machine_unit(home.path())?;
  let mut child = spawn_run_all(home.path(), false)?;
  wait_for(&a, "GET", "/message", 1).await?;
  wait_for(&b, "GET", "/message", 1).await?;

  let (out, calls) = remove(home.path(), &config_a, &[])?;
  let stdout = text(&out.stdout);
  assert_eq!(out.status.code(), Some(0), "{stdout}{}", text(&out.stderr));
  assert!(
    stdout.contains(&format!(
      "asked the toolu-runner service (pid {})",
      child.id()
    )),
    "{stdout}"
  );
  assert_eq!(count(&a, "DELETE", &unregister_path("a")).await?, 1);
  assert_eq!(
    count(&a, "DELETE", "/session/").await?,
    1,
    "the drained loop ended its session"
  );
  assert!(!config_a.exists());

  let before = count(&b, "GET", "/message").await?;
  wait_for(&b, "GET", "/message", before + 2).await?;
  assert_eq!(count(&b, "POST", "/session").await?, 1);
  assert!(
    machine_dest(home.path()).is_file(),
    "another registration remains"
  );
  assert_eq!(calls, "", "the service is left alone");
  assert!(child.try_wait()?.is_none());
  stop(child, home.path(), "supervisor").await
}

#[tokio::test]
async fn removing_the_last_registration_removes_the_service() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  let a = idle_broker().await?;
  let config_a = write_registration(home.path(), &a, Scope::Repo("octo", "a"), false)?;
  install_machine_unit(home.path())?;
  let child = spawn_run_all(home.path(), false)?;
  wait_for(&a, "GET", "/message", 1).await?;

  let (out, calls) = remove(home.path(), &config_a, &[])?;
  let stdout = text(&out.stdout);
  assert_eq!(out.status.code(), Some(0), "{stdout}{}", text(&out.stderr));
  assert!(!config_a.exists());
  let dest = machine_dest(home.path());
  assert!(!dest.exists(), "the machine unit is deleted");
  assert!(
    stdout.contains(&format!(
      "removed the toolu-runner service ({})",
      dest.display()
    )),
    "{stdout}"
  );
  let deactivated = if MACOS {
    format!("bootout gui/{}/sh.toolu.runner", real_uid()?)
  } else {
    "--user disable --now toolu-runner.service".to_owned()
  };
  assert!(calls.lines().any(|line| line == deactivated), "{calls}");
  stop(child, home.path(), "supervisor").await
}

#[tokio::test]
async fn a_registration_running_a_job_is_not_removed_mid_job() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  let busy = job_broker("sleep 6").await?;
  let config = write_registration(home.path(), &busy, Scope::Repo("octo", "busy"), false)?;
  install_machine_unit(home.path())?;
  let child = spawn_run_all(home.path(), true)?;
  wait_for(&busy, "POST", "/acquirejob", 1).await?;

  let (out, calls) = remove(
    home.path(),
    &config,
    &[("TOOLU_RUNNER_REMOVE_WAIT_MS", "500")],
  )?;
  let stderr = text(&out.stderr);
  assert_eq!(out.status.code(), Some(2), "{stderr}");
  assert!(
    stderr.contains("is running a job for this runner"),
    "{stderr}"
  );
  assert!(config.exists(), "nothing is removed mid-job");
  assert!(config.with_file_name(".pending_remove").exists());
  assert!(machine_dest(home.path()).is_file());
  assert_eq!(calls, "");
  assert_eq!(count(&busy, "DELETE", &unregister_path("busy")).await?, 0);

  // The drain never touches the job: it finishes, and the loop then stops
  // at the job boundary instead of re-minting.
  wait_for(&busy, "POST", "/completejob", 1).await?;
  assert_eq!(reported_conclusion(&busy).await?, "succeeded");
  let lock = config.with_file_name(".lock");
  wait_until("the loop to release the lock", || {
    std::fs::read_to_string(&lock).is_ok_and(|body| body.is_empty())
  })
  .await?;
  assert_eq!(
    count(
      &busy,
      "POST",
      "/api/v3/repos/octo/busy/actions/runners/generate-jitconfig"
    )
    .await?,
    0
  );

  let (again, _) = remove(home.path(), &config, &[])?;
  assert_eq!(again.status.code(), Some(0), "{}", text(&again.stderr));
  assert!(!config.exists());
  assert!(
    !machine_dest(home.path()).exists(),
    "it was the last registration"
  );
  stop(child, home.path(), "supervisor").await
}

/// An ephemeral registration whose job ends while `remove` waits removes
/// itself; `remove` reports it and exits 0 instead of failing on the
/// vanished config.
#[tokio::test]
async fn an_ephemeral_runner_finishing_during_the_wait_counts_as_removed() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  let busy = job_broker("sleep 3").await?;
  let config = write_registration(home.path(), &busy, Scope::Repo("octo", "eph"), true)?;
  let child = spawn_run_all(home.path(), true)?;
  wait_for(&busy, "POST", "/acquirejob", 1).await?;

  let (out, _) = remove(home.path(), &config, &[])?;
  let stdout = text(&out.stdout);
  assert_eq!(out.status.code(), Some(0), "{stdout}{}", text(&out.stderr));
  assert!(stdout.contains("already removed itself"), "{stdout}");
  assert!(!config.exists());
  assert!(
    !config.with_file_name(".lock").exists(),
    "no stray lock is left"
  );
  assert!(!config.with_file_name(".pending_remove").exists());
  assert_eq!(
    count(&busy, "DELETE", &unregister_path("eph")).await?,
    1,
    "unregistered once, by the runner"
  );
  assert_eq!(reported_conclusion(&busy).await?, "succeeded");
  stop(child, home.path(), "supervisor").await
}

/// `remove --force` mid-job drains the loop; registering the same repo again
/// before the job ends gets a successor loop that takes over once the old
/// one lets go — no reload needed.
#[tokio::test]
async fn a_registration_written_again_while_draining_resumes_after_the_job() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  let busy = job_broker("sleep 4").await?;
  let config = write_registration(home.path(), &busy, Scope::Repo("octo", "again"), false)?;
  let child = spawn_run_all(home.path(), true)?;
  wait_for(&busy, "POST", "/acquirejob", 1).await?;

  let forced = runner(home.path(), true)
    .args(["remove", "--force", "--config"])
    .arg(&config)
    .output()?;
  assert_eq!(forced.status.code(), Some(0), "{}", text(&forced.stderr));
  assert!(!config.exists());

  write_registration(home.path(), &busy, Scope::Repo("octo", "again"), false)?;
  crate::supervisor_fixtures::signal(&child, "-HUP")?;
  wait_for(&busy, "POST", "/completejob", 1).await?;
  assert_eq!(reported_conclusion(&busy).await?, "succeeded");
  wait_for(&busy, "POST", "/session", 2).await?;
  wait_until("the successor to hold the lock", || {
    crate::supervisor_fixtures::lock_pid(&config) == Some(child.id())
  })
  .await?;
  stop(child, home.path(), "supervisor").await
}

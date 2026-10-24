//! Finding the `run --all` supervisor of a runner home (#208).
//!
//! The supervisor holds an exclusive `fs2` lock on `<home>/supervisor.lock`
//! for its whole life, with its pid in the lock body. Other commands use
//! that file to reach it: `register` and `remove` send it `SIGHUP` so it
//! rescans the registrations without a restart.

use std::path::{Path, PathBuf};

use std::time::{Duration, Instant};

use config::lockfile::{self, LockBody, LockGuard};
use shared::RunnerError;

/// The supervisor lock's file name under the runner home.
pub(crate) const SUPERVISOR_LOCK: &str = "supervisor.lock";

/// `<home>/supervisor.lock`.
pub(crate) fn lock_path(home: &Path) -> PathBuf {
  home.join(SUPERVISOR_LOCK)
}

/// The pid of the live supervisor of `home`, if one runs: its lock is held
/// (a shared `flock` probe fails) and the body's pid is that live process
/// (`holder_alive`: alive and started before the body was written). A stale
/// file is never mistaken for a supervisor, so its pid is never signalled.
pub(crate) fn supervisor_pid(home: &Path) -> Option<u32> {
  let path = lock_path(home);
  let file = std::fs::File::open(&path).ok()?;
  if fs2::FileExt::try_lock_shared(&file).is_ok() {
    // Nobody holds it; dropping `file` releases the probe.
    return None;
  }
  let body: LockBody = serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
  crate::doctor_registrations::holder_alive(&body).then_some(body.pid)
}

/// The command that restarts a stopped registration's loop: SIGHUP to the
/// live supervisor, else starting the machine service.
pub(crate) fn reload_hint(home: &Path) -> String {
  supervisor_pid(home).map_or_else(
    || "toolu-runner install-service".to_owned(),
    |pid| format!("kill -HUP {pid}"),
  )
}

/// Send `SIGHUP` to the live supervisor of `home` so it rescans the
/// registrations now; `Ok(None)` when no supervisor runs.
///
/// # Errors
///
/// The `kill` subprocess could not run or failed (named in the message).
pub(crate) fn notify(home: &Path) -> Result<Option<u32>, String> {
  let Some(pid) = supervisor_pid(home) else {
    return Ok(None);
  };
  let status = std::process::Command::new("kill")
    .args(["-HUP", &pid.to_string()])
    .status()
    .map_err(|e| format!("cannot run `kill -HUP {pid}`: {e}"))?;
  if status.success() {
    Ok(Some(pid))
  } else {
    Err(format!("`kill -HUP {pid}` failed ({status})"))
  }
}

/// What came of asking the supervisor to let go of a registration.
pub(crate) enum Drain {
  /// The lock holder is not this home's supervisor (a foreground `run`, a
  /// per-registration unit): nothing was asked.
  NotSupervised,
  /// The supervisor drained the loop: the caller now holds the lock.
  Released(LockGuard),
  /// The loop did not let go in time (a job is in flight); it stops after it.
  StillBusy(u32),
}

/// `remove` on a registration whose lock `holder` holds: when that is the
/// live supervisor of `home`, SIGHUP it (its rescan drains the loop, since
/// `.pending_remove` exists) and wait up to `wait` for the lock.
pub(crate) async fn drain_and_wait(
  home: &Path,
  holder: u32,
  lock_path: &Path,
  config_path: &Path,
  wait: Duration,
) -> Drain {
  if supervisor_pid(home) != Some(holder) || notify(home).is_err() {
    return Drain::NotSupervised;
  }
  println!(
    "asked the toolu-runner service (pid {holder}) to stop this runner; waiting up to {} s …",
    wait.as_secs()
  );
  let deadline = Instant::now() + wait;
  loop {
    let (lock, config) = (lock_path.to_owned(), config_path.to_owned());
    match crate::supervisor::off_runtime(move || lockfile::acquire(&lock, &config)).await {
      Ok(Ok(guard)) => return Drain::Released(guard),
      Ok(Err(RunnerError::LockHeld { .. })) if Instant::now() < deadline => {
        tokio::time::sleep(Duration::from_millis(250)).await;
      },
      Ok(Err(_)) | Err(_) => return Drain::StillBusy(holder),
    }
  }
}

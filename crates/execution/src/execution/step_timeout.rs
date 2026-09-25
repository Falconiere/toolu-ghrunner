//! Bounded child-process wait shared by the script and node handlers.
//!
//! Wraps `child.wait()` so a step honors its `timeout-minutes` and the
//! in-flight `CancellationToken`: whichever fires first kills the child and
//! the caller reports the step as `failure` (timed out) / `cancelled`.

use std::future::Future;
use std::process::ExitStatus;
use std::time::Duration;
use tokio::time::Instant;

use shared::RunnerError;
use tokio::process::Child;
use tokio_util::sync::CancellationToken;

#[cfg(test)]
#[path = "tests/step_timeout.rs"]
mod tests;

/// Per-step run bounds: one absolute timeout deadline (if any) and the
/// in-flight job `CancellationToken`. Computed once per step and threaded to
/// every child-spawning handler so later children use only remaining time.
pub(crate) struct StepBounds {
  pub(crate) deadline: Option<Instant>,
  pub(crate) cancel: CancellationToken,
}

impl StepBounds {
  /// Keep a nested action inside its parent's absolute deadline.
  pub(crate) fn nested(
    parent_deadline: Option<Instant>,
    timeout_minutes: Option<u32>,
    cancel: CancellationToken,
  ) -> Self {
    let own_timeout = timeout_duration(timeout_minutes);
    let own_deadline = own_timeout.map(|duration| Instant::now() + duration);
    let deadline = match (parent_deadline, own_deadline) {
      (Some(parent), Some(child)) => Some(parent.min(child)),
      (Some(parent), None) => Some(parent),
      (None, child) => child,
    };
    Self { deadline, cancel }
  }

  /// Duration left before the fixed deadline, for the next child process.
  pub(crate) fn remaining_timeout(&self) -> Option<Duration> {
    self
      .deadline
      .map(|deadline| deadline.saturating_duration_since(Instant::now()))
  }

  /// Bound action resolution before a subprocess exists to receive the deadline.
  pub(crate) async fn resolve_within_bounds<T>(
    &self,
    resolution: impl Future<Output = Result<T, RunnerError>>,
  ) -> Result<T, RunnerError> {
    let deadline = async {
      match self.deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending::<()>().await,
      }
    };
    tokio::select! {
      result = resolution => result,
      () = self.cancel.cancelled() => Err(RunnerError::Cancelled),
      () = deadline => Err(RunnerError::StepExecution("step timed out while resolving action".to_owned())),
    }
  }
}

/// Outcome of a bounded child wait.
pub enum WaitOutcome {
  /// The process exited on its own with this status.
  Exited(ExitStatus),
  /// The `timeout-minutes` bound elapsed; the child was killed.
  TimedOut,
  /// The job-level `CancellationToken` fired; the child was killed.
  Cancelled,
}

/// Convert `timeout-minutes` (whole minutes, `0`/`None` = unbounded) to a
/// `Duration`. Returns `None` when no finite bound applies.
pub fn timeout_duration(minutes: Option<u32>) -> Option<Duration> {
  match minutes {
    Some(m) if m > 0 => Some(Duration::from_secs(u64::from(m) * 60)),
    _ => None,
  }
}

/// Wait for `child` to exit, bounded by `timeout` and `cancel`.
///
/// On timeout or cancellation the child is killed (best-effort, then reaped)
/// before returning the corresponding [`WaitOutcome`]. A `timeout` of `None`
/// means only `cancel` bounds the wait.
///
/// # Errors
///
/// Returns `RunnerError` (via `err`) if the underlying `wait()` fails.
pub async fn wait_bounded(
  child: &mut Child,
  timeout: Option<Duration>,
  cancel: &CancellationToken,
  err: impl Fn(String) -> RunnerError,
) -> Result<WaitOutcome, RunnerError> {
  let sleep = async {
    match timeout {
      Some(d) => tokio::time::sleep(d).await,
      // No finite bound: never completes, so only `wait`/`cancel` win.
      None => std::future::pending::<()>().await,
    }
  };

  tokio::select! {
    status = child.wait() => {
      let status = status.map_err(|e| err(format!("wait failed: {e}")))?;
      Ok(WaitOutcome::Exited(status))
    }
    () = sleep => {
      kill_and_reap(child, &err).await?;
      Ok(WaitOutcome::TimedOut)
    }
    () = cancel.cancelled() => {
      kill_and_reap(child, &err).await?;
      Ok(WaitOutcome::Cancelled)
    }
  }
}

/// Kill the child and reap it so no zombie is left behind.
///
/// The reap itself is bounded: a `SIGKILL`ed process normally exits
/// immediately, but one stuck in uninterruptible sleep (D state — NFS, dead
/// device) would block `wait()` forever and hang the job. After the grace
/// period the zombie is abandoned with a warning rather than wedging the
/// runner.
async fn kill_and_reap(
  child: &mut Child,
  err: &impl Fn(String) -> RunnerError,
) -> Result<(), RunnerError> {
  const REAP_GRACE: Duration = Duration::from_secs(10);
  // `start_kill` sends SIGKILL; an already-exited child yields an error we
  // can safely ignore, since the goal is just to ensure it is not running.
  kill_process_group(child.id());
  let _ = child.start_kill();
  match tokio::time::timeout(REAP_GRACE, child.wait()).await {
    Ok(status) => {
      status.map_err(|e| err(format!("reap after kill failed: {e}")))?;
    },
    Err(_elapsed) => {
      tracing::warn!(
        "child did not exit within {REAP_GRACE:?} of SIGKILL (uninterruptible \
         sleep?); abandoning reap"
      );
    },
  }
  Ok(())
}

/// Kill only the process group whose leader is the owned step child.
#[cfg(unix)]
fn kill_process_group(group_id: Option<u32>) {
  use nix::errno::Errno;
  use nix::sys::signal::{Signal, killpg};
  use nix::unistd::Pid;

  let Some(pid) = group_id.and_then(|id| i32::try_from(id).ok()) else {
    return;
  };
  match killpg(Pid::from_raw(pid), Signal::SIGKILL) {
    Ok(()) | Err(Errno::ESRCH) => {},
    Err(error) => tracing::warn!(pid, %error, "failed to kill owned step process group"),
  }
}

#[cfg(not(unix))]
fn kill_process_group(_group_id: Option<u32>) {}

//! Bounded child-process wait shared by the script and node handlers.
//!
//! Wraps `child.wait()` so a step honors its `timeout-minutes` and the
//! in-flight `CancellationToken`: whichever fires first kills the child and
//! the caller reports the step as `failure` (timed out) / `cancelled`.

use std::process::ExitStatus;
use std::time::Duration;

use shared::RunnerError;
use tokio::process::Child;
use tokio_util::sync::CancellationToken;

/// Per-step run bounds: the `timeout-minutes` duration (if any) and the
/// in-flight job `CancellationToken`. Computed once per step and threaded to
/// every child-spawning handler so timeouts and cancellation are honored.
pub(crate) struct StepBounds {
  pub(crate) timeout: Option<Duration>,
  pub(crate) cancel: CancellationToken,
}

impl StepBounds {
  /// Build bounds from a step's `timeout-minutes` and the job cancel token.
  pub(crate) fn new(timeout_minutes: Option<u32>, cancel: CancellationToken) -> Self {
    Self {
      timeout: timeout_duration(timeout_minutes),
      cancel,
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
/// The reap itself is bounded: a SIGKILLed process normally exits
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

//! Bounded retry for reporting calls that must survive a transient network
//! outage — see docs/toolu/specs/2026-08-06-outage-watchdog-design.md.
//!
//! [`retry_transient`] retries only `RunnerError::Network` (GitHub is
//! unreachable right now — a transport failure, 429, or 5xx per
//! `wire::net::run_service`'s classification); any other error is
//! definitive and propagates unchanged. Backs `complete_job`'s
//! report-on-reconnect path in `job_lifecycle.rs` (`poll_and_execute`),
//! which lives in a separate file so `job_lifecycle.rs` — already over the
//! crate's 500-line convention — does not grow.

use std::future::Future;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use shared::RunnerError;

/// Apply decorrelated jitter to a backoff duration so concurrent runners
/// don't synchronize their retries. Returns a duration in
/// `[backoff/2, backoff)` — half the current backoff plus a random offset.
///
/// `pub(crate)`: shared by the poll loop's backoff
/// (`job_lifecycle::backoff_after_poll_error`) and [`retry_transient`]'s
/// completion-report backoff — moved here (from `job_lifecycle.rs`, already
/// over the crate's 500-line convention) rather than duplicated.
pub(crate) fn jittered_backoff(d: Duration) -> Duration {
  // `as_millis()` is u128, `fastrand::u64` needs a u64 bound. Clamp rather
  // than bail on the (unreachable at a 60s cap) overflow: an absurd input
  // still gets jittered instead of silently returning the full backoff.
  let half_ms = u64::try_from(d.as_millis().saturating_div(2)).unwrap_or(u64::MAX);
  if half_ms == 0 {
    return d;
  }
  let jitter = fastrand::u64(0..half_ms);
  Duration::from_millis(half_ms.saturating_add(jitter))
}

/// Total elapsed-time retry budget `poll_and_execute` passes when reporting
/// a real job completion: 10 minutes. Tests inject a millisecond-scale
/// budget instead so AC-11 (budget exhaustion) runs sub-second.
pub(crate) const REPORT_RETRY_MAX: Duration = Duration::from_secs(600);

/// Starting backoff before the first retry sleep — mirrors the poll loop's
/// `POLL_BACKOFF_START` in `job_lifecycle.rs`.
const BACKOFF_START: Duration = Duration::from_secs(1);
/// Cap on the doubling backoff between retries.
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Retry `op` while it returns `RunnerError::Network`, sleeping a
/// [`jittered_backoff`] (1s → 60s cap) between attempts, until `Ok`, a
/// non-`Network` error, `cancel` firing, or `budget` elapsing — the last
/// three return the last `Network` error (or the non-`Network` error)
/// as-is; no new error variant. `what` names the op for the WARN log.
///
/// `budget` bounds only the sleeping between attempts — elapsed time is
/// checked *before* each sleep and the sleep capped to what remains
/// (AC-11) — never an in-flight attempt, so total wall-clock can overshoot
/// `budget` by up to one attempt's duration (bounded by the HTTP client's
/// own timeout).
pub(crate) async fn retry_transient<T, F, Fut>(
  mut op: F,
  cancel: &CancellationToken,
  budget: Duration,
  what: &str,
) -> Result<T, RunnerError>
where
  F: FnMut() -> Fut,
  Fut: Future<Output = Result<T, RunnerError>>,
{
  let start = Instant::now();
  let mut backoff = BACKOFF_START;
  let mut attempt: u32 = 1;
  loop {
    match op().await {
      Ok(value) => return Ok(value),
      Err(RunnerError::Network(msg)) => {
        let elapsed = start.elapsed();
        if elapsed >= budget {
          return Err(RunnerError::Network(msg));
        }
        tracing::warn!(what, attempt, error = %msg, "retrying transient reporting failure");
        let remaining = budget - elapsed;
        let sleep_for = jittered_backoff(backoff).min(remaining);
        tokio::select! {
          () = cancel.cancelled() => return Err(RunnerError::Network(msg)),
          () = tokio::time::sleep(sleep_for) => {},
        }
        backoff = backoff.saturating_mul(2).min(BACKOFF_MAX);
        attempt += 1;
      },
      Err(other) => return Err(other),
    }
  }
}

//! Pure mid-job reporting-outage tracker.
//!
//! Fed only by the renewal task's heartbeat: no tokio, no I/O, no clock of
//! its own — the caller supplies `now`. Mirrors the pure-decision pattern of
//! `loop_decision` / `message_route` in this crate, just stateful across
//! calls instead of a single mapping.

use std::time::{Duration, Instant};

/// Default outage threshold: more than 5 minutes since the last successful
/// renewal trips the watchdog.
pub const OUTAGE_THRESHOLD: Duration = Duration::from_secs(300);

/// Tracks time since the last successful reporting round-trip and latches a
/// single trip decision once the outage exceeds `threshold`.
pub struct OutageWatchdog {
  last_ok: Instant,
  threshold: Duration,
  /// Set once `record_err` has returned `true`; suppresses further trips
  /// until `record_ok` resets the tracker.
  tripped: bool,
}

impl OutageWatchdog {
  /// Start tracking as of `now`, seeding `last_ok = now`.
  pub fn new(now: Instant, threshold: Duration) -> Self {
    Self {
      last_ok: now,
      threshold,
      tripped: false,
    }
  }

  /// Record a successful renewal: resets the outage window to `now` and
  /// clears the trip latch.
  pub fn record_ok(&mut self, now: Instant) {
    self.last_ok = now;
    self.tripped = false;
  }

  /// Record a failed renewal at `now`.
  ///
  /// Returns `true` exactly once, the first time `now - last_ok >
  /// threshold` holds (strictly greater — equal to the threshold does not
  /// trip). Latched: once tripped, further calls return `false` until
  /// `record_ok` resets the tracker.
  pub fn record_err(&mut self, now: Instant) -> bool {
    if self.tripped {
      return false;
    }
    if now.saturating_duration_since(self.last_ok) > self.threshold {
      self.tripped = true;
      return true;
    }
    false
  }
}

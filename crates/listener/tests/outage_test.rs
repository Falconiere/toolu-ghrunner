//! Pure tests for `listener::outage::OutageWatchdog` (AC-1, AC-5).
//!
//! Real data only: every case drives the watchdog with real
//! `std::time::Instant` values (no mocked clock) — the boundary case
//! constructs `now` values by adding a `Duration` to a fixed base instant.

use listener::outage::OutageWatchdog;
use std::time::{Duration, Instant};

/// AC-1: the boundary is strictly-greater-than, and the trip latches until
/// the next `record_ok`.
#[test]
fn boundary_is_strict_and_latches() {
  let threshold = Duration::from_secs(5 * 60);
  let t0 = Instant::now();
  let mut wd = OutageWatchdog::new(t0, threshold);

  // Exactly at the threshold: not strictly greater, does not trip.
  let at_threshold = t0 + threshold;
  assert!(
    !wd.record_err(at_threshold),
    "exactly-at-threshold must not trip"
  );

  // One second past the threshold: first strictly-past call trips.
  let past_threshold = t0 + threshold + Duration::from_secs(1);
  assert!(
    wd.record_err(past_threshold),
    "strictly-past-threshold must trip"
  );

  // Immediately-following call: latched, returns false.
  assert!(
    !wd.record_err(past_threshold),
    "latch must suppress re-trip"
  );
}

/// AC-5: a successful renewal resets the window so a subsequent outage that
/// never exceeds the threshold (measured from the reset point) never trips.
#[test]
fn record_ok_resets_the_window() {
  let threshold = Duration::from_secs(5 * 60);
  let t0 = Instant::now();
  let mut wd = OutageWatchdog::new(t0, threshold);

  // 2 minutes of outage: well under threshold, does not trip.
  let two_min = t0 + Duration::from_secs(2 * 60);
  assert!(!wd.record_err(two_min), "2 min outage must not trip");

  // Reconnect at 2 min 30 s: resets last_ok.
  let ok_at = t0 + Duration::from_secs(2 * 60 + 30);
  wd.record_ok(ok_at);

  // 4 minutes after the reset point: still under threshold measured from
  // the new last_ok, so it must never trip.
  let four_min_after_ok = ok_at + Duration::from_secs(4 * 60);
  assert!(
    !wd.record_err(four_min_after_ok),
    "4 min after reset must not trip (never exceeds threshold from last_ok)"
  );
}

/// The latch clears on `record_ok`, so a fresh outage past the (new) window
/// can trip again.
#[test]
fn latch_clears_on_record_ok_and_can_retrip() {
  let threshold = Duration::from_secs(5 * 60);
  let t0 = Instant::now();
  let mut wd = OutageWatchdog::new(t0, threshold);

  let first_trip_at = t0 + threshold + Duration::from_secs(1);
  assert!(wd.record_err(first_trip_at), "first outage must trip");
  assert!(
    !wd.record_err(first_trip_at),
    "latched immediately after trip"
  );

  // Reconnect clears the latch and resets last_ok.
  wd.record_ok(first_trip_at);

  // A fresh outage strictly past the new last_ok + threshold trips again.
  let second_trip_at = first_trip_at + threshold + Duration::from_secs(1);
  assert!(
    wd.record_err(second_trip_at),
    "a fresh outage past the reset window must trip again"
  );
}

/// `record_ok` moves the outage window forward: errors within `threshold`
/// of the *latest* successful renewal never trip, even if the absolute time
/// since watchdog construction already exceeds the threshold.
#[test]
fn record_ok_moves_the_window_forward() {
  let threshold = Duration::from_secs(5 * 60);
  let t0 = Instant::now();
  let mut wd = OutageWatchdog::new(t0, threshold);

  // Renewals succeed every 4 minutes, each time before the threshold from
  // the previous success — the outage never accumulates past the window.
  let ok1 = t0 + Duration::from_secs(4 * 60);
  wd.record_ok(ok1);
  let ok2 = ok1 + Duration::from_secs(4 * 60);
  wd.record_ok(ok2);

  // An error 4 minutes after the latest ok is still within the threshold
  // measured from that latest ok, even though it is 8+ minutes after t0.
  let err_at = ok2 + Duration::from_secs(4 * 60);
  assert!(
    !wd.record_err(err_at),
    "error within threshold of the latest ok must not trip"
  );
}

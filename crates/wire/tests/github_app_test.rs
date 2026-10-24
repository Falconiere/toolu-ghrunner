//! #201 pure rules: the installation-token reuse deadline (AC-16) and the
//! runner-group name match. The HTTP calls are exercised against a loopback
//! peer in `toolu-runner/tests/github_app_wire_test.rs`.

use wire::net::github_app::{REUSE_MARGIN_SECS, REUSE_MAX_SECS, reuse_deadline};
use wire::net::runner_groups::{RunnerGroup, pick_group};

/// 2026-10-23T20:00:00Z.
const MINTED: i64 = 1_792_785_600;

#[test]
fn a_fresh_one_hour_token_is_reused_for_fifty_minutes() {
  // GitHub's tokens expire one hour after minting.
  let deadline = reuse_deadline("2026-10-23T21:00:00Z", MINTED);
  assert_eq!(deadline, Some(MINTED + REUSE_MAX_SECS));
  assert_eq!(REUSE_MAX_SECS, 50 * 60);
}

#[test]
fn a_token_expiring_soon_stops_five_minutes_before_expiry() {
  // expires 20 min after minting (an offset RFC 3339 form).
  let deadline = reuse_deadline("2026-10-23T22:20:00+02:00", MINTED);
  assert_eq!(deadline, Some(MINTED + 20 * 60 - REUSE_MARGIN_SECS));
}

#[test]
fn an_already_expired_token_is_never_reused() {
  // The documented example's expires_at (2016) is long past.
  let deadline = reuse_deadline("2016-07-11T22:14:10Z", MINTED).expect("parses");
  assert!(
    deadline <= MINTED,
    "deadline {deadline} must not be after minting"
  );
}

#[test]
fn an_unparseable_expiry_means_single_use() {
  assert_eq!(reuse_deadline("", MINTED), None);
  assert_eq!(reuse_deadline("tomorrow", MINTED), None);
}

fn groups() -> Vec<RunnerGroup> {
  [
    (1, "Default"),
    (2, "octo-runner-group"),
    (3, "Mobile-CI"),
    (4, "mobile-ci"),
  ]
  .into_iter()
  .map(|(id, name)| RunnerGroup {
    id,
    name: name.to_owned(),
  })
  .collect()
}

#[test]
fn pick_group_prefers_an_exact_name() {
  assert_eq!(pick_group(&groups(), "octo-runner-group"), Some(2));
  assert_eq!(pick_group(&groups(), "mobile-ci"), Some(4));
}

#[test]
fn pick_group_falls_back_to_a_unique_case_insensitive_match() {
  assert_eq!(pick_group(&groups(), "OCTO-runner-GROUP"), Some(2));
}

#[test]
fn pick_group_refuses_an_ambiguous_or_absent_name() {
  assert_eq!(
    pick_group(&groups(), "MOBILE-CI"),
    None,
    "two case-folded matches"
  );
  assert_eq!(pick_group(&groups(), "release"), None);
}

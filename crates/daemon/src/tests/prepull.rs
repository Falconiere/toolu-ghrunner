//! Tests for the pre-pull decisions: whether serving may begin given what a
//! pull attempt left on the box, and how a pinned image reference is split
//! for Docker's pull endpoint.
//!
//! The one rule these exist to hold: **the listener never binds while the
//! image is absent.** A create against an absent image answers 503,
//! `classifyVpsStatus` reads that as `unavailable`, and `vpsDispositionFor`
//! turns it into fallback plus a five-minute cooldown on this host — so a
//! daemon that bound early would cool the only box in the fleet down for five
//! minutes every time it restarted.
//!
//! Nothing here connects to Docker; the image references are the ones
//! `vps_hosts.image_ref` really holds.

use super::{
  ImagePresence, PullAttempt, PullOutcome, RefreshDecision, StartupDecision, classify_pull,
  refresh_decision, split_image_ref, startup_decision,
};

/// The image `providers/registry.ts` pins by default.
const DEFAULT_IMAGE: &str = "ghcr.io/falconiere/toolu-ghrunner:latest";

#[test]
fn a_settled_pull_that_left_the_image_resident_binds_the_listener() {
  let outcome = classify_pull(PullAttempt::Succeeded, ImagePresence::Resident);

  assert_eq!(outcome, PullOutcome::Pulled);
  assert_eq!(startup_decision(outcome, 4), StartupDecision::Bind);
  assert_eq!(
    startup_decision(outcome, 0),
    StartupDecision::Bind,
    "a resident image binds even on the last attempt"
  );
}

#[test]
fn a_failed_pull_still_binds_when_an_earlier_one_left_the_image_resident() {
  let outcome = classify_pull(PullAttempt::Failed, ImagePresence::Resident);

  assert_eq!(outcome, PullOutcome::FailedButResident);
  assert_eq!(
    startup_decision(outcome, 0),
    StartupDecision::Bind,
    "an unreachable registry must not take a working box out of service"
  );
}

#[test]
fn a_pull_that_reported_success_but_left_nothing_is_not_trusted() {
  assert_eq!(
    classify_pull(PullAttempt::Succeeded, ImagePresence::Absent),
    PullOutcome::FailedAndAbsent,
    "what makes a create work is the image being there, not the pull's exit status"
  );
}

#[test]
fn an_absent_image_never_binds_while_attempts_remain() {
  let outcome = classify_pull(PullAttempt::Failed, ImagePresence::Absent);

  assert_eq!(outcome, PullOutcome::FailedAndAbsent);
  for attempts_left in 1..=4_u32 {
    assert_eq!(
      startup_decision(outcome, attempts_left),
      StartupDecision::Retry,
      "binding here would 503 and cool this host down for five minutes"
    );
  }
}

#[test]
fn an_absent_image_fails_startup_once_the_attempts_run_out() {
  let outcome = classify_pull(PullAttempt::Failed, ImagePresence::Absent);

  assert_eq!(
    startup_decision(outcome, 0),
    StartupDecision::GiveUp,
    "exiting hands the retry to the supervisor; binding would serve nothing but 503s"
  );
}

#[test]
fn a_refresh_never_stops_serving_whatever_it_finds() {
  assert_eq!(
    refresh_decision(PullOutcome::Pulled),
    RefreshDecision::KeepServing
  );
  assert_eq!(
    refresh_decision(PullOutcome::FailedButResident),
    RefreshDecision::KeepServing
  );
  assert_eq!(
    refresh_decision(PullOutcome::FailedAndAbsent),
    RefreshDecision::WarnMissingImage,
    "a missing image is worth a log line, not a closed listener"
  );
}

#[test]
fn an_image_reference_splits_into_the_repository_and_tag_docker_pulls_by() {
  assert_eq!(
    split_image_ref(DEFAULT_IMAGE),
    ("ghcr.io/falconiere/toolu-ghrunner", "latest")
  );
  assert_eq!(
    split_image_ref("ghcr.io/falconiere/toolu-ghrunner:v0.8.0-docker"),
    ("ghcr.io/falconiere/toolu-ghrunner", "v0.8.0-docker")
  );
  assert_eq!(
    split_image_ref("ghcr.io/falconiere/toolu-ghrunner"),
    ("ghcr.io/falconiere/toolu-ghrunner", "latest"),
    "a reference naming no tag pulls latest"
  );
}

#[test]
fn a_registry_port_is_not_mistaken_for_a_tag() {
  assert_eq!(
    split_image_ref("localhost:5000/toolu/runner"),
    ("localhost:5000/toolu/runner", "latest")
  );
  assert_eq!(
    split_image_ref("localhost:5000/toolu/runner:v1"),
    ("localhost:5000/toolu/runner", "v1")
  );
}

#[test]
fn a_digest_pin_carries_its_whole_digest_across_as_the_reference() {
  let digest = "sha256:2ceff2ee1eb1d0b70cb8d9f0c3fa4b0dd2e7f26f6d8a7f5c1cf5aa3d2f0e4b7c";
  let pinned = format!("ghcr.io/falconiere/toolu-ghrunner@{digest}");

  assert_eq!(
    split_image_ref(&pinned),
    ("ghcr.io/falconiere/toolu-ghrunner", digest)
  );
}

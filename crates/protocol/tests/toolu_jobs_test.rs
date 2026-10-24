//! #206 AC-9: the toolu.sh `/runner/*` bodies.
//!
//! Inputs are the contract's own examples, verbatim: the queued list and the
//! machine claim from CodaSignal/toolu.sh `docs/guides/self-hosted-machines.md`
//! ("Claiming a job"), the heartbeat reply, `complete` schema and
//! `claim refused: <reason>` text from `packages/api/src/runner/routes.ts`.

use std::time::Duration;

use protocol::toolu_jobs::{
  Claim, ClaimRefusal, CompleteBody, EpochBody, LeaseRenewal, Outcome, QueuedJobs,
  heartbeat_delay, parse_claim_refusal,
};

/// `GET /runner/jobs/queued` for a machine (guide example).
const QUEUED: &str = r#"{
  "jobs": [
    { "id": "…", "runnerTag": "toolu-self-hosted", "labels": ["gpu"], "createdAt": 1792771691368 }
  ]
}"#;

/// `POST /runner/jobs/:id/claim` for a machine (guide example: no `image`).
const CLAIM: &str =
  r#"{ "jitConfig": "…", "deadline": 1792793291368, "claimEpoch": 0, "leaseExpiresAt": 1792772291368 }"#;

#[test]
fn a_machine_queued_list_parses() {
  let queued: QueuedJobs = serde_json::from_str(QUEUED).expect("parses");
  let job = queued.jobs.first().expect("one job");
  assert_eq!(queued.jobs.len(), 1);
  assert_eq!(job.id, "…");
  assert_eq!(job.runner_tag, "toolu-self-hosted");
  assert_eq!(job.labels, ["gpu"]);
  assert_eq!(job.created_at, 1_792_771_691_368);
}

#[test]
fn an_empty_queued_list_parses() {
  let queued: QueuedJobs = serde_json::from_str(r#"{ "jobs": [] }"#).expect("parses");
  assert!(queued.jobs.is_empty());
}

#[test]
fn a_fleet_job_without_labels_still_parses() {
  // `runner-jobs.md`: a fleet summary carries vcpu/memoryMb, no labels.
  let fleet = r#"{"jobs":[{"id":"j1","runnerTag":"toolu-ubuntu","vcpu":2,"memoryMb":8192,"createdAt":1}]}"#;
  let queued: QueuedJobs = serde_json::from_str(fleet).expect("parses");
  assert!(queued.jobs.first().expect("one job").labels.is_empty());
}

#[test]
fn a_machine_claim_parses() {
  let claim: Claim = serde_json::from_str(CLAIM).expect("parses");
  assert_eq!(claim.jit_config, "…");
  assert_eq!(claim.deadline, 1_792_793_291_368);
  assert_eq!(claim.claim_epoch, 0);
  assert_eq!(claim.lease_expires_at, 1_792_772_291_368);
}

#[test]
fn a_fleet_claim_with_an_image_still_parses() {
  let fleet = CLAIM.replace("\"claimEpoch\"", "\"image\": \"ghcr.io/x:1\", \"claimEpoch\"");
  let claim: Claim = serde_json::from_str(&fleet).expect("extra image ignored");
  assert_eq!(claim.claim_epoch, 0);
}

#[test]
fn a_heartbeat_reply_parses() {
  let reply: LeaseRenewal =
    serde_json::from_str(r#"{"leaseExpiresAt":1792772891368}"#).expect("parses");
  assert_eq!(reply.lease_expires_at, 1_792_772_891_368);
}

#[test]
fn epoch_and_complete_bodies_match_the_route_schemas() {
  let epoch = serde_json::to_value(EpochBody { claim_epoch: 3 }).expect("serializes");
  assert_eq!(epoch, serde_json::json!({ "claimEpoch": 3 }));

  let done = CompleteBody {
    outcome: Outcome::Done,
    error_message: None,
    claim_epoch: 3,
  };
  assert_eq!(
    serde_json::to_value(&done).expect("serializes"),
    serde_json::json!({ "outcome": "done", "claimEpoch": 3 })
  );
  let failed = CompleteBody {
    outcome: Outcome::Failed,
    error_message: Some("listener: broker refused".to_owned()),
    claim_epoch: 0,
  };
  assert_eq!(
    serde_json::to_value(&failed).expect("serializes"),
    serde_json::json!({
      "outcome": "failed",
      "errorMessage": "listener: broker refused",
      "claimEpoch": 0,
    })
  );
}

#[test]
fn claim_refusals_are_classified_from_the_409_body() {
  assert_eq!(
    parse_claim_refusal(r#"{ "error": "claim refused: taken" }"#),
    ClaimRefusal::Taken
  );
  assert_eq!(
    parse_claim_refusal(r#"{ "error": "claim refused: slots-full" }"#),
    ClaimRefusal::SlotsFull
  );
  assert_eq!(
    parse_claim_refusal(r#"{"error":"claim refused: cap-reached"}"#),
    ClaimRefusal::Other("cap-reached".to_owned())
  );
  assert_eq!(
    parse_claim_refusal("<html>bad gateway</html>"),
    ClaimRefusal::Other("<html>bad gateway</html>".to_owned())
  );
}

#[test]
fn heartbeat_delay_is_a_third_of_the_remaining_lease_within_bounds() {
  let now = 1_792_771_691_368;
  // The contract's 10-minute lease renews after ~200 s.
  assert_eq!(
    heartbeat_delay(now + 600_000, now),
    Duration::from_millis(200_000)
  );
  // A short lease still waits at least 1 s; a long one at most 5 min.
  assert_eq!(heartbeat_delay(now + 1_500, now), Duration::from_secs(1));
  assert_eq!(heartbeat_delay(now + 3_600_000, now), Duration::from_secs(300));
  // A lapsed (or exactly expiring) lease is retried every 30 s, never hot.
  assert_eq!(heartbeat_delay(now, now), Duration::from_secs(30));
  assert_eq!(heartbeat_delay(now - 5_000, now), Duration::from_secs(30));
}

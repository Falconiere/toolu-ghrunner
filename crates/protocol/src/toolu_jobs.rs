//! toolu.sh `/runner/*` job-claim bodies (#206): what a paired machine sends
//! and receives while it polls for work.
//!
//! The contract is CodaSignal/toolu.sh `docs/guides/self-hosted-machines.md`
//! ("Claiming a job") and `packages/api/src/runner/routes.ts`. Pure types and
//! rules only; the HTTP calls live in `wire::net::toolu_jobs`. Unknown fields
//! are ignored, so a fleet-shaped body (`image`, `vcpu`) still parses.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Shortest wait before renewing a lease.
const HEARTBEAT_MIN: Duration = Duration::from_secs(1);
/// Longest wait before renewing a lease.
const HEARTBEAT_MAX: Duration = Duration::from_secs(300);
/// Retry interval once the last known lease has lapsed (an outage): slow
/// enough never to hammer, fast enough to renew before the reclaim sweep.
const HEARTBEAT_LAPSED: Duration = Duration::from_secs(30);

/// `GET /runner/jobs/queued`: the jobs this machine may claim, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct QueuedJobs {
  /// At most 20 jobs; empty while the machine holds all its slots.
  pub jobs: Vec<QueuedJob>,
}

/// One queued job's poll-visible fields.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedJob {
  /// The `runner_jobs` row id the claim names.
  pub id: String,
  /// `toolu-self-hosted` for a machine's job.
  pub runner_tag: String,
  /// The labels the job asked for besides the reserved one and
  /// `self-hosted` (absent on a fleet job).
  #[serde(default)]
  pub labels: Vec<String>,
  /// Epoch milliseconds the job was queued.
  pub created_at: u64,
}

/// `POST /runner/jobs/:id/claim` 200: the JIT config to run and the claim.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Claim {
  /// Base64 JIT config minted on GitHub for this claim (a credential).
  pub jit_config: String,
  /// Epoch milliseconds after which the claim is no longer worth running.
  pub deadline: u64,
  /// Fences `dispatched` / `heartbeat` / `complete` to this claim.
  pub claim_epoch: u64,
  /// Epoch milliseconds the lease lapses unless renewed.
  pub lease_expires_at: u64,
}

impl std::fmt::Debug for Claim {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Claim")
      .field("jit_config", &"<redacted>")
      .field("deadline", &self.deadline)
      .field("claim_epoch", &self.claim_epoch)
      .field("lease_expires_at", &self.lease_expires_at)
      .finish()
  }
}

/// `POST /runner/jobs/:id/heartbeat` 200: the renewed lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseRenewal {
  /// Epoch milliseconds the renewed lease lapses.
  pub lease_expires_at: u64,
}

/// The `heartbeat` / `dispatched` request body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EpochBody {
  /// The claim's epoch.
  pub claim_epoch: u64,
}

/// `complete`'s `outcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
  /// The JIT lifecycle ran a job to a conclusion (any conclusion).
  Done,
  /// No job ran, or the lifecycle errored.
  Failed,
}

/// The `complete` request body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompleteBody {
  /// Done or failed.
  pub outcome: Outcome,
  /// Why it failed (masked by the caller); omitted when `None`.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub error_message: Option<String>,
  /// The claim's epoch.
  pub claim_epoch: u64,
}

/// Why a claim answered 409.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimRefusal {
  /// Another claim took the job first: try the next one.
  Taken,
  /// The machine already holds its slots: wait, then poll again.
  SlotsFull,
  /// Any other reason (`cap-reached`, or a body that is not the contract's):
  /// the reason, or the raw body.
  Other(String),
}

/// The `{"error":"…"}` body every refusal carries.
#[derive(Deserialize)]
struct ErrorBody {
  error: String,
}

/// Classify a claim's 409 body (`{"error":"claim refused: <reason>"}`).
pub fn parse_claim_refusal(body: &str) -> ClaimRefusal {
  let Ok(ErrorBody { error }) = serde_json::from_str(body) else {
    return ClaimRefusal::Other(body.to_owned());
  };
  let reason = error
    .strip_prefix("claim refused: ")
    .unwrap_or(&error)
    .trim();
  match reason {
    "taken" => ClaimRefusal::Taken,
    "slots-full" => ClaimRefusal::SlotsFull,
    other => ClaimRefusal::Other(other.to_owned()),
  }
}

/// How long to wait before the next heartbeat: a third of the lease left at
/// `now_ms`, within [1 s, 5 min], so a renewal gets three tries before the
/// lease lapses; 30 s once it has lapsed.
pub fn heartbeat_delay(lease_expires_at_ms: u64, now_ms: u64) -> Duration {
  let remaining = lease_expires_at_ms.saturating_sub(now_ms);
  if remaining == 0 {
    return HEARTBEAT_LAPSED;
  }
  Duration::from_millis(remaining.saturating_div(3)).clamp(HEARTBEAT_MIN, HEARTBEAT_MAX)
}

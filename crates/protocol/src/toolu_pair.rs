//! toolu.sh machine pairing, enrollment and self-service bodies (#207): how
//! a box becomes a machine of a toolu.sh org and how it later reads, refreshes
//! or revokes itself.
//!
//! The contract is CodaSignal/toolu.sh `docs/guides/self-hosted-machines.md`
//! ("Device-code pairing", "Enrollment tokens", "Machine self-service"). Pure
//! types and rules only; the HTTP calls live in `wire::net::toolu_pair`
//! (pairing, enrollment) and `wire::net::toolu_jobs` (`/runner/host`).
//! Every type carrying a credential (`device_code`, `host_token`,
//! `enroll_token`) prints it as `<redacted>` in `Debug`.

use serde::{Deserialize, Serialize};

/// The reserved `runs-on` label that routes a job to an org's machines.
pub const SELF_HOSTED_LABEL: &str = "toolu-self-hosted";

/// What a box reports about itself to `POST /runner/pair`, `POST
/// /runner/enroll` and `PATCH /runner/host`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MachineReport {
  /// The box's hostname.
  pub hostname: String,
  /// `linux` or `macos`.
  pub platform: String,
  /// `x64` or `arm64`.
  pub arch: String,
  /// Logical CPUs; omitted when unknown.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub vcpu: Option<u64>,
  /// Total memory in MiB; omitted when unknown.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub memory_mb: Option<u64>,
  /// Custom runner labels (toolu.sh drops the default ones itself).
  pub labels: Vec<String>,
  /// The toolu-runner version running on the box.
  pub runner_version: String,
}

impl MachineReport {
  /// The `PATCH /runner/host` body refreshing this report (the route takes
  /// no `platform` / `arch`: unknown fields are a 400).
  pub fn patch(&self) -> HostPatch {
    HostPatch {
      hostname: self.hostname.clone(),
      labels: self.labels.clone(),
      runner_version: self.runner_version.clone(),
      vcpu: self.vcpu,
      memory_mb: self.memory_mb,
    }
  }
}

/// The `PATCH /runner/host` body: what a re-run of `setup` refreshes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostPatch {
  /// The box's hostname.
  pub hostname: String,
  /// Custom runner labels.
  pub labels: Vec<String>,
  /// The toolu-runner version running on the box.
  pub runner_version: String,
  /// Logical CPUs; omitted when unknown.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub vcpu: Option<u64>,
  /// Total memory in MiB; omitted when unknown.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub memory_mb: Option<u64>,
}

/// `POST /runner/pair` 200: the device code to poll with and the user code
/// to show.
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct PairStart {
  /// Polled with `POST /runner/pair/token`; a credential the box keeps
  /// secret.
  pub device_code: String,
  /// The code the user types in the console (`XXXX-XXXX`).
  pub user_code: String,
  /// Where the user enters it, e.g. `https://console.toolu.sh/pair`.
  pub verification_uri: String,
  /// The same page with the code filled in.
  pub verification_uri_complete: String,
  /// Seconds until the code expires.
  pub expires_in: u64,
  /// The shortest wait between token polls, in seconds.
  pub interval: u64,
}

impl std::fmt::Debug for PairStart {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("PairStart")
      .field("device_code", &"<redacted>")
      .field("user_code", &self.user_code)
      .field("verification_uri", &self.verification_uri)
      .field("expires_in", &self.expires_in)
      .field("interval", &self.interval)
      .finish_non_exhaustive()
  }
}

/// The `POST /runner/pair/token` request body.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct PairTokenRequest {
  /// The device code from [`PairStart`].
  pub device_code: String,
}

impl std::fmt::Debug for PairTokenRequest {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("PairTokenRequest")
      .field("device_code", &"<redacted>")
      .finish()
  }
}

/// The machine toolu.sh created: `pair/token` and `enroll` 200, the only
/// delivery of its host token.
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct HostGrant {
  /// `tlu_h_…`: the bearer of every later `/runner/*` request.
  pub host_token: String,
  /// The machine's host id.
  pub host_id: String,
  /// The toolu.sh organization id it belongs to.
  pub org: String,
  /// Its stored custom labels.
  #[serde(default)]
  pub labels: Vec<String>,
}

impl std::fmt::Debug for HostGrant {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("HostGrant")
      .field("host_token", &"<redacted>")
      .field("host_id", &self.host_id)
      .field("org", &self.org)
      .field("labels", &self.labels)
      .finish()
  }
}

/// The `POST /runner/enroll` request body: the token plus the machine
/// report.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct EnrollRequest {
  /// The single-use `tlu_e_…` enrollment token.
  pub enroll_token: String,
  /// What the box reports about itself.
  #[serde(flatten)]
  pub machine: MachineReport,
}

impl std::fmt::Debug for EnrollRequest {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("EnrollRequest")
      .field("enroll_token", &"<redacted>")
      .field("machine", &self.machine)
      .finish()
  }
}

/// What one `POST /runner/pair/token` poll said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairPoll {
  /// Approved: the machine and its token.
  Granted(HostGrant),
  /// `authorization_pending`: nobody decided yet.
  Pending,
  /// `slow_down`: polled too soon; wait five more seconds from now on.
  SlowDown,
  /// `access_denied`: an org owner or admin denied it.
  Denied,
  /// `expired_token`: expired, unknown, or already delivered.
  Expired,
  /// Any other 400 `error`, or a body that is not the contract's.
  Other(String),
}

/// The `{"error":"…"}` body every refusal carries.
#[derive(Deserialize)]
struct ErrorBody {
  error: String,
}

/// Classify a `POST /runner/pair/token` 400 body.
pub fn parse_pair_refusal(body: &str) -> PairPoll {
  let Ok(ErrorBody { error }) = serde_json::from_str(body) else {
    return PairPoll::Other(body.to_owned());
  };
  match error.as_str() {
    "authorization_pending" => PairPoll::Pending,
    "slow_down" => PairPoll::SlowDown,
    "access_denied" => PairPoll::Denied,
    "expired_token" => PairPoll::Expired,
    _ => PairPoll::Other(error),
  }
}

/// The `error` text of a `{"error":"…"}` body, else the body itself.
pub fn error_text(body: &str) -> String {
  serde_json::from_str::<ErrorBody>(body).map_or_else(|_| body.to_owned(), |b| b.error)
}

/// `GET` / `PATCH /runner/host` 200: the calling machine.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct HostInfo {
  /// The machine's host id.
  pub host_id: String,
  /// The console name (a console user's to change).
  pub name: String,
  /// The agent-reported hostname.
  #[serde(default)]
  pub hostname: Option<String>,
  /// The organization it belongs to.
  pub org: HostOrg,
  /// `linux` or `macos`.
  pub platform: String,
  /// `x64` or `arm64`.
  pub arch: String,
  /// Its stored custom labels.
  #[serde(default)]
  pub labels: Vec<String>,
  /// Jobs it takes at once.
  pub slots: u64,
  /// The reported runner version.
  #[serde(default)]
  pub runner_version: Option<String>,
  /// Whether an authenticated request has enabled it.
  pub enabled: bool,
  /// Epoch milliseconds of its last authenticated request.
  #[serde(default)]
  pub last_seen_at: Option<u64>,
}

/// A machine's organization.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct HostOrg {
  /// The organization id.
  pub id: String,
  /// Its display name (the workspace name).
  pub name: String,
}

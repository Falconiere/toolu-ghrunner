//! toolu.sh `/runner/*` calls of a paired machine: the job claim (#206: list,
//! claim, dispatched, heartbeat, complete — what the poll loop sends) and the
//! machine's own `/runner/host` (#207: read, refresh, revoke).
//!
//! Every call carries `Authorization: Bearer <host token>` (`tlu_h_…`).
//! Statuses map to [`TooluError`] by the contract's meaning
//! (CodaSignal/toolu.sh `self-hosted-machines.md`, `runner-jobs.md`): 401 is
//! a revoked or unknown machine, 409 a refused or lost claim, 404 an unknown
//! job, 400 refused input; 429, 5xx and transport failures are transient, so
//! the loop backs off. Bodies and rules are `protocol::toolu_jobs` and
//! `protocol::toolu_pair`.

use std::time::Duration;

use protocol::toolu_jobs::{Claim, CompleteBody, EpochBody, LeaseRenewal, QueuedJobs};
use protocol::toolu_pair::{HostInfo, HostPatch};
use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use shared::RunnerError;

/// Per-request timeout of every `/runner/*` call.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// A failed `/runner/*` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TooluError {
  /// 401: the host token is unknown or the machine was revoked; carries the
  /// body's `error` (or the raw body).
  Revoked(String),
  /// 409: the claim was refused or is no longer this machine's; carries the
  /// raw body.
  Conflict(String),
  /// 404: no such job.
  NotFound,
  /// 400: toolu.sh refused the request's input (a reserved label, a bad
  /// field); carries the body's `error` (or the raw body).
  Refused(String),
  /// 429, 5xx, or the request never got an answer: retry later.
  Transient(String),
  /// Any other status, or a 2xx body that does not parse.
  Protocol(String),
}

impl std::fmt::Display for TooluError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Revoked(why) => write!(f, "toolu.sh rejected the host token (401: {why})"),
      Self::Conflict(body) => write!(f, "toolu.sh refused the claim (409: {body})"),
      Self::NotFound => f.write_str("toolu.sh does not know the job (404)"),
      Self::Refused(why) => write!(f, "toolu.sh refused the request (400: {why})"),
      Self::Transient(why) | Self::Protocol(why) => f.write_str(why),
    }
  }
}

impl std::error::Error for TooluError {}

/// The `/runner/*` client of one paired machine.
#[derive(Clone)]
pub struct TooluClient {
  client: reqwest::Client,
  /// The API origin, e.g. `https://console.toolu.sh`.
  base: url::Url,
  bearer: String,
}

impl std::fmt::Debug for TooluClient {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("TooluClient")
      .field("base", &self.base.as_str())
      .field("bearer", &"<redacted>")
      .finish_non_exhaustive()
  }
}

impl TooluClient {
  /// A client for the toolu.sh API at `api_url` authenticating with
  /// `host_token`.
  ///
  /// # Errors
  ///
  /// `RunnerError::Config` when `api_url` is not an absolute `http(s)` URL
  /// with a host, or `host_token` is empty.
  pub fn new(
    client: reqwest::Client,
    api_url: &str,
    host_token: &str,
  ) -> Result<Self, RunnerError> {
    let base = api_base(api_url)?;
    if host_token.is_empty() {
      return Err(RunnerError::Config(
        "the toolu.sh host token (credentials.json access_token) is empty".to_owned(),
      ));
    }
    Ok(Self {
      client,
      base,
      bearer: host_token.to_owned(),
    })
  }

  /// `GET /runner/jobs/queued`: the jobs this machine may claim.
  ///
  /// # Errors
  ///
  /// [`TooluError`] by status (see the module doc).
  pub async fn queued(&self) -> Result<QueuedJobs, TooluError> {
    let url = self.url(&["runner", "jobs", "queued"])?;
    let body = self.send(self.client.get(url), "list queued jobs").await?;
    decode(&body, "list queued jobs")
  }

  /// `POST /runner/jobs/:id/claim`: claim job `id`.
  ///
  /// # Errors
  ///
  /// [`TooluError::Conflict`] when another claim took it or the machine's
  /// slots are full ([`protocol::toolu_jobs::parse_claim_refusal`] reads the
  /// body), [`TooluError::NotFound`] for an unknown or ineligible job, else
  /// by status.
  pub async fn claim(&self, id: &str) -> Result<Claim, TooluError> {
    let url = self.url(&["runner", "jobs", id, "claim"])?;
    let body = self.send(self.client.post(url), "claim").await?;
    decode(&body, "claim")
  }

  /// `POST /runner/jobs/:id/dispatched`: the claimed JIT's listener started.
  ///
  /// # Errors
  ///
  /// [`TooluError`] by status; 409 means the claim is no longer this one.
  pub async fn dispatched(&self, id: &str, claim_epoch: u64) -> Result<(), TooluError> {
    let url = self.url(&["runner", "jobs", id, "dispatched"])?;
    let request = self.client.post(url).json(&EpochBody { claim_epoch });
    self.send(request, "dispatched").await.map(|_body| ())
  }

  /// `POST /runner/jobs/:id/heartbeat`: renew the claim's lease.
  ///
  /// # Errors
  ///
  /// [`TooluError`] by status; 409 means the claim was lost.
  pub async fn heartbeat(&self, id: &str, claim_epoch: u64) -> Result<LeaseRenewal, TooluError> {
    let url = self.url(&["runner", "jobs", id, "heartbeat"])?;
    let request = self.client.post(url).json(&EpochBody { claim_epoch });
    let body = self.send(request, "heartbeat").await?;
    decode(&body, "heartbeat")
  }

  /// `POST /runner/jobs/:id/complete`: report how the claimed job ended.
  ///
  /// # Errors
  ///
  /// [`TooluError`] by status; 409 means the claim was lost.
  pub async fn complete(&self, id: &str, body: &CompleteBody) -> Result<(), TooluError> {
    let url = self.url(&["runner", "jobs", id, "complete"])?;
    let request = self.client.post(url).json(body);
    self.send(request, "complete").await.map(|_body| ())
  }

  /// `GET /runner/host`: this machine and its organization. The first
  /// authenticated request of a newly paired machine also enables it.
  ///
  /// # Errors
  ///
  /// [`TooluError`] by status; 401 means the machine was revoked.
  pub async fn host(&self) -> Result<HostInfo, TooluError> {
    let url = self.url(&["runner", "host"])?;
    let body = self.send(self.client.get(url), "read the machine").await?;
    decode(&body, "read the machine")
  }

  /// `PATCH /runner/host`: refresh what the machine reports.
  ///
  /// # Errors
  ///
  /// [`TooluError`] by status; a refused label set is a 400
  /// [`TooluError::Protocol`] carrying toolu.sh's `error`.
  pub async fn update_host(&self, patch: &HostPatch) -> Result<HostInfo, TooluError> {
    let url = self.url(&["runner", "host"])?;
    let request = self.client.patch(url).json(patch);
    let body = self.send(request, "update the machine").await?;
    decode(&body, "update the machine")
  }

  /// `DELETE /runner/host`: revoke this machine (204).
  ///
  /// # Errors
  ///
  /// [`TooluError`] by status; 401 means it is already revoked.
  pub async fn revoke_host(&self) -> Result<(), TooluError> {
    let url = self.url(&["runner", "host"])?;
    self
      .send(self.client.delete(url), "revoke the machine")
      .await
      .map(|_body| ())
  }

  /// `<base>/<segments…>`, each segment percent-encoded as one component.
  /// An empty, `.` or `..` segment (a job id no route can name; `url` would
  /// silently drop the dots) is a [`TooluError::Protocol`].
  fn url(&self, segments: &[&str]) -> Result<url::Url, TooluError> {
    join(&self.base, segments)
  }

  /// Send `request` with the bearer and timeout; the 2xx body, or the
  /// status's [`TooluError`].
  async fn send(&self, request: reqwest::RequestBuilder, what: &str) -> Result<String, TooluError> {
    let (status, body) = exchange(request.bearer_auth(&self.bearer), what).await?;
    if status.is_success() {
      return Ok(body);
    }
    Err(failure(what, status, body))
  }
}

/// The API origin `/runner/*` is appended to.
///
/// # Errors
///
/// `RunnerError::Config` when `api_url` is not an absolute `http(s)` URL with
/// a host.
pub(super) fn api_base(api_url: &str) -> Result<url::Url, RunnerError> {
  let base = url::Url::parse(api_url)
    .map_err(|e| RunnerError::Config(format!("toolu.sh api_url {api_url:?} is not a URL: {e}")))?;
  if !matches!(base.scheme(), "http" | "https") || base.host_str().is_none() {
    return Err(RunnerError::Config(format!(
      "toolu.sh api_url {api_url:?} must be an absolute http(s) URL"
    )));
  }
  Ok(base)
}

/// `<base>/<segments…>`, each segment percent-encoded as one component.
/// An empty, `.` or `..` segment (a job id no route can name; `url` would
/// silently drop the dots) is a [`TooluError::Protocol`].
pub(super) fn join(base: &url::Url, segments: &[&str]) -> Result<url::Url, TooluError> {
  if let Some(bad) = segments.iter().find(|s| matches!(**s, "" | "." | "..")) {
    return Err(TooluError::Protocol(format!(
      "toolu.sh job id {bad:?} is not a usable path segment"
    )));
  }
  let mut url = base.clone();
  url
    .path_segments_mut()
    .map_err(|()| TooluError::Protocol(format!("toolu.sh api_url {base} has no path")))?
    .pop_if_empty()
    .extend(segments);
  Ok(url)
}

/// Send `request` with the timeout and `User-Agent`; its status and body, or
/// a [`TooluError::Transient`] when no answer arrived.
pub(super) async fn exchange(
  request: reqwest::RequestBuilder,
  what: &str,
) -> Result<(StatusCode, String), TooluError> {
  let response = request
    .timeout(REQUEST_TIMEOUT)
    .header(
      "User-Agent",
      concat!("toolu-runner/", env!("CARGO_PKG_VERSION")),
    )
    .send()
    .await
    .map_err(|e| TooluError::Transient(format!("toolu.sh {what} request failed: {e}")))?;
  let status = response.status();
  let body = response
    .text()
    .await
    .map_err(|e| TooluError::Transient(format!("reading the toolu.sh {what} reply failed: {e}")))?;
  Ok((status, body))
}

/// Map a non-2xx reply of `what` (see the module doc).
pub(super) fn failure(what: &str, status: StatusCode, body: String) -> TooluError {
  match status {
    StatusCode::UNAUTHORIZED => TooluError::Revoked(error_field(&body).unwrap_or(body)),
    StatusCode::CONFLICT => TooluError::Conflict(body),
    StatusCode::NOT_FOUND => TooluError::NotFound,
    StatusCode::BAD_REQUEST => TooluError::Refused(error_field(&body).unwrap_or(body)),
    StatusCode::TOO_MANY_REQUESTS => {
      TooluError::Transient(format!("toolu.sh {what} was rate-limited (429): {body}"))
    },
    s if s.is_server_error() => {
      TooluError::Transient(format!("toolu.sh {what} failed with status {s}: {body}"))
    },
    s => TooluError::Protocol(format!("toolu.sh {what} failed with status {s}: {body}")),
  }
}

/// The `error` field of a `{"error":"…"}` body.
fn error_field(body: &str) -> Option<String> {
  let value: serde_json::Value = serde_json::from_str(body).ok()?;
  value.get("error")?.as_str().map(str::to_owned)
}

/// Decode a 2xx body of `what`.
pub(super) fn decode<T: DeserializeOwned>(body: &str, what: &str) -> Result<T, TooluError> {
  serde_json::from_str(body)
    .map_err(|e| TooluError::Protocol(format!("toolu.sh {what} reply did not parse: {e}")))
}

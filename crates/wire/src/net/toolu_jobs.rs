//! toolu.sh `/runner/*` job-claim calls (#206): list, claim, dispatched,
//! heartbeat, complete — what a paired machine's poll loop sends.
//!
//! Every call carries `Authorization: Bearer <host token>` (`tlu_h_…`).
//! Statuses map to [`TooluError`] by the contract's meaning
//! (CodaSignal/toolu.sh `self-hosted-machines.md`, `runner-jobs.md`): 401 is
//! a revoked or unknown machine, 409 a refused or lost claim, 404 an unknown
//! job; 429, 5xx and transport failures are transient, so the loop backs
//! off. Bodies and rules are `protocol::toolu_jobs`.

use std::time::Duration;

use protocol::toolu_jobs::{Claim, CompleteBody, EpochBody, LeaseRenewal, QueuedJobs};
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
    let base = url::Url::parse(api_url).map_err(|e| {
      RunnerError::Config(format!("toolu.sh api_url {api_url:?} is not a URL: {e}"))
    })?;
    if !matches!(base.scheme(), "http" | "https") || base.host_str().is_none() {
      return Err(RunnerError::Config(format!(
        "toolu.sh api_url {api_url:?} must be an absolute http(s) URL"
      )));
    }
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

  /// `<base>/<segments…>`, each segment percent-encoded as one component.
  fn url(&self, segments: &[&str]) -> Result<url::Url, TooluError> {
    let mut url = self.base.clone();
    url
      .path_segments_mut()
      .map_err(|()| TooluError::Protocol(format!("toolu.sh api_url {} has no path", self.base)))?
      .pop_if_empty()
      .extend(segments);
    Ok(url)
  }

  /// Send `request` with the bearer and timeout; the 2xx body, or the
  /// status's [`TooluError`].
  async fn send(&self, request: reqwest::RequestBuilder, what: &str) -> Result<String, TooluError> {
    let response = request
      .bearer_auth(&self.bearer)
      .timeout(REQUEST_TIMEOUT)
      .header(
        "User-Agent",
        concat!("toolu-runner/", env!("CARGO_PKG_VERSION")),
      )
      .send()
      .await
      .map_err(|e| TooluError::Transient(format!("toolu.sh {what} request failed: {e}")))?;
    let status = response.status();
    let body = response.text().await.map_err(|e| {
      TooluError::Transient(format!("reading the toolu.sh {what} reply failed: {e}"))
    })?;
    if status.is_success() {
      return Ok(body);
    }
    Err(failure(what, status, body))
  }
}

/// Map a non-2xx reply of `what` (see the module doc).
fn failure(what: &str, status: StatusCode, body: String) -> TooluError {
  match status {
    StatusCode::UNAUTHORIZED => TooluError::Revoked(error_field(&body).unwrap_or(body)),
    StatusCode::CONFLICT => TooluError::Conflict(body),
    StatusCode::NOT_FOUND => TooluError::NotFound,
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
fn decode<T: DeserializeOwned>(body: &str, what: &str) -> Result<T, TooluError> {
  serde_json::from_str(body)
    .map_err(|e| TooluError::Protocol(format!("toolu.sh {what} reply did not parse: {e}")))
}

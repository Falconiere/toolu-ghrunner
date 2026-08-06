//! Async transport for the Actions Run Service: acquire / renew / complete job.
//!
//! Request and response types live in [`crate::reporting::run_service`]
//! alongside the higher-level reporting wrappers. This file only owns
//! the HTTP.

use shared::RunnerError;

use crate::reporting::run_service::{
  AcquireJobRequest, AcquireJobResponse, CompleteJobRequest, RenewJobRequest, RenewJobResponse,
};

/// Acquire a job from the run service.
///
/// Reads `x-plan-id` and `x-actions-results-token` headers off the
/// response, parses the body into a JSON value, and packages them
/// into an [`AcquireJobResponse`].
///
/// # Errors
///
/// Returns `RunnerError::Protocol` on HTTP or parse failures.
pub async fn acquire_job(
  client: &reqwest::Client,
  run_service_url: &str,
  token: &str,
  request: &AcquireJobRequest,
) -> Result<AcquireJobResponse, RunnerError> {
  let url = format!("{run_service_url}/acquirejob");

  let response = client
    .post(&url)
    .bearer_auth(token)
    .json(request)
    .send()
    .await
    .map_err(|e| RunnerError::Protocol(format!("acquire job failed: {e}")))?;

  let status = response.status();
  if !status.is_success() {
    let body = response.text().await.unwrap_or_default();
    tracing::debug!(status = %status, body_len = body.len(), "acquire job failed");
    return Err(RunnerError::Protocol(format!(
      "acquire job status {status}: see debug log"
    )));
  }

  let plan_id = response
    .headers()
    .get("x-plan-id")
    .and_then(|v| v.to_str().ok())
    .unwrap_or_default()
    .to_owned();

  let run_service_token = response
    .headers()
    .get("x-actions-results-token")
    .and_then(|v| v.to_str().ok())
    .map(ToOwned::to_owned);

  let body = response
    .json::<serde_json::Value>()
    .await
    .map_err(|e| RunnerError::Protocol(format!("acquire job parse: {e}")))?;

  Ok(AcquireJobResponse {
    plan_id,
    body,
    run_service_token,
  })
}

/// Classify a `reqwest::Error` raised while sending a run-service request,
/// or while reading/decoding its response body.
///
/// `is_decode` — 2xx with a body we can't parse — is a protocol bug, so it
/// maps to `RunnerError::Protocol`. Only the `.json()` call site reaches it;
/// `send()` resolves on headers, before any body read, so a `send()` error
/// is never `is_decode`. Everything else (connection refused, DNS failure,
/// a timeout — including one landing mid-`.json()` under a blackhole, which
/// reports `is_timeout`) is transient and maps to `RunnerError::Network`.
/// Load-bearing for the outage watchdog: it feeds only on `Network`-class
/// renewal failures, so a mid-body-read blackhole must not go unclassified.
fn classify_transport_error(what: &str, err: &reqwest::Error) -> RunnerError {
  if err.is_decode() {
    return RunnerError::Protocol(format!("{what} response was not valid JSON: {err}"));
  }
  RunnerError::Network(format!("{what} failed: {err}"))
}

/// Classify a non-2xx run-service response.
///
/// 429 and 5xx are transient (rate-limited or a server hiccup — retrying
/// can succeed); every other non-2xx is definitive. Mirrors the *pattern*
/// of `net/register.rs::mint_failure`, NOT its variants: a definitive
/// failure here is `Protocol`, not `Auth` — a stuck job report is not an
/// auth problem.
///
/// The body is deliberately absent from the message: these errors are
/// `Display`ed at WARN into the durable `_diag/runner.log`, and
/// `SecretMasker` only redacts registered `secrets.*` — server text has no
/// redaction guarantee. It stays on [`log_error_body`]'s `debug!` line.
fn classify_status(what: &str, status: reqwest::StatusCode) -> RunnerError {
  if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
    return RunnerError::Network(format!(
      "{what} failed with status {status} (transient): see debug log"
    ));
  }
  RunnerError::Protocol(format!("{what} failed with status {status}: see debug log"))
}

/// Chars of the error body kept for the DEBUG snippet.
const ERROR_BODY_SNIPPET_CHARS: usize = 200;
/// Bytes read before the body stream is abandoned. Enough to fill
/// [`ERROR_BODY_SNIPPET_CHARS`] even at 4 bytes per char; nothing past the
/// snippet is ever logged, so reading further would only mean holding a
/// bigger allocation (a 5xx HTML page can be arbitrarily large).
const ERROR_BODY_READ_CAP: usize = ERROR_BODY_SNIPPET_CHARS * 4;

/// Log a non-2xx response body for diagnostics, consuming the response.
///
/// Streams at most [`ERROR_BODY_READ_CAP`] bytes rather than buffering the
/// whole body: `bytes_read` is therefore the capped read length, not the
/// response's `Content-Length`.
///
/// The body itself is DEBUG-only (see [`classify_status`] on why it stays
/// out of the error). A *failure to read* it is different: that is itself a
/// transport error on a reachable path, and `shared::startup` pins the
/// filter to `info` unless `TOOLU_RUNNER_ALLOW_VERBOSE=1`, so a DEBUG-only
/// record of it would be invisible on a default deployment. It gets its own
/// WARN line naming the `reqwest::Error`.
async fn log_error_body(what: &str, status: reqwest::StatusCode, mut response: reqwest::Response) {
  let mut buf: Vec<u8> = Vec::new();
  while buf.len() < ERROR_BODY_READ_CAP {
    match response.chunk().await {
      Ok(Some(chunk)) => buf.extend_from_slice(&chunk),
      Ok(None) => break,
      Err(e) => {
        tracing::warn!(
          status = %status,
          error = %e,
          "{what} failed and its error body could not be read"
        );
        return;
      },
    }
  }
  let snippet: String = String::from_utf8_lossy(&buf)
    .chars()
    .take(ERROR_BODY_SNIPPET_CHARS)
    .collect();
  tracing::debug!(status = %status, bytes_read = buf.len(), body = %snippet, "{what} failed");
}

/// Renew a job lock. Called every ~60 seconds during job execution.
///
/// # Errors
///
/// Returns `RunnerError::Network` on a transport failure, HTTP 429, or any
/// 5xx (transient — GitHub is unreachable or unhealthy; retrying can
/// succeed). Returns `RunnerError::Protocol` on any other non-2xx status or
/// a malformed success body (definitive — retrying cannot help).
pub async fn renew_job(
  client: &reqwest::Client,
  run_service_url: &str,
  token: &str,
  request: &RenewJobRequest,
) -> Result<RenewJobResponse, RunnerError> {
  let url = format!("{run_service_url}/renewjob");

  let response = client
    .post(&url)
    .bearer_auth(token)
    .json(request)
    .send()
    .await
    .map_err(|e| classify_transport_error("renew job", &e))?;

  let status = response.status();
  if !status.is_success() {
    log_error_body("renew job", status, response).await;
    return Err(classify_status("renew job", status));
  }

  response
    .json::<RenewJobResponse>()
    .await
    .map_err(|e| classify_transport_error("renew job", &e))
}

/// Complete a job with its final conclusion, step results, and outputs.
///
/// # Errors
///
/// Returns `RunnerError::Network` on a transport failure, HTTP 429, or any
/// 5xx (transient — GitHub is unreachable or unhealthy; retrying can
/// succeed). Returns `RunnerError::Protocol` on any other non-2xx status
/// (definitive — retrying cannot help).
pub async fn complete_job(
  client: &reqwest::Client,
  run_service_url: &str,
  token: &str,
  request: &CompleteJobRequest,
) -> Result<(), RunnerError> {
  let url = format!("{run_service_url}/completejob");

  let response = client
    .post(&url)
    .bearer_auth(token)
    .json(request)
    .send()
    .await
    .map_err(|e| classify_transport_error("complete job", &e))?;

  let status = response.status();
  if !status.is_success() {
    log_error_body("complete job", status, response).await;
    return Err(classify_status("complete job", status));
  }

  Ok(())
}

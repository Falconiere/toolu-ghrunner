//! Results Service summary transport with bounded requests and safe errors.

use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize};
use shared::RunnerError;

const RECEIVER: &str = "twirp/results.services.receiver.Receiver";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// One immutable masked document and its Results Service identities.
pub struct StepSummary<'a> {
  /// Backend workflow run identity.
  pub run_backend_id: &'a str,
  /// Backend job run identity.
  pub job_backend_id: &'a str,
  /// Backend step or embedded summary identity.
  pub step_backend_id: &'a str,
  /// Masked, newline-normalized UTF-8 document.
  pub content: &'a [u8],
}

#[derive(Serialize)]
struct SummaryIds<'a> {
  #[serde(rename = "workflow_run_backend_id")]
  workflow: &'a str,
  #[serde(rename = "workflow_job_run_backend_id")]
  job: &'a str,
  #[serde(rename = "step_backend_id")]
  step: &'a str,
}

#[derive(Serialize)]
struct Metadata<'a> {
  #[serde(flatten)]
  ids: SummaryIds<'a>,
  size: u64,
  uploaded_at: String,
}

#[derive(Deserialize)]
struct SignedSummary {
  #[serde(alias = "summaryUrl")]
  summary_url: String,
  #[serde(alias = "blobStorageType")]
  blob_storage_type: String,
  #[serde(default, alias = "softSizeLimit", deserialize_with = "integer")]
  soft_size_limit: u64,
}

#[derive(Deserialize)]
struct MetadataResult {
  #[serde(default)]
  ok: bool,
}

fn integer<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
  #[derive(Deserialize)]
  #[serde(untagged)]
  enum Integer {
    Number(u64),
    String(String),
  }
  match Integer::deserialize(deserializer)? {
    Integer::Number(number) => Ok(number),
    Integer::String(number) => number.parse().map_err(serde::de::Error::custom),
  }
}

/// Upload a document and publish metadata only after its blob PUT succeeds.
///
/// # Errors
///
/// Returns safe stage/status errors on HTTP failure, invalid replies, rejected
/// metadata, or a server soft-size limit violation. No body or URL is included.
pub async fn upload_step_summary(
  client: &reqwest::Client,
  results_url: &str,
  token: &str,
  summary: &StepSummary<'_>,
) -> Result<(), RunnerError> {
  let ids = SummaryIds {
    workflow: summary.run_backend_id,
    job: summary.job_backend_id,
    step: summary.step_backend_id,
  };
  let signed: SignedSummary = rpc(
    client,
    results_url,
    token,
    "GetStepSummarySignedBlobURL",
    &ids,
  )
  .await?;
  let size = u64::try_from(summary.content.len())
    .map_err(|error| RunnerError::Reporting(format!("summary size conversion: {error}")))?;
  if size > signed.soft_size_limit {
    return Err(failure("summary exceeds Results Service soft size limit"));
  }
  put_blob(client, &signed, summary.content).await?;
  let metadata = Metadata {
    ids,
    size,
    uploaded_at: chrono::Utc::now().to_rfc3339(),
  };
  let response: MetadataResult = rpc(
    client,
    results_url,
    token,
    "CreateStepSummaryMetadata",
    &metadata,
  )
  .await?;
  if !response.ok {
    return Err(failure("CreateStepSummaryMetadata rejected"));
  }
  Ok(())
}

async fn rpc<T: Serialize, R: serde::de::DeserializeOwned>(
  client: &reqwest::Client,
  results_url: &str,
  token: &str,
  method: &str,
  body: &T,
) -> Result<R, RunnerError> {
  let url = format!("{}/{RECEIVER}/{method}", results_url.trim_end_matches('/'));
  let response = client
    .post(url)
    .bearer_auth(token)
    .header(reqwest::header::ACCEPT, "application/json")
    .timeout(REQUEST_TIMEOUT)
    .json(body)
    .send()
    .await
    .map_err(|error| request_error(method, &error))?;
  check_status(method, response.status())?;
  // Do not include serde's error, which may echo the offending input value.
  response
    .json()
    .await
    .map_err(|error| request_error(method, &error))
}

async fn put_blob(
  client: &reqwest::Client,
  signed: &SignedSummary,
  content: &[u8],
) -> Result<(), RunnerError> {
  // Pinned actions/runner ResultsHttpClient: three Azure blob retries and a
  // 30-second network timeout. Never replay the surrounding RPC transaction.
  let body = bytes::Bytes::copy_from_slice(content);
  for attempt in 0..=3_u32 {
    let mut request = client
      .put(&signed.summary_url)
      .timeout(REQUEST_TIMEOUT)
      .header(reqwest::header::CONTENT_TYPE, "text/plain");
    if signed.blob_storage_type == super::results_service::BLOB_STORAGE_AZURE {
      request = request.header("x-ms-blob-type", "BlockBlob");
    }
    let response = request.body(body.clone()).send().await;
    let retryable = match &response {
      Ok(response) => matches!(
        response.status().as_u16(),
        408 | 429 | 500 | 502 | 503 | 504
      ),
      Err(error) => error.is_timeout() || error.is_connect() || error.is_body(),
    };
    if !retryable || attempt == 3 {
      return match response {
        Ok(response) => check_status("summary blob PUT", response.status()),
        Err(_) => Err(failure("summary blob PUT")),
      };
    }
    let delay = response
      .as_ref()
      .ok()
      .and_then(|response| {
        response
          .headers()
          .get(reqwest::header::RETRY_AFTER)?
          .to_str()
          .ok()?
          .parse::<u64>()
          .ok()
      })
      .map_or_else(
        || Duration::from_millis(800 * 2_u64.pow(attempt)),
        |seconds| Duration::from_secs(seconds.min(60)),
      );
    tokio::time::sleep(delay).await;
  }
  Err(failure("summary blob PUT retries exhausted"))
}

fn check_status(stage: &str, status: reqwest::StatusCode) -> Result<(), RunnerError> {
  if status.is_success() {
    Ok(())
  } else {
    Err(RunnerError::Reporting(format!(
      "{stage} failed with HTTP {}",
      status.as_u16()
    )))
  }
}

fn failure(stage: &str) -> RunnerError {
  RunnerError::Reporting(format!("{stage} failed"))
}

#[cfg(test)]
#[path = "tests/step_summary.rs"]
mod tests;

fn request_error(stage: &str, error: &reqwest::Error) -> RunnerError {
  let kind = if error.is_timeout() {
    "timeout"
  } else if error.is_decode() {
    "invalid response"
  } else {
    "transport error"
  };
  RunnerError::Reporting(format!("{stage} failed: {kind}"))
}

//! Async transport for uploading log content to Azure Blob Storage.
//!
//! The decision of which upload mode (BlockBlob vs AppendBlob) to use
//! lives in [`crate::reporting::log_upload`]; this file owns only the
//! raw `PUT` requests.

use reqwest::header::{HeaderMap, HeaderValue};

use shared::RunnerError;

/// Headers for a BlockBlob PUT request.
pub fn block_blob_headers() -> HeaderMap {
  let mut headers = HeaderMap::new();
  headers.insert("x-ms-blob-type", HeaderValue::from_static("BlockBlob"));
  headers.insert(
    "Content-Type",
    HeaderValue::from_static("application/octet-stream"),
  );
  headers
}

/// Headers for creating an AppendBlob (empty body).
pub fn create_append_blob_headers() -> HeaderMap {
  let mut headers = HeaderMap::new();
  headers.insert("x-ms-blob-type", HeaderValue::from_static("AppendBlob"));
  headers.insert("Content-Length", HeaderValue::from_static("0"));
  headers
}

/// Headers for an AppendBlock request.
pub fn append_block_headers(content_length: usize) -> HeaderMap {
  let mut headers = HeaderMap::new();
  headers.insert(
    "Content-Type",
    HeaderValue::from_static("application/octet-stream"),
  );
  if let Ok(val) = HeaderValue::from_str(&content_length.to_string()) {
    headers.insert("Content-Length", val);
  }
  headers
}

/// Single-shot `PUT` for small log content.
///
/// # Errors
///
/// Returns `RunnerError::Protocol` on HTTP failures.
pub async fn upload_block_blob(
  client: &reqwest::Client,
  signed_url: &str,
  content: &[u8],
) -> Result<(), RunnerError> {
  let response = client
    .put(signed_url)
    .headers(block_blob_headers())
    .body(content.to_vec())
    .send()
    .await
    .map_err(|e| RunnerError::Protocol(format!("block blob upload: {e}")))?;

  if !response.status().is_success() {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    tracing::debug!(status = %status, body_len = body.len(), "block blob upload failed");
    return Err(RunnerError::Protocol(
      "block blob upload failed: see debug log".into(),
    ));
  }
  Ok(())
}

/// Streaming `PUT` for large log content (AppendBlob create + block appends).
///
/// # Errors
///
/// Returns `RunnerError::Protocol` on HTTP failures.
pub async fn upload_log(
  client: &reqwest::Client,
  signed_url: &str,
  content: &[u8],
) -> Result<(), RunnerError> {
  let response = client
    .put(signed_url)
    .headers(create_append_blob_headers())
    .send()
    .await
    .map_err(|e| RunnerError::Protocol(format!("create append blob: {e}")))?;

  if !response.status().is_success() {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    tracing::debug!(status = %status, body_len = body.len(), "create append blob failed");
    return Err(RunnerError::Protocol(
      "create append blob failed: see debug log".into(),
    ));
  }

  let chunk_size = 4 * 1024 * 1024;
  let append_url = format!("{signed_url}&comp=appendblock");

  for chunk in content.chunks(chunk_size) {
    let response = client
      .put(&append_url)
      .headers(append_block_headers(chunk.len()))
      .body(chunk.to_vec())
      .send()
      .await
      .map_err(|e| RunnerError::Protocol(format!("append block: {e}")))?;

    if !response.status().is_success() {
      let status = response.status();
      let body = response.text().await.unwrap_or_default();
      tracing::debug!(status = %status, body_len = body.len(), "append block failed");
      return Err(RunnerError::Protocol(
        "append block failed: see debug log".into(),
      ));
    }
  }

  Ok(())
}

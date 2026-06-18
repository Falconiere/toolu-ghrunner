use reqwest::header::{HeaderMap, HeaderValue};

use shared::RunnerError;

/// Threshold above which AppendBlob is used instead of BlockBlob (4 MB).
const APPEND_BLOB_THRESHOLD: usize = 4 * 1024 * 1024;

/// Upload mode for Azure Blob Storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadMode {
  /// Single-shot upload for small content.
  BlockBlob,
  /// Streaming upload for large content.
  AppendBlob,
}

impl UploadMode {
  /// Determine the upload mode based on content size.
  pub fn for_content(size: usize) -> Self {
    if size > APPEND_BLOB_THRESHOLD {
      Self::AppendBlob
    } else {
      Self::BlockBlob
    }
  }
}

/// Handles uploading log content to Azure Blob Storage signed URLs.
pub struct LogUploader;

impl LogUploader {
  /// Upload log content to a signed blob URL.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Protocol` on HTTP failures.
  pub async fn upload(
    client: &reqwest::Client,
    signed_url: &str,
    content: &[u8],
  ) -> Result<(), RunnerError> {
    match UploadMode::for_content(content.len()) {
      UploadMode::BlockBlob => upload_block(client, signed_url, content).await,
      UploadMode::AppendBlob => upload_append(client, signed_url, content).await,
    }
  }

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
}

async fn upload_block(
  client: &reqwest::Client,
  signed_url: &str,
  content: &[u8],
) -> Result<(), RunnerError> {
  let response = client
    .put(signed_url)
    .headers(LogUploader::block_blob_headers())
    .body(content.to_vec())
    .send()
    .await
    .map_err(|e| RunnerError::Protocol(format!("block blob upload: {e}")))?;

  if !response.status().is_success() {
    let body = response.text().await.unwrap_or_default();
    return Err(RunnerError::Protocol(format!(
      "block blob upload failed: {body}"
    )));
  }
  Ok(())
}

async fn upload_append(
  client: &reqwest::Client,
  signed_url: &str,
  content: &[u8],
) -> Result<(), RunnerError> {
  // Create the append blob
  let response = client
    .put(signed_url)
    .headers(LogUploader::create_append_blob_headers())
    .send()
    .await
    .map_err(|e| RunnerError::Protocol(format!("create append blob: {e}")))?;

  if !response.status().is_success() {
    let body = response.text().await.unwrap_or_default();
    return Err(RunnerError::Protocol(format!(
      "create append blob failed: {body}"
    )));
  }

  // Append content in chunks
  let chunk_size = 4 * 1024 * 1024;
  let append_url = format!("{signed_url}&comp=appendblock");

  for chunk in content.chunks(chunk_size) {
    let response = client
      .put(&append_url)
      .headers(LogUploader::append_block_headers(chunk.len()))
      .body(chunk.to_vec())
      .send()
      .await
      .map_err(|e| RunnerError::Protocol(format!("append block: {e}")))?;

    if !response.status().is_success() {
      let body = response.text().await.unwrap_or_default();
      return Err(RunnerError::Protocol(format!(
        "append block failed: {body}"
      )));
    }
  }

  Ok(())
}

/// Format log lines with timestamps for upload.
///
/// Each line gets a timestamp prefix matching GitHub's log viewer format.
pub fn format_log_lines(lines: &[String], started_at: &str) -> String {
  if lines.is_empty() {
    return String::new();
  }

  let mut output = String::new();
  for line in lines {
    output.push_str(started_at);
    output.push(' ');
    output.push_str(line);
    output.push('\n');
  }

  if output.ends_with('\n') {
    output.pop();
  }
  output
}

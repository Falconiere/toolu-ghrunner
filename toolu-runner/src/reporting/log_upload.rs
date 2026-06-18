use reqwest::header::HeaderMap;

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
      UploadMode::BlockBlob => crate::net::upload_block_blob(client, signed_url, content).await,
      UploadMode::AppendBlob => crate::net::upload_log(client, signed_url, content).await,
    }
  }

  /// Headers for a BlockBlob PUT request.
  pub fn block_blob_headers() -> HeaderMap {
    crate::net::block_blob_headers()
  }

  /// Headers for creating an AppendBlob (empty body).
  pub fn create_append_blob_headers() -> HeaderMap {
    crate::net::create_append_blob_headers()
  }

  /// Headers for an AppendBlock request.
  pub fn append_block_headers(content_length: usize) -> HeaderMap {
    crate::net::append_block_headers(content_length)
  }
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

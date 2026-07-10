//! Upload functions for step-level and job-level log blobs.
//!
//! Both follow the same 3-phase pattern:
//! 1. `Get*LogsSignedBlobURL` — request a SAS URL from Results Service
//! 2. PUT gzipped blob to the signed URL (with one retry)
//! 3. `Create*LogsMetadata` — finalize the upload

use std::io::Write;
use std::time::Duration;

use flate2::Compression;
use flate2::write::GzEncoder;
use tracing::warn;

use crate::listener::helpers::ResultsCtx;
use crate::reporting::results_service::{
  CreateJobLogsMetadataRequest, CreateStepLogsMetadataRequest, GetJobLogsSignedBlobUrlRequest,
  GetStepLogsSignedBlobUrlRequest, create_job_logs_metadata, create_step_logs_metadata,
  get_job_logs_signed_blob_url, get_step_logs_signed_blob_url, upload_log_blob,
};

/// Upload compressed step logs: get signed URL, PUT gzipped blob, finalize
/// metadata. Shared by the streamer actor's finalize path and setup_step.
pub async fn upload_compressed_step_logs<'a>(
  rctx: &ResultsCtx<'a>,
  step_backend_id: &'a str,
  lines: &'a [String],
) -> Option<(String, u64)> {
  if lines.is_empty() {
    return None;
  }
  let line_count = lines.len() as u64;
  let blob = gzip_lines(lines);

  let signed = fetch_step_signed_url(rctx, step_backend_id).await?;
  let logs_url = signed.logs_url.clone();

  if !upload_blob_with_retry(
    rctx.client,
    &signed.logs_url,
    &signed.blob_storage_type,
    blob,
  )
  .await
  {
    return None;
  }

  finalize_step_metadata(rctx, step_backend_id, line_count).await;
  Some((logs_url, line_count))
}

/// Upload combined job-level logs: get signed URL, PUT gzipped blob, finalize
/// metadata. Called after all step uploads complete.
pub async fn upload_job_logs(rctx: &ResultsCtx<'_>, lines: &[String]) -> Option<u64> {
  if lines.is_empty() {
    return None;
  }
  let line_count = lines.len() as u64;
  let blob = gzip_lines(lines);

  let signed = fetch_job_signed_url(rctx).await?;

  if !upload_blob_with_retry(
    rctx.client,
    &signed.logs_url,
    &signed.blob_storage_type,
    blob,
  )
  .await
  {
    return None;
  }

  finalize_job_metadata(rctx, line_count).await;
  Some(line_count)
}

/// Gzip-compress log lines using fast compression.
fn gzip_lines(lines: &[String]) -> Vec<u8> {
  let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
  for line in lines {
    let _ = writeln!(encoder, "{line}");
  }
  encoder.finish().unwrap_or_else(|e| {
    warn!(error = %e, "gzip encoder finish failed");
    Vec::new()
  })
}

async fn fetch_step_signed_url(
  rctx: &ResultsCtx<'_>,
  step_backend_id: &str,
) -> Option<crate::reporting::results_service::SignedBlobUrlResponse> {
  let url_req = GetStepLogsSignedBlobUrlRequest {
    workflow_job_run_backend_id: rctx.job_backend_id.to_owned(),
    workflow_run_backend_id: rctx.run_backend_id.to_owned(),
    step_backend_id: step_backend_id.to_owned(),
  };
  match get_step_logs_signed_blob_url(rctx.client, rctx.results_url, rctx.token, &url_req).await {
    Ok(s) => Some(s),
    Err(e) => {
      warn!(error = %e, step_backend_id, "get step logs signed URL failed");
      None
    },
  }
}

async fn finalize_step_metadata(rctx: &ResultsCtx<'_>, step_backend_id: &str, line_count: u64) {
  let meta_req = CreateStepLogsMetadataRequest {
    workflow_job_run_backend_id: rctx.job_backend_id.to_owned(),
    workflow_run_backend_id: rctx.run_backend_id.to_owned(),
    step_backend_id: step_backend_id.to_owned(),
    uploaded_at: chrono::Utc::now().to_rfc3339(),
    line_count,
  };
  if let Err(e) =
    create_step_logs_metadata(rctx.client, rctx.results_url, rctx.token, &meta_req).await
  {
    warn!(error = %e, step_backend_id, "create step logs metadata failed");
  }
}

async fn fetch_job_signed_url(
  rctx: &ResultsCtx<'_>,
) -> Option<crate::reporting::results_service::SignedBlobUrlResponse> {
  let url_req = GetJobLogsSignedBlobUrlRequest {
    workflow_job_run_backend_id: rctx.job_backend_id.to_owned(),
    workflow_run_backend_id: rctx.run_backend_id.to_owned(),
  };
  match get_job_logs_signed_blob_url(rctx.client, rctx.results_url, rctx.token, &url_req).await {
    Ok(s) => Some(s),
    Err(e) => {
      warn!(error = %e, "get job logs signed URL failed");
      None
    },
  }
}

async fn finalize_job_metadata(rctx: &ResultsCtx<'_>, line_count: u64) {
  let meta_req = CreateJobLogsMetadataRequest {
    workflow_job_run_backend_id: rctx.job_backend_id.to_owned(),
    workflow_run_backend_id: rctx.run_backend_id.to_owned(),
    uploaded_at: chrono::Utc::now().to_rfc3339(),
    line_count,
  };
  if let Err(e) =
    create_job_logs_metadata(rctx.client, rctx.results_url, rctx.token, &meta_req).await
  {
    warn!(error = %e, "create job logs metadata failed");
  }
}

async fn upload_blob_with_retry(
  client: &reqwest::Client,
  sas_url: &str,
  blob_storage_type: &str,
  blob: Vec<u8>,
) -> bool {
  if upload_log_blob(client, sas_url, blob_storage_type, blob.clone(), true)
    .await
    .is_ok()
  {
    return true;
  }
  warn!("log blob upload failed, retrying in 2s");
  tokio::time::sleep(Duration::from_secs(2)).await;
  match upload_log_blob(client, sas_url, blob_storage_type, blob, true).await {
    Ok(()) => true,
    Err(e) => {
      warn!(error = %e, "log blob upload retry failed");
      false
    },
  }
}

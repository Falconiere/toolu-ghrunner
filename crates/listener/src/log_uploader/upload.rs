//! Upload functions for step-level and job-level log blobs.
//!
//! Both follow the same 3-phase pattern:
//! 1. `Get*LogsSignedBlobURL` — request a SAS URL from Results Service
//! 2. PUT gzipped blob to the signed URL (with one retry)
//! 3. `Create*LogsMetadata` — finalize the upload

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tracing::warn;

use crate::helpers::ResultsCtx;
use crate::log_spool::gzip_file;
use wire::reporting::results_service::{
  CreateJobLogsMetadataRequest, CreateStepLogsMetadataRequest, GetJobLogsSignedBlobUrlRequest,
  GetStepLogsSignedBlobUrlRequest, create_job_logs_metadata, create_step_logs_metadata,
  get_job_logs_signed_blob_url, get_step_logs_signed_blob_url, upload_log_file,
};

/// Upload a spooled step log, then remove its plain and compressed files.
pub async fn upload_step_logs(
  rctx: &ResultsCtx<'_>,
  step_backend_id: &str,
  spool: PathBuf,
  line_count: u64,
) -> Option<(String, u64)> {
  let cleanup = Arc::new(SpoolCleanup::new(spool));
  upload_step_logs_inner(rctx, step_backend_id, &cleanup, line_count).await
}

async fn upload_step_logs_inner(
  rctx: &ResultsCtx<'_>,
  step_backend_id: &str,
  cleanup: &Arc<SpoolCleanup>,
  line_count: u64,
) -> Option<(String, u64)> {
  if line_count == 0 {
    return None;
  }
  let signed = fetch_step_signed_url(rctx, step_backend_id).await?;
  let logs_url = signed.logs_url.clone();
  let compressed = compress_spool(cleanup).await?;
  let uploaded = upload_file_with_retry(
    rctx.client,
    &signed.logs_url,
    &signed.blob_storage_type,
    &compressed,
  )
  .await;
  if !uploaded {
    return None;
  }
  finalize_step_metadata(rctx, step_backend_id, line_count).await;
  Some((logs_url, line_count))
}

/// Upload a spooled combined job log, then remove its plain and compressed files.
pub async fn upload_job_logs(
  rctx: &ResultsCtx<'_>,
  spool: PathBuf,
  line_count: u64,
) -> Option<u64> {
  let cleanup = Arc::new(SpoolCleanup::new(spool));
  upload_job_logs_inner(rctx, &cleanup, line_count).await
}

async fn upload_job_logs_inner(
  rctx: &ResultsCtx<'_>,
  cleanup: &Arc<SpoolCleanup>,
  line_count: u64,
) -> Option<u64> {
  if line_count == 0 {
    return None;
  }
  let signed = fetch_job_signed_url(rctx).await?;
  let compressed = compress_spool(cleanup).await?;
  let uploaded = upload_file_with_retry(
    rctx.client,
    &signed.logs_url,
    &signed.blob_storage_type,
    &compressed,
  )
  .await;
  if !uploaded {
    return None;
  }
  finalize_job_metadata(rctx, line_count).await;
  Some(line_count)
}

async fn compress_spool(cleanup: &Arc<SpoolCleanup>) -> Option<PathBuf> {
  let keep_alive = Arc::clone(cleanup);
  match tokio::task::spawn_blocking(move || gzip_file(&keep_alive.plain)).await {
    Ok(Ok(compressed)) => Some(compressed),
    Ok(Err(error)) => {
      warn!(error = %error, "log compression failed");
      None
    },
    Err(error) => {
      warn!(error = %error, "log compression task failed");
      None
    },
  }
}

struct SpoolCleanup {
  plain: PathBuf,
  compressed: PathBuf,
}

impl SpoolCleanup {
  fn new(plain: PathBuf) -> Self {
    let mut name = plain.as_os_str().to_os_string();
    name.push(".gz");
    Self {
      plain,
      compressed: PathBuf::from(name),
    }
  }
}

impl Drop for SpoolCleanup {
  fn drop(&mut self) {
    remove_spool_file(&self.plain);
    remove_spool_file(&self.compressed);
  }
}

fn remove_spool_file(path: &Path) {
  if let Err(error) = std::fs::remove_file(path)
    && error.kind() != std::io::ErrorKind::NotFound
  {
    warn!(error = %error, "could not remove log spool file");
  }
}

async fn fetch_step_signed_url(
  rctx: &ResultsCtx<'_>,
  step_backend_id: &str,
) -> Option<wire::reporting::results_service::SignedBlobUrlResponse> {
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
) -> Option<wire::reporting::results_service::SignedBlobUrlResponse> {
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

async fn upload_file_with_retry(
  client: &reqwest::Client,
  sas_url: &str,
  blob_storage_type: &str,
  path: &Path,
) -> bool {
  if upload_log_file(client, sas_url, blob_storage_type, path)
    .await
    .is_ok()
  {
    return true;
  }
  warn!("log blob upload failed, retrying in 2s");
  tokio::time::sleep(Duration::from_secs(2)).await;
  match upload_log_file(client, sas_url, blob_storage_type, path).await {
    Ok(()) => true,
    Err(e) => {
      warn!(error = %e, "log blob upload retry failed");
      false
    },
  }
}

#[cfg(test)]
#[path = "tests/upload.rs"]
mod tests;

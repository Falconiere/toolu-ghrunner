//! Azure-Blob-compatible upload/download endpoint, mounted at
//! `/_toolu/blob/{token}`.
//!
//! GitHub's `actions/cache` and BuildKit upload cache archives to an Azure Blob
//! signed URL and download via ranged GETs; a later Twirp layer hands out URLs
//! pointing here. Tokens are opaque in-memory nonces (see [`token`]), so this
//! module is testable in isolation by minting tokens directly.
//!
//! Every response — write or read, success or `403` — carries the four headers
//! Azure clients require (`x-ms-request-id`, `ETag`, `Last-Modified`,
//! `x-ms-version`). BuildKit's Go client unconditionally derefs the request id,
//! so a missing header panics the build.

mod block_list;
mod get;
mod put_blob;
mod put_block;
/// In-memory blob token registry (opaque nonces → upload/download targets).
pub mod token;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Path as AxumPath, Query, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::Response;
use axum::routing::put;
use chrono::Utc;
use serde::Deserialize;
use shared::RunnerError;
use uuid::Uuid;

pub use token::{BlobRegistry, BlobTarget};

use super::cas::{CasStore, LeaseSet};

/// Shared state for the blob endpoint: the token registry, the CAS store reads
/// stream from, the GC lease set, and the staging directory uploads land in.
pub struct BlobState {
  /// Token registry (shared, cloneable) resolving nonces to targets.
  pub registry: BlobRegistry,
  /// Content-addressed store backing download streaming.
  pub store: CasStore,
  /// Read leases held for the duration of a download so GC cannot race it.
  pub leases: LeaseSet,
  /// `cas/staging` directory; upload objects and per-block temp data live here.
  pub staging_root: PathBuf,
}

/// PUT query string: `comp` selects the operation, `blockid` names a block.
#[derive(Deserialize)]
struct PutQuery {
  comp: Option<String>,
  blockid: Option<String>,
}

/// Upper bound on a single blob request body buffered in memory.
///
/// A single-shot Put Blob carries the whole object (the toolkit single-shots up
/// to 128 MiB, staging larger objects as ≤64 MiB blocks), so the 2 MiB axum
/// default is too small — but an unbounded body lets one request OOM the runner.
/// 256 MiB gives headroom above the single-shot ceiling while staying bounded.
const MAX_BLOB_BODY: usize = 256 * 1024 * 1024;

/// Build the blob router mounting every Azure op on `/_toolu/blob/{token}`.
pub fn blob_router(state: Arc<BlobState>) -> Router {
  Router::new()
    .route(
      "/_toolu/blob/{token}",
      put(put_entry).head(get::head).get(get::get),
    )
    .layer(DefaultBodyLimit::max(MAX_BLOB_BODY))
    .with_state(state)
}

/// `PUT /_toolu/blob/{token}` — dispatch on the query string to a blob op.
async fn put_entry(
  State(state): State<Arc<BlobState>>,
  AxumPath(token): AxumPath<String>,
  Query(query): Query<PutQuery>,
  headers: HeaderMap,
  body: Bytes,
) -> Response {
  match put_dispatch(&state, &token, &query, &headers, body).await {
    Ok(resp) => resp,
    Err(e) => error_500(&e),
  }
}

/// Resolve the token, then route to Put Block / Put Block List / Put Blob.
async fn put_dispatch(
  state: &BlobState,
  token: &str,
  query: &PutQuery,
  headers: &HeaderMap,
  body: Bytes,
) -> Result<Response, RunnerError> {
  let staging = match state.registry.get(token) {
    Some(BlobTarget::Upload { staging, .. }) => staging,
    Some(BlobTarget::Download { .. }) | None => return forbidden(),
  };
  match query.comp.as_deref() {
    Some("block") => {
      let id = query
        .blockid
        .as_deref()
        .ok_or_else(|| RunnerError::Cache("put block missing blockid".into()))?;
      put_block::store_block(&staging, id, body).await?;
    },
    Some("blocklist") => block_list::commit(&staging, body).await?,
    Some(other) => return Err(RunnerError::Cache(format!("unsupported comp={other}"))),
    None => put_blob_or_reject(&staging, headers, body).await?,
  }
  ok_response(StatusCode::CREATED, token)
}

/// Put Blob requires `x-ms-blob-type: BlockBlob`; anything else is rejected.
async fn put_blob_or_reject(
  staging: &Path,
  headers: &HeaderMap,
  body: Bytes,
) -> Result<(), RunnerError> {
  if header_eq(headers, "x-ms-blob-type", "BlockBlob") {
    put_blob::put(staging, body).await
  } else {
    Err(RunnerError::Cache(
      "PUT without comp requires x-ms-blob-type: BlockBlob".into(),
    ))
  }
}

/// Case-insensitive check that header `name` equals `value`.
fn header_eq(headers: &HeaderMap, name: &str, value: &str) -> bool {
  headers
    .get(name)
    .and_then(|v| v.to_str().ok())
    .is_some_and(|v| v.eq_ignore_ascii_case(value))
}

/// An empty-body response with `status` and the four required Azure headers.
///
/// # Errors
/// `RunnerError::Cache` if a required header value cannot be constructed.
fn ok_response(status: StatusCode, etag: &str) -> Result<Response, RunnerError> {
  let mut resp = Response::new(Body::empty());
  *resp.status_mut() = status;
  add_required_headers(&mut resp, etag)?;
  Ok(resp)
}

/// A `403 Forbidden` for a missing or expired token, with the required headers.
///
/// `403` (never `500`) is the recoverable "re-request the URL" signal clients
/// retry on.
///
/// # Errors
/// `RunnerError::Cache` if a required header value cannot be constructed.
fn forbidden() -> Result<Response, RunnerError> {
  let mut resp = Response::new(Body::empty());
  *resp.status_mut() = StatusCode::FORBIDDEN;
  add_required_headers(&mut resp, "expired")?;
  Ok(resp)
}

/// A best-effort `500` for a genuine internal error (not token expiry).
fn error_500(e: &RunnerError) -> Response {
  let mut resp = Response::new(Body::from(e.to_string()));
  *resp.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
  let _ = add_required_headers(&mut resp, "error");
  resp
}

/// Insert the four Azure-required response headers onto `resp`.
///
/// # Errors
/// `RunnerError::Cache` if a header value is not valid (never, for our inputs).
fn add_required_headers(resp: &mut Response, etag: &str) -> Result<(), RunnerError> {
  let request_id = Uuid::new_v4().to_string();
  let last_modified = Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string();
  let quoted_etag = format!("\"{etag}\"");
  let headers = resp.headers_mut();
  headers.insert(HeaderName::from_static("x-ms-request-id"), hv(&request_id)?);
  headers.insert(HeaderName::from_static("x-ms-version"), hv("2021-08-06")?);
  headers.insert(header::ETAG, hv(&quoted_etag)?);
  headers.insert(header::LAST_MODIFIED, hv(&last_modified)?);
  Ok(())
}

/// Build a `HeaderValue` from an ASCII string.
///
/// # Errors
/// `RunnerError::Cache` if `s` is not a valid header value.
fn hv(s: &str) -> Result<HeaderValue, RunnerError> {
  HeaderValue::from_str(s).map_err(|e| RunnerError::Cache(format!("bad header value: {e}")))
}

/// Remove `cas/staging` entries whose mtime is older than `older_than`,
/// returning the number removed. Abandoned uploads (no commit, no Finalize)
/// are swept here; a fresh in-flight upload is left alone.
///
/// # Errors
/// `RunnerError::Io` if the staging directory or an entry cannot be read or
/// removed.
pub fn sweep_staging(staging_root: &Path, older_than: Duration) -> Result<usize, RunnerError> {
  let read = match std::fs::read_dir(staging_root) {
    Ok(read) => read,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
    Err(e) => return Err(RunnerError::Io(e)),
  };
  let now = SystemTime::now();
  let mut removed = 0usize;
  for entry in read {
    let entry = entry.map_err(RunnerError::Io)?;
    let meta = entry.metadata().map_err(RunnerError::Io)?;
    let modified = meta.modified().map_err(RunnerError::Io)?;
    if now.duration_since(modified).unwrap_or_default() > older_than {
      remove_entry(&entry.path(), meta.is_dir())?;
      removed += 1;
    }
  }
  Ok(removed)
}

/// Remove one staging entry (file or directory); a missing entry is a no-op.
fn remove_entry(path: &Path, is_dir: bool) -> Result<(), RunnerError> {
  let result = if is_dir {
    std::fs::remove_dir_all(path)
  } else {
    std::fs::remove_file(path)
  };
  match result {
    Ok(()) => Ok(()),
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
    Err(e) => Err(RunnerError::Io(e)),
  }
}

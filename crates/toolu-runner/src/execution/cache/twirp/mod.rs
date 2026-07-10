//! GitHub Actions Cache Service v2 Twirp RPCs, served as JSON at
//! `/twirp/github.actions.results.api.v1.CacheService/<Method>`.
//!
//! Three methods — `CreateCacheEntry`, `FinalizeCacheEntryUpload`,
//! `GetCacheEntryDownloadURL` — back the CAS store and the Azure-Blob endpoint.
//! Wire fields are proto snake_case; int64 fields (`size_bytes`, `entry_id`)
//! are decimal strings; the client's `metadata` field is never read. A cache
//! miss is a normal `{"ok":false}` with HTTP 200 — Twirp errors are reserved
//! for transport failures, and a 500 would fail the job rather than let it
//! rebuild.

/// Bearer auth and host resolution shared by the three handlers.
pub mod auth;
/// The three RPC handlers, one file per method.
pub mod handlers;
/// Snake_case request / response wire types.
pub mod types;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::post;

use super::blob::{BlobRegistry, BlobState, blob_router};
use super::cas::{CacheIndex, CasStore, LeaseSet};
use super::scope::CacheScopes;
use super::trust::TrustLevel;

/// Shared state for the Twirp cache handlers.
pub struct TwirpState {
  /// Content-addressed store backing ingest and download streaming.
  pub store: CasStore,
  /// Persistent `(scope, version, key)` → manifest index.
  pub index: CacheIndex,
  /// Blob-token registry shared with the blob endpoint.
  pub registry: BlobRegistry,
  /// Read leases shared with the blob endpoint.
  pub leases: LeaseSet,
  /// The job's write scope and read ladder.
  pub scopes: CacheScopes,
  /// Write trust: an untrusted job may not write a protected scope.
  pub trust: TrustLevel,
  /// Protected scopes (branch refs) an untrusted job may not write.
  pub protected: Vec<String>,
  /// Runtime token every request must present as `Authorization: Bearer`.
  pub bearer: String,
  /// `cas/staging` directory uploads stage into.
  pub staging_root: PathBuf,
  /// TTL for a minted upload token.
  pub upload_ttl: Duration,
  /// TTL for a minted download token.
  pub download_ttl: Duration,
}

/// The Twirp `CacheService` method prefix all three routes share.
const PREFIX: &str = "/twirp/github.actions.results.api.v1.CacheService";

/// Build the Twirp-only router serving the three `CacheService` RPCs.
pub fn twirp_router(state: TwirpState) -> Router {
  let state = Arc::new(state);
  Router::new()
    .route(
      &format!("{PREFIX}/CreateCacheEntry"),
      post(handlers::create::create_cache_entry),
    )
    .route(
      &format!("{PREFIX}/FinalizeCacheEntryUpload"),
      post(handlers::finalize::finalize_cache_entry_upload),
    )
    .route(
      &format!("{PREFIX}/GetCacheEntryDownloadURL"),
      post(handlers::download::get_cache_entry_download_url),
    )
    .with_state(state)
}

/// Build the combined cache app: Twirp RPCs plus the Azure-Blob endpoint,
/// sharing one `CasStore` root, one `BlobRegistry`, and one `LeaseSet`.
///
/// The shared pieces are cloned into a [`BlobState`] before `state` is moved
/// into [`twirp_router`]; the clones are additional handles onto the same
/// on-disk store and the same in-memory token / lease maps, so a URL minted by
/// a Twirp handler resolves in the blob router and streams the bytes a Finalize
/// ingested.
pub fn cache_router(state: TwirpState) -> Router {
  let blob_state = Arc::new(BlobState {
    registry: state.registry.clone(),
    store: state.store.clone(),
    leases: state.leases.clone(),
    staging_root: state.staging_root.clone(),
  });
  twirp_router(state).merge(blob_router(blob_state))
}

//! Legacy GitHub Actions Cache v1 REST protocol, re-hosted on the CAS.
//!
//! The five `/_apis/artifactcache/*` routes (`cache` lookup, `caches` reserve,
//! `caches/{id}` chunk upload + finalize, `download/{id}` streamed read) speak
//! the exact wire shapes `actions/cache@v1`–`v4.1` expect, but store content in
//! the same content-addressed [`CasStore`] and restart-safe [`CacheIndex`] the
//! v2 Twirp layer uses. Offline mode mounts this router so a hermetic run keeps
//! its cache across restarts. Bearer auth (constant-time) guards every route
//! except `download`, whose archive URL is an unguessable, pre-signed capability
//! real clients fetch with no `Authorization` header.

/// The five v1 REST route handlers plus their private helpers.
pub mod handlers;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};

use super::cas::{CacheIndex, CasStore, LeaseSet, Manifest};
use super::mint_capability_token;
use super::scope::CacheScopes;
use super::trust::TrustLevel;

/// One pending upload: its cache key, version, and the staging file chunks land in.
pub(crate) struct Reservation {
  /// Client-supplied, opaque cache key.
  key: String,
  /// Client-supplied, opaque cache version.
  version: String,
  /// Staging file `PATCH` chunks are written into, then `ingest`ed on finalize.
  staging: PathBuf,
}

/// Shared state for the v1 REST cache handlers, backed by the CAS.
///
/// The reservation and download registries are `Arc<Mutex<..>>` so every
/// handler (the router shares one `Arc<V1State>`) sees the same in-memory maps:
/// a `reserve` records an id the later `PATCH`/`finalize` resolve, and a
/// `lookup` records a download id the later `GET download/{id}` streams.
pub struct V1State {
  /// Content-addressed store backing ingest and download streaming.
  pub store: CasStore,
  /// Persistent `(scope, version, key)` → manifest index.
  pub index: CacheIndex,
  /// Read leases held for the duration of a download so GC cannot race it.
  pub leases: LeaseSet,
  /// The job's write scope and read ladder.
  pub scopes: CacheScopes,
  /// Write trust: an untrusted job may not write a protected scope.
  pub trust: TrustLevel,
  /// Protected scopes (branch refs) an untrusted job may not write.
  pub protected: Vec<String>,
  /// Runtime token every request must present as `Authorization: Bearer`.
  pub bearer: String,
  /// `cas/staging` directory reserved uploads stage into.
  pub staging_root: PathBuf,
  /// Reserved (not-yet-finalized) uploads by numeric cache id.
  reservations: Arc<Mutex<HashMap<u64, Reservation>>>,
  /// Minted, unguessable download tokens resolving to the manifest they stream.
  downloads: Arc<Mutex<HashMap<String, Manifest>>>,
  /// Monotonic allocator for reserve cache ids.
  next_id: Arc<AtomicU64>,
}

/// Construction inputs for [`V1State`], grouped so the constructor stays under
/// the argument-count ceiling as new policy fields are added.
pub struct V1Inputs {
  /// Content-addressed store backing ingest and download streaming.
  pub store: CasStore,
  /// Persistent `(scope, version, key)` → manifest index.
  pub index: CacheIndex,
  /// Read leases held for the duration of a download.
  pub leases: LeaseSet,
  /// The job's write scope and read ladder.
  pub scopes: CacheScopes,
  /// Write trust: an untrusted job may not write a protected scope.
  pub trust: TrustLevel,
  /// Protected scopes (branch refs) an untrusted job may not write.
  pub protected: Vec<String>,
  /// Runtime token every request must present as `Authorization: Bearer`.
  pub bearer: String,
  /// `cas/staging` directory reserved uploads stage into.
  pub staging_root: PathBuf,
}

impl V1State {
  /// Build v1 state over a CAS store + index with empty registries.
  pub fn new(inputs: V1Inputs) -> Self {
    let V1Inputs {
      store,
      index,
      leases,
      scopes,
      trust,
      protected,
      bearer,
      staging_root,
    } = inputs;
    Self {
      store,
      index,
      leases,
      scopes,
      trust,
      protected,
      bearer,
      staging_root,
      reservations: Arc::new(Mutex::new(HashMap::new())),
      downloads: Arc::new(Mutex::new(HashMap::new())),
      next_id: Arc::new(AtomicU64::new(1)),
    }
  }

  /// Allocate the next monotonic reserve cache id.
  fn alloc_id(&self) -> u64 {
    self.next_id.fetch_add(1, Ordering::Relaxed)
  }

  /// Record a reservation under `id`; a poisoned lock drops it (the finalize misses).
  fn insert_reservation(&self, id: u64, reservation: Reservation) {
    if let Ok(mut map) = self.reservations.lock() {
      map.insert(id, reservation);
    }
  }

  /// The staging file of reservation `id`, if it is still pending.
  fn reservation_staging(&self, id: u64) -> Option<PathBuf> {
    let map = self.reservations.lock().ok()?;
    Some(map.get(&id)?.staging.clone())
  }

  /// Remove and return reservation `id` (finalize consumes it exactly once).
  fn take_reservation(&self, id: u64) -> Option<Reservation> {
    self.reservations.lock().ok()?.remove(&id)
  }

  /// Register `manifest` under a fresh, unguessable token and return it.
  ///
  /// A poisoned registry lock still returns the token but never stores it, so
  /// the download it names 404s; the WARN is the only trace of the poison, so
  /// it must not stay silent.
  fn register_download(&self, manifest: Manifest) -> String {
    let token = mint_download_token();
    if let Ok(mut map) = self.downloads.lock() {
      map.insert(token.clone(), manifest);
    } else {
      tracing::warn!("v1 download registry poisoned; minted token not stored (will 404 on use)");
    }
    token
  }

  /// The manifest a download token points at, or `None` if unknown.
  fn download_manifest(&self, token: &str) -> Option<Manifest> {
    self.downloads.lock().ok()?.get(token).cloned()
  }
}

/// Mint an unguessable v1 download token.
///
/// The v1 download URL is served without a bearer (real clients send none), so
/// the token itself is the capability; it comes from the cache layer's single
/// shared mint, [`mint_capability_token`], so its format and entropy can never
/// diverge from the v2 blob tokens.
fn mint_download_token() -> String {
  mint_capability_token()
}

/// Upper bound on a single v1 request body buffered in memory. `actions/cache`
/// `PATCH`es multi-MiB chunks that exceed axum's 2 MiB default, but an unbounded
/// body lets one request OOM the runner; 256 MiB clears real chunk sizes while
/// staying bounded.
const MAX_V1_BODY: usize = 256 * 1024 * 1024;

/// Build the v1 REST router mounting the five `artifactcache` routes.
pub fn v1_router(state: V1State) -> Router {
  let state = Arc::new(state);
  Router::new()
    .route("/_apis/artifactcache/cache", get(handlers::lookup))
    .route("/_apis/artifactcache/caches", post(handlers::reserve))
    .route(
      "/_apis/artifactcache/caches/{cache_id}",
      post(handlers::finalize).patch(handlers::upload_chunk),
    )
    .route(
      "/_apis/artifactcache/download/{token}",
      get(handlers::download),
    )
    .layer(DefaultBodyLimit::max(MAX_V1_BODY))
    .with_state(state)
}

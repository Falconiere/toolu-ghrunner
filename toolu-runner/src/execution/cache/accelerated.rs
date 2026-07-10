//! Accelerated services mode: one per-job cache app that serves both cache
//! protocols from a single content-addressed store and reverse-proxies every
//! other path to the real `ACTIONS_RESULTS_URL`.
//!
//! [`accelerated_app`] assembles the v2 Twirp `CacheService` + the Azure-Blob
//! endpoint (via [`cache_router`]) and the legacy v1 REST protocol (via
//! [`v1_router`]) over one shared [`CasStore`] / [`CacheIndex`] /
//! [`BlobRegistry`] / [`LeaseSet`]. Their route prefixes are disjoint
//! (`/twirp/…`, `/_toolu/blob/…`, `/_apis/artifactcache/…`), so merging is
//! unambiguous. Everything else falls through to [`proxied_app`], which
//! forwards it verbatim to real GitHub with the `Authorization` header intact.

use std::path::PathBuf;
use std::time::Duration;

use axum::Router;
use reqwest::Client;

use super::blob::BlobRegistry;
use super::cas::{CacheIndex, CasStore, LeaseSet};
use super::proxy::proxied_app;
use super::scope::CacheScopes;
use super::trust::TrustLevel;
use super::twirp::{TwirpState, cache_router};
use super::v1::{V1Inputs, V1State, v1_router};

/// TTL for a minted blob upload token (one job's upload window).
const UPLOAD_TTL: Duration = Duration::from_secs(3600);
/// TTL for a minted blob download token (one job's restore window).
const DOWNLOAD_TTL: Duration = Duration::from_secs(3600);

/// The shared CAS pieces + proxy upstream that back one accelerated cache app.
///
/// The store / index are cheap value handles onto the same on-disk root; the
/// registry / leases share their in-memory maps through an internal `Arc`. All
/// are cloned into the two protocol states so a URL minted by one protocol
/// resolves in the other and streams the same bytes.
pub struct AcceleratedInputs {
  /// Content-addressed store backing ingest and download streaming.
  pub store: CasStore,
  /// Persistent `(scope, version, key)` → manifest index.
  pub index: CacheIndex,
  /// Blob-token registry shared between the Twirp and blob routers.
  pub registry: BlobRegistry,
  /// Read leases shared across every protocol so GC never races a restore.
  pub leases: LeaseSet,
  /// The job's write scope and read ladder.
  pub scopes: CacheScopes,
  /// Write trust: an untrusted job may not write a protected scope.
  pub trust: TrustLevel,
  /// Protected scopes (branch refs) an untrusted job may not write.
  pub protected: Vec<String>,
  /// Runtime token every local request must present as `Authorization: Bearer`.
  pub bearer: String,
  /// `cas/staging` directory uploads stage into.
  pub staging_root: PathBuf,
  /// Real `ACTIONS_RESULTS_URL` non-cache paths are proxied to.
  pub upstream_results_url: String,
  /// HTTP client the reverse proxy forwards upstream with.
  pub client: Client,
}

/// Build the accelerated cache app: v2 Twirp + Azure blob + v1 REST over one
/// shared CAS, reverse-proxying every other path to the real results URL.
///
/// The Twirp state clones each shared handle; the v1 state consumes the
/// originals — both therefore address the same on-disk store and the same
/// in-memory token / lease maps.
pub fn accelerated_app(inputs: AcceleratedInputs) -> Router {
  let AcceleratedInputs {
    store,
    index,
    registry,
    leases,
    scopes,
    trust,
    protected,
    bearer,
    staging_root,
    upstream_results_url,
    client,
  } = inputs;

  let twirp = cache_router(TwirpState {
    store: store.clone(),
    index: index.clone(),
    registry,
    leases: leases.clone(),
    scopes: scopes.clone(),
    trust,
    protected: protected.clone(),
    bearer: bearer.clone(),
    staging_root: staging_root.clone(),
    upload_ttl: UPLOAD_TTL,
    download_ttl: DOWNLOAD_TTL,
  });
  let v1 = v1_router(V1State::new(V1Inputs {
    store,
    index,
    leases,
    scopes,
    trust,
    protected,
    bearer,
    staging_root,
  }));

  proxied_app(twirp.merge(v1), upstream_results_url, client)
}

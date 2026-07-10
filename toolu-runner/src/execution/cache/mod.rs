/// Accelerated services mode: one local cache app over both protocols + proxy.
pub mod accelerated;
/// Azure-Blob-compatible upload/download endpoint + in-memory token registry.
pub mod blob;
/// Content-addressed store: FastCDC chunks keyed by BLAKE3.
pub mod cas;
/// Selective reverse proxy to the real `ACTIONS_RESULTS_URL` for non-cache paths.
pub mod proxy;
/// Read-ladder and write-scope resolution for the cache index.
pub mod scope;
/// Generic HTTP server harness that binds a `Router` with graceful shutdown.
pub mod server;
/// Optional cold-storage tiers behind the L1 CAS (S3 chunk + manifest mirror).
pub mod tier;
/// Write-side trust classification for the cache index.
pub mod trust;
/// GitHub Actions Cache Service v2 Twirp RPCs (JSON) over the CAS + blob store.
pub mod twirp;
/// Legacy v1 REST cache protocol re-hosted on the CAS store + index.
pub mod v1;

pub use accelerated::{AcceleratedInputs, accelerated_app};
pub use blob::{BlobRegistry, BlobState, BlobTarget, blob_router};
pub use proxy::proxied_app;
pub use tier::{BlobKind, L2Tier};
pub use twirp::{TwirpState, cache_router, twirp_router};

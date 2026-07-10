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

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;

/// Mint an opaque capability token: 32 CSPRNG bytes, base64url-encoded.
///
/// These tokens are bearer capabilities — the v2 blob upload/download URLs and
/// the v1 archive download URL (fetched with no `Authorization` header at all)
/// are authorized by nothing but the token — so they must be infeasible to
/// guess: `rand::thread_rng` is a ChaCha CSPRNG reseeded from OS entropy,
/// unlike the predictable `fastrand` used for non-secret poll jitter. Every
/// cache-layer capability token must come from this one helper so a format or
/// entropy change can never land on only one mint site.
pub(crate) fn mint_capability_token() -> String {
  let mut bytes = [0u8; 32];
  rand::thread_rng().fill_bytes(&mut bytes);
  URL_SAFE_NO_PAD.encode(bytes)
}

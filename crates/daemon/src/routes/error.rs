//! The daemon's error body shape — `{ "error": "<message>" }`, pinned by
//! `vpsErrorBodySchema` in `packages/api/src/providers/vps/client.ts`
//! (toolu.sh repo) — and the helper every non-2xx route response goes
//! through, so no handler can hand-roll a different shape.
//!
//! `Deserialize` is not exercised by the router itself (the daemon only
//! ever serializes this body) — it exists so `tests/fixture.rs` can
//! round-trip the shared contract fixture's error body through this exact
//! type rather than a hand-written duplicate.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

/// The pinned error body: `{ "error": "<message>" }`.
#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorBody {
  /// The human-readable failure reason.
  pub error: String,
}

/// Build a JSON error response with `status` and the pinned `{ error }`
/// shape.
pub fn error_response(status: StatusCode, message: impl Into<String>) -> Response {
  (
    status,
    Json(ErrorBody {
      error: message.into(),
    }),
  )
    .into_response()
}

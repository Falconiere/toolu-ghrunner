//! Bearer-auth middleware: every route requires a valid
//! `Authorization: Bearer <token>` header, verified against the token file
//! via `crate::auth::verify_bearer`. Wraps the whole router — see
//! `crate::routes::build_router`.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::Response;

use crate::auth::verify_bearer;
use crate::routes::error::error_response;

/// The body every rejection answers with, whatever the reason.
///
/// One fixed string, deliberately: `AuthError`'s own `Display` distinguishes a
/// missing header from a bad scheme from a mismatched token, and
/// `AuthError::TokenFile` interpolates a `ConfigError` that carries the token
/// file's absolute path. On the wire that is an oracle — anyone who can reach
/// the tunnel learns where this box keeps its bearer token, and which of the
/// four ways their credential was wrong. The detail goes to the daemon's log,
/// where the operator who needs it already is.
const UNAUTHORIZED_BODY: &str = "unauthorized";

/// Reject any request whose `Authorization` header does not verify against
/// the current (or rotating-previous) bearer token with a 401; otherwise
/// pass it through to the next layer/handler unchanged.
pub async fn require_bearer(
  State(token_file): State<Arc<PathBuf>>,
  request: Request,
  next: Next,
) -> Response {
  let header = request
    .headers()
    .get(AUTHORIZATION)
    .and_then(|value| value.to_str().ok());

  match verify_bearer(header, &token_file) {
    Ok(()) => next.run(request).await,
    Err(err) => {
      tracing::warn!(error = %err, "rejecting a request with invalid bearer credentials");
      error_response(StatusCode::UNAUTHORIZED, UNAUTHORIZED_BODY)
    },
  }
}

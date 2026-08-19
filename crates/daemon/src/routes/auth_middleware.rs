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
    Err(err) => error_response(StatusCode::UNAUTHORIZED, err.to_string()),
  }
}

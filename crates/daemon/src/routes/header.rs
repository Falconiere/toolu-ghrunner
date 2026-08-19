//! The `sh-toolu-daemon: 1` response header every response must carry —
//! `crates/daemon/README.md`'s wire-contract note: without it, a
//! Cloudflare-generated 429 or challenge page in front of the tunnel is
//! indistinguishable from one this daemon produced, and `client.ts`'s
//! `classifyVpsStatus` would misclassify it.

use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;

/// The header name every response carries.
const DAEMON_HEADER_NAME: HeaderName = HeaderName::from_static("sh-toolu-daemon");
/// The header's fixed value.
const DAEMON_HEADER_VALUE: HeaderValue = HeaderValue::from_static("1");

/// Stamp [`DAEMON_HEADER_NAME`] onto every response this router produces —
/// success, error, auth rejection or extractor rejection alike. Applied as
/// the outermost layer in `crate::routes::build_router`, so it wraps every
/// other layer and handler.
pub async fn add_daemon_header(request: Request, next: Next) -> Response {
  let mut response = next.run(request).await;
  response
    .headers_mut()
    .insert(DAEMON_HEADER_NAME, DAEMON_HEADER_VALUE);
  response
}

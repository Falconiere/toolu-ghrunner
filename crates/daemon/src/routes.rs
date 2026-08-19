//! HTTP routing for the daemon's `vps_hosts` contract: `POST /v1/jobs`,
//! `DELETE /v1/jobs/{containerId}` and `DELETE /v1/jobs?jobId=…`. See
//! `crates/daemon/README.md` for the three invariants toolu.sh's client
//! imposes on this crate, and `packages/api/src/providers/vps/client.ts`
//! (toolu.sh repo) for the pinned wire contract this module matches
//! byte-for-byte — field names, status codes and the `{ "error": "…" }`
//! error body are not this crate's to invent.
//!
//! Container orchestration itself is deliberately not here: [`backend`]
//! defines the narrow port the routes call through, so this module depends
//! only on "something that can create/destroy/reap a job container", never
//! on bollard directly. `crates/daemon` wires a bollard-backed
//! implementation of that port; tests wire an in-process recorder.

/// Wire types for the request/response bodies this router accepts and
/// returns — the JSON shapes `client.ts` sends and reads.
pub mod wire;

/// The narrow port between HTTP and container orchestration: create,
/// destroy-by-container-id, reap-by-job-id.
pub mod backend;

/// Router state shared across requests: the job backend, the resource gate
/// and the bearer token file path.
pub mod state;

/// The `sh-toolu-daemon: 1` response header every response must carry.
pub mod header;

/// Bearer-auth middleware wrapping every route.
pub mod auth_middleware;

/// The `{ "error": "<message>" }` error body shape and the helper every
/// non-2xx response goes through.
pub mod error;

/// The three route handlers: create, destroy-by-container-id,
/// reap-by-job-id.
pub mod handlers;

use std::sync::Arc;

use axum::Router;
use axum::routing::{delete, post};

use backend::JobBackend;
use state::AppState;

/// Build the daemon's [`Router`] over `state`.
///
/// Layer order (outermost last, per `Router::layer`'s stacking rule):
/// bearer-auth runs first and can short-circuit with a 401; the
/// `sh-toolu-daemon` header layer wraps everything else, so it stamps every
/// response this router produces — success, auth rejection, extractor
/// rejection (e.g. malformed JSON) and unmatched-route fallback alike.
pub fn build_router<B: JobBackend>(state: AppState<B>) -> Router {
  let token_file = Arc::clone(&state.token_file);

  Router::new()
    .route(
      "/v1/jobs",
      post(handlers::create_job::<B>).delete(handlers::reap_job::<B>),
    )
    .route(
      "/v1/jobs/{container_id}",
      delete(handlers::destroy_job::<B>),
    )
    .with_state(state)
    .layer(axum::middleware::from_fn_with_state(
      token_file,
      auth_middleware::require_bearer,
    ))
    .layer(axum::middleware::from_fn(header::add_daemon_header))
}

#[cfg(test)]
#[path = "tests/routes.rs"]
mod tests;

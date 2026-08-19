//! Shared router state: the job backend, the resource gate, the reaper's
//! start queue and created-container map, and the bearer token file path
//! auth checks against.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::gate::Gate;
use crate::reaper::{CreatedContainers, StartQueue};
use crate::routes::backend::JobBackend;

/// State threaded through every route via axum's `State` extractor, and
/// separately into the bearer-auth middleware (which only needs
/// `token_file`) — see `crate::routes::build_router`.
#[derive(Clone)]
pub struct AppState<B: JobBackend> {
  /// Creates, destroys and reaps job containers — bollard-backed in
  /// production, an in-process recorder in tests.
  pub backend: B,
  /// The daemon's resource gate: queue depth and vCPU/memory accounting.
  /// Shared across requests behind a `std::sync::Mutex` — every critical
  /// section is pure accounting and never held across an `.await`.
  pub gate: Arc<Mutex<Gate>>,
  /// Jobs whose containers exist but have not started yet — pushed to by
  /// `crate::routes::handlers::create_job` once a create finishes, drained
  /// by `crate::reaper::reconcile` as budget frees. Shared with whatever
  /// drives the reconciliation tick, not just this router.
  pub start_queue: Arc<Mutex<StartQueue>>,
  /// The container id recorded for each job whose create has finished —
  /// what makes a redelivered create idempotent, and what `reconcile` reads
  /// to know which container to `docker start` for a promoted job id.
  pub created_containers: Arc<Mutex<CreatedContainers>>,
  /// Bearer token file path, re-read fresh on every request by
  /// `crate::auth::verify_bearer`.
  pub token_file: Arc<PathBuf>,
}

impl<B: JobBackend> AppState<B> {
  /// Build router state from an already-constructed backend and gate. The
  /// start queue and created-container map always begin empty — nothing has
  /// created a container yet.
  pub fn new(backend: B, gate: Gate, token_file: PathBuf) -> Self {
    Self {
      backend,
      gate: Arc::new(Mutex::new(gate)),
      start_queue: Arc::new(Mutex::new(StartQueue::new())),
      created_containers: Arc::new(Mutex::new(CreatedContainers::new())),
      token_file: Arc::new(token_file),
    }
  }
}

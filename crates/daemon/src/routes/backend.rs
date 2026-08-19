//! The seam between HTTP routing and container orchestration: the narrow
//! set of operations the routes need to create, destroy and reap job
//! containers. Deliberately narrow — starting an already-admitted
//! container, watching for its exit, and rebuilding state from Docker
//! labels at startup are backend-internal concerns this trait does not
//! surface. `crate::routes::handlers::create_job` only records that a
//! container exists and queues it to start (`crate::reaper::StartQueue`,
//! `crate::reaper::CreatedContainers`, both carried on
//! `crate::routes::state::AppState`); actually starting it, and noticing
//! when it exits, is `crate::docker::DockerBackend::tick`'s job, driven
//! independently of any one request. The resource gate's admit/release
//! accounting stays in `crate::gate`, called directly from
//! `crate::routes::handlers`.
//!
//! `crates/daemon` implements this trait against bollard; tests implement
//! it as a real in-process recorder — see
//! `crates/daemon/src/tests/routes.rs`. The Docker-backed behaviour itself
//! is exercised live elsewhere, not through this trait's tests.

use std::fmt;
use std::future::Future;

use crate::routes::wire::CreateJobRequest;

/// What [`JobBackend::create`] returns on success.
#[derive(Debug, Clone)]
pub struct CreateJobResult {
  /// The created container's id.
  pub container_id: String,
}

/// Why [`JobBackend::create`] could not create a container. Both variants
/// map to a 503 at the HTTP layer — `crates/daemon/README.md`'s "never pull
/// inside a request" invariant makes the pinned image not yet being
/// resident the documented case; [`Self::Other`] covers any other
/// backend-level fault (e.g. the Docker daemon itself being unreachable)
/// the same way, since both are conditions the caller should simply retry.
#[derive(Debug)]
pub enum CreateError {
  /// The pinned image has not finished pulling yet.
  ImageNotResident,
  /// Any other backend-level failure, with a human-readable cause.
  Other(String),
}

impl fmt::Display for CreateError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::ImageNotResident => write!(f, "pinned image is not resident yet"),
      Self::Other(reason) => write!(f, "{reason}"),
    }
  }
}

impl std::error::Error for CreateError {}

/// What [`JobBackend::destroy`] found for the container id it was given.
///
/// It reports the job id rather than a bare "removed" flag because the
/// resource gate is keyed by GitHub's job id — the key the reap route
/// addresses and the `sh.toolu.job-id` label carries — while this route is
/// addressed by container id. Without the job id the handler would have no
/// way to release the budget it just destroyed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DestroyOutcome {
  /// A container matched and was removed.
  Removed {
    /// The removed container's `sh.toolu.job-id` label. `None` only for a
    /// container this daemon did not create, which therefore holds no gate
    /// budget to release.
    job_id: Option<String>,
  },
  /// Nothing matched — the idempotent-404 case `destroyVpsInstance` in
  /// `client.ts` relies on.
  NotFound,
}

/// A backend-level failure on an operation with no documented failure mode
/// of its own — mapped to a 500 at the HTTP layer.
#[derive(Debug)]
pub struct BackendError(pub String);

impl fmt::Display for BackendError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}", self.0)
  }
}

impl std::error::Error for BackendError {}

/// The port between HTTP routing ([`crate::routes`]) and container
/// orchestration. `Clone + Send + Sync + 'static` because a value of this
/// type is cloned into every request's [`crate::routes::state::AppState`].
pub trait JobBackend: Clone + Send + Sync + 'static {
  /// Create one job container (`docker create`, not `docker start`) and
  /// return its id. Must not pull the image — see
  /// `crates/daemon/README.md`.
  ///
  /// The caller has already admitted `req.job_ref.job_id` to the resource
  /// gate by the time this runs, so an implementation must treat a reap for
  /// that job id arriving mid-create as binding: the container it is about to
  /// create must then never start.
  ///
  /// # Errors
  ///
  /// Returns [`CreateError`] when the container could not be created —
  /// mapped to a 503 at the HTTP layer.
  fn create(
    &self,
    req: &CreateJobRequest,
  ) -> impl Future<Output = Result<CreateJobResult, CreateError>> + Send;

  /// Destroy the container identified by `container_id`, reporting which job
  /// it served so the caller can release that job's budget — see
  /// [`DestroyOutcome`].
  ///
  /// # Errors
  ///
  /// Returns [`BackendError`] on any other failure — mapped to a 500 at the
  /// HTTP layer.
  fn destroy(
    &self,
    container_id: &str,
  ) -> impl Future<Output = Result<DestroyOutcome, BackendError>> + Send;

  /// Best-effort kill of whatever container currently serves `job_id` — the
  /// terminal-timeout outcome `reapByJobId` in `client.ts` drives. Always
  /// resolves: the daemon answers 204 whether or not anything matched, so
  /// there is nowhere honest for a failure here to surface at the HTTP
  /// layer; implementations log their own failures instead.
  fn reap(&self, job_id: &str) -> impl Future<Output = ()> + Send;
}

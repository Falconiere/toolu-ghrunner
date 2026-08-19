//! The three route handlers `crate::routes::build_router` wires up:
//! create, destroy-by-container-id and reap-by-job-id.

use std::sync::PoisonError;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::gate::{AdmitError, JobId, JobSize};
use crate::routes::backend::{DestroyOutcome, JobBackend};
use crate::routes::error::error_response;
use crate::routes::state::AppState;
use crate::routes::wire::{CreateJobRequest, CreateJobResponse, ReapQuery};

/// `POST /v1/jobs`: admit `jobRef.jobId` to the resource gate, then `docker
/// create` via the backend.
///
/// **Admission comes first, before any Docker call.** The gate is keyed by
/// GitHub's job id, and recording that id is what makes
/// `DELETE /v1/jobs?jobId=…` able to address a create that is still in
/// flight — the client's recovery path when its 10-second request timed out
/// and the container's fate is unknown. A container that does not exist yet
/// carries no label, so a reap arriving before the id was recorded would
/// no-op: the daemon would finish creating, the runner would boot against a
/// JIT config toolu has already marked `failed`, and it would serve a real
/// job for up to six hours with no destroy handle.
///
/// A malformed body is rejected with the JSON extractor's own status (a 4xx)
/// and the pinned `{ error }` shape rather than axum's default plain-text
/// rejection body.
pub async fn create_job<B: JobBackend>(
  State(state): State<AppState<B>>,
  body: Result<Json<CreateJobRequest>, JsonRejection>,
) -> Response {
  let req = match body {
    Ok(Json(req)) => req,
    Err(rejection) => return error_response(rejection.status(), rejection.body_text()),
  };

  let job_id = JobId::new(req.job_ref.job_id.clone());
  let size = JobSize {
    vcpu: req.size.vcpu,
    memory_mb: req.size.memory_mb,
  };

  if let Err(err) = admit(&state, &job_id, size) {
    return match err {
      // GitHub redelivers `workflow_job.queued` at least once, so a repeat
      // jobId the gate still tracks is real and benign, not this daemon's
      // fault — see `duplicate_create_response`.
      AdmitError::DuplicateJobId => duplicate_create_response(&state, &req.job_ref.job_id),
      AdmitError::ExceedsBudget | AdmitError::QueueFull => admit_error_response(err),
    };
  }

  match state.backend.create(&req).await {
    Ok(created) => {
      remember_created(&state, &job_id, &created.container_id);
      created_response(&created.container_id)
    },
    // Nothing was created, so there is nothing to undo — only the admission
    // to give back. Both `CreateError` variants are conditions the caller
    // should retry, which `classifyVpsStatus` reads off a 503.
    Err(err) => {
      release(&state, &job_id);
      error_response(StatusCode::SERVICE_UNAVAILABLE, err.to_string())
    },
  }
}

/// Answer a duplicate `jobId` exactly as the first, successful create did:
/// 201 with the same container id, no second admission consumed and no
/// second `docker create` issued. This is what makes GitHub's at-least-once
/// webhook delivery survivable — a 500 here (what an unhandled
/// `AdmitError::DuplicateJobId` used to produce) classifies as `unavailable`
/// on the client, which triggers fallback to another host **and** a cooldown
/// on this one, for a condition that is not actually a fault.
///
/// A duplicate that arrives before the first create has recorded its
/// container id is a narrow race, not the redelivery case above — GitHub's
/// retries are minutes apart, not concurrent with the original request. That
/// case answers 503, the same retryable shape as any other in-flight backend
/// condition, rather than fabricate a container id.
fn duplicate_create_response<B: JobBackend>(state: &AppState<B>, job_id: &str) -> Response {
  let created = state
    .created_containers
    .lock()
    .unwrap_or_else(PoisonError::into_inner);
  match created.existing(job_id) {
    Some(container_id) => created_response(container_id),
    None => error_response(
      StatusCode::SERVICE_UNAVAILABLE,
      "job is already being created".to_owned(),
    ),
  }
}

/// Record `job_id`'s newly created container and queue it to start —
/// `crate::reaper::reconcile` drains the queue as budget frees.
fn remember_created<B: JobBackend>(state: &AppState<B>, job_id: &JobId, container_id: &str) {
  let mut created = state
    .created_containers
    .lock()
    .unwrap_or_else(PoisonError::into_inner);
  created.record(job_id.as_str(), container_id);
  drop(created);

  let mut queue = state
    .start_queue
    .lock()
    .unwrap_or_else(PoisonError::into_inner);
  queue.push(job_id.clone());
}

/// Admit `job_id` to the gate. The lock is held for the accounting only,
/// never across an `.await`.
fn admit<B: JobBackend>(
  state: &AppState<B>,
  job_id: &JobId,
  size: JobSize,
) -> Result<(), AdmitError> {
  let mut gate = state.gate.lock().unwrap_or_else(PoisonError::into_inner);
  gate.admit(job_id, size)
}

/// Give `job_id`'s queue slot and budget back to the gate, and forget
/// whatever this daemon recorded about its container: its idempotent
/// create-response memory, and its place in the start queue if it never
/// started. Idempotent throughout: an id nothing tracks anymore is simply
/// not there.
fn release<B: JobBackend>(state: &AppState<B>, job_id: &JobId) {
  let mut gate = state.gate.lock().unwrap_or_else(PoisonError::into_inner);
  gate.release(job_id);
  drop(gate);

  let mut queue = state
    .start_queue
    .lock()
    .unwrap_or_else(PoisonError::into_inner);
  queue.remove(job_id);
  drop(queue);

  let mut created = state
    .created_containers
    .lock()
    .unwrap_or_else(PoisonError::into_inner);
  created.forget(job_id.as_str());
}

/// The status a refused admission answers with. The queue ceiling is the
/// daemon's one and only 429 — see `crate::gate`. `ExceedsBudget` cannot
/// occur against a correct caller (host selection already filters by
/// budget), so it is this daemon's own fault and says so with a 500.
/// `DuplicateJobId` is handled by `duplicate_create_response` before this is
/// ever reached for it — its 500 here is a defensive fallback only, not a
/// path `create_job` takes.
fn admit_error_response(err: AdmitError) -> Response {
  let status = match err {
    AdmitError::QueueFull => StatusCode::TOO_MANY_REQUESTS,
    AdmitError::ExceedsBudget | AdmitError::DuplicateJobId => StatusCode::INTERNAL_SERVER_ERROR,
  };
  error_response(status, err.to_string())
}

/// Build the `201 { containerId }` success response.
fn created_response(container_id: &str) -> Response {
  (
    StatusCode::CREATED,
    Json(CreateJobResponse {
      container_id: container_id.to_owned(),
    }),
  )
    .into_response()
}

/// `DELETE /v1/jobs/{containerId}`: 204 when found and removed, 404 when
/// already gone — the idempotent-destroy shape `destroyVpsInstance` in
/// `client.ts` relies on. The removed container's own `sh.toolu.job-id`
/// label names the gate entry to release; a container carrying none was not
/// created by this daemon and holds no budget of ours.
pub async fn destroy_job<B: JobBackend>(
  State(state): State<AppState<B>>,
  Path(container_id): Path<String>,
) -> Response {
  match state.backend.destroy(&container_id).await {
    Ok(DestroyOutcome::Removed { job_id }) => {
      if let Some(job_id) = job_id {
        release(&state, &JobId::new(job_id));
      }
      StatusCode::NO_CONTENT.into_response()
    },
    Ok(DestroyOutcome::NotFound) => error_response(StatusCode::NOT_FOUND, "container not found"),
    Err(err) => error_response(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
  }
}

/// `DELETE /v1/jobs?jobId=…`: always 204, whether or not `jobId` resolved
/// to anything — `client.ts`'s `reapByJobId`, and the mechanism
/// `vps/verify.ts` (toolu.sh repo) relies on to probe credentials with a
/// sentinel id that matches nothing.
///
/// The gate entry is released after the backend has done its work: a reaped
/// job is over, and its budget would otherwise stay consumed until something
/// else noticed the container was gone. Releasing an id the gate never held
/// is a no-op, which is exactly what the sentinel probe does.
pub async fn reap_job<B: JobBackend>(
  State(state): State<AppState<B>>,
  Query(query): Query<ReapQuery>,
) -> Response {
  state.backend.reap(&query.job_id).await;
  release(&state, &JobId::new(query.job_id));
  StatusCode::NO_CONTENT.into_response()
}

//! The three route handlers `crate::routes::build_router` wires up:
//! create, destroy-by-container-id and reap-by-job-id.

use std::sync::PoisonError;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::gate::{AdmitError, JobId, JobSize};
use crate::routes::backend::{DestroyOutcome, JobBackend, ReapOutcome};
use crate::routes::error::error_response;
use crate::routes::state::AppState;
use crate::routes::wire::{CreateJobRequest, CreateJobResponse, ReapQuery};

/// What the detached create task decided, in the terms the handler renders
/// into a response. The task returns this rather than a [`Response`] so the
/// bookkeeping and the HTTP shape stay separable — and so the task's output
/// is a plain, inspectable value.
#[derive(Debug)]
enum CreateOutcome {
  /// The container exists and this daemon is tracking it.
  Created {
    /// The created container's id.
    container_id: String,
  },
  /// The container was created, but the job stopped being this daemon's
  /// while that was happening (reaped, destroyed, or reconciled away), so
  /// nothing will ever start it.
  Abandoned,
  /// Nothing was created; the admission has already been given back.
  Failed {
    /// The backend's own words, for the 503 body.
    reason: String,
  },
}

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
/// **Everything after that admission runs on a detached task.** The client
/// aborts with `AbortSignal.timeout` at ten seconds — the exact case this
/// daemon's create/start split exists for — and axum drops a handler future
/// at its next await when that happens. Awaiting the backend inline meant the
/// admission taken above stood while neither the "record the container" nor
/// the "give the slot back" path ever ran: one disconnect leaked a queue slot
/// permanently, and `TOOLU_DAEMON_QUEUE_MAX` of them wedged the host at 429
/// with nothing able to recover it (`crate::reaper::reconcile`'s exit pass
/// only walks jobs the gate marks *running*). A spawned task is not cancelled
/// when the future awaiting it is dropped, so the bookkeeping always
/// completes.
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

  if let Some(refusal) = refuse_foreign_image(&state, &req) {
    return refusal;
  }

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

  let worker_state = state.clone();
  let worker_job_id = job_id.clone();
  let task =
    tokio::spawn(async move { run_admitted_create(&worker_state, &worker_job_id, &req).await });

  match task.await {
    Ok(outcome) => create_outcome_response(outcome),
    // The task panicked — it is never aborted, so nothing else joins with an
    // error. The bookkeeping may not have run, and a leaked admission is the
    // one failure nothing else recovers from, so the slot is given back here.
    // A container the task did manage to create is left to the deadline pass
    // of `crate::reaper::reconcile`, which removes it off its own label
    // whether or not the gate ever knew about it.
    Err(join_err) => {
      release(&state, &job_id);
      error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        format!("the create for this job did not finish: {join_err}"),
      )
    },
  }
}

/// The create itself, run where a client disconnect cannot cancel it. Owns
/// every bookkeeping decision that follows the backend call: record the
/// container, or give the admission back.
async fn run_admitted_create<B: JobBackend>(
  state: &AppState<B>,
  job_id: &JobId,
  req: &CreateJobRequest,
) -> CreateOutcome {
  match state.backend.create(req).await {
    Ok(created) => {
      if remember_created(state, job_id, &created.container_id) {
        return CreateOutcome::Created {
          container_id: created.container_id,
        };
      }
      abandon_untracked_container(state, job_id, &created.container_id).await;
      CreateOutcome::Abandoned
    },
    // Nothing was created, so there is nothing to undo — only the admission
    // to give back. Both `CreateError` variants are conditions the caller
    // should retry, which `classifyVpsStatus` reads off a 503.
    Err(err) => {
      release(state, job_id);
      CreateOutcome::Failed {
        reason: err.to_string(),
      }
    },
  }
}

/// Remove a container whose job the gate no longer tracks. Nothing will ever
/// start it — it is in neither the start queue nor the created map — so
/// leaving it would hold disk until its six-hour deadline brought the reaper
/// round. A removal that fails is logged and left to that reaper, which
/// removes anything past its own `sh.toolu.deadline` label regardless of
/// whether this process ever tracked it.
async fn abandon_untracked_container<B: JobBackend>(
  state: &AppState<B>,
  job_id: &JobId,
  container_id: &str,
) {
  tracing::warn!(
    job_id = job_id.as_str(),
    container_id,
    "the job was released while its container was being created; removing it unstarted"
  );
  match state.backend.destroy(container_id).await {
    Ok(DestroyOutcome::Removed { .. } | DestroyOutcome::NotFound) => {},
    Err(err) => {
      tracing::warn!(
        job_id = job_id.as_str(),
        container_id,
        error = %err,
        "could not remove the container of a job that was released mid-create"
      );
    },
  }
}

/// Render what the detached create task decided.
fn create_outcome_response(outcome: CreateOutcome) -> Response {
  match outcome {
    CreateOutcome::Created { container_id } => created_response(&container_id),
    CreateOutcome::Abandoned => error_response(
      StatusCode::SERVICE_UNAVAILABLE,
      "job was released while its container was being created".to_owned(),
    ),
    CreateOutcome::Failed { reason } => error_response(StatusCode::SERVICE_UNAVAILABLE, reason),
  }
}

/// Refuse — loudly — a create for any image other than the one this host
/// pins, before it consumes a queue slot.
///
/// `TOOLU_DAEMON_IMAGE` is the only image `crate::prepull` ever pulls or
/// keeps resident, while the image a create runs comes off the request
/// (`vps_hosts.image_ref` on toolu.sh's side). When the two drift, every
/// create 404s inside Docker, classifies as "the image is not resident yet",
/// and answers a 503 that `vpsDispositionFor` reads as fallback plus a
/// five-minute cooldown re-stamped on every delivery — a total outage for
/// this host that looks like a transient pull. The refusal here is the same
/// 503 (this host genuinely cannot serve the job) but says exactly which two
/// values disagree, in the response body and in the daemon's own log.
fn refuse_foreign_image<B: JobBackend>(
  state: &AppState<B>,
  req: &CreateJobRequest,
) -> Option<Response> {
  if req.image == *state.pinned_image {
    return None;
  }

  let pinned: &str = &state.pinned_image;
  tracing::error!(
    job_id = req.job_ref.job_id.as_str(),
    requested_image = req.image.as_str(),
    pinned_image = pinned,
    "refusing a create: TOOLU_DAEMON_IMAGE and the requested image have drifted, so this host \
     can serve no job at all until they match"
  );
  Some(error_response(
    StatusCode::SERVICE_UNAVAILABLE,
    format!(
      "image mismatch: this host pins {pinned} (TOOLU_DAEMON_IMAGE) and cannot run {}",
      req.image
    ),
  ))
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
///
/// Returns `false`, having recorded nothing, when the gate no longer tracks
/// `job_id`. A reap (or a destroy, or a deadline the reaper acted on) can
/// land between the backend's create finishing and this call; it clears the
/// gate entry, the queue and the created map, and re-populating two of the
/// three afterwards leaves entries `reconcile` can never drain — `try_start`
/// answers `None` forever for a job the gate does not hold, so nothing is
/// ever promoted and nothing is ever released. That is unbounded growth, one
/// pair per occurrence, plus a 201 naming a container the reap has already
/// removed.
///
/// The gate is therefore consulted and both maps written under one critical
/// section, in the crate's lock order (gate → registry → queue → created), so
/// no release can slip between the check and the writes.
fn remember_created<B: JobBackend>(
  state: &AppState<B>,
  job_id: &JobId,
  container_id: &str,
) -> bool {
  let gate = state.gate.lock().unwrap_or_else(PoisonError::into_inner);
  if !gate.tracks(job_id) {
    return false;
  }

  let mut queue = state
    .start_queue
    .lock()
    .unwrap_or_else(PoisonError::into_inner);
  let mut created = state
    .created_containers
    .lock()
    .unwrap_or_else(PoisonError::into_inner);
  created.record(job_id.as_str(), container_id);
  queue.push(job_id.clone());
  true
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
/// The gate entry is released after the backend has done its work, and only
/// when that work actually [`ReapOutcome::Settled`]: a reaped job is over,
/// and its budget would otherwise stay consumed until something else noticed
/// the container was gone. Releasing an id the gate never held is a no-op,
/// which is exactly what the sentinel probe does.
///
/// A reap the backend could not settle — a removal that failed, or a
/// container listing Docker would not answer — keeps its budget. The
/// container may still be running and still holding real vCPU and memory;
/// handing that share to the next job would overcommit the box by exactly
/// its size. `crate::reaper::reconcile` releases it when the container
/// actually exits or its deadline passes.
pub async fn reap_job<B: JobBackend>(
  State(state): State<AppState<B>>,
  Query(query): Query<ReapQuery>,
) -> Response {
  match state.backend.reap(&query.job_id).await {
    ReapOutcome::Settled => release(&state, &JobId::new(query.job_id)),
    ReapOutcome::Unresolved => {
      tracing::warn!(
        job_id = query.job_id.as_str(),
        "the reap could not be confirmed; keeping this job's budget until the reaper sees its \
         container gone"
      );
    },
  }
  StatusCode::NO_CONTENT.into_response()
}

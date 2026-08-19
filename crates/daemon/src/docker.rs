//! Docker orchestration: the bollard-backed [`JobBackend`] the routes call
//! through in production.
//!
//! Three properties from `crates/daemon/README.md` are implemented here, not
//! merely respected:
//!
//! - **Create, never start.** [`DockerBackend::create`] issues `docker
//!   create` and returns the id. Starting is the resource gate's decision and
//!   arrives with the start loop; returning immediately is what makes the
//!   201 honest inside the client's 10-second timeout.
//! - **Never pull inside a request.** Nothing here pulls. A create against an
//!   image that is not resident comes back from Docker as a 404 and leaves as
//!   [`CreateError::ImageNotResident`], which the route answers 503 — a cold
//!   pull inside the request would blow a timeout that is
//!   terminal-with-cooldown on the client's side.
//! - **Creates survive a client disconnect.** The client aborts with
//!   `AbortSignal.timeout`, which cancels the axum handler at its next await —
//!   potentially inside `create_container`. Both orchestrating calls run on a
//!   detached task keyed by the job id and the handler awaits its result, so a
//!   disconnect can never leave Docker work half done.
//!
//! [`spec`] decides what a job container *is*; [`registry`] holds the
//! tombstone that lets a reap cancel a create still in flight;
//! [`DockerBackend::tick`] carries out the reaper-and-scheduler decisions
//! [`crate::reaper::reconcile`] makes — starting whatever the resource gate
//! now has room for, and force-removing whatever exited on its own or
//! outlived its `sh.toolu.deadline` label.

/// The tombstone registry: in-flight creates, live containers, and recently
/// reaped job ids.
pub mod registry;

/// The pure container specification: units, labels, environment.
pub mod spec;

/// Reading this daemon's own containers back off Docker at startup, for
/// `crate::adopt`.
pub mod inventory;

/// Pulling and checking the pinned image, for `crate::prepull`.
pub mod image;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use bollard::Docker;
use bollard::errors::Error as DockerError;
use bollard::models::{ContainerCreateBody, ContainerSummary, ContainerSummaryStateEnum};
use bollard::query_parameters::{
  CreateContainerOptions, InspectContainerOptions, ListContainersOptions, RemoveContainerOptions,
  StartContainerOptions,
};

use crate::gate::{Gate, JobId};
use crate::reaper::{self, CreatedContainers, StartQueue, StartRequest};
use crate::routes::backend::{
  BackendError, CreateError, CreateJobResult, DestroyOutcome, JobBackend, ReapOutcome,
};
use crate::routes::wire::CreateJobRequest;
use registry::{BeginOutcome, FinishOutcome, JobRegistry};
use spec::{JobLabels, LABEL_JOB_ID};

/// Docker's status code for "no such container", and — on `POST
/// /containers/create` — for "no such image", which is the only 404 that
/// route produces.
const HTTP_NOT_FOUND: u16 = 404;

/// The bollard-backed [`JobBackend`]: one sysbox-isolated container per job.
///
/// Cloning is cheap and shares everything — bollard's client is `Arc`-backed
/// and the registry sits behind one — which is what lets each request clone
/// it into a detached task.
#[derive(Clone)]
pub struct DockerBackend {
  /// The Docker daemon client.
  docker: Docker,
  /// Container runtime every job container is created under —
  /// `TOOLU_DAEMON_RUNTIME`, `sysbox-runc` by default.
  runtime: Arc<str>,
  /// Job ids in flight, live and recently reaped. Every critical section is
  /// pure accounting and is never held across an `.await`.
  jobs: Arc<Mutex<JobRegistry>>,
}

impl DockerBackend {
  /// Build a backend over an already-connected client.
  pub fn new(docker: Docker, runtime: &str) -> Self {
    Self {
      docker,
      runtime: Arc::from(runtime),
      jobs: Arc::new(Mutex::new(JobRegistry::new())),
    }
  }

  /// Connect to the Docker daemon `DOCKER_HOST` names, falling back to the
  /// local socket, and create job containers under `runtime`.
  ///
  /// # Errors
  ///
  /// Returns the bollard error when the endpoint cannot be used — a daemon
  /// this process cannot reach must fail loudly at startup rather than turn
  /// every job into a 503.
  pub fn connect(runtime: &str) -> Result<Self, DockerError> {
    Ok(Self::new(Docker::connect_with_defaults()?, runtime))
  }

  /// The registry, with a poisoned lock recovered rather than propagated: the
  /// guarded state is a plain map, so a panic elsewhere cannot have left it
  /// torn, and refusing every later job over it would be worse than the
  /// panic.
  fn registry(&self) -> std::sync::MutexGuard<'_, JobRegistry> {
    self.jobs.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// The body of a create, run on a detached task: `docker create`, then the
  /// tombstone check that decides whether the container it produced may be
  /// kept. The task owns its own clone of the backend and of `job_id`, which
  /// is what lets it outlive the request that spawned it.
  async fn run_create(
    &self,
    job_id: &str,
    config: ContainerCreateBody,
  ) -> Result<CreateJobResult, CreateError> {
    let created = match self
      .docker
      .create_container(None::<CreateContainerOptions>, config)
      .await
    {
      Ok(created) => created,
      Err(err) => {
        self.registry().abandon_create(job_id);
        return Err(classify_create_error(&err));
      },
    };

    let disposition = self
      .registry()
      .finish_create(job_id, &created.id, Instant::now());
    match disposition {
      FinishOutcome::Keep => Ok(CreateJobResult {
        container_id: created.id,
      }),
      FinishOutcome::DiscardReaped => {
        tracing::warn!(
          job_id,
          container_id = created.id.as_str(),
          "job was reaped while its container was being created; removing it unstarted"
        );
        self.force_remove(&created.id).await;
        Err(CreateError::Other(format!(
          "job {job_id} was reaped while its container was being created"
        )))
      },
    }
  }

  /// Reap on a detached task: mark the tombstone, then remove the container
  /// this process knows about and anything Docker still labels with `job_id`
  /// — the second half covers containers created before a daemon restart.
  ///
  /// Reports [`ReapOutcome::Unresolved`] unless every removal succeeded *and*
  /// the listing that proves nothing else matched succeeded. The caller
  /// answers 204 regardless; what it must not do on `Unresolved` is release
  /// the job's budget, because a container that is still running still holds
  /// the box's real vCPU and memory.
  async fn run_reap(&self, job_id: &str) -> ReapOutcome {
    let known_container = self.registry().reap(job_id, Instant::now());
    let mut settled = true;
    if let Some(container_id) = known_container {
      settled &= self.force_remove(&container_id).await;
    }

    match self.containers_labelled(job_id).await {
      Ok(container_ids) => {
        for container_id in container_ids {
          settled &= self.force_remove(&container_id).await;
        }
      },
      Err(err) => {
        tracing::warn!(job_id, error = %err, "listing containers for reap failed");
        settled = false;
      },
    }

    if settled {
      ReapOutcome::Settled
    } else {
      ReapOutcome::Unresolved
    }
  }

  /// Every container id Docker currently labels `sh.toolu.job-id=<job_id>`,
  /// running or not.
  ///
  /// # Errors
  ///
  /// Returns the bollard error when the listing itself failed. That is not
  /// the same answer as an empty list and must not be flattened into one:
  /// "Docker says this job has no containers" is what lets a reap release the
  /// job's budget, while "Docker would not say" leaves a container that may
  /// still be running unaccounted for.
  async fn containers_labelled(&self, job_id: &str) -> Result<Vec<String>, DockerError> {
    let filters = HashMap::from([("label".to_owned(), vec![format!("{LABEL_JOB_ID}={job_id}")])]);
    let options = ListContainersOptions {
      all: true,
      filters: Some(filters),
      ..ListContainersOptions::default()
    };

    let containers = self.docker.list_containers(Some(options)).await?;
    Ok(
      containers
        .into_iter()
        .filter_map(|container| container.id)
        .collect(),
    )
  }

  /// Kill-and-remove `container_id`, reporting whether it is now certainly
  /// gone — `true` also for a container Docker no longer has, which is the
  /// removal already having happened rather than a failure.
  ///
  /// `false` means the container may well still be running. Callers that have
  /// nowhere to report that (the discard-a-reaped-create path, the reaper's
  /// own removals) let the deadline reaper try again; the reap route uses it
  /// to decide whether the job's budget may be released at all.
  async fn force_remove(&self, container_id: &str) -> bool {
    match self
      .docker
      .remove_container(container_id, Some(force_remove_options()))
      .await
    {
      Ok(()) => true,
      Err(err) if is_not_found(&err) => true,
      Err(err) => {
        tracing::warn!(container_id, error = %err, "removing container failed");
        false
      },
    }
  }

  /// The `sh.toolu.job-id` of an existing container, read from its labels.
  /// `None` for a container that is not one of ours, or whose labels are
  /// unreadable — either way there is no gate budget of ours to release.
  async fn job_id_of(&self, container_id: &str) -> Result<Option<String>, DockerError> {
    let inspected = self
      .docker
      .inspect_container(container_id, None::<InspectContainerOptions>)
      .await?;
    let labels = inspected.config.and_then(|config| config.labels);
    Ok(
      labels
        .and_then(|labels| JobLabels::from_map(&labels).ok())
        .map(|labels| labels.job_id),
    )
  }

  /// `docker start` a container `create` has already made, called only once
  /// the resource gate has decided its job's turn has come.
  ///
  /// # Errors
  ///
  /// Returns [`StartFailure`] on any failure. `Self::start_promoted` is the
  /// only caller and acts on the distinction: a container Docker no longer
  /// has is gone for good and its job is released, while any other failure
  /// (sysbox-runc not ready yet, a cgroup hiccup) puts the job back in the
  /// start queue for the next tick to retry. Neither leaves the gate marking
  /// a job running that is not — which is what made a single transient
  /// failure remove a container the customer already had a 201 for.
  async fn start_container(&self, container_id: &str) -> Result<(), StartFailure> {
    self
      .docker
      .start_container(container_id, None::<StartContainerOptions>)
      .await
      .map_err(|err| {
        if is_not_found(&err) {
          StartFailure::NoSuchContainer
        } else {
          StartFailure::Transient(format!("docker start failed: {err}"))
        }
      })
  }

  /// Every container this daemon labelled, live or exited, as
  /// [`reaper::ContainerSnapshot`]s — the ground truth [`Self::tick`]
  /// reconciles against. A listing failure here *is* logged and read as
  /// "none" — unlike [`Self::containers_labelled`], nothing is released off
  /// an empty snapshot, so the only cost is a tick that decides nothing and a
  /// next tick that tries again.
  async fn snapshot(&self) -> Vec<reaper::ContainerSnapshot> {
    let filters = HashMap::from([("label".to_owned(), vec![LABEL_JOB_ID.to_owned()])]);
    let options = ListContainersOptions {
      all: true,
      filters: Some(filters),
      ..ListContainersOptions::default()
    };

    match self.docker.list_containers(Some(options)).await {
      Ok(containers) => containers.iter().filter_map(container_snapshot).collect(),
      Err(err) => {
        tracing::warn!(error = %err, "listing containers for the reaper tick failed");
        Vec::new()
      },
    }
  }

  /// One reconciliation tick, real end to end: list every container this
  /// daemon labelled, hand the snapshot to [`reaper::reconcile`], then
  /// carry out what it decided — force-remove whatever exited or outlived
  /// its deadline, `docker start` whatever the freed budget now promotes.
  /// This is the daemon's whole reaper-and-scheduler duty from
  /// `crates/daemon/README.md`; every call is self-contained, reading its
  /// state fresh from Docker each time, so there is nothing to resume
  /// across calls beyond `gate`/`queue`/`created` themselves.
  ///
  /// Meant to be driven on a timer once the daemon is fully wired at
  /// startup (a later step) — see the module docs.
  pub async fn tick(
    &self,
    gate: &Mutex<Gate>,
    queue: &Mutex<StartQueue>,
    created: &Mutex<CreatedContainers>,
    now_ms: i64,
  ) {
    let snapshot = self.snapshot().await;
    let outcome = {
      let mut gate = gate.lock().unwrap_or_else(PoisonError::into_inner);
      let mut registry = self.registry();
      let mut queue = queue.lock().unwrap_or_else(PoisonError::into_inner);
      let mut created = created.lock().unwrap_or_else(PoisonError::into_inner);
      reaper::reconcile(
        &mut gate,
        &mut registry,
        &mut queue,
        &mut created,
        &snapshot,
        now_ms,
      )
    };

    for container_id in &outcome.remove {
      self.force_remove(container_id).await;
    }
    self
      .start_promoted(&outcome.start, gate, queue, created)
      .await;
  }

  /// `docker start` everything this tick promoted, and put back whatever the
  /// start could not deliver.
  ///
  /// [`reaper::reconcile`] marks a promoted job running in the gate and drops
  /// it from the start queue *before* this runs, so a start that fails leaves
  /// the gate holding a claim nothing backs. Both failure paths undo that
  /// claim rather than log and move on: the next tick would otherwise see the
  /// gate's "running" against Docker's "not running", read it as an exit, and
  /// force-remove a container the customer already has a 201 for — turning
  /// one transient failure into a job that hangs to GitHub's 24-hour timeout.
  async fn start_promoted(
    &self,
    requests: &[StartRequest],
    gate: &Mutex<Gate>,
    queue: &Mutex<StartQueue>,
    created: &Mutex<CreatedContainers>,
  ) {
    for request in requests {
      let container_id = request.container_id.as_str();
      match self.start_container(container_id).await {
        Ok(()) => {},
        Err(StartFailure::Transient(reason)) => {
          tracing::warn!(
            container_id,
            job_id = request.job_id.as_str(),
            error = reason.as_str(),
            "starting a promoted container failed; re-queueing it for the next tick"
          );
          requeue_after_failed_start(gate, queue, &request.job_id);
        },
        Err(StartFailure::NoSuchContainer) => {
          tracing::warn!(
            container_id,
            job_id = request.job_id.as_str(),
            "the container promoted for this job no longer exists; releasing the job"
          );
          self.release_vanished(gate, queue, created, &request.job_id);
        },
      }
    }
  }

  /// Release a promoted job whose container Docker no longer has. No later
  /// snapshot can mention a container that does not exist, so neither the
  /// exit pass nor the deadline pass of [`reaper::reconcile`] would ever
  /// reach this job — re-queueing it would hold its slot until the daemon
  /// restarts.
  fn release_vanished(
    &self,
    gate: &Mutex<Gate>,
    queue: &Mutex<StartQueue>,
    created: &Mutex<CreatedContainers>,
    job_id: &JobId,
  ) {
    let mut gate = gate.lock().unwrap_or_else(PoisonError::into_inner);
    let mut registry = self.registry();
    let mut queue = queue.lock().unwrap_or_else(PoisonError::into_inner);
    let mut created = created.lock().unwrap_or_else(PoisonError::into_inner);
    reaper::release_job(
      &mut gate,
      &mut registry,
      &mut queue,
      &mut created,
      job_id.as_str(),
    );
  }
}

/// Why a promoted container could not be started.
#[derive(Debug)]
enum StartFailure {
  /// Docker has no such container — it was removed out from under this
  /// daemon, so there is nothing left to retry.
  NoSuchContainer,
  /// Any other failure, with Docker's own words. Assumed retryable: the
  /// container still exists, and the conditions that produce this (a runtime
  /// that is not ready yet, a cgroup that could not be created) clear on
  /// their own.
  Transient(String),
}

/// Undo the gate claim [`reaper::reconcile`] took for a job whose `docker
/// start` failed, and put it back at the end of the start queue.
///
/// Back, not front: a container that just refused to start is the one least
/// likely to start on the next attempt, and everything already queued has
/// been waiting at least as long. A job the gate no longer tracks — reaped or
/// destroyed while the start was in flight — is not re-queued at all; there
/// is no job left to run.
fn requeue_after_failed_start(gate: &Mutex<Gate>, queue: &Mutex<StartQueue>, job_id: &JobId) {
  let mut gate = gate.lock().unwrap_or_else(PoisonError::into_inner);
  let still_tracked = gate.unstart(job_id);
  drop(gate);
  if !still_tracked {
    return;
  }

  let mut queue = queue.lock().unwrap_or_else(PoisonError::into_inner);
  queue.push(job_id.clone());
}

/// Build a [`reaper::ContainerSnapshot`] from one listed container, skipping
/// anything missing an id, our labels, or a readable deadline — none of
/// those are this daemon's to act on.
fn container_snapshot(container: &ContainerSummary) -> Option<reaper::ContainerSnapshot> {
  let container_id = container.id.clone()?;
  let labels = container.labels.as_ref()?;
  let parsed = JobLabels::from_map(labels).ok()?;
  let running = matches!(container.state, Some(ContainerSummaryStateEnum::RUNNING));
  Some(reaper::ContainerSnapshot::new(
    parsed.job_id,
    container_id,
    parsed.deadline,
    running,
  ))
}

/// Options that kill a running container before removing it — the reap and
/// destroy paths both need the container gone, not merely stopped.
fn force_remove_options() -> RemoveContainerOptions {
  RemoveContainerOptions {
    force: true,
    ..RemoveContainerOptions::default()
  }
}

/// Whether a bollard failure is Docker's "no such container/image" 404 — the
/// idempotent-destroy case, not an error.
fn is_not_found(err: &DockerError) -> bool {
  matches!(
    err,
    DockerError::DockerResponseServerError { status_code, .. } if *status_code == HTTP_NOT_FOUND
  )
}

/// Map a `docker create` failure onto the route's two 503 shapes.
///
/// A 404 from `POST /containers/create` means one thing only — the image is
/// not resident — and it is the case the daemon must never answer by pulling:
/// the background pre-pull will make the retry succeed, where a pull here
/// would blow the client's 10-second timeout.
fn classify_create_error(err: &DockerError) -> CreateError {
  if let DockerError::DockerResponseServerError { status_code, .. } = err
    && *status_code == HTTP_NOT_FOUND
  {
    return CreateError::ImageNotResident;
  }
  CreateError::Other(format!("docker create failed: {err}"))
}

impl JobBackend for DockerBackend {
  async fn create(&self, req: &CreateJobRequest) -> Result<CreateJobResult, CreateError> {
    let job_id = req.job_ref.job_id.clone();
    let config = spec::container_config(req, &self.runtime);

    // Claimed before the first Docker call and before the first await, so a
    // reap arriving from here on can always address this job.
    let claim = self.registry().begin_create(&job_id, Instant::now());
    match claim {
      BeginOutcome::Proceed => {},
      BeginOutcome::AlreadyReaped => {
        return Err(CreateError::Other(format!(
          "job {job_id} was reaped before its container was created"
        )));
      },
      BeginOutcome::AlreadyTracked => {
        return Err(CreateError::Other(format!(
          "job {job_id} already has a container being created"
        )));
      },
    }

    // Detached and keyed by the job id: the client aborts with
    // `AbortSignal.timeout`, which cancels this handler at the await below.
    // The task owns everything it needs, so the create runs to completion —
    // and its own tombstone check decides what to do with the result.
    let worker = self.clone();
    let owned_job_id = job_id.clone();
    let task = tokio::spawn(async move { worker.run_create(&owned_job_id, config).await });
    match task.await {
      Ok(result) => result,
      Err(join_err) => {
        self.registry().abandon_create(&job_id);
        Err(CreateError::Other(format!(
          "create task for job {job_id} did not finish: {join_err}"
        )))
      },
    }
  }

  async fn destroy(&self, container_id: &str) -> Result<DestroyOutcome, BackendError> {
    let job_id = match self.job_id_of(container_id).await {
      Ok(job_id) => job_id,
      Err(err) if is_not_found(&err) => return Ok(DestroyOutcome::NotFound),
      Err(err) => return Err(BackendError(format!("docker inspect failed: {err}"))),
    };

    match self
      .docker
      .remove_container(container_id, Some(force_remove_options()))
      .await
    {
      Ok(()) => {
        if let Some(job_id) = &job_id {
          self.registry().forget(job_id);
        }
        Ok(DestroyOutcome::Removed { job_id })
      },
      Err(err) if is_not_found(&err) => Ok(DestroyOutcome::NotFound),
      Err(err) => Err(BackendError(format!("docker remove failed: {err}"))),
    }
  }

  async fn reap(&self, job_id: &str) -> ReapOutcome {
    let worker = self.clone();
    let owned_job_id = job_id.to_owned();
    let task = tokio::spawn(async move { worker.run_reap(&owned_job_id).await });
    match task.await {
      Ok(outcome) => outcome,
      Err(join_err) => {
        tracing::warn!(job_id, error = %join_err, "reap task did not finish");
        ReapOutcome::Unresolved
      },
    }
  }
}

#[cfg(test)]
#[path = "tests/docker.rs"]
mod tests;

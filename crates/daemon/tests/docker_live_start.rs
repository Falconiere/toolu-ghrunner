//! Live coverage for the half of a reconcile tick that talks to Docker with
//! something at stake: `docker start` on a container the gate has already
//! promoted.
//!
//! `reaper::reconcile` marks a promoted job running and drops it from the
//! start queue *before* the start is issued, so what the daemon does with a
//! start that fails is not a detail — it decides whether one transient
//! failure costs a customer their container. Only a real Docker daemon can
//! produce those failures honestly: a container whose entrypoint does not
//! exist (create succeeds, start fails, container survives) and a container
//! removed out from under the daemon (start fails 404, nothing survives).
//!
//! `#[ignore]`'d for the reason `docker_live.rs`'s module docs give — no
//! sysbox needed here, only a reachable Docker daemon.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use bollard::Docker;
use bollard::models::{ContainerCreateBody, HostConfig};
use bollard::query_parameters::{
  CreateContainerOptions, InspectContainerOptions, RemoveContainerOptions,
};

use daemon::docker::DockerBackend;
use daemon::gate::{Gate, JobId, JobSize};
use daemon::reaper::{CreatedContainers, StartQueue};

/// Small, cheap to create, and already pulled by the rest of the live suite.
const IMAGE: &str = "alpine:3.20";

/// Job-id/deadline labels a real job container carries — see
/// `daemon::docker::spec`. Hardcoded rather than imported so a drift in the
/// production label string shows up here as a container this test can no
/// longer find, instead of silently agreeing with itself.
const LABEL_JOB_ID: &str = "sh.toolu.job-id";
const LABEL_DEADLINE: &str = "sh.toolu.deadline";

/// A command that does not exist in the image: `docker create` accepts it and
/// `docker start` fails on it, leaving the container in `created` — exactly
/// the shape of the transient failures this path exists for (sysbox-runc not
/// registered yet, a cgroup that could not be made).
const MISSING_ENTRYPOINT: &str = "/no-such-binary";

/// One job's footprint, and the whole box's budget below, so promotion is a
/// real decision rather than a formality.
const SIZE: JobSize = JobSize {
  vcpu: 1,
  memory_mb: 512,
};

/// Six hours, the window `client.ts` puts between `nowMs` and `deadline`.
const SIX_HOURS_MS: i64 = 6 * 60 * 60 * 1000;

/// Anything a live test can fail on: bollard, I/O, the clock.
type LiveResult<T> = Result<T, Box<dyn std::error::Error>>;

/// Wall-clock epoch milliseconds — the same clock `main.rs` reads.
///
/// # Errors
///
/// Returns the clock error if the system time cannot be expressed in epoch
/// milliseconds.
fn now_ms() -> LiveResult<i64> {
  let since_epoch = SystemTime::now().duration_since(UNIX_EPOCH)?;
  Ok(i64::try_from(since_epoch.as_millis())?)
}

/// Create a labelled container running `cmd`, without starting it — the same
/// create-never-start split `DockerBackend::create` keeps in production.
///
/// # Errors
///
/// Returns the bollard error on a failed create.
async fn create_labelled_container(
  docker: &Docker,
  job_id: &str,
  deadline_ms: i64,
  cmd: &str,
) -> Result<String, bollard::errors::Error> {
  let labels = HashMap::from([
    (LABEL_JOB_ID.to_owned(), job_id.to_owned()),
    (LABEL_DEADLINE.to_owned(), deadline_ms.to_string()),
  ]);
  let config = ContainerCreateBody {
    image: Some(IMAGE.to_owned()),
    cmd: Some(vec![cmd.to_owned()]),
    labels: Some(labels),
    host_config: Some(HostConfig::default()),
    ..ContainerCreateBody::default()
  };
  let created = docker
    .create_container(None::<CreateContainerOptions>, config)
    .await?;
  Ok(created.id)
}

/// Whether Docker still knows about `container_id` at all.
async fn container_still_known(docker: &Docker, container_id: &str) -> bool {
  docker
    .inspect_container(container_id, None::<InspectContainerOptions>)
    .await
    .is_ok()
}

/// Best-effort cleanup so a failed assertion never leaks a container.
async fn force_remove(docker: &Docker, container_id: &str) {
  let options = RemoveContainerOptions {
    force: true,
    ..RemoveContainerOptions::default()
  };
  let _ = docker.remove_container(container_id, Some(options)).await;
}

/// The gate/queue/created trio a promoted job needs, seeded exactly as
/// `create_job` leaves them after a 201.
struct Rig {
  gate: Mutex<Gate>,
  queue: Mutex<StartQueue>,
  created: Mutex<CreatedContainers>,
}

impl Rig {
  /// A rig holding `job_id`, admitted and created but never started.
  ///
  /// # Errors
  ///
  /// Returns the gate's `AdmitError` if the job cannot be admitted.
  fn admitted(job_id: &str, container_id: &str) -> LiveResult<Self> {
    let rig = Self {
      gate: Mutex::new(Gate::new(SIZE, 8)),
      queue: Mutex::new(StartQueue::new()),
      created: Mutex::new(CreatedContainers::new()),
    };
    let job = JobId::new(job_id);
    rig
      .gate
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .admit(&job, SIZE)?;
    rig
      .queue
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .push(job);
    rig
      .created
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .record(job_id, container_id);
    Ok(rig)
  }

  /// One real reconcile tick against Docker.
  async fn tick(&self, backend: &DockerBackend, now_ms: i64) {
    backend
      .tick(&self.gate, &self.queue, &self.created, now_ms)
      .await;
  }

  /// How many jobs are still waiting to start.
  fn queued(&self) -> usize {
    self
      .queue
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .len()
  }

  /// Job ids the gate believes are running right now.
  fn running(&self) -> Vec<JobId> {
    self
      .gate
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .running_job_ids()
  }

  /// Jobs the gate still tracks at all, running or queued.
  fn tracked(&self) -> u32 {
    self
      .gate
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .consumption()
      .queue_depth
  }
}

/// A `docker start` that fails must cost the job its turn, never its
/// container.
///
/// Before this was fixed, the failure was only logged: the gate went on
/// marking the job running, the very next tick read that against Docker's
/// "created, not running", called it an exit and force-removed a container
/// the customer already had a 201 for — the job then hung to GitHub's
/// 24-hour timeout. Two ticks over the same unchanged container is exactly
/// that sequence.
#[tokio::test]
#[ignore = "live docker test — requires a reachable Docker daemon (docker info)"]
async fn a_start_docker_refuses_leaves_the_container_alone_and_retries_it() -> LiveResult<()> {
  let docker = Docker::connect_with_defaults()?;
  let backend = DockerBackend::new(docker.clone(), "runc");
  backend.attempt_pull(IMAGE).await;

  let base = now_ms()?;
  let job_id = format!("start-fail-{base}");
  let container_id =
    create_labelled_container(&docker, &job_id, base + SIX_HOURS_MS, MISSING_ENTRYPOINT).await?;
  let rig = Rig::admitted(&job_id, &container_id)?;

  // First tick: the gate promotes the job and the start genuinely fails.
  rig.tick(&backend, base).await;
  assert!(
    container_still_known(&docker, &container_id).await,
    "a failed start must not remove the container"
  );
  assert!(
    rig.running().is_empty(),
    "the gate must not go on claiming a container is running that Docker refused to start"
  );
  assert_eq!(rig.queued(), 1, "the job goes back in the start queue");

  // Second tick, with Docker reporting exactly what it did before.
  rig.tick(&backend, base).await;
  assert!(
    container_still_known(&docker, &container_id).await,
    "one transient start failure must not turn into a removal on the next tick"
  );
  assert_eq!(rig.queued(), 1, "still queued, still this daemon's job");

  force_remove(&docker, &container_id).await;
  Ok(())
}

/// The other failure the same call produces: the container is simply gone —
/// removed out from under the daemon. There is nothing to retry and no
/// snapshot will ever mention it again, so re-queueing would hold the job's
/// slot until the daemon restarts. It is released instead.
#[tokio::test]
#[ignore = "live docker test — requires a reachable Docker daemon (docker info)"]
async fn a_promoted_container_that_no_longer_exists_releases_its_job() -> LiveResult<()> {
  let docker = Docker::connect_with_defaults()?;
  let backend = DockerBackend::new(docker.clone(), "runc");
  backend.attempt_pull(IMAGE).await;

  let base = now_ms()?;
  let job_id = format!("start-vanished-{base}");
  let container_id =
    create_labelled_container(&docker, &job_id, base + SIX_HOURS_MS, "/bin/true").await?;
  let rig = Rig::admitted(&job_id, &container_id)?;

  // Gone before the tick reaches it — and gone from the snapshot too, so no
  // later reconcile pass can ever see this job again.
  force_remove(&docker, &container_id).await;
  assert!(!container_still_known(&docker, &container_id).await);

  rig.tick(&backend, base).await;
  assert_eq!(
    rig.queued(),
    0,
    "a job whose container no longer exists must not keep waiting for it"
  );
  assert_eq!(rig.tracked(), 0, "…and must not keep holding a queue slot");
  Ok(())
}

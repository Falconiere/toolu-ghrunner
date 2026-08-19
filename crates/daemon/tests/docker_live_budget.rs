//! Live coverage for AC-16: a job container that exits on its own releases
//! its budget, and the job that was waiting for it is promoted — proving
//! the reconcile tick does not wedge the box once its budget is fully
//! spoken for, across more than one cycle.
//!
//! `#[ignore]`'d for the reason `docker_live.rs`'s module docs give. Drives
//! `DockerBackend::tick` directly, the same seam `docker_live.rs`'s reaper
//! test uses, rather than through HTTP: the contention this proves is
//! between the gate's admission and Docker's own exit, neither of which the
//! router adds anything to.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
/// production label string would show up here as a real container this test
/// can no longer find, not silently agree with itself.
const LABEL_JOB_ID: &str = "sh.toolu.job-id";
const LABEL_DEADLINE: &str = "sh.toolu.deadline";

/// How long a round waits for its running job to exit on its own. The
/// container itself sleeps for [`EXIT_AFTER_SECS`]; this must clear that
/// with real margin.
const WAIT_FOR_EXIT: Duration = Duration::from_secs(4);

/// How long each round's container runs before exiting normally.
const EXIT_AFTER_SECS: u32 = 2;

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

/// Create a labelled container that runs `sleep <EXIT_AFTER_SECS>` and then
/// exits on its own — created, never started here, the same split
/// `DockerBackend::create` keeps in production.
///
/// # Errors
///
/// Returns the bollard error on a failed create.
async fn create_sleep_container(
  docker: &Docker,
  job_id: &str,
  deadline_ms: i64,
) -> Result<String, bollard::errors::Error> {
  let labels = HashMap::from([
    (LABEL_JOB_ID.to_owned(), job_id.to_owned()),
    (LABEL_DEADLINE.to_owned(), deadline_ms.to_string()),
  ]);
  let config = ContainerCreateBody {
    image: Some(IMAGE.to_owned()),
    cmd: Some(vec!["sleep".to_owned(), EXIT_AFTER_SECS.to_string()]),
    labels: Some(labels),
    host_config: Some(HostConfig::default()),
    ..ContainerCreateBody::default()
  };
  let created = docker
    .create_container(None::<CreateContainerOptions>, config)
    .await?;
  Ok(created.id)
}

/// Whether Docker currently reports `container_id` running.
///
/// # Errors
///
/// Returns the bollard error on a failed inspect.
async fn is_running(docker: &Docker, container_id: &str) -> Result<bool, bollard::errors::Error> {
  let inspected = docker
    .inspect_container(container_id, None::<InspectContainerOptions>)
    .await?;
  Ok(
    inspected
      .state
      .and_then(|state| state.running)
      .unwrap_or(false),
  )
}

/// Best-effort cleanup so a failed assertion never leaks a container.
async fn force_remove(docker: &Docker, container_id: &str) {
  let options = RemoveContainerOptions {
    force: true,
    ..RemoveContainerOptions::default()
  };
  let _ = docker.remove_container(container_id, Some(options)).await;
}

/// The gate/queue/created-map trio a real request handler shares, bundled
/// so the helpers below stay under the workspace's argument-count ceiling.
struct Rig<'a> {
  backend: &'a DockerBackend,
  gate: &'a Mutex<Gate>,
  queue: &'a Mutex<StartQueue>,
  created: &'a Mutex<CreatedContainers>,
}

impl Rig<'_> {
  /// Admit `job_id` to the gate, queue it to start and record its
  /// container — the same bookkeeping
  /// `crate::routes::handlers::create_job` does on a real 201, reproduced
  /// here so this test can drive `tick` without the router — then run one
  /// reconcile tick.
  ///
  /// # Errors
  ///
  /// Returns the gate's [`daemon::gate::AdmitError`] if `job_id` cannot be
  /// admitted.
  async fn admit_created_and_tick(
    &self,
    job_id: &str,
    container_id: &str,
    budget: JobSize,
    now_ms: i64,
  ) -> LiveResult<()> {
    {
      let mut gate = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
      gate.admit(&JobId::new(job_id), budget)?;
    }
    {
      let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
      queue.push(JobId::new(job_id));
    }
    {
      let mut created = self.created.lock().unwrap_or_else(PoisonError::into_inner);
      created.record(job_id, container_id);
    }
    self
      .backend
      .tick(self.gate, self.queue, self.created, now_ms)
      .await;
    Ok(())
  }

  /// Run one more reconcile tick without admitting anything new — used to
  /// let a freed budget promote whatever is already queued.
  async fn tick(&self, now_ms: i64) {
    self
      .backend
      .tick(self.gate, self.queue, self.created, now_ms)
      .await;
  }

  /// The gate's current vCPU consumption.
  fn vcpu_used(&self) -> u32 {
    self
      .gate
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .consumption()
      .vcpu_used
  }
}

#[tokio::test]
#[ignore = "live docker test — requires a reachable Docker daemon (docker info)"]
async fn an_exited_job_releases_its_budget_and_the_next_admits() -> LiveResult<()> {
  let docker = Docker::connect_with_defaults()?;
  let backend = DockerBackend::new(docker.clone(), "runc");

  // Budget for exactly one job at a time — contention is the whole point.
  let budget = JobSize {
    vcpu: 1,
    memory_mb: 512,
  };
  let gate = Mutex::new(Gate::new(budget, 8));
  let queue = Mutex::new(StartQueue::new());
  let created = Mutex::new(CreatedContainers::new());
  let rig = Rig {
    backend: &backend,
    gate: &gate,
    queue: &queue,
    created: &created,
  };

  let base = now_ms()?;
  let far_future_deadline = base + 6 * 60 * 60 * 1000;

  // Round 1: job A takes the box's only slot; job B is admitted and created
  // while A still holds it, so promoting B has to wait on a real exit.
  let job_a = format!("ac16-a-{base}");
  let container_a = create_sleep_container(&docker, &job_a, far_future_deadline).await?;
  rig
    .admit_created_and_tick(&job_a, &container_a, budget, base)
    .await?;
  assert!(is_running(&docker, &container_a).await?, "A should start");
  assert_eq!(rig.vcpu_used(), 1, "A's vCPU is now consumed");

  let job_b = format!("ac16-b-{base}");
  let container_b = create_sleep_container(&docker, &job_b, far_future_deadline).await?;
  rig
    .admit_created_and_tick(&job_b, &container_b, budget, base)
    .await?;
  assert!(
    !is_running(&docker, &container_b).await?,
    "B must wait — the box's only vCPU is still A's"
  );

  tokio::time::sleep(WAIT_FOR_EXIT).await;
  rig.tick(base).await;
  assert!(
    !container_still_known(&docker, &container_a).await,
    "A's container must be reaped once it exits on its own"
  );
  assert!(
    is_running(&docker, &container_b).await?,
    "A's exit must free the budget B was waiting on"
  );
  assert_eq!(rig.vcpu_used(), 1, "B now holds the vCPU A gave back");

  // Round 2: prove it repeats — B's own exit frees the box for a third job,
  // the "does not wedge after N jobs" half of AC-16.
  let job_c = format!("ac16-c-{base}");
  let container_c = create_sleep_container(&docker, &job_c, far_future_deadline).await?;
  rig
    .admit_created_and_tick(&job_c, &container_c, budget, base)
    .await?;
  assert!(
    !is_running(&docker, &container_c).await?,
    "C must wait — B is still running"
  );

  tokio::time::sleep(WAIT_FOR_EXIT).await;
  rig.tick(base).await;
  assert!(
    !container_still_known(&docker, &container_b).await,
    "B's container must be reaped once it exits on its own"
  );
  assert!(
    is_running(&docker, &container_c).await?,
    "B's exit must free the budget C was waiting on"
  );

  force_remove(&docker, &container_c).await;
  Ok(())
}

/// Whether Docker still knows about `container_id` at all — the reaped
/// containers in this test are removed outright, not merely stopped.
async fn container_still_known(docker: &Docker, container_id: &str) -> bool {
  docker
    .inspect_container(container_id, None::<InspectContainerOptions>)
    .await
    .is_ok()
}

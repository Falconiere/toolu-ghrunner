//! Live end-to-end coverage for the daemon's real Docker half — the reaper
//! tick, startup adoption and the pre-pull — none of which the pure suites
//! can reach (CI's `ubuntu-latest` has no sysbox, and `macos-14` has no
//! Docker at all; see `crates/daemon/src/tests/reaper.rs`'s module docs).
//! Every test here is `#[ignore]`'d for that reason and is meant to run
//! against a real Docker daemon — the toolu-operated box's `daemon-live.yml`
//! (a later step) is the intended runner, not `cargo test --workspace`.
//!
//! Nothing here uses `sysbox-runc`: no test exercises isolation, only whether
//! the daemon issues the right Docker calls against real container state, so
//! the default runtime is enough and keeps this runnable on a plain Docker
//! install.
//!
//! Each test labels its containers with a job id unique to that run, and
//! asserts only over its own — the file's tests may run concurrently, and
//! `existing_jobs` deliberately sees every container this daemon ever
//! labelled.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use bollard::Docker;
use bollard::models::{ContainerCreateBody, HostConfig};
use bollard::query_parameters::{
  CreateContainerOptions, InspectContainerOptions, RemoveContainerOptions, RemoveImageOptions,
  StartContainerOptions,
};

use daemon::adopt::{ContainerLifecycle, adopt};
use daemon::docker::DockerBackend;
use daemon::docker::spec::{ENV_DEADLINE, memory_bytes, nano_cpus};
use daemon::gate::{Gate, JobId, JobSize};
use daemon::reaper::{CreatedContainers, StartQueue};
use daemon::routes::build_router;
use daemon::routes::state::AppState;

/// The image every container in this file runs — small, always available
/// once pulled, and kept alive long enough to inspect with `sleep`.
const IMAGE: &str = "alpine:3.20";
/// Job-id/deadline labels a real job container carries — see
/// `daemon::docker::spec`.
const LABEL_JOB_ID: &str = "sh.toolu.job-id";
const LABEL_DEADLINE: &str = "sh.toolu.deadline";

/// Create a labelled, long-lived (`sleep 60`) container for `job_id`,
/// without starting it — mirrors `docker create`, never `docker start`, the
/// same split `DockerBackend::create` keeps in production.
///
/// # Errors
///
/// Returns the bollard error on a failed create.
async fn create_labelled_container(
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
    cmd: Some(vec!["sleep".to_owned(), "60".to_owned()]),
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

#[tokio::test]
#[ignore = "live docker test — requires a reachable Docker daemon (docker info)"]
async fn tick_starts_a_promoted_job_and_removes_one_past_its_deadline()
-> Result<(), bollard::errors::Error> {
  let docker = Docker::connect_with_defaults()?;
  let backend = DockerBackend::new(docker.clone(), "runc");

  let now_ms = 1_700_000_000_000_i64;
  let far_future_deadline = now_ms + 6 * 60 * 60 * 1000;
  let already_expired_deadline = now_ms - 1;

  // Budget for exactly one job at a time.
  let budget = JobSize {
    vcpu: 1,
    memory_mb: 512,
  };
  let gate = Mutex::new(Gate::new(budget, 8));
  let queue = Mutex::new(StartQueue::new());
  let created = Mutex::new(CreatedContainers::new());

  let waiting_container =
    create_labelled_container(&docker, "job-waiting", far_future_deadline).await?;
  let expired_container =
    create_labelled_container(&docker, "job-expired", already_expired_deadline).await?;

  {
    let mut gate = gate
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(gate.admit(&JobId::new("job-waiting"), budget).is_ok());
  }
  {
    let mut queue = queue
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    queue.push(JobId::new("job-waiting"));
  }
  {
    let mut created = created
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    created.record("job-waiting", &waiting_container);
  }

  // `job-expired` was never admitted to this gate at all — it stands in for
  // a container this daemon knows only from Docker's own labels (e.g.
  // adopted at startup), proving the deadline kill does not depend on the
  // gate having tracked it first.
  backend.tick(&gate, &queue, &created, now_ms).await;

  let started = is_running(&docker, &waiting_container).await?;
  assert!(started, "the waiting job should have been started");
  let expired_gone = docker
    .inspect_container(&expired_container, None::<InspectContainerOptions>)
    .await
    .is_err();
  assert!(expired_gone, "the expired job's container should be gone");

  force_remove(&docker, &waiting_container).await;
  Ok(())
}

/// The Linux floor tag `runner-tags.ts` sells, and the footprint both
/// adoption tests below create their containers with.
const TAG_2VCPU_4GB: JobSize = JobSize {
  vcpu: 2,
  memory_mb: 4096,
};

/// Six hours, the window `client.ts` puts between `nowMs` and `deadline`.
const SIX_HOURS_MS: i64 = 6 * 60 * 60 * 1000;

/// An image nothing else in this file uses, so removing it to prove the
/// absent-image path cannot disturb another test.
const PREPULL_IMAGE: &str = "busybox:1.36";

/// The bearer token the live router is configured with.
const LIVE_TOKEN: &str = "live-daemon-token";

/// Anything a live test can fail on: bollard, reqwest, I/O, the clock. The
/// helpers below propagate rather than unwrap, because clippy's
/// `allow-expect-in-tests` covers `#[test]` functions only — and a helper
/// that panics reports the wrong line anyway.
type LiveResult<T> = Result<T, Box<dyn std::error::Error>>;

/// Wall-clock epoch milliseconds — the same clock `main.rs` reads and passes
/// into adoption and every reconcile tick.
///
/// # Errors
///
/// Returns the clock error if the system time is before the Unix epoch or
/// beyond what epoch milliseconds can express.
fn now_ms() -> LiveResult<i64> {
  let since_epoch = SystemTime::now().duration_since(UNIX_EPOCH)?;
  Ok(i64::try_from(since_epoch.as_millis())?)
}

/// A job id unique to this test run, so a test asserting over
/// `existing_jobs` — which sees every container this daemon ever labelled —
/// can pick out its own.
///
/// # Errors
///
/// Returns the clock error from [`now_ms`].
fn unique_job_id(prefix: &str) -> LiveResult<String> {
  Ok(format!("{prefix}-{}", now_ms()?))
}

/// Create a container exactly as `docker create` would for a real job:
/// both labels, the real `NanoCpus`/`Memory` limits for `size`, and a
/// `TOOLU_DEADLINE` env var deliberately set to a *different* number than
/// the label carries — adoption must read the label and never the
/// environment, which is where the single-use JIT credential also lives.
///
/// # Errors
///
/// Returns the bollard error on a failed create.
async fn create_job_container(
  docker: &Docker,
  job_id: &str,
  deadline_ms: i64,
  size: JobSize,
) -> Result<String, bollard::errors::Error> {
  let labels = HashMap::from([
    (LABEL_JOB_ID.to_owned(), job_id.to_owned()),
    (LABEL_DEADLINE.to_owned(), deadline_ms.to_string()),
  ]);
  let config = ContainerCreateBody {
    image: Some(IMAGE.to_owned()),
    cmd: Some(vec!["sleep".to_owned(), "60".to_owned()]),
    env: Some(vec![format!("{ENV_DEADLINE}=1")]),
    labels: Some(labels),
    host_config: Some(HostConfig {
      nano_cpus: Some(nano_cpus(size.vcpu)),
      memory: Some(memory_bytes(size.memory_mb)),
      ..HostConfig::default()
    }),
    ..ContainerCreateBody::default()
  };
  let created = docker
    .create_container(None::<CreateContainerOptions>, config)
    .await?;
  Ok(created.id)
}

#[tokio::test]
#[ignore = "live docker test — requires a reachable Docker daemon (docker info)"]
async fn a_restart_re_adopts_a_running_containers_budget_from_its_labels() -> LiveResult<()> {
  let docker = Docker::connect_with_defaults()?;
  let backend = DockerBackend::new(docker.clone(), "runc");

  let started_at = now_ms()?;
  let deadline_ms = started_at + SIX_HOURS_MS;
  let running_job = unique_job_id("adopt-running")?;
  let pending_job = unique_job_id("adopt-pending")?;

  let running_container =
    create_job_container(&docker, &running_job, deadline_ms, TAG_2VCPU_4GB).await?;
  let pending_container =
    create_job_container(&docker, &pending_job, deadline_ms, TAG_2VCPU_4GB).await?;
  docker
    .start_container(&running_container, None::<StartContainerOptions>)
    .await?;

  // A restart: nothing in memory, everything still on the box. This is what
  // `main.rs` does between connecting to Docker and binding the listener.
  let found = backend.existing_jobs().await?;
  let mine: Vec<_> = found
    .into_iter()
    .filter(|container| container.job_id == running_job || container.job_id == pending_job)
    .collect();
  assert_eq!(
    mine.len(),
    2,
    "both containers must be found by their label"
  );
  for container in &mine {
    let expected = if container.job_id == running_job {
      ContainerLifecycle::Running
    } else {
      ContainerLifecycle::Created
    };
    assert_eq!(
      container.lifecycle, expected,
      "Docker's own status word decides what the container holds"
    );
    assert_eq!(
      container.size, TAG_2VCPU_4GB,
      "read back off the real limits"
    );
  }

  let budget = JobSize {
    vcpu: 16,
    memory_mb: 32768,
  };
  let mut gate = Gate::new(budget, 32);
  let mut queue = StartQueue::new();
  let mut created = CreatedContainers::new();
  let adoption = adopt(&mut gate, &mut queue, &mut created, &mine, started_at);

  let consumption = gate.consumption();
  assert_eq!(
    consumption.vcpu_used, TAG_2VCPU_4GB.vcpu,
    "only the running container's vCPU is consumed"
  );
  assert_eq!(
    consumption.memory_mb_used, TAG_2VCPU_4GB.memory_mb,
    "the memory limit is read back off the real container"
  );

  let resumed = adoption
    .resumed
    .iter()
    .find(|armed| armed.job_id == JobId::new(running_job.clone()))
    .expect("the running container is resumed");
  assert_eq!(
    resumed.deadline_ms, deadline_ms,
    "the deadline comes from the sh.toolu.deadline label, not TOOLU_DEADLINE=1"
  );
  assert_eq!(resumed.container_id, running_container);

  assert_eq!(adoption.requeued.len(), 1, "the unstarted one is re-queued");
  assert_eq!(queue.len(), 1);
  assert_eq!(
    created.existing(&pending_job),
    Some(pending_container.as_str())
  );
  assert!(
    adoption.remove.is_empty(),
    "neither container is past its deadline"
  );

  // The adopted budget is real: a job that no longer fits waits.
  let newcomer = JobId::new("adopt-newcomer");
  assert!(gate.admit(&newcomer, budget).is_ok());
  assert_eq!(
    gate.try_start(&newcomer),
    Some(daemon::gate::StartOutcome::Deferred),
    "the whole box no longer fits while an adopted job holds 2 vCPU"
  );

  force_remove(&docker, &running_container).await;
  force_remove(&docker, &pending_container).await;
  Ok(())
}

/// Serve the real Docker-backed router on an ephemeral loopback port for the
/// rest of the test, returning the address it bound.
///
/// Pinned to [`PREPULL_IMAGE`], the image its one caller's requests name: the
/// daemon serves exactly the image `TOOLU_DAEMON_IMAGE` names and refuses
/// every other outright, so a router pinned to anything else would refuse
/// this test's creates before they ever reached the residency question they
/// are about.
///
/// # Errors
///
/// Returns the I/O error if the ephemeral port cannot be bound.
async fn spawn_live_daemon(
  backend: DockerBackend,
  token_file: std::path::PathBuf,
) -> LiveResult<SocketAddr> {
  let budget = JobSize {
    vcpu: 16,
    memory_mb: 32768,
  };
  let state = AppState::new(backend, Gate::new(budget, 32), token_file, PREPULL_IMAGE);
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
  let addr = listener.local_addr()?;
  tokio::spawn(async move {
    if let Err(err) = axum::serve(listener, build_router(state)).await {
      eprintln!("the live router stopped: {err}");
    }
  });
  Ok(addr)
}

/// POST a real `createVpsInstance` body naming `image` for `job_id`.
///
/// # Errors
///
/// Returns the reqwest error if the request could not be sent.
async fn post_live_create(
  client: &reqwest::Client,
  addr: SocketAddr,
  job_id: &str,
  image: &str,
  deadline_ms: i64,
) -> LiveResult<reqwest::Response> {
  Ok(
    client
      .post(format!("http://{addr}/v1/jobs"))
      .bearer_auth(LIVE_TOKEN)
      .json(&serde_json::json!({
        "jitConfig": "base64-encoded-jit-config",
        "image": image,
        "size": { "vcpu": TAG_2VCPU_4GB.vcpu, "memoryMb": TAG_2VCPU_4GB.memory_mb },
        "jobRef": { "org": "acme", "repo": "widgets", "jobId": job_id },
        "purpose": "live pre-pull test",
        "deadline": deadline_ms,
      }))
      .send()
      .await?,
  )
}

#[tokio::test]
#[ignore = "live docker test — requires a reachable Docker daemon and registry access"]
async fn an_absent_image_answers_503_and_the_same_request_answers_201_after_the_pre_pull()
-> LiveResult<()> {
  let docker = Docker::connect_with_defaults()?;
  let backend = DockerBackend::new(docker.clone(), "runc");

  // Start from a box that genuinely does not hold the pinned image.
  let _removed = docker
    .remove_image(
      PREPULL_IMAGE,
      Some(RemoveImageOptions {
        force: true,
        ..RemoveImageOptions::default()
      }),
      None,
    )
    .await;
  assert!(
    !backend.image_present(PREPULL_IMAGE).await?,
    "the image must be absent for this test to mean anything"
  );

  let dir = tempfile::tempdir()?;
  let token_file = dir.path().join("token");
  std::fs::write(&token_file, LIVE_TOKEN)?;
  let addr = spawn_live_daemon(backend.clone(), token_file).await?;
  let client = reqwest::Client::new();

  let job_id = unique_job_id("prepull")?;
  let deadline_ms = now_ms()? + SIX_HOURS_MS;

  let refused = post_live_create(&client, addr, &job_id, PREPULL_IMAGE, deadline_ms).await?;
  assert_eq!(
    refused.status().as_u16(),
    503,
    "an absent image is a fast 503, never an inline pull"
  );
  assert!(
    backend
      .existing_jobs()
      .await?
      .iter()
      .all(|container| container.job_id != job_id),
    "nothing was created for a job the image could not serve"
  );
  assert!(
    !backend.image_present(PREPULL_IMAGE).await?,
    "the request must not have pulled anything"
  );

  // The pre-pull `main.rs` runs before binding — here it runs after, which
  // is the only way to observe both sides of the transition in one process.
  let outcome = backend.attempt_pull(PREPULL_IMAGE).await;
  assert_eq!(outcome, daemon::prepull::PullOutcome::Pulled);

  let accepted = post_live_create(&client, addr, &job_id, PREPULL_IMAGE, deadline_ms).await?;
  assert_eq!(
    accepted.status().as_u16(),
    201,
    "the same request succeeds once the pre-pull has landed"
  );
  let body: serde_json::Value = accepted.json().await?;
  let container_id = body
    .get("containerId")
    .and_then(serde_json::Value::as_str)
    .ok_or("the 201 body must carry containerId")?
    .to_owned();

  force_remove(&docker, &container_id).await;
  Ok(())
}

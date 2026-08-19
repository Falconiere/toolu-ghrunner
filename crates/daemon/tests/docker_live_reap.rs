//! Live coverage for AC-6: `DELETE /v1/jobs?jobId=…` kills exactly the
//! container carrying that job's `sh.toolu.job-id` label and always answers
//! 204 — including for a job id nothing matches, the shape `vps/verify.ts`
//! (toolu.sh repo) relies on to probe credentials. The harder half: issued
//! against a create still in flight, it must prevent that container from
//! ever starting — the tombstone `daemon::docker::registry` (G7) exists
//! for.
//!
//! `#[ignore]`'d for the reason `docker_live.rs`'s module docs give.

use std::net::SocketAddr;
use std::time::{SystemTime, UNIX_EPOCH};

use bollard::Docker;
use bollard::query_parameters::{InspectContainerOptions, RemoveContainerOptions};

use daemon::docker::DockerBackend;
use daemon::gate::{Gate, JobSize};
use daemon::routes::backend::JobBackend;
use daemon::routes::build_router;
use daemon::routes::state::AppState;
use daemon::routes::wire::{CreateJobRequest, JobRefWire, JobSizeWire};

/// Small, cheap to create, and already pulled by the rest of the live suite.
const IMAGE: &str = "alpine:3.20";

/// The bearer token the live router is configured with.
const LIVE_TOKEN: &str = "live-daemon-token";

/// Six hours, the window `client.ts` puts between `nowMs` and `deadline`.
const SIX_HOURS_MS: i64 = 6 * 60 * 60 * 1000;

/// Anything a live test can fail on: bollard, reqwest, I/O, the clock.
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

/// Serve the real Docker-backed router on an ephemeral loopback port.
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
  let state = AppState::new(backend, Gate::new(budget, 32), token_file);
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
  let addr = listener.local_addr()?;
  tokio::spawn(async move {
    if let Err(err) = axum::serve(listener, build_router(state)).await {
      eprintln!("the live router stopped: {err}");
    }
  });
  Ok(addr)
}

/// POST a real `createVpsInstance` body for `job_id`.
///
/// # Errors
///
/// Returns the reqwest error if the request could not be sent.
async fn post_create(
  client: &reqwest::Client,
  addr: SocketAddr,
  job_id: &str,
  deadline_ms: i64,
) -> LiveResult<reqwest::Response> {
  Ok(
    client
      .post(format!("http://{addr}/v1/jobs"))
      .bearer_auth(LIVE_TOKEN)
      .json(&serde_json::json!({
        "jitConfig": "eyJyZWFwIjogInByb2JlIn0=",
        "image": IMAGE,
        "size": { "vcpu": 2, "memoryMb": 4096 },
        "jobRef": { "org": "acme", "repo": "widgets", "jobId": job_id },
        "purpose": "live reap test (AC-6)",
        "deadline": deadline_ms,
      }))
      .send()
      .await?,
  )
}

/// Read `containerId` out of a `201` create response body.
///
/// # Errors
///
/// Returns an error if the body cannot be parsed or carries no
/// `containerId`.
async fn container_id_of(response: reqwest::Response) -> LiveResult<String> {
  let body: serde_json::Value = response.json().await?;
  Ok(
    body
      .get("containerId")
      .and_then(serde_json::Value::as_str)
      .ok_or("the 201 body must carry containerId")?
      .to_owned(),
  )
}

/// Whether Docker still knows about `container_id` at all.
async fn container_exists(docker: &Docker, container_id: &str) -> bool {
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

#[tokio::test]
#[ignore = "live docker test — requires a reachable Docker daemon (docker info)"]
async fn reap_kills_exactly_the_labelled_container_and_a_sentinel_id_is_also_204() -> LiveResult<()>
{
  let docker = Docker::connect_with_defaults()?;
  let backend = DockerBackend::new(docker.clone(), "runc");
  backend.attempt_pull(IMAGE).await;
  assert!(
    backend.image_present(IMAGE).await?,
    "the live suite's shared image must be resident"
  );

  let dir = tempfile::tempdir()?;
  let token_file = dir.path().join("token");
  std::fs::write(&token_file, LIVE_TOKEN)?;
  let addr = spawn_live_daemon(backend.clone(), token_file).await?;
  let client = reqwest::Client::new();

  let base = now_ms()?;
  let deadline_ms = base + SIX_HOURS_MS;
  let target_job = format!("ac6-target-{base}");
  let bystander_job = format!("ac6-bystander-{base}");

  let target_response = post_create(&client, addr, &target_job, deadline_ms).await?;
  assert_eq!(target_response.status().as_u16(), 201);
  let target_container = container_id_of(target_response).await?;

  let bystander_response = post_create(&client, addr, &bystander_job, deadline_ms).await?;
  assert_eq!(bystander_response.status().as_u16(), 201);
  let bystander_container = container_id_of(bystander_response).await?;

  let reap_target = client
    .delete(format!("http://{addr}/v1/jobs?jobId={target_job}"))
    .bearer_auth(LIVE_TOKEN)
    .send()
    .await?;
  assert_eq!(reap_target.status().as_u16(), 204);

  assert!(
    !container_exists(&docker, &target_container).await,
    "the labelled container must be gone"
  );
  assert!(
    container_exists(&docker, &bystander_container).await,
    "an unrelated job's container must survive"
  );

  let sentinel_job = format!("ac6-sentinel-{base}-does-not-exist");
  let reap_sentinel = client
    .delete(format!("http://{addr}/v1/jobs?jobId={sentinel_job}"))
    .bearer_auth(LIVE_TOKEN)
    .send()
    .await?;
  assert_eq!(
    reap_sentinel.status().as_u16(),
    204,
    "an unknown job id is also 204 — vps/verify.ts's credential probe"
  );

  force_remove(&docker, &bystander_container).await;
  Ok(())
}

/// The `docker create` body a real job would carry, at a size distinct from
/// the other live tests.
fn race_request(job_id: &str, deadline_ms: i64) -> CreateJobRequest {
  CreateJobRequest {
    jit_config: "eyJyYWNlIjogdHJ1ZX0=".to_owned(),
    image: IMAGE.to_owned(),
    size: JobSizeWire {
      vcpu: 1,
      memory_mb: 512,
    },
    job_ref: JobRefWire {
      org: "acme".to_owned(),
      repo: "widgets".to_owned(),
      job_id: job_id.to_owned(),
    },
    purpose: "live reap-race test (AC-6)".to_owned(),
    deadline: deadline_ms,
  }
}

#[tokio::test]
#[ignore = "live docker test — requires a reachable Docker daemon (docker info)"]
async fn a_reap_racing_an_in_flight_create_stops_the_container_from_ever_starting() -> LiveResult<()>
{
  let docker = Docker::connect_with_defaults()?;
  let backend = DockerBackend::new(docker.clone(), "runc");
  backend.attempt_pull(IMAGE).await;
  assert!(
    backend.image_present(IMAGE).await?,
    "the live suite's shared image must be resident"
  );

  let job_id = format!("ac6-race-{}", now_ms()?);
  let deadline_ms = now_ms()? + SIX_HOURS_MS;
  let request = race_request(&job_id, deadline_ms);

  // `create`'s registry claim (`begin_create`) runs synchronously on its
  // very first poll, before `join!` ever gives `reap` a turn, so the reap
  // can only ever land during or after the create's own Docker call — never
  // before it, which would prove nothing about the in-flight race this test
  // exists for. The single `yield_now` is enough to let `create`'s detached
  // task get spawned first.
  let reap_backend = backend.clone();
  let reap_job_id = job_id.clone();
  let create_call = backend.create(&request);
  let reap_call = async move {
    tokio::task::yield_now().await;
    reap_backend.reap(&reap_job_id).await;
  };
  let (create_result, ()) = tokio::join!(create_call, reap_call);

  if let Ok(created) = create_result {
    // The reap lost the internal race: `finish_create` had already run
    // before the tombstone landed. `reap`'s own task — already awaited to
    // completion above — must still have removed what `create` "returned".
    let still_there = container_exists(&docker, &created.container_id).await;
    assert!(
      !still_there,
      "a create that raced a reap must not leave its container behind, even when it briefly won"
    );
  }
  // else: the far likelier ordering — the tombstone landed while `docker
  // create` was still in flight, and the container was discarded the
  // moment it appeared — see `JobRegistry::finish_create`.

  let survivors = backend.existing_jobs().await?;
  assert!(
    survivors.iter().all(|container| container.job_id != job_id),
    "no container may remain on the box under a job id that was reaped mid-create"
  );

  Ok(())
}

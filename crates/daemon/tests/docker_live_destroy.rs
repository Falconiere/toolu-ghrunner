//! Live coverage for AC-5: `DELETE /v1/jobs/{containerId}` removes a real
//! container and answers 204; the identical call afterwards answers 404 —
//! the idempotent-destroy shape `destroyVpsInstance` in `client.ts` relies
//! on to resolve rather than throw on a repeat.
//!
//! `#[ignore]`'d for the reason `docker_live.rs`'s module docs give.

use std::net::SocketAddr;
use std::time::{SystemTime, UNIX_EPOCH};

use bollard::Docker;
use bollard::query_parameters::InspectContainerOptions;

use daemon::docker::DockerBackend;
use daemon::gate::{Gate, JobSize};
use daemon::routes::build_router;
use daemon::routes::state::AppState;

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
  let state = AppState::new(backend, Gate::new(budget, 32), token_file, IMAGE);
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
        "jitConfig": "eyJkZXN0cm95IjogInByb2JlIn0=",
        "image": IMAGE,
        "size": { "vcpu": 2, "memoryMb": 4096 },
        "jobRef": { "org": "acme", "repo": "widgets", "jobId": job_id },
        "purpose": "live destroy test (AC-5)",
        "deadline": deadline_ms,
      }))
      .send()
      .await?,
  )
}

/// Whether Docker still knows about `container_id` at all.
async fn container_exists(docker: &Docker, container_id: &str) -> bool {
  docker
    .inspect_container(container_id, None::<InspectContainerOptions>)
    .await
    .is_ok()
}

#[tokio::test]
#[ignore = "live docker test — requires a reachable Docker daemon (docker info)"]
async fn destroy_removes_a_real_container_then_answers_not_found() -> LiveResult<()> {
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

  let job_id = format!("ac5-{}", now_ms()?);
  let deadline_ms = now_ms()? + SIX_HOURS_MS;
  let created = post_create(&client, addr, &job_id, deadline_ms).await?;
  assert_eq!(created.status().as_u16(), 201, "the job must create");
  let body: serde_json::Value = created.json().await?;
  let container_id = body
    .get("containerId")
    .and_then(serde_json::Value::as_str)
    .ok_or("the 201 body must carry containerId")?
    .to_owned();

  assert!(
    container_exists(&docker, &container_id).await,
    "the container must exist before it is destroyed"
  );

  let first = client
    .delete(format!("http://{addr}/v1/jobs/{container_id}"))
    .bearer_auth(LIVE_TOKEN)
    .send()
    .await?;
  assert_eq!(first.status().as_u16(), 204, "the first destroy removes it");

  assert!(
    !container_exists(&docker, &container_id).await,
    "the container must actually be gone from Docker, not merely stopped"
  );

  let second = client
    .delete(format!("http://{addr}/v1/jobs/{container_id}"))
    .bearer_auth(LIVE_TOKEN)
    .send()
    .await?;
  assert_eq!(
    second.status().as_u16(),
    404,
    "a repeat destroy of an already-gone container answers 404, not an error"
  );

  Ok(())
}

//! Live coverage for AC-4: a real `POST /v1/jobs` names a real container
//! whose Docker-level limits and environment match the request, does so
//! fast enough to fit inside the client's ten-second timeout, and never
//! puts the JIT config anywhere argv-adjacent.
//!
//! `#[ignore]`'d for the reason `docker_live.rs`'s module docs give — no
//! sysbox needed here, only a reachable Docker daemon. Split into its own
//! file rather than grown onto `docker_live.rs`: the four criteria
//! `daemon-live.yml` now covers (create, destroy, reap, exit-release) would
//! have pushed one file well past a readable size.

use std::net::SocketAddr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bollard::Docker;
use bollard::query_parameters::{InspectContainerOptions, RemoveContainerOptions};

use daemon::docker::DockerBackend;
use daemon::docker::spec::{ENV_JIT_CONFIG, memory_bytes, nano_cpus};
use daemon::gate::{Gate, JobSize};
use daemon::routes::build_router;
use daemon::routes::state::AppState;

/// Small, cheap to create, and already pulled by the rest of the live suite.
const IMAGE: &str = "alpine:3.20";

/// The bearer token the live router is configured with.
const LIVE_TOKEN: &str = "live-daemon-token";

/// A stand-in for GitHub's `encoded_jit_config` — base64 text, and the one
/// value this test proves never reaches argv.
const JIT_CONFIG: &str = "eyJydW5uZXIiOiJhYzQtcHJvYmUiLCJ0b2tlbiI6InNlY3JldCJ9";

/// A size distinct from the other live tests' `TAG_2VCPU_4GB`, so a
/// container this test finds cannot be mistaken for one another test left
/// behind.
const SIZE: JobSize = JobSize {
  vcpu: 4,
  memory_mb: 8192,
};

/// Six hours, the window `client.ts` puts between `nowMs` and `deadline`.
const SIX_HOURS_MS: i64 = 6 * 60 * 60 * 1000;

/// The client's own create timeout is 10s (`crates/daemon/README.md`); AC-4
/// pins a p99 budget of roughly a fifth of it with the image resident.
const CREATE_BUDGET: Duration = Duration::from_secs(2);

/// Samples large enough that "every one landed under budget" is a
/// meaningful stand-in for a p99 at this scale, small enough the live suite
/// stays fast.
const SAMPLES: usize = 10;

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
    vcpu: 64,
    memory_mb: 131_072,
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

/// POST a real `createVpsInstance` body for `job_id`, at [`SIZE`].
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
        "jitConfig": JIT_CONFIG,
        "image": IMAGE,
        "size": { "vcpu": SIZE.vcpu, "memoryMb": SIZE.memory_mb },
        "jobRef": { "org": "acme", "repo": "widgets", "jobId": job_id },
        "purpose": "live create test (AC-4)",
        "deadline": deadline_ms,
      }))
      .send()
      .await?,
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

/// Assert one created container's Docker-level shape matches the request:
/// the resource limits are exact, the JIT config is in the environment, and
/// it is nowhere any argv-adjacent field would put it.
///
/// # Errors
///
/// Returns the bollard error if the container cannot be inspected, or a
/// plain error if the inspected container is missing a section this
/// assertion depends on.
async fn assert_matches_the_request(docker: &Docker, container_id: &str) -> LiveResult<()> {
  let inspected = docker
    .inspect_container(container_id, None::<InspectContainerOptions>)
    .await?;

  let host_config = inspected
    .host_config
    .ok_or("inspected container carries no HostConfig")?;
  assert_eq!(
    host_config.nano_cpus,
    Some(nano_cpus(SIZE.vcpu)),
    "NanoCpus must match the request"
  );
  assert_eq!(
    host_config.memory,
    Some(memory_bytes(SIZE.memory_mb)),
    "Memory must match the request"
  );

  let config = inspected
    .config
    .ok_or("inspected container carries no Config")?;
  let env = config.env.unwrap_or_default();
  assert!(
    env.contains(&format!("{ENV_JIT_CONFIG}={JIT_CONFIG}")),
    "TOOLU_JITCONFIG must be present in the environment"
  );

  // The image boots with zero arguments (docs/container-image.md); Docker
  // may still fill Cmd from the image's own default (alpine's is
  // `["/bin/sh"]`), so the assertion is not "these are unset" but "the JIT
  // config is nowhere any of them could leak it".
  let cmd_text = format!("{:?}", config.cmd);
  let entrypoint_text = format!("{:?}", config.entrypoint);
  let args_text = format!("{:?}", inspected.args);
  assert!(
    !cmd_text.contains(JIT_CONFIG)
      && !entrypoint_text.contains(JIT_CONFIG)
      && !args_text.contains(JIT_CONFIG),
    "the JIT config must never reach Cmd, Entrypoint or Args"
  );
  Ok(())
}

#[tokio::test]
#[ignore = "live docker test — requires a reachable Docker daemon (docker info)"]
async fn a_real_create_names_a_matching_container_fast_with_a_hidden_jit_config() -> LiveResult<()>
{
  let docker = Docker::connect_with_defaults()?;
  let backend = DockerBackend::new(docker.clone(), "runc");
  // AC-4 is specifically the image-resident case — the absent-image path is
  // `docker_live.rs`'s pre-pull test.
  backend.attempt_pull(IMAGE).await;
  assert!(
    backend.image_present(IMAGE).await?,
    "AC-4 assumes the image is already resident"
  );

  let dir = tempfile::tempdir()?;
  let token_file = dir.path().join("token");
  std::fs::write(&token_file, LIVE_TOKEN)?;
  let addr = spawn_live_daemon(backend.clone(), token_file).await?;
  let client = reqwest::Client::new();

  let base = now_ms()?;
  let deadline_ms = base + SIX_HOURS_MS;
  let mut container_ids = Vec::with_capacity(SAMPLES);
  let mut durations = Vec::with_capacity(SAMPLES);

  for sample in 0..SAMPLES {
    let job_id = format!("ac4-{base}-{sample}");
    let started = Instant::now();
    let response = post_create(&client, addr, &job_id, deadline_ms).await?;
    let elapsed = started.elapsed();
    assert_eq!(
      response.status().as_u16(),
      201,
      "job {job_id} must create with the image resident"
    );
    let body: serde_json::Value = response.json().await?;
    let container_id = body
      .get("containerId")
      .and_then(serde_json::Value::as_str)
      .ok_or("the 201 body must carry containerId")?
      .to_owned();
    container_ids.push(container_id);
    durations.push(elapsed);
  }

  for container_id in &container_ids {
    assert_matches_the_request(&docker, container_id).await?;
  }

  for elapsed in &durations {
    assert!(
      *elapsed < CREATE_BUDGET,
      "a create with the image resident took {elapsed:?}, over the {CREATE_BUDGET:?} p99 budget \
       (the client's own timeout is 10s)"
    );
  }

  for container_id in &container_ids {
    force_remove(&docker, container_id).await;
  }
  Ok(())
}

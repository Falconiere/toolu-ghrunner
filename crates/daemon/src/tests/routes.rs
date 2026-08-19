//! Tests for the daemon's HTTP routing: a real axum server bound to a real
//! ephemeral TCP port, driven by real `reqwest` HTTP requests — no mock
//! transport and no handler called directly. [`RecordingBackend`] is a real
//! in-process [`JobBackend`] that records what it was asked to do; the
//! Docker-backed backend itself is exercised live elsewhere, not here.

use std::collections::HashMap;
use std::fs;
use std::future::Future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

use super::backend::{BackendError, CreateError, CreateJobResult, DestroyOutcome, JobBackend};
use super::state::AppState;
use super::wire::CreateJobRequest;
use crate::gate::{Gate, JobSize};

/// The bearer token every test server is configured with.
const TOKEN: &str = "test-daemon-token";
/// The header every response must carry — see `super::header`.
const DAEMON_HEADER: &str = "sh-toolu-daemon";

/// One call [`RecordingBackend`] was asked to make.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RecordedCall {
  Create { image: String },
  Destroy { container_id: String },
  Reap { job_id: String },
}

/// A real, in-process [`JobBackend`] that records every call it receives
/// and never touches Docker. Shares its recording storage across clones (an
/// `Arc` inside), so a handle kept by the test after the server has taken
/// its own clone still observes the same calls.
#[derive(Clone)]
struct RecordingBackend {
  calls: Arc<Mutex<Vec<RecordedCall>>>,
  next_id: Arc<AtomicU64>,
  image_resident: Arc<AtomicBool>,
  /// Container id -> the job id it was created for, the pairing a real
  /// backend reads back off the container's `sh.toolu.job-id` label.
  known_containers: Arc<Mutex<HashMap<String, String>>>,
  /// When present, every create parks on this semaphore after recording
  /// itself — a real in-flight create, held open for as long as a test needs
  /// to observe what the daemon does while one is outstanding.
  create_hold: Option<Arc<Semaphore>>,
}

impl RecordingBackend {
  /// A backend whose image is resident and whose creates always succeed.
  fn new() -> Self {
    Self {
      calls: Arc::new(Mutex::new(Vec::new())),
      next_id: Arc::new(AtomicU64::new(0)),
      image_resident: Arc::new(AtomicBool::new(true)),
      known_containers: Arc::new(Mutex::new(HashMap::new())),
      create_hold: None,
    }
  }

  /// A backend whose creates hang until [`Self::release_creates`] — the only
  /// way to ask the daemon a question about a create that has not finished.
  fn blocking_create() -> Self {
    Self {
      create_hold: Some(Arc::new(Semaphore::new(0))),
      ..Self::new()
    }
  }

  /// Let every parked create through.
  fn release_creates(&self) {
    if let Some(hold) = &self.create_hold {
      hold.add_permits(Semaphore::MAX_PERMITS);
    }
  }

  /// Block until at least one create has reached the backend, so a test can
  /// act while it is genuinely in flight rather than on a guess about timing.
  /// Reports whether one arrived within five seconds — the caller asserts, so
  /// a timeout fails its own test rather than this helper.
  async fn wait_for_create_in_flight(&self) -> bool {
    for _attempt in 0..500_u32 {
      let started = self
        .calls()
        .iter()
        .any(|call| matches!(call, RecordedCall::Create { .. }));
      if started {
        return true;
      }
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
  }

  /// A backend whose pinned image is not resident yet, so every create
  /// fails with [`CreateError::ImageNotResident`].
  fn image_not_resident() -> Self {
    let backend = Self::new();
    backend.image_resident.store(false, Ordering::SeqCst);
    backend
  }

  /// Every call this backend has recorded so far, in order.
  fn calls(&self) -> Vec<RecordedCall> {
    self
      .calls
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone()
  }
}

impl JobBackend for RecordingBackend {
  async fn create(&self, req: &CreateJobRequest) -> Result<CreateJobResult, CreateError> {
    if !self.image_resident.load(Ordering::SeqCst) {
      return Err(CreateError::ImageNotResident);
    }
    self
      .calls
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .push(RecordedCall::Create {
        image: req.image.clone(),
      });
    if let Some(hold) = &self.create_hold {
      let permit = hold.acquire().await.expect("create hold semaphore");
      permit.forget();
    }
    let id = self.next_id.fetch_add(1, Ordering::SeqCst);
    let container_id = format!("fake-container-{id}");
    self
      .known_containers
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .insert(container_id.clone(), req.job_ref.job_id.clone());
    Ok(CreateJobResult { container_id })
  }

  async fn destroy(&self, container_id: &str) -> Result<DestroyOutcome, BackendError> {
    self
      .calls
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .push(RecordedCall::Destroy {
        container_id: container_id.to_owned(),
      });
    let removed = self
      .known_containers
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .remove(container_id);
    match removed {
      Some(job_id) => Ok(DestroyOutcome::Removed {
        job_id: Some(job_id),
      }),
      None => Ok(DestroyOutcome::NotFound),
    }
  }

  async fn reap(&self, job_id: &str) {
    self
      .calls
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .push(RecordedCall::Reap {
        job_id: job_id.to_owned(),
      });
  }
}

/// Write a bearer token file with the given contents, returning its path.
fn write_token_file(dir: &Path, contents: &str) -> PathBuf {
  let path = dir.join("token");
  fs::write(&path, contents).expect("write token file");
  path
}

/// Bind a real axum server for `backend` on an ephemeral loopback port and
/// serve it on a detached task for the rest of the test. Returns the bound
/// address.
async fn spawn_daemon(
  backend: RecordingBackend,
  queue_max: u32,
  token_file: PathBuf,
) -> SocketAddr {
  let budget = JobSize {
    vcpu: 64,
    memory_mb: 131_072,
  };
  let gate = Gate::new(budget, queue_max);
  let state = AppState::new(backend, gate, token_file);
  let router = super::build_router(state);

  let listener = TcpListener::bind("127.0.0.1:0")
    .await
    .expect("bind ephemeral port");
  let addr = listener.local_addr().expect("local addr");
  tokio::spawn(async move {
    axum::serve(listener, router)
      .await
      .expect("serve axum router");
  });
  addr
}

/// A well-formed `POST /v1/jobs` body for GitHub job `job_id`. The job id is
/// a parameter because it is the gate's key: two requests naming the same one
/// are the same job, not two jobs competing for the queue.
fn create_request_body_for(job_id: &str) -> Value {
  json!({
    "jitConfig": "base64-encoded-jit-config",
    "image": "ghcr.io/falconiere/toolu-ghrunner:latest",
    "size": { "vcpu": 2, "memoryMb": 4096 },
    "jobRef": { "org": "acme", "repo": "widgets", "jobId": job_id },
    "purpose": "routes test",
    "deadline": 1_700_000_000_000_i64,
  })
}

/// A well-formed `POST /v1/jobs` body for the default job id.
fn create_request_body() -> Value {
  create_request_body_for("123")
}

/// POST a well-formed create request for `job_id` with `token` as the bearer.
async fn post_create_job(
  client: &reqwest::Client,
  addr: SocketAddr,
  token: &str,
  job_id: &str,
) -> reqwest::Response {
  client
    .post(format!("http://{addr}/v1/jobs"))
    .bearer_auth(token)
    .json(&create_request_body_for(job_id))
    .send()
    .await
    .expect("send create request")
}

/// POST a well-formed create request with `token` as the bearer.
async fn post_create(client: &reqwest::Client, addr: SocketAddr, token: &str) -> reqwest::Response {
  post_create_job(client, addr, token, "123").await
}

/// Assert the response carries `sh-toolu-daemon: 1`.
fn assert_daemon_header(response: &reqwest::Response) {
  let value = response
    .headers()
    .get(DAEMON_HEADER)
    .expect("missing sh-toolu-daemon header");
  assert_eq!(value, "1");
}

/// Run an async test body against a fresh token file and daemon server —
/// the setup every test below needs, factored out to keep each test itself
/// short.
async fn with_daemon<F, Fut>(backend: RecordingBackend, queue_max: u32, body: F)
where
  F: FnOnce(SocketAddr) -> Fut,
  Fut: Future<Output = ()>,
{
  let dir = tempfile::tempdir().expect("tempdir");
  let token_path = write_token_file(dir.path(), &format!("{TOKEN}\n"));
  let addr = spawn_daemon(backend, queue_max, token_path).await;
  body(addr).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn valid_create_returns_201_with_container_id_and_header() {
  with_daemon(RecordingBackend::new(), 32, |addr| async move {
    let client = reqwest::Client::new();
    let response = post_create(&client, addr, TOKEN).await;

    assert_eq!(response.status(), reqwest::StatusCode::CREATED);
    assert_daemon_header(&response);
    let body: Value = response.json().await.expect("json body");
    assert!(body.get("containerId").and_then(Value::as_str).is_some());
  })
  .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_bearer_is_rejected_with_401_and_header() {
  with_daemon(RecordingBackend::new(), 32, |addr| async move {
    let client = reqwest::Client::new();
    let response = client
      .post(format!("http://{addr}/v1/jobs"))
      .json(&create_request_body())
      .send()
      .await
      .expect("send request");

    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert_daemon_header(&response);
  })
  .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_bearer_is_rejected_with_401_and_header() {
  with_daemon(RecordingBackend::new(), 32, |addr| async move {
    let client = reqwest::Client::new();
    let response = post_create(&client, addr, "not-the-right-token").await;

    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert_daemon_header(&response);
  })
  .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn create_past_the_queue_ceiling_returns_429_with_header() {
  with_daemon(RecordingBackend::new(), 1, |addr| async move {
    let client = reqwest::Client::new();

    // Two DIFFERENT jobs: the ceiling is a queue depth, so proving it needs
    // a second job the gate would otherwise happily admit.
    let first = post_create_job(&client, addr, TOKEN, "job-1").await;
    assert_eq!(first.status(), reqwest::StatusCode::CREATED);

    let second = post_create_job(&client, addr, TOKEN, "job-2").await;
    assert_eq!(second.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    assert_daemon_header(&second);
    let body: Value = second.json().await.expect("json body");
    assert!(
      body
        .get("error")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty())
    );
  })
  .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn image_not_resident_returns_503_with_header() {
  with_daemon(
    RecordingBackend::image_not_resident(),
    32,
    |addr| async move {
      let client = reqwest::Client::new();
      let response = post_create(&client, addr, TOKEN).await;

      assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
      assert_daemon_header(&response);
    },
  )
  .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_json_body_returns_4xx_with_error_shape_and_header() {
  with_daemon(RecordingBackend::new(), 32, |addr| async move {
    let client = reqwest::Client::new();
    let response = client
      .post(format!("http://{addr}/v1/jobs"))
      .bearer_auth(TOKEN)
      .header("content-type", "application/json")
      .body("{ not valid json")
      .send()
      .await
      .expect("send malformed request");

    assert!(response.status().is_client_error());
    assert_daemon_header(&response);
    let body: Value = response.json().await.expect("json body");
    assert!(body.get("error").and_then(Value::as_str).is_some());
  })
  .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn destroy_known_container_returns_204_then_404_when_gone() {
  with_daemon(RecordingBackend::new(), 32, |addr| async move {
    let client = reqwest::Client::new();
    let created = post_create(&client, addr, TOKEN).await;
    let created_body: Value = created.json().await.expect("json body");
    let container_id = created_body
      .get("containerId")
      .and_then(Value::as_str)
      .expect("containerId present")
      .to_owned();

    let first_delete = client
      .delete(format!("http://{addr}/v1/jobs/{container_id}"))
      .bearer_auth(TOKEN)
      .send()
      .await
      .expect("send destroy request");
    assert_eq!(first_delete.status(), reqwest::StatusCode::NO_CONTENT);
    assert_daemon_header(&first_delete);

    let second_delete = client
      .delete(format!("http://{addr}/v1/jobs/{container_id}"))
      .bearer_auth(TOKEN)
      .send()
      .await
      .expect("send second destroy request");
    assert_eq!(second_delete.status(), reqwest::StatusCode::NOT_FOUND);
    assert_daemon_header(&second_delete);
  })
  .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reap_returns_204_for_known_and_unknown_job_id() {
  let backend = RecordingBackend::new();
  let backend_handle = backend.clone();
  with_daemon(backend, 32, |addr| async move {
    let client = reqwest::Client::new();

    let unknown = client
      .delete(format!("http://{addr}/v1/jobs?jobId=never-existed"))
      .bearer_auth(TOKEN)
      .send()
      .await
      .expect("send reap request for unknown job id");
    assert_eq!(unknown.status(), reqwest::StatusCode::NO_CONTENT);
    assert_daemon_header(&unknown);

    let known = client
      .delete(format!("http://{addr}/v1/jobs?jobId=42"))
      .bearer_auth(TOKEN)
      .send()
      .await
      .expect("send reap request for known job id");
    assert_eq!(known.status(), reqwest::StatusCode::NO_CONTENT);
    assert_daemon_header(&known);
  })
  .await;

  let calls = backend_handle.calls();
  assert!(calls.contains(&RecordedCall::Reap {
    job_id: "never-existed".to_owned()
  }));
  assert!(calls.contains(&RecordedCall::Reap {
    job_id: "42".to_owned()
  }));
}

/// The ordering `crate::routes::handlers::create_job` documents: the job id
/// is admitted to the gate BEFORE any Docker call, so a create still in
/// flight already holds its queue slot.
///
/// Proved through HTTP alone, with a queue ceiling of one: while job-1's
/// create is parked inside the backend, job-2 must be refused. Under the
/// reverse order job-1 would hold nothing yet, and job-2 would be admitted
/// and park in the backend too — the request would never answer, which the
/// timeout below reports rather than hangs on.
#[tokio::test(flavor = "multi_thread")]
async fn a_job_holds_its_queue_slot_while_its_create_is_still_in_flight() {
  let backend = RecordingBackend::blocking_create();
  let handle = backend.clone();

  with_daemon(backend, 1, |addr| async move {
    let client = reqwest::Client::new();
    let first_client = client.clone();
    let first =
      tokio::spawn(async move { post_create_job(&first_client, addr, TOKEN, "job-1").await });

    assert!(
      handle.wait_for_create_in_flight().await,
      "no create reached the backend within 5s"
    );

    let second = tokio::time::timeout(
      Duration::from_secs(5),
      post_create_job(&client, addr, TOKEN, "job-2"),
    )
    .await
    .expect("the second create must answer without waiting on the first");
    assert_eq!(second.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    assert_daemon_header(&second);

    handle.release_creates();
    let first = first.await.expect("join the first create");
    assert_eq!(first.status(), reqwest::StatusCode::CREATED);
  })
  .await;
}

/// A create that fails hands its queue slot back: the admission taken before
/// the Docker call is released when that call comes back empty-handed, or the
/// box would wedge at 429 after `TOOLU_DAEMON_QUEUE_MAX` failures.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_create_gives_its_queue_slot_back() {
  let backend = RecordingBackend::image_not_resident();
  with_daemon(backend, 1, |addr| async move {
    let client = reqwest::Client::new();

    for _attempt in 0..3_u32 {
      let response = post_create_job(&client, addr, TOKEN, "job-1").await;
      assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    }
  })
  .await;
}

/// GitHub redelivers `workflow_job.queued` at least once. A repeat `jobId`
/// while the gate still tracks it must answer exactly as the first call
/// did — 201 with the same container id — not the 500 an unhandled
/// `AdmitError::DuplicateJobId` used to produce, which `classifyVpsStatus`
/// on the client reads as `unavailable` and answers with fallback **plus a
/// host cooldown** for a condition that is not actually a fault. Only one
/// `docker create` may reach the backend, and only one queue slot may be
/// consumed, however many times the same job id is posted.
#[tokio::test(flavor = "multi_thread")]
async fn a_repeated_create_is_idempotent() {
  let backend = RecordingBackend::new();
  let handle = backend.clone();
  with_daemon(backend, 1, |addr| async move {
    let client = reqwest::Client::new();

    let first = post_create_job(&client, addr, TOKEN, "job-redelivered").await;
    assert_eq!(first.status(), reqwest::StatusCode::CREATED);
    let first_body: Value = first.json().await.expect("json body");
    let first_container_id = first_body
      .get("containerId")
      .and_then(Value::as_str)
      .expect("containerId present")
      .to_owned();

    let second = post_create_job(&client, addr, TOKEN, "job-redelivered").await;
    assert_eq!(second.status(), reqwest::StatusCode::CREATED);
    assert_daemon_header(&second);
    let second_body: Value = second.json().await.expect("json body");
    let second_container_id = second_body
      .get("containerId")
      .and_then(Value::as_str)
      .expect("containerId present");
    assert_eq!(second_container_id, first_container_id);
  })
  .await;

  // Exactly one `docker create` reached the backend — the redelivery never
  // called it a second time.
  let create_calls = handle
    .calls()
    .into_iter()
    .filter(|call| matches!(call, RecordedCall::Create { .. }))
    .count();
  assert_eq!(create_calls, 1);
}

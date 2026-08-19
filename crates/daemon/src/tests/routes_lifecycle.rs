//! The daemon's own bookkeeping, driven through the same real axum server
//! and real `reqwest` client as the rest of `routes.rs`'s tests — but asked
//! about the things a status code alone cannot answer: what a client
//! disconnect leaves behind, what a reap racing a create leaves behind, what
//! a create for an image this host does not serve costs, whether a reap that
//! settled nothing may still hand the box's capacity away, and what a 401
//! tells a caller who has no business knowing it.
//!
//! Every helper here comes from `routes.rs`; only the scenarios are new.

use std::net::SocketAddr;
use std::sync::PoisonError;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::{
  PINNED_IMAGE, RecordedCall, RecordingBackend, TOKEN, assert_daemon_header, create_request_bytes,
  post_create_job, spawn_daemon_with_state, with_daemon, with_daemon_state,
};
use crate::routes::state::AppState;

/// How long a test waits for the detached create task to finish its
/// bookkeeping. Generous — it is only ever reached in full when the invariant
/// under test is broken, in which case the test is failing anyway.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a disconnected client waits for the daemon to notice it is gone
/// before calling that the failure. Only reached when axum stops cancelling
/// dropped connections, which is the premise these tests rest on.
const HANG_UP_TIMEOUT: Duration = Duration::from_secs(5);

/// Poll `condition` until it holds or [`SETTLE_TIMEOUT`] passes. Reports
/// whether it ever held, so the caller asserts and a timeout fails its own
/// test rather than this helper.
async fn eventually(mut condition: impl FnMut() -> bool) -> bool {
  let step = Duration::from_millis(10);
  let attempts = SETTLE_TIMEOUT.as_millis() / step.as_millis();
  for _attempt in 0..attempts {
    if condition() {
      return true;
    }
    tokio::time::sleep(step).await;
  }
  false
}

/// The gate's current queue depth — the number the `TOOLU_DAEMON_QUEUE_MAX`
/// ceiling is compared against, and the one a leaked admission inflates.
fn queue_depth(state: &AppState<RecordingBackend>) -> u32 {
  state
    .gate
    .lock()
    .unwrap_or_else(PoisonError::into_inner)
    .consumption()
    .queue_depth
}

/// Whether the daemon has recorded a container for `job_id`.
fn has_recorded_container(state: &AppState<RecordingBackend>, job_id: &str) -> bool {
  state
    .created_containers
    .lock()
    .unwrap_or_else(PoisonError::into_inner)
    .existing(job_id)
    .is_some()
}

/// Send a real create for `job_id` over a raw socket, wait until it is
/// genuinely in flight inside the backend, then hang up — and confirm the
/// daemon noticed.
///
/// This is the production disconnect: `createVpsInstance` aborts with
/// `AbortSignal.timeout` at ten seconds and the connection goes away while
/// the daemon is still working. A raw socket rather than an HTTP client's
/// timeout because the moment of the hang-up has to be *chosen* — after the
/// create is provably in flight, not whenever a timer happens to fire.
///
/// The write half is closed and the read half kept open, which turns the
/// premise into an assertion: axum drops a handler whose client hung up, so
/// the daemon closes the connection without ever answering. A response
/// arriving here, or nothing arriving at all, means the disconnect never
/// reached the handler and the test that follows would prove nothing.
async fn hang_up_mid_create(addr: SocketAddr, job_id: &str, backend: &RecordingBackend) {
  let mut socket = TcpStream::connect(addr).await.expect("connect");
  socket
    .write_all(&create_request_bytes(addr, job_id))
    .await
    .expect("send the create request");
  socket.flush().await.expect("flush the create request");

  assert!(
    backend.wait_for_create_in_flight().await,
    "no create reached the backend within 5s"
  );
  socket.shutdown().await.expect("hang up on the daemon");

  let mut answer = Vec::new();
  let closed = tokio::time::timeout(HANG_UP_TIMEOUT, socket.read_to_end(&mut answer)).await;
  assert!(
    closed.is_ok(),
    "the daemon went on holding a connection whose client had hung up — these tests rest on \
     axum dropping that handler, so nothing below would prove anything"
  );
  assert!(
    answer.is_empty(),
    "the daemon answered a client that was already gone: {}",
    String::from_utf8_lossy(&answer)
  );
}

/// POST a create for `job_id` asking for `image` — the drift case, where
/// `vps_hosts.image_ref` no longer matches this host's `TOOLU_DAEMON_IMAGE`.
async fn post_create_for_image(
  client: &reqwest::Client,
  addr: SocketAddr,
  job_id: &str,
  image: &str,
) -> reqwest::Response {
  client
    .post(format!("http://{addr}/v1/jobs"))
    .bearer_auth(TOKEN)
    .json(&json!({
      "jitConfig": "base64-encoded-jit-config",
      "image": image,
      "size": { "vcpu": 2, "memoryMb": 4096 },
      "jobRef": { "org": "acme", "repo": "widgets", "jobId": job_id },
      "purpose": "routes lifecycle test",
      "deadline": 1_700_000_000_000_i64,
    }))
    .send()
    .await
    .expect("send create request")
}

/// A create that fails hands its queue slot back — proved the only way it
/// can be: by admitting a DIFFERENT job afterwards. Asserting the failures'
/// own status proves nothing, because a leaked slot answers 503 too (the
/// repeat lands on `AdmitError::DuplicateJobId`, whose empty
/// created-container lookup is also a 503). Only the second job id can tell
/// the two worlds apart, and it does: 201 when the slot came back, 429 when
/// it did not.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_create_gives_its_queue_slot_back() {
  let backend = RecordingBackend::image_not_resident();
  let handle = backend.clone();
  with_daemon(backend, 1, |addr| async move {
    let client = reqwest::Client::new();

    for _attempt in 0..3_u32 {
      let response = post_create_job(&client, addr, TOKEN, "job-1").await;
      assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    }

    handle.set_image_resident(true);
    let next_job = post_create_job(&client, addr, TOKEN, "job-2").await;
    assert_eq!(
      next_job.status(),
      reqwest::StatusCode::CREATED,
      "three failed creates must leave the single queue slot free; a 429 here means each \
       failure kept its admission"
    );
    assert_daemon_header(&next_job);
  })
  .await;
}

/// The disconnect this daemon's whole create/start split exists for: the
/// client aborts at ten seconds, axum drops the handler future at its next
/// await, and the admission taken before that await must not be all the
/// daemon is left with. The container it went on to create has to be
/// recorded, or the job holds a queue slot nothing can ever address — the
/// reaper's exit pass only walks jobs the gate marks *running*.
#[tokio::test(flavor = "multi_thread")]
async fn a_create_whose_client_disconnects_still_records_its_container() {
  let backend = RecordingBackend::blocking_create();
  let handle = backend.clone();

  with_daemon_state(backend, 1, |addr, state| async move {
    hang_up_mid_create(addr, "job-1", &handle).await;

    handle.release_creates();
    assert!(
      eventually(|| has_recorded_container(&state, "job-1")).await,
      "the create outlived the request that started it, so its container must be recorded — \
       otherwise this job's queue slot is held by nothing"
    );

    // GitHub's redelivery of the same job now answers as the first create
    // would have: the recorded container, not "already being created".
    let redelivered = post_create_job(&reqwest::Client::new(), addr, TOKEN, "job-1").await;
    assert_eq!(redelivered.status(), reqwest::StatusCode::CREATED);
    let body: Value = redelivered.json().await.expect("json body");
    assert!(body.get("containerId").and_then(Value::as_str).is_some());
  })
  .await;
}

/// The same disconnect, over a create that then fails: nothing was created,
/// so the queue slot has to come back. One aborted request used to consume
/// it permanently, and `TOOLU_DAEMON_QUEUE_MAX` of them wedged the host at
/// 429 — which the client reads as `capacity`, draining silently to other
/// hosts with no cooldown and no signal.
#[tokio::test(flavor = "multi_thread")]
async fn a_failing_create_whose_client_disconnects_gives_its_queue_slot_back() {
  let backend = RecordingBackend::blocking_create();
  backend.set_image_resident(false);
  let handle = backend.clone();

  with_daemon_state(backend, 1, |addr, state| async move {
    hang_up_mid_create(addr, "job-1", &handle).await;
    assert_eq!(queue_depth(&state), 1, "the admission is taken up front");

    handle.release_creates();
    assert!(
      eventually(|| queue_depth(&state) == 0).await,
      "a create that failed after the client hung up must still release its admission"
    );

    handle.set_image_resident(true);
    let next_job = post_create_job(&reqwest::Client::new(), addr, TOKEN, "job-2").await;
    assert_eq!(
      next_job.status(),
      reqwest::StatusCode::CREATED,
      "a disconnect must not cost the host a queue slot"
    );
  })
  .await;
}

/// A reap that lands while the create is still in flight wins: it has
/// already cleared the gate, the queue and the created map, so the container
/// that arrives afterwards belongs to nothing. Re-recording it would leave
/// two maps holding an entry `reconcile` can never drain — `try_start`
/// answers `None` forever for a job the gate no longer holds — and would
/// answer 201 with a container the reap has already removed.
#[tokio::test(flavor = "multi_thread")]
async fn a_reap_that_wins_a_race_with_a_create_leaves_nothing_behind() {
  let backend = RecordingBackend::blocking_create();
  let handle = backend.clone();

  with_daemon_state(backend, 1, |addr, state| async move {
    let client = reqwest::Client::new();
    let create_client = client.clone();
    let create =
      tokio::spawn(async move { post_create_job(&create_client, addr, TOKEN, "job-1").await });
    assert!(
      handle.wait_for_create_in_flight().await,
      "no create reached the backend within 5s"
    );

    let reaped = client
      .delete(format!("http://{addr}/v1/jobs?jobId=job-1"))
      .bearer_auth(TOKEN)
      .send()
      .await
      .expect("send reap request");
    assert_eq!(reaped.status(), reqwest::StatusCode::NO_CONTENT);

    handle.release_creates();
    let create = create.await.expect("join the create");
    assert_eq!(
      create.status(),
      reqwest::StatusCode::SERVICE_UNAVAILABLE,
      "a create whose job was reaped mid-flight must not answer 201 with a removed container"
    );

    assert!(
      !has_recorded_container(&state, "job-1"),
      "the created-container map must not be re-populated behind the reap"
    );
    assert!(
      state
        .start_queue
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .is_empty(),
      "the start queue must not be re-populated behind the reap"
    );
    assert_eq!(queue_depth(&state), 0, "the gate let this job go");
    assert!(
      handle
        .calls()
        .iter()
        .any(|call| matches!(call, RecordedCall::Destroy { .. })),
      "the container nothing tracks anymore has to be removed, not left to its deadline"
    );
  })
  .await;
}

/// `TOOLU_DAEMON_IMAGE` is the only image this host ever pre-pulls, so a
/// `vps_hosts.image_ref` that has drifted from it is a total outage — every
/// create 404s inside Docker and comes back as "not resident yet", a 503 that
/// re-stamps a five-minute cooldown on every delivery while looking like a
/// slow pull. It is refused here instead: named, logged, and — crucially —
/// before it takes a queue slot.
#[tokio::test(flavor = "multi_thread")]
async fn a_create_for_an_image_this_host_does_not_serve_is_refused_by_name() {
  let backend = RecordingBackend::new();
  let handle = backend.clone();

  with_daemon_state(backend, 1, |addr, state| async move {
    let client = reqwest::Client::new();
    let drifted = "ghcr.io/falconiere/toolu-ghrunner:some-other-digest";
    let refused = post_create_for_image(&client, addr, "job-1", drifted).await;

    assert_eq!(refused.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    assert_daemon_header(&refused);
    let body: Value = refused.json().await.expect("json body");
    let message = body
      .get("error")
      .and_then(Value::as_str)
      .expect("error body")
      .to_owned();
    assert!(
      message.contains(drifted) && message.contains(PINNED_IMAGE),
      "the refusal has to name both images — this is the only place the drift is visible; got \
       {message:?}"
    );

    assert!(
      !handle
        .calls()
        .iter()
        .any(|call| matches!(call, RecordedCall::Create { .. })),
      "a create for an image this host cannot run must never reach Docker"
    );
    assert_eq!(
      queue_depth(&state),
      0,
      "a refused create must not hold a queue slot"
    );

    let served = post_create_job(&client, addr, TOKEN, "job-2").await;
    assert_eq!(served.status(), reqwest::StatusCode::CREATED);
  })
  .await;
}

/// A reap answers 204 whatever happened — `vps/verify.ts` probes with a
/// sentinel job id and depends on it. Releasing the job's budget is a
/// different claim: that the box has that vCPU and memory back. A removal
/// that failed, or a listing Docker would not answer, proves no such thing,
/// and handing that share to the next job overcommits the machine by exactly
/// the size of a container that is still running.
#[tokio::test(flavor = "multi_thread")]
async fn a_reap_that_settled_nothing_keeps_the_jobs_budget() {
  let backend = RecordingBackend::unresolved_reap();
  with_daemon(backend, 1, |addr| async move {
    let client = reqwest::Client::new();
    let created = post_create_job(&client, addr, TOKEN, "job-1").await;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);

    let reaped = client
      .delete(format!("http://{addr}/v1/jobs?jobId=job-1"))
      .bearer_auth(TOKEN)
      .send()
      .await
      .expect("send reap request");
    assert_eq!(
      reaped.status(),
      reqwest::StatusCode::NO_CONTENT,
      "the 204 is owed to the client either way"
    );

    let next_job = post_create_job(&client, addr, TOKEN, "job-2").await;
    assert_eq!(
      next_job.status(),
      reqwest::StatusCode::TOO_MANY_REQUESTS,
      "an unconfirmed reap must not free capacity a running container still holds"
    );
  })
  .await;
}

/// The other half of the pair: a reap the backend *did* settle releases the
/// job, so the next one is admitted. Without this, the test above would pass
/// just as well against a daemon that never released anything.
#[tokio::test(flavor = "multi_thread")]
async fn a_reap_that_settled_releases_the_jobs_budget() {
  with_daemon(RecordingBackend::new(), 1, |addr| async move {
    let client = reqwest::Client::new();
    let created = post_create_job(&client, addr, TOKEN, "job-1").await;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);

    let reaped = client
      .delete(format!("http://{addr}/v1/jobs?jobId=job-1"))
      .bearer_auth(TOKEN)
      .send()
      .await
      .expect("send reap request");
    assert_eq!(reaped.status(), reqwest::StatusCode::NO_CONTENT);

    let next_job = post_create_job(&client, addr, TOKEN, "job-2").await;
    assert_eq!(
      next_job.status(),
      reqwest::StatusCode::CREATED,
      "a confirmed reap gives the box its capacity back"
    );
  })
  .await;
}

/// The 401 body is one fixed word, whatever went wrong. `AuthError`'s own
/// `Display` distinguishes four failure modes, and `AuthError::TokenFile`
/// interpolates a `ConfigError` carrying the token file's absolute path —
/// which put the location of this box's bearer token in a response anyone who
/// can reach the tunnel can ask for, without a credential.
#[tokio::test(flavor = "multi_thread")]
async fn an_unauthorized_response_never_names_the_token_file() {
  let dir = tempfile::tempdir().expect("tempdir");
  let token_path = dir.path().join("daemon-token");
  std::fs::write(&token_path, format!("{TOKEN}\n")).expect("write token file");
  let (addr, _state) =
    spawn_daemon_with_state(RecordingBackend::new(), 1, token_path.clone()).await;

  // The token file is gone — a rotation half-applied, a bad mount, a wrong
  // path in the unit file. Every request now fails to verify.
  std::fs::remove_file(&token_path).expect("remove the token file");

  let client = reqwest::Client::new();
  let unreadable = post_create_job(&client, addr, TOKEN, "job-1").await;
  assert_eq!(unreadable.status(), reqwest::StatusCode::UNAUTHORIZED);
  assert_daemon_header(&unreadable);
  let body: Value = unreadable.json().await.expect("json body");
  let message = body
    .get("error")
    .and_then(Value::as_str)
    .expect("error body")
    .to_owned();

  let path_text = token_path.display().to_string();
  assert!(
    !message.contains(&path_text),
    "a 401 must not disclose the token file path; got {message:?}"
  );
  assert_eq!(
    message, "unauthorized",
    "every rejection answers the same, so the body cannot be used to tell one failure from \
     another"
  );

  // …and a plain wrong token is indistinguishable from the above.
  let wrong = post_create_job(&client, addr, "not-the-right-token", "job-1").await;
  assert_eq!(wrong.status(), reqwest::StatusCode::UNAUTHORIZED);
  let wrong_body: Value = wrong.json().await.expect("json body");
  assert_eq!(
    wrong_body.get("error").and_then(Value::as_str),
    Some("unauthorized")
  );
}

//! `retry_session_conflict` over the production `wire::net::create_session`
//! against a broker replaying the live github.com 409 capture
//! (`crates/wire/tests/session_conflict_409.json`: status 409, its headers,
//! empty body). The 401/500 replies are controlled failures proving that
//! other errors stop the retry with their own classification.

use std::num::NonZeroU32;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use shared::RunnerError;

use crate::session_conflict::{SessionConflictPolicy, retry_session_conflict};

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

const CAPTURE: &str = include_str!("../../../wire/tests/session_conflict_409.json");

/// The captured 409 reply: its status, every recorded header, its empty body.
fn captured_conflict() -> TestResult<ResponseTemplate> {
  let fixture: Value = serde_json::from_str(CAPTURE)?;
  let field = |pointer: &str| {
    fixture
      .pointer(pointer)
      .ok_or(format!("fixture lacks {pointer}"))
  };
  let status = u16::try_from(field("/response/status")?.as_u64().ok_or("status")?)?;
  let mut template = ResponseTemplate::new(status);
  for (name, value) in field("/response/headers")?.as_object().ok_or("headers")? {
    // wiremock computes content-length itself from the (empty) body.
    if name != "content-length" {
      template = template.insert_header(name.as_str(), value.as_str().ok_or("header")?);
    }
  }
  Ok(template.set_body_string(field("/response/body")?.as_str().ok_or("body")?))
}

fn session_created() -> ResponseTemplate {
  ResponseTemplate::new(200).set_body_json(json!({
    "sessionId": "11111111-2222-3333-4444-555555555555",
    "ownerName": "toolu-91",
  }))
}

/// Mount `first` for the first `times` session POSTs, then `then` forever.
async fn broker(first: ResponseTemplate, times: u64, then: ResponseTemplate) -> MockServer {
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .and(path("/session"))
    .respond_with(first)
    .up_to_n_times(times)
    .with_priority(1)
    .mount(&server)
    .await;
  Mock::given(method("POST"))
    .and(path("/session"))
    .respond_with(then)
    .with_priority(2)
    .mount(&server)
    .await;
  server
}

async fn session_posts(server: &MockServer) -> TestResult<usize> {
  Ok(
    server
      .received_requests()
      .await
      .ok_or("request recording was disabled")?
      .len(),
  )
}

async fn attempt(
  server: &MockServer,
  cancel: &CancellationToken,
  policy: SessionConflictPolicy,
) -> Result<Option<protocol::CreateSessionResponse>, RunnerError> {
  let client = reqwest::Client::new();
  let request = protocol::build_session_request(158, "toolu-91-probe");
  let url = server.uri();
  retry_session_conflict(
    || wire::net::create_session(&client, &url, "access-token", &request),
    cancel,
    policy,
  )
  .await
}

fn fast(max_attempts: NonZeroU32) -> SessionConflictPolicy {
  SessionConflictPolicy {
    interval: Duration::from_millis(10),
    max_attempts,
  }
}

/// Upstream's eight attempts.
fn eight() -> NonZeroU32 {
  SessionConflictPolicy::default().max_attempts
}

#[test]
fn default_policy_is_upstreams_thirty_seconds_by_eight_attempts() {
  let policy = SessionConflictPolicy::default();
  assert_eq!(policy.interval, Duration::from_secs(30));
  assert_eq!(policy.max_attempts.get(), 8);
}

/// 91-S1: one captured conflict, then the broker frees the registration.
#[tokio::test]
async fn conflict_then_success_creates_the_session() -> TestResult<()> {
  let server = broker(captured_conflict()?, 1, session_created()).await;
  let session = attempt(&server, &CancellationToken::new(), fast(eight()))
    .await?
    .ok_or("not cancelled, so a session is expected")?;
  assert_eq!(session.session_id, "11111111-2222-3333-4444-555555555555");
  assert_eq!(session_posts(&server).await?, 2);
  Ok(())
}

/// 91-S2: a conflict on every attempt ends after exactly `max_attempts`.
#[tokio::test]
async fn persistent_conflict_stops_after_max_attempts() -> TestResult<()> {
  let server = broker(captured_conflict()?, 1, captured_conflict()?).await;
  let err = attempt(&server, &CancellationToken::new(), fast(eight()))
    .await
    .err()
    .ok_or("a persistent conflict must fail")?;
  assert!(matches!(err, RunnerError::SessionConflict(_)), "{err:?}");
  assert_eq!(session_posts(&server).await?, 8);
  Ok(())
}

/// 91-S2: an unrelated failure inside the window ends the retry unchanged.
#[tokio::test]
async fn unrelated_failure_after_a_conflict_keeps_its_classification() -> TestResult<()> {
  for status in [401, 500] {
    let server = broker(captured_conflict()?, 1, ResponseTemplate::new(status)).await;
    let err = attempt(&server, &CancellationToken::new(), fast(eight()))
      .await
      .err()
      .ok_or("the follow-up failure must surface")?;
    assert!(
      matches!(err, RunnerError::Protocol(ref m) if m.contains(&status.to_string())),
      "status {status}: {err:?}"
    );
    assert_eq!(session_posts(&server).await?, 2, "status {status}");
  }
  Ok(())
}

/// 91-S2: cancellation wakes the 30 s wait immediately and is not a failure.
#[tokio::test]
async fn cancel_during_the_retry_wait_returns_none_promptly() -> TestResult<()> {
  let server = broker(captured_conflict()?, 1, captured_conflict()?).await;
  let cancel = CancellationToken::new();
  let trigger = cancel.clone();
  tokio::spawn(async move {
    tokio::time::sleep(Duration::from_millis(200)).await;
    trigger.cancel();
  });
  let started = Instant::now();
  let outcome = attempt(&server, &cancel, SessionConflictPolicy::default()).await?;
  assert!(outcome.is_none(), "cancelled retry must return None");
  assert!(
    started.elapsed() < Duration::from_secs(2),
    "{:?}",
    started.elapsed()
  );
  assert_eq!(session_posts(&server).await?, 1);
  Ok(())
}

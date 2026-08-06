//! Wire-classification tests for `renew_job` / `complete_job` (AC-6,
//! `docs/toolu/specs/2026-08-06-outage-watchdog-design.md`).
//!
//! Pins the transient-vs-definitive split `wire::net::run_service` adopts:
//! a transport error, HTTP 429, or any 5xx maps to `RunnerError::Network`
//! (retryable — this is the class the B-001 outage watchdog feeds on); any
//! other non-2xx status maps to `RunnerError::Protocol` (definitive,
//! propagate). Real loopback HTTP throughout — a `wiremock::MockServer` for
//! the status-code cases, and a genuinely dead TCP port (bound then
//! dropped, no mock) for the connection-refused case — no mocks of
//! internal types. Pattern: `net_test.rs` (wiremock) +
//! `cache_proxy_test.rs::dead_upstream_base` (dead-port technique).

use std::time::Duration;

use shared::RunnerError;
use wire::reporting::ReportConclusion;
use wire::reporting::run_service::{CompleteJobRequest, RenewJobRequest, complete_job, renew_job};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Boxed error alias for test helpers that use `?` (matches
/// `cache_proxy_test.rs::TestResult`) — `clippy::expect_used` /
/// `unwrap_used` are only allowed inside `#[test]`/`#[tokio::test]`
/// functions themselves (`clippy.toml: allow-expect-in-tests`), not in the
/// plain helper functions below, so those propagate with `?` instead.
type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

fn renew_request() -> RenewJobRequest {
  RenewJobRequest {
    plan_id: "plan-1".to_owned(),
    job_id: "job-1".to_owned(),
  }
}

fn complete_request() -> CompleteJobRequest {
  CompleteJobRequest {
    plan_id: "plan-1".to_owned(),
    job_id: "job-1".to_owned(),
    request_id: 1,
    conclusion: ReportConclusion::Success,
    outputs: serde_json::Value::Object(serde_json::Map::new()),
    step_results: Vec::new(),
    annotations: Vec::new(),
  }
}

/// Bind an ephemeral port, read its address, and drop the listener so the
/// address now refuses connections — a genuine dead upstream, no mock
/// (technique: `cache_proxy_test.rs::dead_upstream_base`; no earlier
/// precedent in `wire`/`toolu-runner`'s own tests).
fn dead_upstream_base() -> TestResult<String> {
  let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
  let addr = listener.local_addr()?;
  drop(listener);
  Ok(format!("http://{addr}"))
}

/// Short-timeout client for the "response lands past the client timeout"
/// cases (item 5): 200ms is far below wiremock's 2s `set_delay`, so the
/// timeout fires deterministically without slowing the test suite down.
fn short_timeout_client() -> TestResult<reqwest::Client> {
  Ok(
    reqwest::Client::builder()
      .timeout(Duration::from_millis(200))
      .build()?,
  )
}

fn assert_network(result: Result<impl std::fmt::Debug, RunnerError>, what: &str) -> TestResult<()> {
  match result {
    Ok(v) => Err(format!("{what}: expected an error, got Ok({v:?})").into()),
    Err(RunnerError::Network(_)) => Ok(()),
    Err(other) => Err(format!("{what}: expected RunnerError::Network, got {other:?}").into()),
  }
}

fn assert_protocol(
  result: Result<impl std::fmt::Debug, RunnerError>,
  what: &str,
) -> TestResult<()> {
  match result {
    Ok(v) => Err(format!("{what}: expected an error, got Ok({v:?})").into()),
    Err(RunnerError::Protocol(_)) => Ok(()),
    Err(other) => Err(format!("{what}: expected RunnerError::Protocol, got {other:?}").into()),
  }
}

// --- 1. wiremock 503 -> Network ---------------------------------------------

#[tokio::test]
async fn renew_job_503_is_network() -> TestResult<()> {
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .and(path("/renewjob"))
    .respond_with(ResponseTemplate::new(503).set_body_string("upstream unavailable"))
    .expect(1)
    .mount(&server)
    .await;

  let client = reqwest::Client::new();
  let result = renew_job(&client, &server.uri(), "t", &renew_request()).await;
  assert_network(result, "renew_job 503")
}

#[tokio::test]
async fn complete_job_503_is_network() -> TestResult<()> {
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .and(path("/completejob"))
    .respond_with(ResponseTemplate::new(503).set_body_string("upstream unavailable"))
    .expect(1)
    .mount(&server)
    .await;

  let client = reqwest::Client::new();
  let result = complete_job(&client, &server.uri(), "t", &complete_request()).await;
  assert_network(result, "complete_job 503")
}

// --- 2. wiremock 400 -> Protocol ---------------------------------------------

#[tokio::test]
async fn renew_job_400_is_protocol() -> TestResult<()> {
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .and(path("/renewjob"))
    .respond_with(ResponseTemplate::new(400).set_body_string("bad request"))
    .expect(1)
    .mount(&server)
    .await;

  let client = reqwest::Client::new();
  let result = renew_job(&client, &server.uri(), "t", &renew_request()).await;
  assert_protocol(result, "renew_job 400")
}

#[tokio::test]
async fn complete_job_400_is_protocol() -> TestResult<()> {
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .and(path("/completejob"))
    .respond_with(ResponseTemplate::new(400).set_body_string("bad request"))
    .expect(1)
    .mount(&server)
    .await;

  let client = reqwest::Client::new();
  let result = complete_job(&client, &server.uri(), "t", &complete_request()).await;
  assert_protocol(result, "complete_job 400")
}

// --- 3. connection refused -> Network ----------------------------------------

#[tokio::test]
async fn renew_job_connection_refused_is_network() -> TestResult<()> {
  let base = dead_upstream_base()?;
  let client = reqwest::Client::new();
  let result = renew_job(&client, &base, "t", &renew_request()).await;
  assert_network(result, "renew_job connection refused")
}

#[tokio::test]
async fn complete_job_connection_refused_is_network() -> TestResult<()> {
  let base = dead_upstream_base()?;
  let client = reqwest::Client::new();
  let result = complete_job(&client, &base, "t", &complete_request()).await;
  assert_network(result, "complete_job connection refused")
}

// --- 4. 200 + garbage (non-JSON) body -> Protocol ----------------------------
//
// `renew_job` decodes `RenewJobResponse` off a 2xx body, so a malformed body
// is a genuine `is_decode()` failure -> `Protocol`. `complete_job` reports
// only success/failure (`Result<(), RunnerError>`) and never reads or
// decodes a 2xx body — this step changes error *mapping* only (see
// `run_service.rs::classify_transport_error` / `classify_status`), so it
// adds no new decode step there. The second test below pins that: a
// malformed 200 body still succeeds for `complete_job`, matching production
// behavior (no risk of a real, body-less `completejob` 200 suddenly failing).

#[tokio::test]
async fn renew_job_garbage_body_on_200_is_protocol() -> TestResult<()> {
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .and(path("/renewjob"))
    .respond_with(ResponseTemplate::new(200).set_body_string("not-json-at-all"))
    .expect(1)
    .mount(&server)
    .await;

  let client = reqwest::Client::new();
  let result = renew_job(&client, &server.uri(), "t", &renew_request()).await;
  assert_protocol(result, "renew_job garbage 200 body")
}

#[tokio::test]
async fn complete_job_garbage_body_on_200_still_succeeds() -> TestResult<()> {
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .and(path("/completejob"))
    .respond_with(ResponseTemplate::new(200).set_body_string("not-json-at-all"))
    .expect(1)
    .mount(&server)
    .await;

  let client = reqwest::Client::new();
  complete_job(&client, &server.uri(), "t", &complete_request()).await?;
  Ok(())
}

// --- 5. 200 with a delay past a short client timeout -> Network -------------

#[tokio::test]
async fn renew_job_response_past_client_timeout_is_network() -> TestResult<()> {
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .and(path("/renewjob"))
    .respond_with(
      ResponseTemplate::new(200)
        .set_body_json(serde_json::json!({ "lockedUntil": "2026-08-06T00:00:00Z" }))
        .set_delay(Duration::from_secs(2)),
    )
    .expect(1)
    .mount(&server)
    .await;

  let client = short_timeout_client()?;
  let result = renew_job(&client, &server.uri(), "t", &renew_request()).await;
  assert_network(result, "renew_job past client timeout")
}

#[tokio::test]
async fn complete_job_response_past_client_timeout_is_network() -> TestResult<()> {
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .and(path("/completejob"))
    .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(2)))
    .expect(1)
    .mount(&server)
    .await;

  let client = short_timeout_client()?;
  let result = complete_job(&client, &server.uri(), "t", &complete_request()).await;
  assert_network(result, "complete_job past client timeout")
}

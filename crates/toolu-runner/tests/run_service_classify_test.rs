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

// --- 6. non-2xx whose error body cannot be read -> WARN + still classified --

/// Serve exactly one request: a 500 status line promising a 100-byte body,
/// then close the socket without sending it. `reqwest`'s body read then fails
/// mid-message — a real truncated-response transport error over loopback, no
/// mocking of internal types.
fn truncated_500_server() -> TestResult<String> {
  let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
  let addr = listener.local_addr()?;
  std::thread::spawn(move || {
    if let Ok((mut sock, _peer)) = listener.accept() {
      // Drain the request head so the client's write completes before we reply.
      let mut buf = [0_u8; 2048];
      let _ = std::io::Read::read(&mut sock, &mut buf);
      let _ = std::io::Write::write_all(
        &mut sock,
        b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 100\r\n\r\n",
      );
      let _ = std::io::Write::flush(&mut sock);
      // Dropped here — the promised 100 bytes never arrive.
    }
  });
  Ok(format!("http://{addr}"))
}

/// Minimal hand-rolled `tracing::Subscriber` that records every event as
/// `(level, field name, Debug of value)`. Mirrors `listener::helpers`'s test
/// subscriber — asserting on emitted diagnostics needs no
/// `tracing-subscriber` dev-dependency, since `tracing::Subscriber` comes
/// with the plain `tracing` dep. `Visit`'s other `record_*` methods default
/// to forwarding into `record_debug`, so integer fields land here too.
#[derive(Clone, Default)]
struct EventCapture {
  events: std::sync::Arc<std::sync::Mutex<Vec<(tracing::Level, String, String)>>>,
}

impl EventCapture {
  /// Every recorded value for `field` at `level`.
  fn values(&self, level: tracing::Level, field: &str) -> Vec<String> {
    let Ok(events) = self.events.lock() else {
      return Vec::new();
    };
    events
      .iter()
      .filter(|(lvl, name, _)| *lvl == level && name == field)
      .map(|(_, _, value)| value.clone())
      .collect()
  }

  /// Whether any value recorded at `level` contains `needle`.
  fn saw(&self, level: tracing::Level, needle: &str) -> bool {
    let Ok(events) = self.events.lock() else {
      return false;
    };
    events
      .iter()
      .any(|(lvl, _, value)| *lvl == level && value.contains(needle))
  }
}

impl tracing::Subscriber for EventCapture {
  fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
    true
  }
  fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
    tracing::span::Id::from_u64(1)
  }
  fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
  fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
  fn event(&self, event: &tracing::Event<'_>) {
    struct Collect<'a> {
      level: tracing::Level,
      events: &'a std::sync::Mutex<Vec<(tracing::Level, String, String)>>,
    }
    impl tracing::field::Visit for Collect<'_> {
      fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if let Ok(mut events) = self.events.lock() {
          events.push((self.level, field.name().to_owned(), format!("{value:?}")));
        }
      }
    }
    event.record(&mut Collect {
      level: *event.metadata().level(),
      events: &self.events,
    });
  }
  fn enter(&self, _span: &tracing::span::Id) {}
  fn exit(&self, _span: &tracing::span::Id) {}
}

/// A 5xx whose body cannot be read must (a) still classify off the status
/// code — `Network`, the class the outage watchdog feeds on — and (b) surface
/// the body-read transport error at WARN, not only at the DEBUG level that
/// `shared::startup` filters out unless `TOOLU_RUNNER_ALLOW_VERBOSE=1`.
#[tokio::test]
async fn renew_job_unreadable_error_body_warns_and_still_classifies() -> TestResult<()> {
  let capture = EventCapture::default();
  let base = truncated_500_server()?;
  let client = short_timeout_client()?;

  let result = {
    let _guard = tracing::subscriber::set_default(capture.clone());
    renew_job(&client, &base, "token", &renew_request()).await
  };

  assert_network(result, "renew_job 500 with a truncated body")?;
  if !capture.saw(tracing::Level::WARN, "error body could not be read") {
    return Err("expected a WARN naming the unreadable error body".into());
  }
  Ok(())
}

/// An error body far larger than the cap must not be buffered whole:
/// `log_error_body` trims each chunk to the remaining budget, so
/// `bytes_read` lands on exactly `ERROR_BODY_READ_CAP` (200 chars x 4) and
/// the logged snippet stays at 200 chars — no matter how big the page is.
#[tokio::test]
async fn renew_job_oversized_error_body_is_read_up_to_the_cap() -> TestResult<()> {
  let capture = EventCapture::default();
  let huge = "x".repeat(64 * 1024);
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .and(path("/renewjob"))
    .respond_with(ResponseTemplate::new(500).set_body_string(huge))
    .mount(&server)
    .await;
  let client = short_timeout_client()?;

  let result = {
    let _guard = tracing::subscriber::set_default(capture.clone());
    renew_job(&client, &server.uri(), "token", &renew_request()).await
  };

  assert_network(result, "renew_job 500 with an oversized body")?;

  let bytes_read = capture.values(tracing::Level::DEBUG, "bytes_read");
  if bytes_read != vec!["800".to_owned()] {
    return Err(format!("expected a single bytes_read of 800, got {bytes_read:?}").into());
  }
  let bodies = capture.values(tracing::Level::DEBUG, "body");
  let [body] = bodies.as_slice() else {
    return Err(format!("expected exactly one body field, got {bodies:?}").into());
  };
  let chars = body.chars().count();
  if chars != 200 {
    return Err(format!("expected a 200-char snippet, got {chars}").into());
  }
  Ok(())
}

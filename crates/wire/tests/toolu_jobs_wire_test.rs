//! #206: `wire::net::toolu_jobs::TooluClient` against a raw loopback peer.
//!
//! The 401 replies replay the live `dev-console.toolu.sh` captures in
//! `toolu-runner/tests/fixtures/toolu_sh/` (status, headers and body
//! verbatim); the 200/409 bodies are that directory's contract-derived
//! shapes. Each peer records the request line, `Authorization` header and
//! body the production client sent.

#![cfg(test)]

use protocol::toolu_jobs::{CompleteBody, Outcome};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use wire::net::toolu_jobs::{TooluClient, TooluError};

const INVALID_TOKEN_401: &str =
  include_str!("../../toolu-runner/tests/fixtures/toolu_sh/invalid_token_401.json");
const MISSING_BEARER_401: &str =
  include_str!("../../toolu-runner/tests/fixtures/toolu_sh/missing_bearer_401.json");
const QUEUED_200: &str = include_str!("../../toolu-runner/tests/fixtures/toolu_sh/queued_200.json");
const CLAIM_200: &str = include_str!("../../toolu-runner/tests/fixtures/toolu_sh/claim_200.json");
const TAKEN_409: &str =
  include_str!("../../toolu-runner/tests/fixtures/toolu_sh/claim_taken_409.json");
const HEARTBEAT_200: &str =
  include_str!("../../toolu-runner/tests/fixtures/toolu_sh/heartbeat_200.json");
const OK_200: &str = include_str!("../../toolu-runner/tests/fixtures/toolu_sh/ok_200.json");

/// A synthetic, well-formed host token (`tlu_h_` + 43 base64url chars).
const HOST_TOKEN: &str = "tlu_h_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";

/// What the peer received.
struct Received {
  request_line: String,
  authorization: Option<String>,
  body: String,
}

/// Raw HTTP response bytes for `status` with `headers` and `body`.
fn response_bytes(status: u16, headers: &[(String, String)], body: &str) -> Vec<u8> {
  let mut out = format!("HTTP/1.1 {status} X\r\n");
  for (name, value) in headers {
    if !name.eq_ignore_ascii_case("content-length") {
      out.push_str(name);
      out.push_str(": ");
      out.push_str(value);
      out.push_str("\r\n");
    }
  }
  out.push_str("content-length: ");
  out.push_str(&body.len().to_string());
  out.push_str("\r\nconnection: close\r\n\r\n");
  out.push_str(body);
  out.into_bytes()
}

/// A live capture's status, headers and body.
fn capture(fixture: &str) -> (u16, Vec<(String, String)>, String) {
  let value: Value = serde_json::from_str(fixture).expect("fixture is JSON");
  let status = value
    .pointer("/response/status")
    .and_then(Value::as_u64)
    .and_then(|s| u16::try_from(s).ok())
    .expect("status");
  let headers = value
    .pointer("/response/headers")
    .and_then(Value::as_object)
    .expect("headers")
    .iter()
    .map(|(k, v)| (k.clone(), v.as_str().expect("header str").to_owned()))
    .collect();
  let body = value
    .pointer("/response/body")
    .and_then(Value::as_str)
    .expect("body")
    .to_owned();
  (status, headers, body)
}

/// Accept one request, record it, answer `response`.
async fn serve_once(listener: TcpListener, response: Vec<u8>) -> Received {
  let (socket, _) = listener.accept().await.expect("accept");
  let mut reader = BufReader::new(socket);
  let mut request_line = String::new();
  reader
    .read_line(&mut request_line)
    .await
    .expect("request line");
  let mut authorization = None;
  let mut length = 0usize;
  let mut line = String::new();
  loop {
    line.clear();
    assert!(reader.read_line(&mut line).await.expect("header") > 0);
    if line == "\r\n" {
      break;
    }
    if let Some((name, value)) = line.split_once(':') {
      if name.eq_ignore_ascii_case("content-length") {
        length = value.trim().parse().expect("content-length");
      }
      if name.eq_ignore_ascii_case("authorization") {
        authorization = Some(value.trim().to_owned());
      }
    }
  }
  let mut body = vec![0; length];
  reader.read_exact(&mut body).await.expect("request body");
  let mut socket = reader.into_inner();
  socket.write_all(&response).await.expect("write");
  socket.shutdown().await.expect("shutdown");
  Received {
    request_line: request_line.trim_end().to_owned(),
    authorization,
    body: String::from_utf8(body).expect("utf-8 body"),
  }
}

/// A loopback peer answering one request with `response`, and a client for it.
async fn peer(response: Vec<u8>) -> (TooluClient, tokio::task::JoinHandle<Received>) {
  let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
  let url = format!("http://{}", listener.local_addr().expect("addr"));
  let client = TooluClient::new(reqwest::Client::new(), &url, HOST_TOKEN).expect("client");
  (client, tokio::spawn(serve_once(listener, response)))
}

fn json_200(body: &str) -> Vec<u8> {
  response_bytes(
    200,
    &[("content-type".to_owned(), "application/json".to_owned())],
    body,
  )
}

#[tokio::test]
async fn queued_sends_the_host_bearer_and_parses_the_list() {
  let (client, server) = peer(json_200(QUEUED_200)).await;
  let queued = client.queued().await.expect("200");
  let received = server.await.expect("server");
  assert_eq!(received.request_line, "GET /runner/jobs/queued HTTP/1.1");
  assert_eq!(
    received.authorization.as_deref(),
    Some(format!("Bearer {HOST_TOKEN}").as_str())
  );
  assert_eq!(queued.jobs.len(), 1);
}

#[tokio::test]
async fn the_live_401_captures_are_revoked_with_their_error() {
  for (fixture, why) in [
    (INVALID_TOKEN_401, "invalid or revoked token"),
    (MISSING_BEARER_401, "missing bearer token"),
  ] {
    let (status, headers, body) = capture(fixture);
    assert_eq!(status, 401);
    let (client, server) = peer(response_bytes(status, &headers, &body)).await;
    let err = client.queued().await.expect_err("401");
    server.await.expect("server");
    assert_eq!(err, TooluError::Revoked(why.to_owned()));
    let message = err.to_string();
    assert!(
      message.contains(why) && !message.contains(HOST_TOKEN),
      "{message}"
    );
  }
}

#[tokio::test]
async fn claim_encodes_the_id_and_parses_the_jit() {
  let (client, server) = peer(json_200(CLAIM_200)).await;
  let claim = client.claim("job 1/x").await.expect("200");
  let received = server.await.expect("server");
  assert_eq!(
    received.request_line,
    "POST /runner/jobs/job%201%2Fx/claim HTTP/1.1"
  );
  assert_eq!(claim.claim_epoch, 0);
  assert!(
    !format!("{claim:?}").contains(&claim.jit_config),
    "Debug must not print the JIT"
  );
}

#[tokio::test]
async fn a_lost_claim_race_is_a_conflict_carrying_the_body() {
  let (client, server) = peer(response_bytes(409, &[], TAKEN_409.trim())).await;
  let err = client.claim("j1").await.expect_err("409");
  server.await.expect("server");
  assert_eq!(err, TooluError::Conflict(TAKEN_409.trim().to_owned()));
}

#[tokio::test]
async fn not_found_rate_limit_server_error_and_other_statuses_are_classified() {
  for (status, check) in [
    (
      404,
      (|e: &TooluError| *e == TooluError::NotFound) as fn(&TooluError) -> bool,
    ),
    (
      429,
      |e| matches!(e, TooluError::Transient(m) if m.contains("429")),
    ),
    (
      503,
      |e| matches!(e, TooluError::Transient(m) if m.contains("503")),
    ),
    (
      400,
      |e| matches!(e, TooluError::Protocol(m) if m.contains("400")),
    ),
  ] {
    let (client, server) = peer(response_bytes(status, &[], "{\"error\":\"x\"}")).await;
    let err = client.claim("j1").await.expect_err("non-2xx");
    server.await.expect("server");
    assert!(check(&err), "status {status}: {err:?}");
  }
}

#[tokio::test]
async fn an_unparseable_200_is_a_protocol_error() {
  let (client, server) = peer(json_200("<html>maintenance</html>")).await;
  let err = client.queued().await.expect_err("bad body");
  server.await.expect("server");
  assert!(matches!(err, TooluError::Protocol(_)), "{err:?}");
}

#[tokio::test]
async fn a_closed_port_is_transient() {
  let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
  let url = format!("http://{}", listener.local_addr().expect("addr"));
  drop(listener);
  let client = TooluClient::new(reqwest::Client::new(), &url, HOST_TOKEN).expect("client");
  let err = client.queued().await.expect_err("connection refused");
  assert!(matches!(err, TooluError::Transient(_)), "{err:?}");
}

#[tokio::test]
async fn heartbeat_dispatched_and_complete_send_the_claim_epoch() {
  let (client, server) = peer(json_200(HEARTBEAT_200)).await;
  let renewal = client.heartbeat("j1", 4).await.expect("200");
  let received = server.await.expect("server");
  assert_eq!(
    received.request_line,
    "POST /runner/jobs/j1/heartbeat HTTP/1.1"
  );
  assert_eq!(received.body, r#"{"claimEpoch":4}"#);
  assert_eq!(renewal.lease_expires_at, 1_792_772_891_368);

  let (client, server) = peer(json_200(OK_200)).await;
  client.dispatched("j1", 4).await.expect("200");
  let received = server.await.expect("server");
  assert_eq!(
    received.request_line,
    "POST /runner/jobs/j1/dispatched HTTP/1.1"
  );
  assert_eq!(received.body, r#"{"claimEpoch":4}"#);

  let (client, server) = peer(json_200(OK_200)).await;
  let body = CompleteBody {
    outcome: Outcome::Done,
    error_message: None,
    claim_epoch: 4,
  };
  client.complete("j1", &body).await.expect("200");
  let received = server.await.expect("server");
  assert_eq!(
    received.request_line,
    "POST /runner/jobs/j1/complete HTTP/1.1"
  );
  assert_eq!(received.body, r#"{"outcome":"done","claimEpoch":4}"#);
}

#[test]
fn a_bad_api_url_or_empty_token_is_a_config_error() {
  for (url, token) in [
    ("console.toolu.sh", HOST_TOKEN),
    ("ftp://console.toolu.sh", HOST_TOKEN),
    ("https://console.toolu.sh", ""),
  ] {
    let err = TooluClient::new(reqwest::Client::new(), url, token).expect_err("refused");
    assert!(
      matches!(err, shared::RunnerError::Config(_)),
      "{url:?}: {err:?}"
    );
  }
}

#[test]
fn debug_never_prints_the_host_token() {
  let client = TooluClient::new(
    reqwest::Client::new(),
    "https://console.toolu.sh",
    HOST_TOKEN,
  )
  .expect("client");
  assert!(!format!("{client:?}").contains(HOST_TOKEN));
}

//! #207: `wire::net::toolu_pair` and `TooluClient`'s `/runner/host` calls
//! against a raw loopback peer.
//!
//! The pairing replies replay the live `dev-console.toolu.sh` captures in
//! `toolu-runner/tests/fixtures/toolu_sh/` (status, headers and body
//! verbatim); the approved grant and `/runner/host` 200 bodies are the
//! contract's examples. Each peer records the request the client sent.

#![cfg(test)]

use protocol::toolu_pair::{MachineReport, PairPoll};
use serde_json::Value;
use tokio::net::TcpListener;
use wire::net::toolu_jobs::TooluError;
use wire::net::toolu_pair::{enroll, pair_start, pair_token};

use crate::toolu_jobs_wire_test::{
  HOST_TOKEN, INVALID_TOKEN_401, Received, capture, json_200, peer, response_bytes, serve_once,
};

const PAIR_200: &str = include_str!("../../toolu-runner/tests/fixtures/toolu_sh/pair_200.json");
const PENDING_400: &str =
  include_str!("../../toolu-runner/tests/fixtures/toolu_sh/pair_token_pending_400.json");
const SLOW_DOWN_400: &str =
  include_str!("../../toolu-runner/tests/fixtures/toolu_sh/pair_token_slow_down_400.json");
const EXPIRED_400: &str =
  include_str!("../../toolu-runner/tests/fixtures/toolu_sh/pair_token_expired_400.json");
const RESERVED_LABEL_400: &str =
  include_str!("../../toolu-runner/tests/fixtures/toolu_sh/pair_reserved_label_400.json");
const ENROLL_401: &str =
  include_str!("../../toolu-runner/tests/fixtures/toolu_sh/enroll_invalid_401.json");

/// The contract's approved `pair/token` / `enroll` 200.
const GRANT: &str = r#"{"host_token":"tlu_h_pairgrant207pairgrant207pairgrant207pairg","host_id":"83dca27b-4052-420a-a767-04c5cd758654","org":"org_123","labels":["gpu"]}"#;

/// The contract's `GET /runner/host` 200.
const HOST: &str = r#"{"host_id":"83dca27b-4052-420a-a767-04c5cd758654","name":"Build box","hostname":"worker-7","org":{"id":"org_123","name":"Example team"},"platform":"linux","arch":"x64","labels":["GPU"],"slots":2,"runner_version":"2.330.0","enabled":true,"last_seen_at":1792771200000}"#;

fn report() -> MachineReport {
  MachineReport {
    hostname: "build-box".to_owned(),
    platform: "linux".to_owned(),
    arch: "x64".to_owned(),
    vcpu: Some(8),
    memory_mb: Some(32_768),
    labels: vec!["gpu".to_owned()],
    runner_version: "0.12.2".to_owned(),
  }
}

/// A public-route peer answering one request with `response`, and its URL.
async fn bare_peer(response: Vec<u8>) -> (String, tokio::task::JoinHandle<Received>) {
  let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
  let url = format!("http://{}", listener.local_addr().expect("addr"));
  (url, tokio::spawn(serve_once(listener, response)))
}

/// A live capture as raw response bytes.
fn replay(fixture: &str) -> Vec<u8> {
  let (status, headers, body) = capture(fixture);
  response_bytes(status, &headers, &body)
}

#[tokio::test]
async fn pair_start_posts_the_report_and_parses_the_live_reply() {
  let (url, server) = bare_peer(replay(PAIR_200)).await;
  let start = pair_start(&reqwest::Client::new(), &url, &report())
    .await
    .expect("200");
  let received = server.await.expect("server");
  assert_eq!(received.request_line, "POST /runner/pair HTTP/1.1");
  assert_eq!(received.authorization, None, "pairing is unauthenticated");
  let sent: Value = serde_json::from_str(&received.body).expect("json body");
  assert_eq!(sent, serde_json::to_value(report()).expect("report"));
  assert_eq!(start.interval, 5);
  assert_eq!(start.expires_in, 900);
  assert!(start.verification_uri.ends_with("/pair"), "{start:?}");
  assert!(
    start
      .verification_uri_complete
      .ends_with(&format!("?code={}", start.user_code))
  );
}

#[tokio::test]
async fn the_live_pair_token_refusals_are_outcomes() {
  for (fixture, want) in [
    (PENDING_400, PairPoll::Pending),
    (SLOW_DOWN_400, PairPoll::SlowDown),
    (EXPIRED_400, PairPoll::Expired),
  ] {
    let (url, server) = bare_peer(replay(fixture)).await;
    let poll = pair_token(&reqwest::Client::new(), &url, "dc-secret")
      .await
      .expect("a 400 outcome");
    let received = server.await.expect("server");
    assert_eq!(received.request_line, "POST /runner/pair/token HTTP/1.1");
    assert_eq!(received.body, r#"{"device_code":"dc-secret"}"#);
    assert_eq!(poll, want);
  }
}

#[tokio::test]
async fn an_approved_poll_is_the_grant_and_a_503_is_transient() {
  let (url, server) = bare_peer(json_200(GRANT)).await;
  let poll = pair_token(&reqwest::Client::new(), &url, "dc")
    .await
    .expect("200");
  server.await.expect("server");
  assert!(
    matches!(&poll, PairPoll::Granted(grant) if grant.org == "org_123"),
    "{poll:?}"
  );
  let (url, server) = bare_peer(response_bytes(503, &[], "{\"error\":\"down\"}")).await;
  let err = pair_token(&reqwest::Client::new(), &url, "dc")
    .await
    .expect_err("503");
  server.await.expect("server");
  assert!(matches!(err, TooluError::Transient(_)), "{err:?}");
}

#[tokio::test]
async fn a_refused_enrollment_token_is_revoked_and_a_reserved_label_is_refused() {
  let (url, server) = bare_peer(replay(ENROLL_401)).await;
  let err = enroll(&reqwest::Client::new(), &url, "tlu_e_used", &report())
    .await
    .expect_err("401");
  let received = server.await.expect("server");
  assert_eq!(received.request_line, "POST /runner/enroll HTTP/1.1");
  let sent: Value = serde_json::from_str(&received.body).expect("json body");
  assert_eq!(
    sent.get("enroll_token").and_then(Value::as_str),
    Some("tlu_e_used")
  );
  assert_eq!(err, TooluError::Revoked("invalid_enroll_token".to_owned()));
  let (url, server) = bare_peer(replay(RESERVED_LABEL_400)).await;
  let err = pair_start(&reqwest::Client::new(), &url, &report())
    .await
    .expect_err("400");
  server.await.expect("server");
  assert!(
    matches!(&err, TooluError::Refused(m) if m.contains("toolu-gpu")),
    "{err:?}"
  );
}

#[tokio::test]
async fn an_accepted_enrollment_is_the_grant() {
  let (url, server) = bare_peer(json_200(GRANT)).await;
  let grant = enroll(&reqwest::Client::new(), &url, "tlu_e_x", &report())
    .await
    .expect("200");
  server.await.expect("server");
  assert_eq!(grant.host_id, "83dca27b-4052-420a-a767-04c5cd758654");
}

#[tokio::test]
async fn host_patch_and_delete_send_the_bearer() {
  let (client, server) = peer(json_200(HOST)).await;
  let host = client.host().await.expect("200");
  let received = server.await.expect("server");
  assert_eq!(received.request_line, "GET /runner/host HTTP/1.1");
  assert_eq!(
    received.authorization.as_deref(),
    Some(format!("Bearer {HOST_TOKEN}").as_str())
  );
  assert_eq!(host.org.name, "Example team");

  let (client, server) = peer(json_200(HOST)).await;
  client.update_host(&report().patch()).await.expect("200");
  let received = server.await.expect("server");
  assert_eq!(received.request_line, "PATCH /runner/host HTTP/1.1");
  let sent: Value = serde_json::from_str(&received.body).expect("json body");
  assert!(sent.get("platform").is_none(), "{sent}");
  assert_eq!(
    sent.get("runner_version").and_then(Value::as_str),
    Some("0.12.2")
  );

  let (client, server) = peer(response_bytes(204, &[], "")).await;
  client.revoke_host().await.expect("204");
  let received = server.await.expect("server");
  assert_eq!(received.request_line, "DELETE /runner/host HTTP/1.1");
}

#[tokio::test]
async fn a_revoked_machine_reading_itself_is_revoked() {
  let (client, server) = peer(replay(INVALID_TOKEN_401)).await;
  let err = client.host().await.expect_err("401");
  server.await.expect("server");
  assert_eq!(
    err,
    TooluError::Revoked("invalid or revoked token".to_owned())
  );
}

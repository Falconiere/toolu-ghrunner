//! Integration tests for the `toolu_runner::net` transport layer.
//!
//! Uses `wiremock` to simulate the broker + token endpoint and verify
//! the public `net::*` functions issue the right HTTP requests. These
//! tests don't touch the listener — they pin the wire protocol.

use serde_json::json;
use wiremock::matchers::{body_string, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn exchange_token_posts_form_to_authorization_url() {
  let server = MockServer::start().await;

  Mock::given(method("POST"))
    .and(path("/_apis/distributedtracing/oauth2/token"))
    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
      "access_token": "ghs_test_token_value",
      "expires_in": 1800,
      "token_type": "bearer",
    })))
    .expect(1)
    .mount(&server)
    .await;

  let client = reqwest::Client::new();
  let token = toolu_runner::net::exchange_token(
    &client,
    &format!("{}/_apis/distributedtracing/oauth2/token", server.uri()),
    "fake.jwt.value",
  )
  .await
  .expect("exchange_token should succeed");

  assert_eq!(token.access_token, "ghs_test_token_value");
  assert_eq!(token.token_type, "bearer");
  assert_eq!(token.expires_in, 1800);
}

#[tokio::test]
async fn exchange_token_returns_protocol_error_on_http_failure() {
  let server = MockServer::start().await;

  Mock::given(method("POST"))
    .and(path("/oauth/token"))
    .respond_with(ResponseTemplate::new(401).set_body_string("invalid_client"))
    .expect(1)
    .mount(&server)
    .await;

  let client = reqwest::Client::new();
  let err =
    toolu_runner::net::exchange_token(&client, &format!("{}/oauth/token", server.uri()), "bad.jwt")
      .await
      .expect_err("should error on 401");

  let msg = format!("{err}");
  assert!(msg.contains("401"), "expected status in error: {msg}");
  assert!(
    msg.contains("see debug log"),
    "expected redacted body in error: {msg}"
  );
}

#[tokio::test]
async fn acknowledge_message_posts_message_id_to_broker() {
  let server = MockServer::start().await;

  Mock::given(method("POST"))
    .and(path("/acknowledge"))
    .and(header("authorization", "Bearer test-token"))
    .and(body_string(r#"{"messageId":42}"#))
    .respond_with(ResponseTemplate::new(200))
    .expect(1)
    .mount(&server)
    .await;

  let client = reqwest::Client::new();
  toolu_runner::net::acknowledge_message(&client, &server.uri(), "test-token", 42)
    .await
    .expect("ack should succeed");
}

#[tokio::test]
async fn poll_message_returns_none_on_202() {
  let server = MockServer::start().await;

  Mock::given(method("GET"))
    .and(path("/message"))
    .and(query_param("sessionId", "test-session"))
    .respond_with(ResponseTemplate::new(202))
    .expect(1)
    .mount(&server)
    .await;

  let client = reqwest::Client::new();
  let params = toolu_runner::net::PollParams {
    client: &client,
    server_url_v2: &server.uri(),
    token: "t",
    session_id: "test-session",
    runner_version: "3.0.0",
    os: "linux",
    architecture: "x64",
  };

  let msg = toolu_runner::net::poll_message(&params)
    .await
    .expect("poll should succeed");

  assert!(msg.is_none(), "expected None on 202, got {msg:?}");
}

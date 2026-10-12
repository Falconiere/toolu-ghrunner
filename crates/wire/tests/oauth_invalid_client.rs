//! Token-exchange classification against the live github.com capture (#92).
//!
//! `oauth_invalid_client_400.json` is GitHub's verbatim reply to the OAuth
//! JWT exchange of a JIT registration whose runner was deleted on GitHub:
//! 400 with `{"error":"invalid_client",…}`. A loopback server replays it as
//! raw HTTP to the production `exchange_token`. A 400 with any other OAuth
//! error, the same body under 500, and a non-JSON 400 are controlled
//! variations proving those keep their classification.

#![cfg(test)]

use serde_json::Value;
use shared::RunnerError;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use wire::net::exchange_token;

const CAPTURE: &str = include_str!("oauth_invalid_client_400.json");

fn capture() -> Value {
  serde_json::from_str(CAPTURE).expect("fixture is JSON")
}

/// The fixture value at JSON `pointer` (`Null` when absent).
fn at<'a>(fixture: &'a Value, pointer: &str) -> &'a Value {
  fixture.pointer(pointer).unwrap_or(&Value::Null)
}

/// Raw HTTP response: `status`, the captured headers (with a recomputed
/// `content-length`), and `body`.
fn response_bytes(status: u16, body: &str) -> Vec<u8> {
  let fixture = capture();
  let reason = if status == 400 {
    "Bad Request"
  } else {
    "Internal Server Error"
  };
  let mut out = format!("HTTP/1.1 {status} {reason}\r\n");
  for (name, value) in at(&fixture, "/response/headers")
    .as_object()
    .expect("headers object")
  {
    if name == "content-length" {
      continue;
    }
    out.push_str(name);
    out.push_str(": ");
    out.push_str(value.as_str().expect("header str"));
    out.push_str("\r\n");
  }
  out.push_str("content-length: ");
  out.push_str(&body.len().to_string());
  out.push_str("\r\nconnection: close\r\n\r\n");
  out.push_str(body);
  out.into_bytes()
}

/// Accept one request, drain it, and answer with `status` / `body`.
async fn serve_once(listener: TcpListener, status: u16, body: String) {
  let (socket, _) = listener.accept().await.expect("accept");
  let mut reader = BufReader::new(socket);
  let mut line = String::new();
  let mut length = 0usize;
  loop {
    line.clear();
    assert!(reader.read_line(&mut line).await.expect("header") > 0);
    if line == "\r\n" {
      break;
    }
    if let Some((name, value)) = line.split_once(':')
      && name.eq_ignore_ascii_case("content-length")
    {
      length = value.trim().parse().expect("content-length");
    }
  }
  let mut request_body = vec![0; length];
  reader
    .read_exact(&mut request_body)
    .await
    .expect("request body");
  let mut socket = reader.into_inner();
  socket
    .write_all(&response_bytes(status, &body))
    .await
    .expect("write");
  socket.shutdown().await.expect("shutdown");
}

/// Run the production `exchange_token` against one replayed reply.
async fn exchange(status: u16, body: String) -> Result<protocol::AccessToken, RunnerError> {
  let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
  let url = format!(
    "http://{}/_apis/oauth2/token",
    listener.local_addr().expect("addr")
  );
  let server = tokio::spawn(serve_once(listener, status, body));
  let result = exchange_token(&reqwest::Client::new(), &url, "jwt-not-checked").await;
  server.await.expect("server task");
  result
}

fn captured_body() -> String {
  at(&capture(), "/response/body")
    .as_str()
    .expect("body str")
    .to_owned()
}

/// The fixture is the verbatim capture: 400, JSON, `invalid_client`.
#[test]
fn fixture_is_the_verbatim_live_capture() {
  let fixture = capture();
  assert_eq!(at(&fixture, "/response/status"), 400);
  assert_eq!(
    at(&fixture, "/response/headers/content-length"),
    &Value::from(captured_body().len().to_string())
  );
  let body: Value = serde_json::from_str(&captured_body()).expect("body is JSON");
  assert_eq!(body.get("error"), Some(&Value::from("invalid_client")));
}

/// A deleted registration is an auth failure (the JIT is dead), never a
/// transient protocol error to retry with the same config.
#[tokio::test]
async fn captured_invalid_client_is_an_auth_error() {
  let result = exchange(400, captured_body()).await;
  assert!(
    matches!(&result, Err(RunnerError::Auth(msg)) if msg.contains("invalid_client")),
    "{result:?}"
  );
}

/// Other replies keep their classification: another OAuth error or a
/// non-JSON 400 stays `Protocol`; a 500 stays `Network` even with the body.
#[tokio::test]
async fn other_failures_keep_their_classification() {
  let other = captured_body().replace("invalid_client", "invalid_grant");
  let result = exchange(400, other).await;
  assert!(
    matches!(result, Err(RunnerError::Protocol(_))),
    "{result:?}"
  );
  let result = exchange(400, "<html>bad request</html>".to_owned()).await;
  assert!(
    matches!(result, Err(RunnerError::Protocol(_))),
    "{result:?}"
  );
  let result = exchange(500, captured_body()).await;
  assert!(matches!(result, Err(RunnerError::Network(_))), "{result:?}");
}

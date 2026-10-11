//! Session-create status classification against the live github.com capture.
//!
//! `session_conflict_409.json` is the broker's verbatim reply to a second
//! `POST /session` for a JIT registration that already had a live session.
//! A loopback server replays its status, headers and (empty) body as raw HTTP
//! to the production `create_session`. The 401/403/500 cases change only the
//! status line, as controlled failures proving those keep their classification.

#![cfg(test)]

use serde_json::Value;
use shared::RunnerError;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use wire::net::create_session;

const CAPTURE: &str = include_str!("session_conflict_409.json");

fn capture() -> Value {
  serde_json::from_str(CAPTURE).expect("fixture is JSON")
}

/// Raw HTTP response bytes: `status` plus the captured headers and body.
fn response_bytes(status: u16) -> Vec<u8> {
  let fixture = capture();
  let response = &fixture["response"];
  let reason = match status {
    401 => "Unauthorized",
    403 => "Forbidden",
    409 => "Conflict",
    _ => "Internal Server Error",
  };
  let mut out = format!("HTTP/1.1 {status} {reason}\r\n");
  for (name, value) in response["headers"].as_object().expect("headers object") {
    out.push_str(&format!("{name}: {}\r\n", value.as_str().expect("header str")));
  }
  out.push_str("connection: close\r\n\r\n");
  out.push_str(response["body"].as_str().expect("body str"));
  out.into_bytes()
}

/// Accept one request, drain it, and answer with `status` replayed from the capture.
async fn serve_once(listener: TcpListener, status: u16) {
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
  let mut body = vec![0; length];
  reader.read_exact(&mut body).await.expect("request body");
  let mut socket = reader.into_inner();
  socket.write_all(&response_bytes(status)).await.expect("write");
  socket.shutdown().await.expect("shutdown");
}

async fn create_against(status: u16) -> Result<protocol::CreateSessionResponse, RunnerError> {
  let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
  let url = format!("http://{}", listener.local_addr().expect("addr"));
  let server = tokio::spawn(serve_once(listener, status));
  let request = protocol::build_session_request(158, "toolu-91-probe");
  let result = create_session(&reqwest::Client::new(), &url, "access-token", &request).await;
  server.await.expect("server task");
  result
}

#[test]
fn fixture_is_the_verbatim_live_capture() {
  let fixture = capture();
  let response = &fixture["response"];
  assert_eq!(response["status"], 409);
  assert_eq!(response["body"], "");
  assert_eq!(response["headers"]["content-length"], "0");
  assert_eq!(response["headers"]["server"], "github.com");
  let provenance = &fixture["provenance"];
  for key in ["captured", "host", "request", "registrations", "method"] {
    assert!(
      provenance[key].as_str().is_some_and(|v| !v.is_empty()),
      "provenance.{key} must be recorded"
    );
  }
  assert_eq!(provenance["host"], "github.com");
}

#[tokio::test]
async fn captured_409_is_a_session_conflict() {
  let err = create_against(409).await.expect_err("409 must fail");
  assert!(
    matches!(err, RunnerError::SessionConflict(_)),
    "409 must classify as SessionConflict, got {err:?}"
  );
  let message = err.to_string();
  assert!(
    message.contains("another runner session is active for this registration"),
    "{message}"
  );
  assert!(!message.contains("access-token"), "{message}");
}

#[tokio::test]
async fn other_failures_keep_the_protocol_classification() {
  for status in [401, 403, 500] {
    let err = create_against(status).await.expect_err("non-2xx must fail");
    assert!(
      matches!(err, RunnerError::Protocol(ref m) if m.contains(&status.to_string())),
      "status {status} must stay Protocol, got {err:?}"
    );
  }
}

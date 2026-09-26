//! Real outgoing HTTP requests from one build, without fake GitHub success replies.
//!
//! The loopback recorder closes each connection after receiving the request.
//! This proves the wire identity and error propagation, not queue eligibility.

#![cfg(test)]

use std::collections::HashMap;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::net::TcpListener;
use wire::net::{PollParams, acknowledge_message, create_session, poll_message};

struct CapturedRequest {
  target: String,
  body: Vec<u8>,
}

async fn capture(listener: &TcpListener) -> CapturedRequest {
  let (socket, _) = listener.accept().await.expect("accept real client request");
  let mut reader = BufReader::new(socket);
  let mut line = String::new();
  reader.read_line(&mut line).await.expect("request line");
  let target = line.split_whitespace().nth(1).expect("target").to_owned();
  let mut length = 0;
  loop {
    line.clear();
    assert!(reader.read_line(&mut line).await.expect("header") > 0);
    if line == "\r\n" {
      break;
    }
    if let Some((key, value)) = line.split_once(':')
      && key.eq_ignore_ascii_case("content-length")
    {
      length = value.trim().parse().expect("content length");
    }
  }
  let mut body = vec![0; length];
  reader.read_exact(&mut body).await.expect("request body");
  CapturedRequest { target, body }
}

fn query(request: &CapturedRequest) -> HashMap<String, String> {
  url::Url::parse(&format!("http://localhost{}", request.target))
    .expect("request URL")
    .query_pairs()
    .into_owned()
    .collect()
}

#[tokio::test]
async fn one_build_emits_one_compatibility_identity_on_all_broker_requests() {
  let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
  let base = format!("http://{}", listener.local_addr().expect("address"));
  let recorder = tokio::spawn(async move {
    let mut requests = Vec::new();
    for _ in 0..4 {
      requests.push(capture(&listener).await);
    }
    requests
  });
  let client = reqwest::Client::builder()
    .no_proxy()
    .http1_only()
    .timeout(Duration::from_secs(3))
    .build()
    .expect("real HTTP client");
  let session = protocol::build_session_request(1, "version-wire-probe");
  assert!(create_session(&client, &base, "", &session).await.is_err());
  for last_message_id in [0, 99] {
    let params = PollParams {
      client: &client,
      server_url_v2: &base,
      token: "",
      session_id: &session.session_id,
      os: shared::platform::runner_os(),
      architecture: shared::platform::runner_arch(),
      last_message_id,
    };
    assert!(poll_message(&params).await.is_err());
  }
  assert!(
    acknowledge_message(&client, &base, "", "version-probe")
      .await
      .is_err()
  );
  let requests = tokio::time::timeout(Duration::from_secs(5), recorder)
    .await
    .expect("bounded recorder")
    .expect("recorder task");
  let session_request = requests.first().expect("session request");
  assert_eq!(session_request.target, "/session");
  let body: serde_json::Value = serde_json::from_slice(&session_request.body).expect("JSON");
  // Independently pinned to actions/runner cab9d1c / src/runnerversion.
  assert_eq!(
    body
      .pointer("/agent/version")
      .and_then(serde_json::Value::as_str),
    Some("2.337.0")
  );
  for (request, cursor) in requests.iter().skip(1).take(2).zip(["0", "99"]) {
    assert!(request.target.starts_with("/message?"));
    let params = query(request);
    assert_eq!(
      params.get("runnerVersion").map(String::as_str),
      Some("2.337.0")
    );
    assert_eq!(
      params.get("disableUpdate").map(String::as_str),
      Some("true")
    );
    assert_eq!(
      params.get("lastMessageId").map(String::as_str),
      Some(cursor)
    );
  }
  let ack = requests.last().expect("acknowledge request");
  assert!(ack.target.starts_with("/acknowledge?"));
  assert_eq!(
    query(ack).get("runnerVersion").map(String::as_str),
    Some("2.337.0")
  );
  assert_eq!(
    query(ack).get("runnerRequestId").map(String::as_str),
    Some("version-probe")
  );
}

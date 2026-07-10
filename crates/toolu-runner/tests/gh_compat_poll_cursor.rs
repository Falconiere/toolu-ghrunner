//! S4 — lastMessageId redelivery cursor on the broker poll URL.
//!
//! Asserts the poll URL carries the `lastMessageId` query param so the
//! broker skips already-handled messages. The first poll sends `0`; once a
//! message id is known it is threaded onto the next poll.

use toolu_runner::net::{PollParams, build_poll_url};

fn params(last_message_id: i64) -> PollParams<'static> {
  PollParams {
    client: leaked_client(),
    server_url_v2: "https://broker.example.com/v2",
    token: "tok",
    session_id: "11111111-1111-1111-1111-111111111111",
    runner_version: "3.0.0",
    os: "linux",
    architecture: "x64",
    last_message_id,
  }
}

/// A leaked client so `PollParams<'static>` can hold the reference in a test
/// helper without a borrow-checker fight. The test never issues a request.
fn leaked_client() -> &'static reqwest::Client {
  Box::leak(Box::new(reqwest::Client::new()))
}

#[test]
fn first_poll_uses_zero_cursor() {
  let url = build_poll_url(&params(0));
  assert!(
    url.contains("lastMessageId=0"),
    "first poll must send lastMessageId=0, got: {url}"
  );
}

#[test]
fn subsequent_poll_threads_known_message_id() {
  let url = build_poll_url(&params(987));
  assert!(
    url.contains("lastMessageId=987"),
    "cursor must carry the prior message id, got: {url}"
  );
}

#[test]
fn poll_url_keeps_required_params() {
  let url = build_poll_url(&params(5));
  for expected in [
    "/message?sessionId=11111111-1111-1111-1111-111111111111",
    "status=Online",
    "runnerVersion=3.0.0",
    "os=linux",
    "architecture=x64",
    "lastMessageId=5",
    "disableUpdate=true",
  ] {
    assert!(url.contains(expected), "missing `{expected}` in: {url}");
  }
}

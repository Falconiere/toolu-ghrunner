//! Tests for `helpers`: `system_vss_access_token` lookup casing and
//! `cleanup_session` (real local listener, no mocks).

use super::*;

/// Build an `AgentJobRequestMessage` with a single endpoint carrying one
/// authorization parameter. Uses JSON as the construction surface so the
/// test only depends on the public wire shape, not on internal struct
/// fields.
fn job_msg_with_endpoint(name: &str, key: &str, value: &str) -> AgentJobRequestMessage {
  let json = format!(
    r#"{{
      "messageType": "JobRequest",
      "plan": {{ "planId": "p1" }},
      "jobId": "1",
      "jobDisplayName": "test",
      "jobName": "test",
      "resources": {{
        "endpoints": [{{
          "name": {name:?},
          "authorization": {{
            "scheme": "OAuth",
            "parameters": {{ {key:?}: {value:?} }}
          }}
        }}]
      }}
    }}"#,
  );
  serde_json::from_str(&json).expect("valid job message")
}

#[test]
fn lookup_finds_canonical_casing() {
  let msg = job_msg_with_endpoint("SystemVssConnection", "AccessToken", "tok-1");
  assert_eq!(system_vss_access_token(&msg).as_deref(), Some("tok-1"));
}

#[test]
fn lookup_finds_lowercase_name() {
  let msg = job_msg_with_endpoint("systemvssconnection", "AccessToken", "tok-2");
  assert_eq!(system_vss_access_token(&msg).as_deref(), Some("tok-2"));
}

#[test]
fn lookup_finds_uppercase_name() {
  let msg = job_msg_with_endpoint("SYSTEMVSSCONNECTION", "AccessToken", "tok-3");
  assert_eq!(system_vss_access_token(&msg).as_deref(), Some("tok-3"));
}

#[test]
fn lookup_finds_lowercase_key() {
  let msg = job_msg_with_endpoint("SystemVssConnection", "accesstoken", "tok-4");
  assert_eq!(system_vss_access_token(&msg).as_deref(), Some("tok-4"));
}

#[test]
fn lookup_returns_none_for_missing_endpoint() {
  let msg = job_msg_with_endpoint("SomeOtherEndpoint", "AccessToken", "tok-5");
  assert_eq!(system_vss_access_token(&msg), None);
}

#[test]
fn lookup_returns_none_for_missing_key() {
  let msg = job_msg_with_endpoint("SystemVssConnection", "DifferentKey", "tok-6");
  assert_eq!(system_vss_access_token(&msg), None);
}

// -- cleanup_session: real local listener, no mocks --------------------
//
// `cleanup_session` is `pub(super)` and `SessionCtx` is `pub(crate)`, so
// this cannot be an external `tests/`-crate integration test — it has to
// live in this sibling file (reached via `helpers.rs`'s `#[path]` include).

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Bind a real loopback listener, accept exactly one connection, read
/// until the request line is captured, reply with a raw `404`, and send
/// the request line back over the returned channel.
async fn spawn_404_listener() -> (SocketAddr, tokio::sync::oneshot::Receiver<String>) {
  let listener = TcpListener::bind("127.0.0.1:0")
    .await
    .expect("bind loopback listener");
  let addr = listener.local_addr().expect("read bound local addr");
  let (tx, rx) = tokio::sync::oneshot::channel();
  tokio::spawn(async move {
    let Ok((mut stream, _)) = listener.accept().await else {
      return;
    };
    let mut buf = [0_u8; 4096];
    let mut request = String::new();
    // Bounded so a malformed/bodyless request can't hang the listener
    // task — we only need the request line, which arrives in the first
    // read over loopback.
    let _ = tokio::time::timeout(Duration::from_secs(1), async {
      loop {
        match stream.read(&mut buf).await {
          Ok(0) | Err(_) => break,
          Ok(n) => {
            if let Some(chunk) = buf.get(..n) {
              request.push_str(&String::from_utf8_lossy(chunk));
            }
            if request.contains("\r\n") {
              break;
            }
          },
        }
      }
    })
    .await;
    let _ = stream
      .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
      .await;
    let _ = tx.send(request);
  });
  (addr, rx)
}

/// Build a `SessionCtx` pointed at a local test listener. `live_log` lets
/// each test install its own handle (or none) without duplicating the
/// remaining fields. `job_log_upload` stays `None` here: the
/// combined job-log upload's join is proven end-to-end (through a real
/// `poll_and_execute` + `cleanup_session`) in `tests/finalize_split.rs`.
fn test_ctx(broker_url: String, live_log: Option<tokio::task::JoinHandle<()>>) -> SessionCtx {
  let (tx, _rx) = mpsc::channel(1);
  SessionCtx {
    client: reqwest::Client::new(),
    token: "test-token".to_owned(),
    broker_url,
    session_id: "sess-1".to_owned(),
    config: shared::RunnerConfig::default(),
    masker: std::sync::Arc::new(std::sync::Mutex::new(shared::SecretMasker::new())),
    cancel: CancellationToken::new(),
    tx,
    encryption_key: None,
    use_fips_encryption: false,
    rsa_private_key_der: Vec::new(),
    live_log,
    job_log_upload: None,
    watchdog: crate::helpers::WatchdogConfig::default(),
  }
}

/// Minimal hand-rolled `tracing::Subscriber` that flags whether any
/// recorded field's Debug output contains `needle`. Avoids adding a
/// `tracing-subscriber` dev-dependency just to assert a WARN fired —
/// `tracing::Subscriber` is already part of this crate's normal `tracing`
/// dependency.
struct SubstringCapture {
  needle: &'static str,
  saw_needle: std::sync::Arc<AtomicBool>,
}

impl tracing::Subscriber for SubstringCapture {
  fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
    true
  }
  fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
    tracing::span::Id::from_u64(1)
  }
  fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
  fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
  fn event(&self, event: &tracing::Event<'_>) {
    struct Grep<'a> {
      needle: &'a str,
      saw_needle: &'a AtomicBool,
    }
    impl tracing::field::Visit for Grep<'_> {
      fn record_debug(&mut self, _field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if format!("{value:?}").contains(self.needle) {
          self.saw_needle.store(true, Ordering::SeqCst);
        }
      }
    }
    event.record(&mut Grep {
      needle: self.needle,
      saw_needle: &self.saw_needle,
    });
  }
  fn enter(&self, _span: &tracing::span::Id) {}
  fn exit(&self, _span: &tracing::span::Id) {}
}

/// AC-1: no sleep, and exactly one `DELETE` reaches the session URL —
/// the direct regression guard on the deleted 5s sleep.
#[tokio::test]
async fn cleanup_session_has_no_sleep_and_issues_exactly_one_delete() {
  let (addr, request_rx) = spawn_404_listener().await;
  let mut ctx = test_ctx(format!("http://{addr}"), None);

  let start = Instant::now();
  cleanup_session(&mut ctx).await;
  let elapsed = start.elapsed();

  assert!(
    elapsed < Duration::from_millis(500),
    "cleanup_session took {elapsed:?} — the old 5s sleep would fail this"
  );

  let request = request_rx
    .await
    .expect("listener should have received exactly one request");
  assert!(
    request.starts_with("DELETE"),
    "expected a DELETE request line, got: {request}"
  );
  assert!(
    request.contains("/session/sess-1"),
    "expected the session URL in the request line, got: {request}"
  );
}

/// AC-1b (prompt case): a live-log handle that completes quickly is
/// joined — not merely spawned-and-abandoned — before `cleanup_session`
/// returns.
#[tokio::test]
async fn cleanup_session_joins_a_prompt_live_log_handle_before_returning() {
  let (addr, request_rx) = spawn_404_listener().await;
  let completed = std::sync::Arc::new(AtomicBool::new(false));
  let completed_writer = std::sync::Arc::clone(&completed);
  let handle = tokio::spawn(async move {
    tokio::time::sleep(Duration::from_millis(20)).await;
    completed_writer.store(true, Ordering::SeqCst);
  });
  let mut ctx = test_ctx(format!("http://{addr}"), Some(handle));

  let start = Instant::now();
  cleanup_session(&mut ctx).await;
  let elapsed = start.elapsed();

  assert!(
    completed.load(Ordering::SeqCst),
    "cleanup_session must join the live-log handle before returning, not abandon it"
  );
  assert!(
    elapsed < Duration::from_millis(500),
    "joining a prompt handle should not add meaningful delay, took {elapsed:?}"
  );
  let request = request_rx
    .await
    .expect("listener should have received a request");
  assert!(
    request.starts_with("DELETE"),
    "expected a DELETE, got: {request}"
  );
}

/// AC-1b (stuck case): a live-log handle that never completes is
/// abandoned after the 2s timeout rather than blocking teardown forever,
/// and the WARN-and-continue path actually fires.
#[tokio::test]
async fn cleanup_session_abandons_a_stuck_live_log_handle_after_timeout() {
  let (addr, _request_rx) = spawn_404_listener().await;
  let saw_needle = std::sync::Arc::new(AtomicBool::new(false));
  // Current-thread test runtime: the spawned `stuck` task and the warn!
  // call inside `cleanup_session` both run on this same OS thread, so a
  // thread-local subscriber default observes both.
  let _guard = tracing::subscriber::set_default(SubstringCapture {
    needle: "live-log flush timed out",
    saw_needle: std::sync::Arc::clone(&saw_needle),
  });

  let stuck = tokio::spawn(async {
    tokio::time::sleep(Duration::from_secs(3600)).await;
  });
  let mut ctx = test_ctx(format!("http://{addr}"), Some(stuck));

  let start = Instant::now();
  cleanup_session(&mut ctx).await;
  let elapsed = start.elapsed();

  assert!(
    elapsed < Duration::from_secs(3),
    "a stuck live-log handle must be abandoned well under the old 5s sleep, took {elapsed:?}"
  );
  assert!(
    elapsed >= Duration::from_secs(2),
    "the 2s flush timeout should still have elapsed before giving up, took {elapsed:?}"
  );
  assert!(
    saw_needle.load(Ordering::SeqCst),
    "expected the flush-timed-out WARN to fire"
  );
}

//! Shared real-server test harness for `listener` integration tests: a
//! recording HTTP server (axum — real requests, arrival `Instant`s,
//! test-controlled held responses) and a minimal accept-side WebSocket
//! server standing in for GitHub's live-log feed. Real local listeners, no
//! mock framework: wiremock (used elsewhere in this crate for the GitHub
//! broker/Run-Service surface) only records request order, not arrival
//! time, and cannot hold a response open pending a test-controlled release.
//!
//! Declared as its own top-level `#[cfg(test)]` module — mirroring
//! `watchdog_retry.rs` / `watchdog_trip.rs` — so every other `tests/`
//! integration module can `use crate::support::{...}`. First consumer:
//! `startup_overlap.rs` (S7, AC-1); reused by the step-report-queue and
//! finalize-split integration tests that follow it (S9, S10) — see
//! docs/toolu/specs/2026-08-11-job-speed-improvements-design.md.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::Router;
use axum::extract::State;
use axum::http::{Method, StatusCode, Uri};
use axum::response::IntoResponse;
use futures_util::StreamExt;
use tokio::net::TcpListener;
use tokio::sync::Notify;

/// A one-shot release gate: every waiter blocks in [`Self::wait`] until
/// [`Self::release`] is called once, after which every current AND future
/// call to `wait` (including on a clone) returns immediately. Models "hold
/// this response until the test releases it" for exactly one release event,
/// shared cheaply (`Clone`) between the server task and the test.
#[derive(Clone, Default)]
pub(crate) struct Gate {
  notify: Arc<Notify>,
  released: Arc<AtomicBool>,
}

impl Gate {
  /// A fresh, unreleased gate.
  pub(crate) fn new() -> Self {
    Self::default()
  }

  /// Block until [`Self::release`] is called (on this gate or a clone of
  /// it), or return immediately if it already has been.
  pub(crate) async fn wait(&self) {
    // Register interest BEFORE checking the flag: the pre-registered
    // `Notified` future observes a `release()` that races in right after
    // the flag check, so this cannot miss a wakeup the way a bare
    // check-then-await would.
    let notified = self.notify.notified();
    if !self.released.load(Ordering::SeqCst) {
      notified.await;
    }
  }

  /// Release every current and future waiter on this gate. Idempotent.
  pub(crate) fn release(&self) {
    self.released.store(true, Ordering::SeqCst);
    self.notify.notify_waiters();
  }
}

/// One recorded HTTP request: arrival time (for ordering/overlap
/// assertions), method, path, and raw body — recorded BEFORE any
/// configured hold is applied, so `arrived_at` reflects when the request
/// reached the server, not when it was answered.
#[derive(Debug, Clone)]
pub(crate) struct RecordedRequest {
  /// The instant the request reached the server.
  pub(crate) arrived_at: Instant,
  /// The HTTP method.
  pub(crate) method: Method,
  /// The request path (no scheme/host/query).
  pub(crate) path: String,
  /// The raw request body.
  pub(crate) body: Vec<u8>,
}

/// A configured response for one `(method, path)` route on
/// [`RecordingServer`].
pub(crate) struct RouteResponse {
  pub(crate) status: StatusCode,
  pub(crate) body: Vec<u8>,
  /// Awaited before responding, when set — see [`Gate`].
  pub(crate) hold: Option<Gate>,
}

impl RouteResponse {
  /// A plain `200` with an empty JSON object body and no hold — the
  /// default shape for a Twirp RPC whose caller only checks the status.
  pub(crate) fn ok_empty() -> Self {
    Self {
      status: StatusCode::OK,
      body: b"{}".to_vec(),
      hold: None,
    }
  }

  /// [`Self::ok_empty`], held behind `gate` before responding.
  pub(crate) fn ok_empty_held(gate: Gate) -> Self {
    Self {
      hold: Some(gate),
      ..Self::ok_empty()
    }
  }

  /// A `200` with a JSON body, no hold.
  pub(crate) fn ok_json(body: &serde_json::Value) -> Self {
    Self {
      status: StatusCode::OK,
      body: body.to_string().into_bytes(),
      hold: None,
    }
  }
}

#[derive(Clone)]
struct RecorderState {
  requests: Arc<Mutex<Vec<RecordedRequest>>>,
  routes: Arc<HashMap<(Method, String), RouteResponse>>,
}

fn push_request(requests: &Mutex<Vec<RecordedRequest>>, entry: RecordedRequest) {
  match requests.lock() {
    Ok(mut g) => g.push(entry),
    Err(poisoned) => poisoned.into_inner().push(entry),
  }
}

async fn catch_all(
  State(state): State<RecorderState>,
  method: Method,
  uri: Uri,
  body: axum::body::Bytes,
) -> impl IntoResponse {
  let arrived_at = Instant::now();
  let path = uri.path().to_owned();
  push_request(
    &state.requests,
    RecordedRequest {
      arrived_at,
      method: method.clone(),
      path: path.clone(),
      body: body.to_vec(),
    },
  );

  let Some(route) = state.routes.get(&(method, path)) else {
    return (StatusCode::OK, b"{}".to_vec());
  };
  if let Some(gate) = &route.hold {
    gate.wait().await;
  }
  (route.status, route.body.clone())
}

/// A local recording HTTP server: every request (any method, any path) is
/// recorded with its arrival `Instant`, then answered per `routes` (exact
/// `(method, path)` match) or with a bare `200 {}` for anything
/// unconfigured.
pub(crate) struct RecordingServer {
  /// The bound loopback address.
  pub(crate) addr: SocketAddr,
  requests: Arc<Mutex<Vec<RecordedRequest>>>,
}

impl RecordingServer {
  /// Bind a fresh loopback port and start serving in the background the
  /// routes `build_routes` returns — called with this server's OWN
  /// `base_url()` already resolved, so a route (e.g. a signed-blob-URL
  /// response) can point back at this same server without a bind/rebind
  /// dance. The server task runs for the test process's remaining lifetime
  /// — matches the rest of the crate's local-listener test fixtures (e.g.
  /// `tests/watchdog_trip.rs`'s wiremock servers), none of which shut down
  /// explicitly either.
  pub(crate) async fn start(
    build_routes: impl FnOnce(&str) -> HashMap<(Method, String), RouteResponse>,
  ) -> Self {
    let listener = TcpListener::bind("127.0.0.1:0")
      .await
      .expect("bind loopback recording server");
    let addr = listener.local_addr().expect("read bound local addr");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let routes = build_routes(&format!("http://{addr}"));
    let state = RecorderState {
      requests: Arc::clone(&requests),
      routes: Arc::new(routes),
    };
    let app = Router::new().fallback(catch_all).with_state(state);
    tokio::spawn(async move {
      let _ = axum::serve(listener, app).await;
    });
    Self { addr, requests }
  }

  /// Base HTTP URL (`http://127.0.0.1:PORT`, no trailing slash).
  pub(crate) fn base_url(&self) -> String {
    format!("http://{}", self.addr)
  }

  /// Every request recorded so far, oldest first.
  pub(crate) fn requests(&self) -> Vec<RecordedRequest> {
    match self.requests.lock() {
      Ok(g) => g.clone(),
      Err(poisoned) => poisoned.into_inner().clone(),
    }
  }

  /// Recorded requests whose path exactly matches `path`, oldest first.
  pub(crate) fn requests_for(&self, path: &str) -> Vec<RecordedRequest> {
    self
      .requests()
      .into_iter()
      .filter(|r| r.path == path)
      .collect()
  }
}

fn set_instant(slot: &Mutex<Option<Instant>>, value: Instant) {
  match slot.lock() {
    Ok(mut g) => *g = Some(value),
    Err(poisoned) => *poisoned.into_inner() = Some(value),
  }
}

fn read_instant(slot: &Mutex<Option<Instant>>) -> Option<Instant> {
  match slot.lock() {
    Ok(g) => *g,
    Err(poisoned) => *poisoned.into_inner(),
  }
}

/// A minimal accept-side WebSocket server standing in for GitHub's
/// `FeedStreamUrl` live-log feed. Accepts exactly one connection, records
/// the TCP-accept `Instant` immediately (the earliest observable "the
/// client's WS upgrade attempt arrived" signal — before any bytes of the
/// HTTP Upgrade request are even read), then waits on the gate passed to
/// [`Self::start`] before completing the WS handshake — so a test can hold
/// the handshake open and prove some OTHER concurrent operation does not
/// wait on it.
pub(crate) struct WsServer {
  /// The bound loopback address.
  pub(crate) addr: SocketAddr,
  tcp_accepted_at: Arc<Mutex<Option<Instant>>>,
  handshake_completed_at: Arc<Mutex<Option<Instant>>>,
}

impl WsServer {
  /// Bind a fresh loopback port and accept one connection in the
  /// background, gated on `gate` before the WS handshake completes.
  pub(crate) async fn start(gate: Gate) -> Self {
    let listener = TcpListener::bind("127.0.0.1:0")
      .await
      .expect("bind loopback ws listener");
    let addr = listener.local_addr().expect("read bound local addr");
    let tcp_accepted_at = Arc::new(Mutex::new(None));
    let handshake_completed_at = Arc::new(Mutex::new(None));
    let tcp_accepted_writer = Arc::clone(&tcp_accepted_at);
    let handshake_writer = Arc::clone(&handshake_completed_at);
    tokio::spawn(async move {
      let Ok((stream, _)) = listener.accept().await else {
        return;
      };
      set_instant(&tcp_accepted_writer, Instant::now());
      gate.wait().await;
      let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
        return;
      };
      set_instant(&handshake_writer, Instant::now());
      // Drain (and discard) frames until the client closes — keeps the
      // connection alive for the rest of the job instead of an immediate
      // close the streamer would log as a failure.
      while ws.next().await.is_some() {}
    });
    Self {
      addr,
      tcp_accepted_at,
      handshake_completed_at,
    }
  }

  /// The plaintext `http://` form of this server's URL — pass this as the
  /// job message's `FeedStreamUrl`; production code
  /// (`AgentJobRequestMessage::feed_stream_url`) converts it to `ws://`.
  pub(crate) fn feed_stream_url_http(&self) -> String {
    format!("http://{}/livelog", self.addr)
  }

  /// The instant the TCP connection was accepted, if it has been yet.
  pub(crate) fn tcp_accepted_at(&self) -> Option<Instant> {
    read_instant(&self.tcp_accepted_at)
  }

  /// The instant the WS handshake completed (post-gate), if it has yet.
  pub(crate) fn handshake_completed_at(&self) -> Option<Instant> {
    read_instant(&self.handshake_completed_at)
  }
}

/// Poll `pred` on every Tokio yield point until it returns `true`. Callers
/// wrap this in `tokio::time::timeout` — an unbounded `wait_until` would
/// hang the test suite on a real regression instead of failing it.
pub(crate) async fn wait_until(mut pred: impl FnMut() -> bool) {
  while !pred() {
    tokio::task::yield_now().await;
  }
}

//! Issue #77: the broker's `ForceTokenRefresh` re-exchange itself, not just
//! its classification (`tests/broker_control.rs` owns that).
//!
//! Every case drives production `handle_idle_outcome` — the idle loop's
//! publication seam — against a real loopback OAuth endpoint, with a real
//! PS256 assertion signed by the committed PKCS#1 test key. Nothing is
//! mocked: the peer answers HTTP, `wire::net::exchange_token` parses it, and
//! the following broker poll shows which bearer the runner now presents.
//! The mid-job seam (`job_lifecycle::refresh_watcher_token`, which also
//! feeds the renewal watcher) shares `refresh_access_token` but is not
//! driven here.
//!
//! The clock stays real: a paused clock auto-advances whenever the runtime
//! waits on a socket, which trips the very timeouts under test. The two
//! bounds (`broker_refresh.rs`: 10s per request, 30s of retry sleep) are
//! therefore measured at the peer: the 10s request deadline shows up as the
//! gap between two arrivals, since the retry can only reach the peer once
//! the first request was abandoned. The 30s retry-sleep budget is the one
//! bound waited out for real, because exhausting it is the only way to see
//! the terminal failure it produces. Per the issue, no strict total duration
//! is asserted — only the floor the budget guarantees.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::StatusCode;
use axum::response::IntoResponse;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::idle::{IdleDecision, handle_idle_outcome};
use super::{POLL_BACKOFF_START, poll_once};
use crate::broker_message::PollOutcome;
use crate::broker_refresh::REFRESH_RETRY_BUDGET;
use crate::handler::{RefreshAuth, SessionCtx};
use shared::{ListenerEvent, RunnerConfig, RunnerError, SecretMasker};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// A real 2048-bit PKCS#1 key. It authenticates nothing — the loopback peer
/// never verifies the assertion — but it has to be a signable key so
/// `protocol::build_jwt` runs its production path.
const TEST_KEY_PEM: &str = include_str!("../../../protocol/tests/fixtures/app_jwt_test_key.pem");
/// The bearer the session holds before any refresh.
const OLD_TOKEN: &str = "broker-token-before-refresh";
/// The bearer the OAuth peer hands back.
const NEW_TOKEN: &str = "broker-token-after-refresh";
/// The broker message id the control envelope carries.
const MESSAGE_ID: i64 = 99;

/// One request the peer answered.
#[derive(Clone, Debug)]
struct Recorded {
  path: String,
  /// When the request reached the peer, for measuring the caller's own
  /// per-request deadline from the outside.
  arrived_at: std::time::Instant,
  authorization: Option<String>,
  body: String,
}

/// A loopback stand-in for the JIT OAuth token endpoint and the broker.
///
/// Token-endpoint replies are scripted: each request consumes the next one
/// and the last one repeats, which is how a transient failure followed by a
/// success is expressed. `stall` makes the endpoint accept a request and
/// never answer it.
struct Peer {
  base: String,
  seen: Arc<Mutex<Vec<Recorded>>>,
}

impl Peer {
  /// Start a peer whose token endpoint answers `replies` in order.
  async fn start(replies: Vec<(StatusCode, String)>, stall: bool) -> TestResult<Self> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let seen: Arc<Mutex<Vec<Recorded>>> = Arc::new(Mutex::new(Vec::new()));
    let queue = Arc::new(Mutex::new(VecDeque::from(replies)));
    let token_seen = Arc::clone(&seen);
    let token = move |headers: axum::http::HeaderMap, body: String| {
      let queue = Arc::clone(&queue);
      let seen = Arc::clone(&token_seen);
      async move {
        record(&seen, "/oauth2/token", &headers, body);
        if stall {
          // Never answers, so the caller's own 10 s request deadline is
          // what ends the attempt.
          std::future::pending::<()>().await;
        }
        let mut queue = queue
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner);
        let reply = if queue.len() > 1 {
          queue.pop_front()
        } else {
          queue.front().cloned()
        };
        let (status, body) = reply.unwrap_or((StatusCode::NOT_FOUND, String::new()));
        (status, body).into_response()
      }
    };
    let message_seen = Arc::clone(&seen);
    let message = move |headers: axum::http::HeaderMap| {
      let seen = Arc::clone(&message_seen);
      async move {
        record(&seen, "/message", &headers, String::new());
        StatusCode::ACCEPTED.into_response()
      }
    };
    let app = axum::Router::new()
      .route("/oauth2/token", axum::routing::post(token))
      .route("/message", axum::routing::get(message));
    tokio::spawn(async move {
      let _ = axum::serve(listener, app).await;
    });
    Ok(Self { base, seen })
  }

  /// The recorded requests for one path, in arrival order.
  fn seen(&self, path: &str) -> Vec<Recorded> {
    self
      .seen
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .iter()
      .filter(|request| request.path == path)
      .cloned()
      .collect()
  }

  /// Arrival instants for `path`, oldest first.
  fn arrivals(&self, path: &str) -> Vec<std::time::Instant> {
    self
      .seen(path)
      .iter()
      .map(|request| request.arrived_at)
      .collect()
  }
}

fn record(seen: &Mutex<Vec<Recorded>>, path: &str, headers: &axum::http::HeaderMap, body: String) {
  seen
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .push(Recorded {
      path: path.to_owned(),
      arrived_at: std::time::Instant::now(),
      authorization: headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned),
      body,
    });
}

/// The PKCS#1 DER inside the armored test key.
fn key_der() -> TestResult<Vec<u8>> {
  let body: String = TEST_KEY_PEM
    .lines()
    .filter(|line| !line.starts_with("-----"))
    .collect();
  Ok(base64::Engine::decode(
    &base64::engine::general_purpose::STANDARD,
    body,
  )?)
}

/// A session whose broker and OAuth endpoints both live on `peer`.
fn session(peer: &Peer) -> TestResult<SessionCtx> {
  let (tx, mut rx) = mpsc::channel::<ListenerEvent>(16);
  tokio::spawn(async move { while rx.recv().await.is_some() {} });
  Ok(SessionCtx {
    runner_name: None,
    client: reqwest::Client::new(),
    token: OLD_TOKEN.to_owned(),
    broker_url: peer.base.clone(),
    session_id: "session-77".to_owned(),
    config: RunnerConfig::default(),
    masker: Arc::new(Mutex::new(SecretMasker::new())),
    cancel: CancellationToken::new(),
    tx,
    encryption_key: None,
    use_fips_encryption: false,
    rsa_private_key_der: key_der()?,
    refresh_auth: Some(RefreshAuth {
      client_id: "jit-client-77".to_owned(),
      authorization_url: format!("{}/oauth2/token", peer.base),
    }),
    live_log: None,
    job_log_upload: None,
    watchdog: crate::helpers::WatchdogConfig::default(),
    job_acquired: false,
  })
}

/// A well-formed OAuth success body carrying `token`.
fn granted(token: &str) -> String {
  serde_json::json!({
    "access_token": token,
    "expires_in": 600,
    "token_type": "Bearer",
  })
  .to_string()
}

/// Drive the production refresh route for one `ForceTokenRefresh`.
async fn refresh(ctx: &mut SessionCtx) -> Result<IdleDecision, RunnerError> {
  let mut backoff = POLL_BACKOFF_START;
  handle_idle_outcome(
    ctx,
    PollOutcome::RefreshToken {
      message_id: MESSAGE_ID,
    },
    &mut backoff,
  )
  .await
}

/// #77 AC-2: a successful re-exchange publishes the new token, and the very
/// next broker poll presents it instead of the spent one.
#[tokio::test]
async fn forced_refresh_makes_the_next_broker_poll_use_the_new_token() -> TestResult {
  let peer = Peer::start(vec![(StatusCode::OK, granted(NEW_TOKEN))], false).await?;
  let mut ctx = session(&peer)?;

  assert!(matches!(refresh(&mut ctx).await?, IdleDecision::Continue));
  assert_eq!(
    ctx.token, NEW_TOKEN,
    "the refreshed token was not published"
  );

  let exchanges = peer.seen("/oauth2/token");
  let [exchange] = exchanges.as_slice() else {
    return Err(format!("expected one token exchange, saw {exchanges:?}").into());
  };
  assert!(
    exchange.body.contains("grant_type=client_credentials"),
    "{} is not a client-credentials exchange",
    exchange.body
  );
  // The assertion really was signed: three base64url JWS segments.
  let assertion = exchange
    .body
    .split('&')
    .find_map(|pair| pair.strip_prefix("client_assertion="))
    .ok_or("the exchange sent no assertion")?;
  assert_eq!(assertion.split('.').count(), 3, "{assertion} is not a JWS");

  let outcome = poll_once(&ctx, &ctx.token, 0).await;
  assert!(
    matches!(outcome, PollOutcome::NoWork),
    "the broker poll after a refresh failed"
  );
  let polls = peer.seen("/message");
  let [poll] = polls.as_slice() else {
    return Err(format!("expected one broker poll, saw {polls:?}").into());
  };
  assert_eq!(
    poll.authorization.as_deref(),
    Some(format!("Bearer {NEW_TOKEN}").as_str()),
    "the broker poll still presents the spent token"
  );
  Ok(())
}

/// #77 77-S3: a transient 500 is retried, and the success behind it
/// publishes the new token after exactly two exchanges.
#[tokio::test]
async fn a_transient_failure_is_retried_and_the_success_publishes() -> TestResult {
  let peer = Peer::start(
    vec![
      (
        StatusCode::INTERNAL_SERVER_ERROR,
        "{\"error\":\"server_error\"}".to_owned(),
      ),
      (StatusCode::OK, granted(NEW_TOKEN)),
    ],
    false,
  )
  .await?;
  let mut ctx = session(&peer)?;
  assert!(matches!(refresh(&mut ctx).await?, IdleDecision::Continue));
  assert_eq!(ctx.token, NEW_TOKEN);
  assert_eq!(
    peer.seen("/oauth2/token").len(),
    2,
    "the transient failure was not retried exactly once"
  );
  Ok(())
}

/// #77 77-S3: a peer that only ever fails is retried with backoff until the
/// 30s retry-sleep budget is spent, and then gives up as a `Network` failure
/// with the spent bearer still in place — it neither abandons the refresh on
/// the first failure nor hammers the endpoint.
///
/// `retry_transient` returns a failure only once `elapsed >= budget`, so the
/// measured duration is the exhaustion itself; no strict total is asserted
/// (per #235), only the floor the budget guarantees. The doubling backoff
/// (1s, 2s, 4s, …, each jittered into `[b/2, b)`) is what keeps the attempts
/// to a handful across those 30s.
///
/// This costs the suite about 30s of real time. `REFRESH_RETRY_BUDGET` is a
/// production constant, and shortening it for the test would leave the bound
/// untested — which is what the earlier five-second window did.
#[tokio::test]
async fn persistent_transient_failures_exhaust_the_budget_without_publishing() -> TestResult {
  let peer = Peer::start(
    vec![(
      StatusCode::SERVICE_UNAVAILABLE,
      "{\"error\":\"unavailable\"}".to_owned(),
    )],
    false,
  )
  .await?;
  let mut ctx = session(&peer)?;
  let started = std::time::Instant::now();
  // Generous guard: the test fails rather than hangs if the budget stops
  // bounding the loop. Exhaustion is expected to end the call well inside it.
  let finished = tokio::time::timeout(Duration::from_secs(90), refresh(&mut ctx)).await?;
  let elapsed = started.elapsed();

  let Err(error) = finished else {
    return Err("a peer that only ever fails must not produce a decision".into());
  };
  assert!(
    matches!(error, RunnerError::Network(_)),
    "exhaustion must stay a Network failure, got {error}"
  );
  assert_eq!(ctx.token, OLD_TOKEN, "a failed refresh published a token");
  assert!(
    elapsed >= REFRESH_RETRY_BUDGET,
    "{elapsed:?} is short of the {REFRESH_RETRY_BUDGET:?} retry-sleep budget, so it was never exhausted"
  );
  let attempts = peer.seen("/oauth2/token").len();
  assert!(
    // Six or seven by construction: the five sleeps before the budget runs
    // out sum to 15.5s..31s, and the sixth is clamped to what remains. Ten
    // would mean the backoff stopped doubling.
    (5..=8).contains(&attempts),
    "{attempts} attempts across the budget is not a doubling backoff"
  );
  Ok(())
}

/// #77 77-S3: a peer that accepts the request and never answers is abandoned
/// at the per-request deadline instead of hanging the idle loop, and
/// publishes nothing. A second attempt inside the window is the observable:
/// it can only exist because the first request was given up on.
#[tokio::test]
async fn a_stalled_token_endpoint_is_abandoned_at_its_deadline() -> TestResult {
  let peer = Peer::start(vec![(StatusCode::OK, granted(NEW_TOKEN))], true).await?;
  let mut ctx = session(&peer)?;
  let started = std::time::Instant::now();
  // The stall never ends, so this guard is expected to fire: it is here to
  // keep the test bounded, and the evidence is what the peer saw meanwhile.
  let _ = tokio::time::timeout(Duration::from_secs(13), refresh(&mut ctx)).await;
  // The retry only reaches the peer once the first request is abandoned, so
  // the gap between the two arrivals is the 10s request deadline plus the
  // first retry sleep — `jittered_backoff(1s)`, which is 0.5s to 1.0s. That
  // puts the gap in 10.5s..11.0s; the window below adds scheduling slack but
  // still excludes a deadline of 9s or less. Measured at the peer, so it is
  // independent of this test's own 13s guard.
  let arrivals = peer.arrivals("/oauth2/token");
  let [first, second] = arrivals.as_slice() else {
    return Err(
      format!(
        "expected exactly two attempts, saw {}",
        peer.seen("/oauth2/token").len()
      )
      .into(),
    );
  };
  let deadline = second.duration_since(*first);
  assert!(
    (Duration::from_millis(10_400)..Duration::from_millis(12_000)).contains(&deadline),
    "the stalled request was abandoned after {deadline:?} (deadline + first \
     retry sleep), which is not the 10s deadline plus a 0.5s-1.0s sleep"
  );
  assert!(started.elapsed() >= Duration::from_secs(10));
  assert_eq!(
    ctx.token, OLD_TOKEN,
    "a timed-out refresh published a token"
  );
  Ok(())
}

/// #77 77-S3: a rejected credential is fatal and never retried, and an empty
/// grant is refused before publication. Neither error echoes a token.
#[tokio::test]
async fn rejection_and_an_empty_grant_never_publish_or_leak() -> TestResult {
  for (status, body, rejected) in [
    (StatusCode::UNAUTHORIZED, granted(NEW_TOKEN), true),
    (StatusCode::OK, granted(""), false),
  ] {
    let peer = Peer::start(vec![(status, body)], false).await?;
    let mut ctx = session(&peer)?;
    let Err(error) = refresh(&mut ctx).await else {
      return Err(format!("{status} was accepted as a refresh").into());
    };
    if rejected {
      assert!(
        matches!(error, RunnerError::Auth(_)),
        "a rejected credential must be fatal"
      );
      assert_eq!(
        peer.seen("/oauth2/token").len(),
        1,
        "a rejected credential must not be retried"
      );
    } else {
      assert!(
        matches!(error, RunnerError::Protocol(_)),
        "an empty grant must be a protocol failure"
      );
    }
    assert_eq!(ctx.token, OLD_TOKEN, "{status} published a token");
    let rendered = error.to_string();
    assert!(!rendered.contains(NEW_TOKEN), "{rendered}");
    assert!(!rendered.contains(OLD_TOKEN), "{rendered}");
  }
  Ok(())
}

/// #77 77-S3: cancelling mid-exchange ends the idle loop without publishing
/// and without waiting out the request timeout.
#[tokio::test]
async fn cancellation_mid_exchange_exits_without_publishing() -> TestResult {
  let peer = Peer::start(vec![(StatusCode::OK, granted(NEW_TOKEN))], true).await?;
  let mut ctx = session(&peer)?;
  let cancel = ctx.cancel.clone();
  let arrived = Arc::clone(&peer.seen);
  // Cancel only once the exchange is demonstrably in flight. A fixed sleep
  // would also pass if signing were slow enough that nothing had been sent,
  // which is the one thing "mid-exchange" has to rule out.
  tokio::spawn(async move {
    while arrived
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .is_empty()
    {
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
    cancel.cancel();
  });
  // Bounded well under the 10 s request deadline: cancellation must end
  // the exchange, not the timeout.
  let decision = tokio::time::timeout(Duration::from_secs(5), refresh(&mut ctx)).await??;
  assert!(
    matches!(decision, IdleDecision::Exit),
    "a cancelled refresh must end the idle loop"
  );
  assert_eq!(
    ctx.token, OLD_TOKEN,
    "a cancelled refresh published a token"
  );
  Ok(())
}

/// #77 77-S3 ("secrets not logged"): the two lines this path writes — the
/// retry WARN in `retry::retry_transient` and the publication INFO in
/// `idle::refresh_idle_token` — carry neither the spent nor the granted
/// token.
///
/// The capture is a plain `fmt` subscriber, so these are the raw lines as
/// production formats them, before the `SecretMasker` redactor a real run
/// installs would see them. The assertion is therefore the stronger one:
/// the refresh path never formats a token at all, rather than relying on
/// masking to hide one it did.
///
/// Runs in a child process. Only `set_default` is usable while tests share a
/// process, and it is thread-local, while `tracing`'s callsite interest is
/// global — a concurrent test without a subscriber can leave the publication
/// callsite disabled for this thread too. Isolating the one capture keeps
/// `cargo test -p listener --lib broker_control` working without serializing
/// the suite.
#[test]
fn refresh_log_lines_carry_neither_the_old_nor_the_new_token() -> TestResult {
  let output = std::process::Command::new(std::env::current_exe()?)
    .args(["--exact", CAPTURE_CHILD, "--nocapture"])
    .env(CAPTURE_PROBE, "1")
    .output()?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(
    output.status.success(),
    "capture child failed: {stdout}{}",
    String::from_utf8_lossy(&output.stderr)
  );
  // A filter that matches nothing still exits 0, so the child says so itself.
  assert!(
    stdout.contains(CAPTURE_RAN),
    "the child never ran the capture: {stdout}"
  );
  Ok(())
}

/// Env marker that tells the child process it is the capture run.
const CAPTURE_PROBE: &str = "TOOLU_TEST_REFRESH_LOG_CAPTURE";
/// Module-qualified name of the child test, as `--exact` expects it.
const CAPTURE_CHILD: &str =
  "job_lifecycle::broker_control_refresh::refresh_log_capture_child_probe";
/// Printed by the child once the assertions have run.
const CAPTURE_RAN: &str = "refresh log capture ran";

/// The body of [`refresh_log_lines_carry_neither_the_old_nor_the_new_token`],
/// run alone in a child process. Without the marker it is a no-op, so the
/// ordinary suite never pays for it twice.
#[tokio::test]
async fn refresh_log_capture_child_probe() -> TestResult {
  if std::env::var_os(CAPTURE_PROBE).is_none() {
    return Ok(());
  }
  let log = tempfile::NamedTempFile::new()?;
  let subscriber = tracing_subscriber::fmt()
    .with_ansi(false)
    .without_time()
    // Every level, so the capture never depends on what the default filter
    // would have admitted at these callsites.
    .with_max_level(tracing::Level::TRACE)
    .with_writer(Arc::new(log.reopen()?))
    .finish();
  let peer = Peer::start(
    vec![
      (StatusCode::SERVICE_UNAVAILABLE, String::new()),
      (StatusCode::OK, granted(NEW_TOKEN)),
    ],
    false,
  )
  .await?;
  let mut ctx = session(&peer)?;
  {
    // Thread-local default: a current-thread test runtime polls the refresh
    // on this very thread, so both production log calls land in the file.
    let _guard = tracing::subscriber::set_default(subscriber);
    assert!(matches!(refresh(&mut ctx).await?, IdleDecision::Continue));
  }
  assert_eq!(
    ctx.token, NEW_TOKEN,
    "the refreshed token was not published"
  );

  let written = std::fs::read_to_string(log.path())?;
  assert!(
    written.contains("retrying transient reporting failure"),
    "the retry WARN never reached the capture, so there is nothing to scan: {written:?}"
  );
  assert!(
    written.contains("broker token refreshed"),
    "the publication INFO never reached the capture: {written:?}"
  );
  assert!(
    !written.contains(NEW_TOKEN),
    "a log line carried the granted token: {written}"
  );
  assert!(
    !written.contains(OLD_TOKEN),
    "a log line carried the spent token: {written}"
  );
  println!("{CAPTURE_RAN}");
  Ok(())
}

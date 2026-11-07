//! Issue #77: the broker's `ForceTokenRefresh` re-exchange itself, not just
//! its classification (`tests/broker_control.rs` owns that).
//!
//! Every case drives production `handle_idle_outcome` — the only code that
//! publishes a refreshed token — against a real loopback OAuth endpoint, with
//! a real PS256 assertion signed by the committed PKCS#1 test key. Nothing is
//! mocked: the peer answers HTTP, `wire::net::exchange_token` parses it, and
//! the following broker poll shows which bearer the runner now presents.
//!
//! The clock stays real: a paused clock auto-advances whenever the runtime
//! waits on a socket, which trips the very timeouts under test. The two
//! bounds (`broker_refresh.rs`: 10s per request, 30s of retry sleep) are
//! therefore observed through what the peer sees inside a short window — a
//! second attempt can only exist if the first was abandoned at its deadline
//! — rather than by waiting a bound out. Per the issue, no strict total
//! duration is asserted; `tests/watchdog_retry.rs` owns full budget
//! exhaustion for `retry_transient` itself.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::StatusCode;
use axum::response::IntoResponse;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::idle::{IdleDecision, handle_idle_outcome};
use super::{POLL_BACKOFF_START, poll_once};
use crate::broker_message::PollOutcome;
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
    let stalled = Arc::new(AtomicBool::new(stall));
    let token_seen = Arc::clone(&seen);
    let token = move |headers: axum::http::HeaderMap, body: String| {
      let queue = Arc::clone(&queue);
      let seen = Arc::clone(&token_seen);
      let stalled = Arc::clone(&stalled);
      async move {
        record(&seen, "/oauth2/token", &headers, body);
        if stalled.load(Ordering::SeqCst) {
          // Never answers. Under a paused clock the request timeout fires
          // the moment the runtime has nothing else to do.
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
}

fn record(seen: &Mutex<Vec<Recorded>>, path: &str, headers: &axum::http::HeaderMap, body: String) {
  seen
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .push(Recorded {
      path: path.to_owned(),
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

/// #77 77-S3: a transient 500 is retried, and the success behind it publishes
/// the new token exactly once.
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

/// #77 77-S3: a peer that only ever fails keeps retrying inside the budget
/// and publishes nothing — it neither gives up on the first failure nor
/// hammers the endpoint.
#[tokio::test]
async fn persistent_transient_failures_retry_inside_the_budget_without_publishing() -> TestResult {
  let peer = Peer::start(
    vec![(
      StatusCode::SERVICE_UNAVAILABLE,
      "{\"error\":\"unavailable\"}".to_owned(),
    )],
    false,
  )
  .await?;
  let mut ctx = session(&peer)?;
  // Observe a slice of the 30s budget rather than waiting it out.
  let outcome = tokio::time::timeout(Duration::from_secs(5), refresh(&mut ctx)).await;
  if let Ok(finished) = outcome {
    assert!(
      matches!(finished, Err(RunnerError::Network(_))),
      "exhaustion must stay a Network failure"
    );
  }
  assert_eq!(ctx.token, OLD_TOKEN, "a failed refresh published a token");
  let attempts = peer.seen("/oauth2/token").len();
  assert!(
    (2..=5).contains(&attempts),
    "{attempts} attempts in 5s is not a backed-off retry of a 30s budget"
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
  let outcome = tokio::time::timeout(Duration::from_secs(13), refresh(&mut ctx)).await;
  if let Ok(finished) = outcome {
    assert!(
      matches!(finished, Err(RunnerError::Network(_))),
      "a stalled peer must surface as a bounded Network failure"
    );
  }
  let attempts = peer.seen("/oauth2/token").len();
  assert_eq!(
    attempts, 2,
    "the first stalled request was not abandoned at its 10s deadline"
  );
  assert!(
    started.elapsed() >= Duration::from_secs(10),
    "the deadline fired early"
  );
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
  tokio::spawn(async move {
    tokio::time::sleep(Duration::from_millis(50)).await;
    cancel.cancel();
  });
  let decision = refresh(&mut ctx).await?;
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

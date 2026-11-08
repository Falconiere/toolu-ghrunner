//! #251 D2: the broker sees `status=Busy` while a job runs and
//! `status=Online` while idle, as upstream `BrokerMessageListener` reports.
//!
//! Live evidence (run 42389172867): toolu's mid-job poll sent `Online` and the
//! broker answered HTTP 404 on every one, so the `JobCancellation` watcher was
//! blind for the whole job. Both production loops are driven here against a
//! real loopback broker that records each poll's query string.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::{StatusCode, Uri};
use axum::response::IntoResponse;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use super::idle::poll_until_job;
use super::watch_for_gh_cancel;
use crate::handler::SessionCtx;
use shared::{ListenerEvent, RunnerConfig, SecretMasker};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Start a broker that answers every poll `202 Accepted` and records its query.
async fn broker() -> TestResult<(String, Arc<Mutex<Vec<String>>>)> {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
  let base = format!("http://{}", listener.local_addr()?);
  let seen = Arc::new(Mutex::new(Vec::new()));
  let recorded = Arc::clone(&seen);
  let message = move |uri: Uri| {
    let recorded = Arc::clone(&recorded);
    async move {
      recorded
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(uri.query().unwrap_or_default().to_owned());
      StatusCode::ACCEPTED.into_response()
    }
  };
  let app = axum::Router::new().route("/message", axum::routing::get(message));
  tokio::spawn(async move {
    let _ = axum::serve(listener, app).await;
  });
  Ok((base, seen))
}

fn session(base: &str) -> SessionCtx {
  let (tx, mut rx) = mpsc::channel::<ListenerEvent>(16);
  tokio::spawn(async move { while rx.recv().await.is_some() {} });
  SessionCtx {
    runner_name: None,
    client: reqwest::Client::new(),
    token: "broker-token".to_owned(),
    broker_url: base.to_owned(),
    session_id: "session-251".to_owned(),
    config: RunnerConfig::default(),
    masker: Arc::new(Mutex::new(SecretMasker::new())),
    cancel: CancellationToken::new(),
    tx,
    encryption_key: None,
    use_fips_encryption: false,
    rsa_private_key_der: Vec::new(),
    refresh_auth: None,
    live_log: None,
    job_log_upload: None,
    watchdog: crate::helpers::WatchdogConfig::default(),
    job_acquired: false,
  }
}

/// Wait until the broker has seen at least `count` polls, at most 10 s.
async fn polls(seen: &Mutex<Vec<String>>, count: usize) -> TestResult<Vec<String>> {
  for _ in 0..200 {
    let current = seen
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone();
    if current.len() >= count {
      return Ok(current);
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
  }
  Err(format!("broker saw fewer than {count} polls").into())
}

#[tokio::test]
async fn the_mid_job_watcher_polls_as_busy() -> TestResult {
  let (base, seen) = broker().await?;
  let ctx = session(&base);
  let job_cancel = ctx.cancel.child_token();
  let (token_tx, _token_rx) = watch::channel(None);
  let watcher = tokio::time::timeout(
    Duration::from_secs(10),
    watch_for_gh_cancel(&ctx, &job_cancel, &token_tx),
  );
  let observed = tokio::select! {
    _ = watcher => return Err("the watcher stopped polling".into()),
    observed = polls(&seen, 2) => observed?,
  };
  for query in &observed {
    assert!(
      query.contains("status=Busy&"),
      "mid-job poll was not Busy: {query}"
    );
  }
  Ok(())
}

#[tokio::test]
async fn the_idle_loop_polls_as_online() -> TestResult {
  let (base, seen) = broker().await?;
  let mut ctx = session(&base);
  let drain = CancellationToken::new();
  let idle = tokio::time::timeout(Duration::from_secs(10), poll_until_job(&mut ctx, &drain));
  let observed = tokio::select! {
    _ = idle => return Err("the idle loop stopped polling".into()),
    observed = polls(&seen, 2) => observed?,
  };
  for query in &observed {
    assert!(
      query.contains("status=Online&"),
      "idle poll was not Online: {query}"
    );
  }
  Ok(())
}

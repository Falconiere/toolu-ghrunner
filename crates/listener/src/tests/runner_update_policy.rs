//! Capture-shaped refresh variants through the actual idle and busy handlers.
//!
//! The committed broker job envelope supplies wire shape; replacing its type/body
//! is an explicit replay variation, not a captured GitHub update instruction.

use std::sync::{Arc, Mutex};
use tracing::instrument::WithSubscriber;

use super::*;
use crate::broker_message::classify_received_message;
use crate::helpers::WatchdogConfig;
use shared::{RunnerConfig, SecretMasker};

fn context() -> SessionCtx {
  let (tx, _) = tokio::sync::mpsc::channel(8);
  SessionCtx {
    client: reqwest::Client::new(),
    token: String::new(),
    broker_url: String::new(),
    session_id: String::new(),
    config: RunnerConfig::default(),
    masker: Arc::new(Mutex::new(SecretMasker::new())),
    cancel: CancellationToken::new(),
    tx,
    encryption_key: Some(protocol::EncryptionKey {
      encrypted: true,
      value: String::new(),
    }),
    use_fips_encryption: false,
    rsa_private_key_der: Vec::new(),
    refresh_auth: None,
    live_log: None,
    job_log_upload: None,
    watchdog: WatchdogConfig::default(),
  }
}

fn envelope(kind: &str, body: &str) -> BrokerMessage {
  let mut message: BrokerMessage = serde_json::from_str(include_str!(
    "../../../toolu-runner/tests/fixtures/broker_message_job_request.json"
  ))
  .expect("committed envelope");
  message.message_type = serde_json::from_value(serde_json::json!(kind)).expect("message type");
  message.body = body.to_owned();
  message
}

#[tokio::test]
async fn refresh_warns_and_continues_in_idle_and_busy_handlers_without_reading_body() {
  let log = tempfile::NamedTempFile::new().expect("log file");
  let subscriber = tracing_subscriber::fmt()
    .with_ansi(false)
    .without_time()
    .with_writer(Arc::new(log.reopen().expect("log writer")))
    .finish();
  async {
    for kind in ["RunnerRefresh", "AgentRefresh"] {
      for body in ["", "opaque-update-body-do-not-log"] {
        let mut ctx = context();
        let job_cancel = ctx.cancel.child_token();
        let mut backoff = POLL_BACKOFF_MAX;
        let outcome = classify_received_message(&ctx, envelope(kind, body));
        let cursor = outcome.message_id().expect("refresh ID");
        assert!(!is_redelivery(cursor - 1, &outcome));
        assert!(is_redelivery(cursor, &outcome));
        assert!(is_redelivery(cursor + 1, &outcome));
        assert!(matches!(
          handle_idle_outcome(&mut ctx, outcome, &mut backoff)
            .await
            .expect("idle handler"),
          IdleDecision::Continue
        ));
        assert_eq!(backoff, POLL_BACKOFF_START);
        let (token_tx, token_rx) = watch::channel(None);
        let mut token = String::new();
        let outcome = classify_received_message(&ctx, envelope(kind, body));
        assert_eq!(
          watch_step(
            &ctx,
            &job_cancel,
            &mut token,
            &token_tx,
            outcome,
            POLL_BACKOFF_MAX
          )
          .await,
          Some(POLL_BACKOFF_START)
        );
        assert!(!ctx.cancel.is_cancelled());
        assert!(!job_cancel.is_cancelled());
        assert!(token_rx.borrow().is_none());
        // Refresh needs no valid encrypted body; subsequent plaintext job still routes.
        ctx.encryption_key = None;
        let mut job: BrokerMessage = serde_json::from_str(include_str!(
          "../../../toolu-runner/tests/fixtures/broker_message_job_request.json"
        ))
        .expect("committed job");
        job.message_id = cursor + 1;
        let next = classify_received_message(&ctx, job);
        assert!(!is_redelivery(cursor, &next));
        assert!(matches!(
          handle_idle_outcome(&mut ctx, next, &mut backoff)
            .await
            .expect("next job"),
          IdleDecision::Job(_)
        ));
      }
    }
  }
  .with_subscriber(subscriber)
  .await;
  let text = std::fs::read_to_string(log.path()).expect("log output");
  assert_eq!(
    text
      .lines()
      .filter(|line| line.contains("updates are operator-managed"))
      .count(),
    8
  );
  assert!(text.contains("RunnerRefresh"));
  assert!(text.contains("AgentRefresh"));
  assert!(text.contains("install a validated toolu release"));
  assert!(!text.contains("opaque-update-body-do-not-log"));
}

//! Broker envelope classification and body decryption.
//!
//! Only job, migration, and cancellation messages need decoded bodies.

use crate::SessionCtx;
use crate::message_route::{MessageRoute, route};
use protocol::messages::{BrokerMessage, BrokerMigrationBody, JobCancelBody, MessageType};
use shared::RunnerError;

/// Outcome of a single `poll_message` call, classified for the loop.
pub(crate) enum PollOutcome {
  /// Long-poll returned 202 — broker accepted the connection but had
  /// no work.
  NoWork,
  /// Long-poll returned a `BrokerMigration` message; carries the new
  /// broker URL and the message id (to advance the redelivery cursor).
  Migrated { url: String, message_id: i64 },
  /// Long-poll returned a `RunnerJobRequest` — caller should acquire.
  Job(BrokerMessage),
  /// Long-poll returned a `JobCancellation` — caller should cancel the
  /// in-flight token. No broker ack is sent (a cancel body carries no
  /// `runner_request_id`); the message is carried so `message_id()` advances
  /// the redelivery cursor, plus the target `jobId` for logging / scoping.
  Cancel { msg: BrokerMessage, job_id: String },
  /// A request to replace the session OAuth access token.
  RefreshToken { message_id: i64 },
  /// A recognized update/configuration request with no local updater.
  UnsupportedControl { message_id: i64, kind: &'static str },
  /// A broker request to stop the listener.
  Shutdown { message_id: i64 },
  /// A future message type whose body is intentionally opaque.
  Unknown { message_id: i64 },
  /// A received message that could not be decrypted or parsed. Carries its
  /// id so the cursor advances past it (the broker won't re-serve it), so the
  /// runner does not wedge re-fetching one poisoned message forever.
  Skip { message_id: i64 },
  /// Network/HTTP failure — caller should back off and retry.
  NetworkError(RunnerError),
  /// Cancellation token tripped during the poll — caller should exit.
  Cancelled,
}

impl PollOutcome {
  /// The broker message id, when the outcome carried a real message.
  /// Used to advance the `lastMessageId` redelivery cursor.
  pub(crate) fn message_id(&self) -> Option<i64> {
    match self {
      Self::Migrated { message_id, .. }
      | Self::Skip { message_id }
      | Self::RefreshToken { message_id }
      | Self::UnsupportedControl { message_id, .. }
      | Self::Shutdown { message_id }
      | Self::Unknown { message_id } => Some(*message_id),
      Self::Job(msg) | Self::Cancel { msg, .. } => Some(msg.message_id),
      Self::NoWork | Self::NetworkError(_) | Self::Cancelled => None,
    }
  }
}

/// Control types without a body contract bypass decryption entirely.
pub(crate) fn classify_received_message(ctx: &SessionCtx, mut msg: BrokerMessage) -> PollOutcome {
  let needs_body = matches!(
    route(&msg.message_type),
    MessageRoute::AcquireJob | MessageRoute::Migrate | MessageRoute::Cancel
  );
  if needs_body && let Err(e) = decrypt_body_if_needed(ctx, &mut msg) {
    tracing::warn!(message_id = msg.message_id, error = %e, "broker message decrypt failed");
    return PollOutcome::Skip {
      message_id: msg.message_id,
    };
  }
  classify_message(msg)
}

/// Classify a (decrypted) broker message into a poll outcome.
///
/// The message-type → action decision is the pure
/// [`super::message_route::route`]; this fn attaches the parsed body.
pub(crate) fn classify_message(msg: BrokerMessage) -> PollOutcome {
  let message_id = msg.message_id;
  match route(&msg.message_type) {
    MessageRoute::Migrate => match parse_migration(&msg.body) {
      Ok(url) => PollOutcome::Migrated { url, message_id },
      // An unparseable control message must not wedge the cursor: skip it.
      // A job request is never dropped here — `AcquireJob` is returned intact.
      Err(e) => {
        tracing::warn!(message_id, error = %e, "broker migration message unparseable");
        PollOutcome::Skip { message_id }
      },
    },
    MessageRoute::AcquireJob => PollOutcome::Job(msg),
    MessageRoute::RefreshToken => PollOutcome::RefreshToken { message_id },
    MessageRoute::UnsupportedControl => {
      let kind = match msg.message_type {
        MessageType::RunnerRefresh => "RunnerRefresh",
        MessageType::AgentRefresh => "AgentRefresh",
        MessageType::RunnerRefreshConfig => "RunnerRefreshConfig",
        MessageType::RunnerJobRequest
        | MessageType::BrokerMigration
        | MessageType::JobCancellation
        | MessageType::ForceTokenRefresh
        | MessageType::HostedRunnerShutdown
        | MessageType::Unknown(_) => "known control",
      };
      PollOutcome::UnsupportedControl { message_id, kind }
    },
    MessageRoute::Shutdown => PollOutcome::Shutdown { message_id },
    MessageRoute::SkipUnknown => PollOutcome::Unknown { message_id },
    MessageRoute::Cancel => match parse_cancel(&msg.body) {
      Ok(job_id) => PollOutcome::Cancel { msg, job_id },
      Err(e) => {
        tracing::warn!(message_id, error = %e, "broker cancel message unparseable");
        PollOutcome::Skip { message_id }
      },
    },
  }
}

/// Decrypt `msg.body` in place when the session negotiated encryption.
///
/// No-op (plaintext passthrough) when the session has no encryption key —
/// the common github.com JIT case where broker bodies arrive in cleartext.
///
/// # Errors
///
/// Returns `RunnerError::Protocol` on a missing IV, key unwrap failure, or
/// AES-CBC decryption failure.
fn decrypt_body_if_needed(ctx: &SessionCtx, msg: &mut BrokerMessage) -> Result<(), RunnerError> {
  let Some(key) = ctx.encryption_key.as_ref() else {
    return Ok(());
  };
  let iv = msg
    .iv
    .as_deref()
    .ok_or_else(|| RunnerError::Protocol("encrypted broker message missing iv".to_owned()))?;
  let plaintext = protocol::decrypt_broker_body(
    &msg.body,
    iv,
    key,
    &ctx.rsa_private_key_der,
    ctx.use_fips_encryption,
  )?;
  msg.body = String::from_utf8(plaintext)
    .map_err(|e| RunnerError::Protocol(format!("decrypted broker body not UTF-8: {e}")))?;
  Ok(())
}

fn parse_migration(body: &str) -> Result<String, RunnerError> {
  let migration: BrokerMigrationBody = serde_json::from_str(body)
    .map_err(|e| RunnerError::Protocol(format!("migration parse: {e}")))?;
  tracing::info!(new_url = %migration.broker_base_url, "broker migration");
  Ok(migration.broker_base_url)
}

/// Parse a `JobCancellation` body, returning the target `jobId`.
fn parse_cancel(body: &str) -> Result<String, RunnerError> {
  let cancel: JobCancelBody = serde_json::from_str(body)
    .map_err(|e| RunnerError::Protocol(format!("cancel body parse: {e}")))?;
  Ok(cancel.job_id)
}

/// Emit bounded metadata for a broker envelope that takes no action.
pub(crate) fn log_skipped_control(outcome: &PollOutcome) {
  if let PollOutcome::Skip { message_id } = outcome {
    log_bad_message(*message_id);
  } else if let PollOutcome::Unknown { message_id } = outcome {
    log_unknown_message(*message_id);
  } else if let PollOutcome::UnsupportedControl { message_id, kind } = outcome {
    log_unsupported_control(*message_id, kind);
  }
}

fn log_bad_message(message_id: i64) {
  tracing::warn!(
    message_id,
    "skipping undecryptable/unparseable broker message"
  );
}

fn log_unknown_message(message_id: i64) {
  tracing::warn!(message_id, "skipping unknown broker message type");
}

fn log_unsupported_control(message_id: i64, kind: &str) {
  tracing::warn!(
    message_id,
    kind,
    "broker control request has no local updater"
  );
}

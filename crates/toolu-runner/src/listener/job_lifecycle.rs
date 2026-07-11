use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::SessionCtx;
use super::execution_loop::execute_with_renewal;
use super::helpers::map_conclusion;
use crate::net::{PollParams, acknowledge_message, poll_message};
use crate::reporting::StepResult;
use crate::reporting::live_log::LiveLogStreamer;
use crate::reporting::run_service::{
  AcquireJobRequest, CompleteJobRequest, acquire_job, complete_job,
};
use protocol::messages::{BrokerMessage, BrokerMigrationBody, JobCancelBody};
use shared::{AgentJobRequestMessage, Conclusion, ListenerEvent, RunnerError};

use super::message_route::MessageRoute;

/// Starting backoff for a network error during the poll loop.
const POLL_BACKOFF_START: Duration = Duration::from_secs(1);
/// Cap on the exponential backoff between poll retries.
const POLL_BACKOFF_MAX: Duration = Duration::from_secs(60);

pub(super) async fn poll_and_execute(ctx: &mut SessionCtx) -> Result<(), RunnerError> {
  let Some(msg) = poll_until_job(ctx).await? else {
    return Ok(());
  };

  let body = parse_job_request_body(&msg)?;

  let acquire_req = AcquireJobRequest {
    job_message_id: body.runner_request_id.clone(),
    runner_os: std::env::consts::OS.to_owned(),
    billing_owner_id: body.billing_owner_id.clone(),
  };
  let acquired = acquire_job(&ctx.client, &body.run_service_url, &ctx.token, &acquire_req).await?;
  let rs_token = acquired
    .run_service_token
    .clone()
    .unwrap_or_else(|| ctx.token.clone());
  tracing::info!(plan_id = %acquired.plan_id, "acquired job");
  let plan_id = acquired.plan_id.clone();

  let (conclusion, job_id, request_id, step_results, job_token) =
    run_job_with_cancel_watch(ctx, &body.run_service_url, &rs_token, &acquired).await;
  let rs_token = job_token.unwrap_or(rs_token);

  // Acknowledge the broker message — required by the JIT protocol before complete_job.
  // The broker keys the ack on the job's runner_request_id (UUID), not the
  // numeric broker message id.
  acknowledge_message(
    &ctx.client,
    &ctx.broker_url,
    &ctx.token,
    &body.runner_request_id,
  )
  .await?;

  let complete_req = CompleteJobRequest {
    plan_id,
    job_id,
    request_id,
    conclusion: map_conclusion(conclusion),
    outputs: serde_json::Value::Object(serde_json::Map::new()),
    step_results,
    annotations: Vec::new(),
  };
  complete_job(&ctx.client, &body.run_service_url, &rs_token, &complete_req).await
}

/// Parse and execute the acquired job. Always returns a conclusion — never leaves
/// the job hanging on GitHub even if parsing fails.
/// Returns `(conclusion, job_id, request_id, step_results, optional_token)`.
/// Result tuple for a job that never started because its message failed
/// to parse — completes with failure rather than hanging on GitHub.
type JobOutcome = (Conclusion, String, i64, Vec<StepResult>, Option<String>);

/// Parse the acquired job body, or return a ready-made failure outcome.
fn parse_job_message(
  acquired: &crate::reporting::run_service::AcquireJobResponse,
) -> Result<AgentJobRequestMessage, JobOutcome> {
  serde_json::from_value(acquired.body.clone()).map_err(|e| {
    tracing::error!(error = %e, "job message parse failed — completing with failure");
    (
      Conclusion::Failure,
      "unknown".to_owned(),
      0,
      Vec::new(),
      None,
    )
  })
}

/// Run the acquired job while a sidecar future keeps polling the broker
/// for a mid-job `JobCancellation` (the C# runner's listener does the
/// same while its worker executes). The watcher trips `job_cancel` — a
/// child of the session token, so SIGINT/SIGTERM still propagates — and
/// the engine winds the job down; the normal completion path then
/// reports `cancelled` to GitHub.
async fn run_job_with_cancel_watch(
  ctx: &SessionCtx,
  run_service_url: &str,
  rs_token: &str,
  acquired: &crate::reporting::run_service::AcquireJobResponse,
) -> JobOutcome {
  let job_cancel = ctx.cancel.child_token();
  let exec = run_acquired_job(ctx, run_service_url, rs_token, acquired, &job_cancel);
  tokio::pin!(exec);
  let watch = watch_for_gh_cancel(ctx, &job_cancel);
  tokio::pin!(watch);
  tokio::select! {
    outcome = &mut exec => outcome,
    // The watcher pends forever after signalling, so this arm only
    // fires if it somehow returns — finish the job either way.
    () = &mut watch => (&mut exec).await,
  }
}

/// Poll the broker for a `JobCancellation` while a job is in flight.
///
/// On a cancel message: trip `job_cancel` and go dormant (the caller's
/// select! keeps driving only the job). Anything else is skipped with
/// the cursor advanced so the broker does not re-serve it after the
/// job completes. Never returns; the caller drops this future when the
/// job finishes.
async fn watch_for_gh_cancel(ctx: &SessionCtx, job_cancel: &CancellationToken) {
  let mut last_message_id: i64 = 0;
  let mut backoff = POLL_BACKOFF_START;
  loop {
    let outcome = poll_once(ctx, last_message_id).await;
    if let Some(id) = outcome.message_id() {
      last_message_id = id;
    }
    match watch_step(ctx, job_cancel, outcome, backoff).await {
      Some(next) => backoff = next,
      None => return std::future::pending::<()>().await,
    }
  }
}

/// React to one mid-job poll outcome. Returns the next backoff, or
/// `None` when the watcher should go dormant (cancellation signalled,
/// session token tripped, or backoff interrupted by shutdown).
async fn watch_step(
  ctx: &SessionCtx,
  job_cancel: &CancellationToken,
  outcome: PollOutcome,
  backoff: Duration,
) -> Option<Duration> {
  match outcome {
    PollOutcome::Cancel { job_id, .. } => {
      note_cancel_mid_job(&job_id);
      job_cancel.cancel();
      None
    },
    // Session token tripped (SIGINT/SIGTERM): `job_cancel` is a child
    // token, so the job is already winding down — just go dormant.
    PollOutcome::Cancelled => None,
    PollOutcome::NetworkError(e) => backoff_after_poll_error(ctx, &e, backoff).await,
    PollOutcome::NoWork | PollOutcome::Skip { .. } => Some(POLL_BACKOFF_START),
    PollOutcome::Migrated { url, .. } => {
      note_migration_mid_job(&url);
      Some(POLL_BACKOFF_START)
    },
    PollOutcome::Job(msg) => {
      note_unexpected_job_mid_job(msg.message_id);
      Some(POLL_BACKOFF_START)
    },
  }
}

/// Log a mid-job `JobCancellation` before tripping the job token.
fn note_cancel_mid_job(job_id: &str) {
  tracing::info!(
    job_id,
    "JobCancellation received mid-job — cancelling in-flight job"
  );
}

/// Log a broker migration that arrived while a job is in flight. The
/// watcher keeps polling the old URL; the main loop applies the new one
/// after the job completes.
fn note_migration_mid_job(url: &str) {
  tracing::warn!(new_url = %url, "broker migration mid-job — watcher keeps polling the old URL");
}

/// Log (and skip) a job message that arrived while a job is in flight —
/// a single-job runner never takes a second job.
fn note_unexpected_job_mid_job(message_id: i64) {
  tracing::warn!(
    message_id,
    "unexpected job message while a job is in flight — skipped"
  );
}

async fn run_acquired_job(
  ctx: &SessionCtx,
  run_service_url: &str,
  rs_token: &str,
  acquired: &crate::reporting::run_service::AcquireJobResponse,
  job_cancel: &CancellationToken,
) -> JobOutcome {
  let job_msg: AgentJobRequestMessage = match parse_job_message(acquired) {
    Ok(msg) => msg,
    Err(failure) => return failure,
  };

  let job_token = extract_system_token(&job_msg);

  let _ = ctx
    .tx
    .send(ListenerEvent::JobAcquired {
      job_id: job_msg.job_id.clone(),
      run_service_url: run_service_url.to_owned(),
    })
    .await;

  let request_id = job_msg.request_id;
  let effective_token = job_token.as_deref().unwrap_or(rs_token);

  // Connect live log WebSocket for real-time log streaming to GitHub UI.
  let live_log_tx = connect_live_log(&job_msg, effective_token).await;

  let route = super::execution_loop::JobRoute {
    run_service_url,
    rs_token: effective_token,
    plan_id: &acquired.plan_id,
  };
  let (conclusion, step_results) =
    execute_with_renewal(ctx, &route, &job_msg, live_log_tx, job_cancel).await;
  (
    conclusion,
    job_msg.job_id,
    request_id,
    step_results,
    job_token,
  )
}

async fn poll_until_job(ctx: &mut SessionCtx) -> Result<Option<BrokerMessage>, RunnerError> {
  let mut backoff = POLL_BACKOFF_START;
  // Redelivery cursor: `0` until the first message, then the id of the last
  // message handled. Sent on every poll so the broker skips re-served ones.
  let mut last_message_id: i64 = 0;
  loop {
    let result = poll_once(ctx, last_message_id).await;
    if let Some(id) = result.message_id() {
      last_message_id = id;
    }
    match result {
      PollOutcome::Cancelled => return Ok(None),
      PollOutcome::NoWork => {
        backoff = POLL_BACKOFF_START;
        continue;
      },
      PollOutcome::Skip { message_id } => {
        // An undecryptable / unparseable message. The cursor was already
        // advanced past it above (`result.message_id()`), so the broker
        // won't re-serve it — otherwise we'd wedge in an infinite backoff
        // loop on one poisoned message. Reset backoff and keep polling.
        tracing::warn!(
          message_id,
          "skipping undecryptable/unparseable broker message"
        );
        backoff = POLL_BACKOFF_START;
        continue;
      },
      PollOutcome::Migrated { url, .. } => {
        ctx.broker_url = url;
        backoff = POLL_BACKOFF_START;
        continue;
      },
      PollOutcome::Job(msg) => return Ok(Some(msg)),
      PollOutcome::Cancel { msg: _, job_id } => {
        handle_cancellation(ctx, &job_id);
        return Ok(None);
      },
      PollOutcome::NetworkError(e) => match backoff_after_poll_error(ctx, &e, backoff).await {
        Some(next) => backoff = next,
        None => return Ok(None),
      },
    }
  }
}

/// Warn about a failed poll and sleep out the jittered backoff. Returns the
/// doubled (capped) backoff for the next attempt, or `None` when cancellation
/// fired during the sleep and the poll loop must exit.
async fn backoff_after_poll_error(
  ctx: &SessionCtx,
  e: &RunnerError,
  backoff: Duration,
) -> Option<Duration> {
  let backoff_ms = u64::try_from(backoff.as_millis()).unwrap_or(u64::MAX);
  tracing::warn!(
    error = %e,
    backoff_ms,
    "poll failed — retrying after backoff"
  );
  let jittered = jittered_backoff(backoff);
  if sleep_or_cancel(ctx, jittered).await {
    return None;
  }
  Some(backoff.saturating_mul(2).min(POLL_BACKOFF_MAX))
}

/// Apply decorrelated jitter to a backoff duration so concurrent runners
/// don't synchronize their retries. Returns a duration in
/// `[backoff/2, backoff)` — half the current backoff plus a random offset.
fn jittered_backoff(d: Duration) -> Duration {
  let Ok(half_ms) = u64::try_from(d.as_millis().saturating_div(2)) else {
    return d;
  };
  if half_ms == 0 {
    return d;
  }
  let jitter = fastrand::u64(0..half_ms);
  Duration::from_millis(half_ms + jitter)
}

/// Outcome of a single `poll_message` call, classified for the loop.
enum PollOutcome {
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
  fn message_id(&self) -> Option<i64> {
    match self {
      Self::Migrated { message_id, .. } | Self::Skip { message_id } => Some(*message_id),
      Self::Job(msg) | Self::Cancel { msg, .. } => Some(msg.message_id),
      Self::NoWork | Self::NetworkError(_) | Self::Cancelled => None,
    }
  }
}

async fn poll_once(ctx: &SessionCtx, last_message_id: i64) -> PollOutcome {
  let params = PollParams {
    client: &ctx.client,
    server_url_v2: &ctx.broker_url,
    token: &ctx.token,
    session_id: &ctx.session_id,
    runner_version: "3.0.0",
    // Derive os/arch from the same helpers the acknowledge path uses so a
    // single runner advertises one consistent identity on both calls
    // (the raw `std::env::consts` values "linux"/"x86_64" differed from the
    // canonical GitHub "Linux"/"X64" the acknowledge request sends).
    os: shared::platform::runner_os(),
    architecture: shared::platform::runner_arch(),
    last_message_id,
  };

  let poll_fut = poll_message(&params);
  tokio::select! {
    biased;
    () = ctx.cancel.cancelled() => PollOutcome::Cancelled,
    result = poll_fut => match result {
      Ok(None) => PollOutcome::NoWork,
      Ok(Some(mut msg)) => match decrypt_body_if_needed(ctx, &mut msg) {
        Ok(()) => classify_message(msg),
        // A decrypt failure is per-message, not a transport fault: skip past
        // this message id so the broker stops re-serving it (see F5).
        Err(e) => {
          tracing::warn!(message_id = msg.message_id, error = %e, "broker message decrypt failed");
          PollOutcome::Skip {
            message_id: msg.message_id,
          }
        },
      },
      Err(e) => PollOutcome::NetworkError(e),
    },
  }
}

/// Classify a (decrypted) broker message into a poll outcome.
///
/// The message-type → action decision is the pure
/// [`super::message_route::route`]; this fn attaches the parsed body.
fn classify_message(msg: BrokerMessage) -> PollOutcome {
  let message_id = msg.message_id;
  match super::message_route::route(&msg.message_type) {
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

/// Cancel the in-flight job token.
///
/// The shared `ctx.cancel` token is observed by the poll loop, the renewal
/// task, and the running job, so cancelling it stops the whole pipeline.
/// No broker ack is sent: `acknowledge` validates `runnerRequestId` (the
/// job request's UUID) and a `JobCancellation` body carries only a `jobId`,
/// so an ack keyed by it is always rejected with a 400. Redelivery is
/// prevented by the poll's `lastMessageId` cursor advancing past the message.
fn handle_cancellation(ctx: &SessionCtx, job_id: &str) {
  tracing::info!(
    job_id,
    "received JobCancellation — cancelling in-flight job"
  );
  ctx.cancel.cancel();
}

/// Sleep for `duration`, returning `true` if cancellation fired first.
async fn sleep_or_cancel(ctx: &SessionCtx, duration: Duration) -> bool {
  tokio::select! {
    biased;
    () = ctx.cancel.cancelled() => true,
    () = tokio::time::sleep(duration) => false,
  }
}

fn parse_job_request_body(
  msg: &BrokerMessage,
) -> Result<protocol::messages::RunnerJobRequestBody, RunnerError> {
  serde_json::from_str(&msg.body)
    .map_err(|e| RunnerError::Protocol(format!("job request body parse: {e}")))
}

/// Connect live log WebSocket for real-time log streaming to GitHub UI.
/// Returns None on failure — live logs are best-effort.
async fn connect_live_log(
  job_msg: &AgentJobRequestMessage,
  fallback_token: &str,
) -> Option<tokio::sync::mpsc::Sender<crate::reporting::live_log::LiveLogLine>> {
  let url = job_msg.feed_stream_url()?;
  let token = super::helpers::system_vss_access_token(job_msg);
  let ws_token = token.as_deref().unwrap_or(fallback_token);
  let (tx, handle) = LiveLogStreamer::connect(&url, ws_token).await?;
  // Detach — handle completes when tx is dropped.
  tokio::spawn(async move {
    if let Err(e) = handle.await {
      tracing::warn!(error = %e, "live log WebSocket task panicked");
    }
  });
  Some(tx)
}

/// Extract the SystemVssConnection AccessToken from job message endpoints.
fn extract_system_token(job_msg: &AgentJobRequestMessage) -> Option<String> {
  let token = super::helpers::system_vss_access_token(job_msg);
  if token.is_none() {
    let endpoint = job_msg
      .resources
      .endpoints
      .iter()
      .find(|e| e.name.eq_ignore_ascii_case("SystemVssConnection"));
    let auth_keys: Vec<&str> = endpoint
      .and_then(|e| e.authorization.as_ref())
      .map(|a| a.parameters.keys().map(String::as_str).collect())
      .unwrap_or_default();
    tracing::warn!(
      endpoint_found = endpoint.is_some(),
      auth_scheme = endpoint
        .and_then(|e| e.authorization.as_ref())
        .map(|a| a.scheme.as_str())
        .unwrap_or("<none>"),
      auth_keys = ?auth_keys,
      "SystemVssConnection AccessToken not found"
    );
  }
  token
}

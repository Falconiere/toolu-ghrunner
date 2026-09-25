use std::time::Duration;

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::SessionCtx;
use super::execution_loop::{JobExecution, execute_with_renewal};
use super::helpers::map_conclusion;
use protocol::messages::BrokerMessage;
use shared::{AgentJobRequestMessage, Conclusion, ListenerEvent, RunnerError};
use wire::net::{PollParams, acknowledge_message, poll_message};
use wire::reporting::run_service::{
  AcquireJobRequest, CompleteJobRequest, acquire_job, complete_job,
};

use crate::broker_message::{PollOutcome, classify_received_message, log_skipped_control};
use crate::broker_refresh::refresh_access_token;

/// Starting backoff for a network error during the poll loop.
const POLL_BACKOFF_START: Duration = Duration::from_secs(1);
/// Cap on the exponential backoff between poll retries.
const POLL_BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Poll until a job arrives (or the lifecycle exits with no job), then
/// acquire, run, and report it. `Ok(None)` means no job was ever acquired;
/// `Ok(Some(conclusion))` is the completed job's final conclusion, threaded
/// up so [`super::handler::GitHubListener::run`] can surface it.
pub(super) async fn poll_and_execute(
  ctx: &mut SessionCtx,
) -> Result<Option<Conclusion>, RunnerError> {
  let Some(msg) = poll_until_job(ctx).await? else {
    return Ok(None);
  };

  let body = parse_job_request_body(&msg)?;

  // Deliberately the RAW lowercase `std::env::consts::OS` ("linux"/"macos"),
  // NOT the canonical `shared::platform::runner_os()` the poll and acknowledge
  // paths send. This is a live acquisition contract that works as-is; there is
  // no evidence the Run Service validates the field, and a wrong guess here
  // takes the runner fully offline. Do not "converge" it without that evidence.
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

  // Ack right after acquire, not after the job finishes — see the "early ack"
  // rationale + accepted risk on `acknowledge_best_effort`'s doc comment.
  acknowledge_best_effort(ctx, &body.runner_request_id).await;

  let (mut outcome, refreshed_token) =
    run_job_with_cancel_watch(ctx, &body.run_service_url, &rs_token, &acquired).await;
  if let Some(token) = refreshed_token {
    ctx.token = token;
  }
  // Stash the two detached-task handles now, before any further `?` in this
  // function — `ctx` is a full `&mut` borrow again here (the call above only
  // reborrowed it immutably), so these assignments survive an early return
  // below, and `cleanup_session` (called unconditionally by the caller) can
  // join both on the `Ok` and `Err` paths. The job-log upload in particular
  // is deliberately still in flight across `report_completion` below, whose
  // `?` is the early return in question.
  ctx.live_log = outcome.live_log_handle.take();
  ctx.job_log_upload = outcome.job_log_upload.take();
  let rs_token = outcome.job_token.clone().unwrap_or(rs_token);
  // `Conclusion` is `Copy`; take it before `outcome` moves into the report so
  // the caller still gets the job's verdict.
  let conclusion = outcome.conclusion;

  report_completion(ctx, &body.run_service_url, plan_id, rs_token, outcome).await?;
  Ok(Some(conclusion))
}

/// Best-effort acknowledge of the broker message: single attempt,
/// WARN-on-failure, non-gating (independent endpoint from `complete_job`).
///
/// Called by [`poll_and_execute`] right after `acquire_job`, not after the
/// job: an un-acked message is re-served (and skipped) on
/// `watch_for_gh_cancel`'s first poll — acking early removes that re-see.
/// Cancels are unaffected (separately keyed message). **Accepted risk**
/// (design doc, Architecture item 3): a runner death between this ack and
/// `complete_job` now loses broker redelivery for the whole job, not just
/// the old post-job window.
async fn acknowledge_best_effort(ctx: &SessionCtx, runner_request_id: &str) {
  if let Err(e) =
    acknowledge_message(&ctx.client, &ctx.broker_url, &ctx.token, runner_request_id).await
  {
    tracing::warn!(error = %e, "broker acknowledge failed — continuing to report completion");
  }
}

/// Report the job's outcome to the Run Service, retrying `RunnerError::Network`
/// failures (see [`crate::retry::retry_transient`]) so the report survives a
/// still-recovering connection instead of being lost when the always-online
/// loop re-polls.
async fn report_completion(
  ctx: &SessionCtx,
  run_service_url: &str,
  plan_id: String,
  rs_token: String,
  outcome: JobOutcome,
) -> Result<(), RunnerError> {
  let complete_req = CompleteJobRequest {
    plan_id,
    job_id: outcome.job_id,
    request_id: outcome.request_id,
    conclusion: map_conclusion(outcome.conclusion),
    outputs: serde_json::Value::Object(serde_json::Map::new()),
    step_results: outcome.step_results,
    annotations: outcome.annotations,
  };
  // `retry_transient` bounds its closure as `FnMut() -> Fut` with no `'static`
  // on `Fut`, so each attempt can borrow these rather than clone them. The
  // request in particular owns the step results and annotations — cloning it
  // per retry allocated the whole job's reporting payload again each time.
  crate::retry::retry_transient(
    || complete_job(&ctx.client, run_service_url, &rs_token, &complete_req),
    &ctx.cancel,
    crate::retry::REPORT_RETRY_MAX,
    "complete_job",
  )
  .await
}

/// Outcome of running (or failing to parse) an acquired job — everything
/// `report_completion` needs to build the `CompleteJobRequest`.
///
/// Replaces the previous bare 6-tuple alias so the `annotations` field
/// (populated by the outage watchdog's failure-override path in
/// [`execute_with_renewal`]) has a name instead of a positional slot.
struct JobOutcome {
  conclusion: Conclusion,
  job_id: String,
  request_id: i64,
  step_results: Vec<wire::reporting::StepResult>,
  /// `SystemVssConnection` token extracted from the job message, when
  /// present — overrides the run-service token used for reporting.
  job_token: Option<String>,
  annotations: Vec<wire::reporting::Annotation>,
  /// The live-log wrapper task's `JoinHandle`, threaded up so
  /// `poll_and_execute` can stash it on `ctx` before any further fallible
  /// call — see the design note at its assignment site.
  live_log_handle: Option<tokio::task::JoinHandle<()>>,
  /// The combined job-log upload task's `JoinHandle`, threaded up the same
  /// way and for the same reason: it is still running while
  /// `report_completion` is on the wire, and must be joined whichever way
  /// that call returns.
  job_log_upload: Option<tokio::task::JoinHandle<()>>,
}

/// Parse the acquired job body, or return a ready-made failure outcome —
/// never leaves the job hanging on GitHub even if parsing fails.
///
/// The failure outcome is boxed: `JobOutcome` carries the whole job's step
/// results, so an unboxed `Err` variant would make every caller of this
/// `Result` pay that size (clippy's `result_large_err`). The one caller
/// dereferences it straight into its own return value.
fn parse_job_message(
  acquired: &wire::reporting::run_service::AcquireJobResponse,
) -> Result<AgentJobRequestMessage, Box<JobOutcome>> {
  serde_json::from_value(acquired.body.clone()).map_err(|e| {
    tracing::error!(error = %e, "job message parse failed — completing with failure");
    Box::new(JobOutcome {
      conclusion: Conclusion::Failure,
      job_id: "unknown".to_owned(),
      request_id: 0,
      step_results: Vec::new(),
      job_token: None,
      annotations: Vec::new(),
      live_log_handle: None,
      job_log_upload: None,
    })
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
  acquired: &wire::reporting::run_service::AcquireJobResponse,
) -> (JobOutcome, Option<String>) {
  let job_cancel = ctx.cancel.child_token();
  let (token_tx, token_rx) = watch::channel(None);
  let exec = run_acquired_job(ctx, run_service_url, rs_token, acquired, &job_cancel);
  tokio::pin!(exec);
  let watcher = watch_for_gh_cancel(ctx, &job_cancel, &token_tx);
  tokio::pin!(watcher);
  let outcome = tokio::select! {
    outcome = &mut exec => outcome,
    // The watcher pends forever after signalling, so this arm only
    // fires if it somehow returns — finish the job either way.
    () = &mut watcher => (&mut exec).await,
  };
  let refreshed_token = token_rx.borrow().clone();
  (outcome, refreshed_token)
}

/// Poll the broker for a `JobCancellation` while a job is in flight.
///
/// On a cancel message: trip `job_cancel` and go dormant (the caller's
/// select! keeps driving only the job). Anything else is skipped with
/// the cursor advanced so the broker does not re-serve it after the
/// job completes. Never returns; the caller drops this future when the
/// job finishes.
async fn watch_for_gh_cancel(
  ctx: &SessionCtx,
  job_cancel: &CancellationToken,
  token_tx: &watch::Sender<Option<String>>,
) {
  let mut last_message_id: i64 = 0;
  let mut backoff = POLL_BACKOFF_START;
  let mut token = ctx.token.clone();
  loop {
    let outcome = poll_once(ctx, &token, last_message_id).await;
    if let Some(id) = outcome.message_id() {
      if is_redelivery(last_message_id, &outcome) {
        tracing::warn!(
          message_id = id,
          "broker redelivered an already-seen message"
        );
        if sleep_or_cancel(ctx, backoff).await {
          return std::future::pending::<()>().await;
        }
        backoff = backoff.saturating_mul(2).min(POLL_BACKOFF_MAX);
        continue;
      }
      last_message_id = id;
    }
    match watch_step(ctx, job_cancel, &mut token, token_tx, outcome, backoff).await {
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
  token: &mut String,
  token_tx: &watch::Sender<Option<String>>,
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
    control @ (PollOutcome::NoWork
    | PollOutcome::Skip { .. }
    | PollOutcome::Unknown { .. }
    | PollOutcome::UnsupportedControl { .. }
    | PollOutcome::Shutdown { .. }
    | PollOutcome::RefreshToken { .. }
    | PollOutcome::Migrated { .. }
    | PollOutcome::Job(_)) => watch_control_step(ctx, token, token_tx, control, backoff).await,
  }
}

/// Apply broker control messages while a job remains in flight.
async fn watch_control_step(
  ctx: &SessionCtx,
  token: &mut String,
  token_tx: &watch::Sender<Option<String>>,
  outcome: PollOutcome,
  backoff: Duration,
) -> Option<Duration> {
  match outcome {
    PollOutcome::NoWork
    | PollOutcome::Skip { .. }
    | PollOutcome::Unknown { .. }
    | PollOutcome::UnsupportedControl { .. } => {
      log_skipped_control(&outcome);
      Some(POLL_BACKOFF_START)
    },
    PollOutcome::Shutdown { message_id } => {
      tracing::info!(message_id, "broker requested runner shutdown");
      ctx.cancel.cancel();
      None
    },
    PollOutcome::RefreshToken { message_id } => {
      refresh_watcher_token(ctx, token, token_tx, message_id, backoff).await
    },
    PollOutcome::Migrated { url, .. } => {
      note_migration_mid_job(&url);
      Some(POLL_BACKOFF_START)
    },
    PollOutcome::Job(msg) => {
      note_unexpected_job_mid_job(msg.message_id);
      Some(POLL_BACKOFF_START)
    },
    PollOutcome::Cancel { .. } | PollOutcome::Cancelled | PollOutcome::NetworkError(_) => None,
  }
}

/// Replace the watcher's token only after an exchange succeeds.
async fn refresh_watcher_token(
  ctx: &SessionCtx,
  token: &mut String,
  token_tx: &watch::Sender<Option<String>>,
  message_id: i64,
  backoff: Duration,
) -> Option<Duration> {
  match refresh_access_token(ctx).await {
    Ok(next) => {
      token_tx.send_replace(Some(next.clone()));
      *token = next;
      tracing::info!(message_id, "broker token refreshed");
      Some(POLL_BACKOFF_START)
    },
    Err(RunnerError::Cancelled) => None,
    Err(e) => backoff_after_poll_error(ctx, &e, backoff).await,
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
  acquired: &wire::reporting::run_service::AcquireJobResponse,
  job_cancel: &CancellationToken,
) -> JobOutcome {
  let job_msg: AgentJobRequestMessage = match parse_job_message(acquired) {
    Ok(msg) => msg,
    Err(failure) => return *failure,
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

  let route = super::execution_loop::JobRoute {
    run_service_url,
    rs_token: effective_token,
    plan_id: &acquired.plan_id,
  };
  // `live_log_handle` is the wrapper task's `JoinHandle` for the live-log
  // WebSocket connection `execute_with_renewal` opens (concurrently with the
  // setup-step report); `job_log_upload` is the combined job-log upload it
  // spawned at conclusion time. Both are threaded back up through
  // `JobOutcome` so `poll_and_execute` can stash them on `ctx` for
  // `cleanup_session` to join, instead of being dropped un-joined here.
  let JobExecution {
    conclusion,
    steps,
    annotations,
    live_log_handle,
    job_log_upload,
  } = execute_with_renewal(ctx, &route, &job_msg, job_cancel).await;
  JobOutcome {
    conclusion,
    job_id: job_msg.job_id,
    request_id,
    step_results: steps,
    job_token,
    annotations,
    live_log_handle,
    job_log_upload,
  }
}

async fn poll_until_job(ctx: &mut SessionCtx) -> Result<Option<BrokerMessage>, RunnerError> {
  let mut backoff = POLL_BACKOFF_START;
  // Redelivery cursor: `0` until the first message, then the id of the last
  // message handled. Sent on every poll so the broker skips re-served ones.
  let mut last_message_id: i64 = 0;
  loop {
    let result = poll_once(ctx, &ctx.token, last_message_id).await;
    if let Some(id) = result.message_id() {
      if is_redelivery(last_message_id, &result) {
        tracing::warn!(
          message_id = id,
          "broker redelivered an already-seen message"
        );
        if sleep_or_cancel(ctx, backoff).await {
          return Ok(None);
        }
        backoff = backoff.saturating_mul(2).min(POLL_BACKOFF_MAX);
        continue;
      }
      last_message_id = id;
    }
    match handle_idle_outcome(ctx, result, &mut backoff).await? {
      IdleDecision::Continue => {},
      IdleDecision::Exit => return Ok(None),
      IdleDecision::Job(msg) => return Ok(Some(msg)),
    }
  }
}

/// A broker reply at or behind the cursor must not trigger a second action.
pub(crate) fn is_redelivery(last_message_id: i64, outcome: &PollOutcome) -> bool {
  outcome
    .message_id()
    .is_some_and(|message_id| message_id <= last_message_id)
}

/// Effect of one broker outcome on the idle poll loop.
enum IdleDecision {
  Continue,
  Exit,
  Job(BrokerMessage),
}

/// Apply a classified broker outcome after its cursor has been advanced.
async fn handle_idle_outcome(
  ctx: &mut SessionCtx,
  outcome: PollOutcome,
  backoff: &mut Duration,
) -> Result<IdleDecision, RunnerError> {
  match outcome {
    PollOutcome::Cancelled => return Ok(IdleDecision::Exit),
    PollOutcome::NoWork => {},
    PollOutcome::Migrated { url, .. } => ctx.broker_url = url,
    PollOutcome::Job(msg) => return Ok(IdleDecision::Job(msg)),
    PollOutcome::Cancel { msg: _, job_id } => {
      handle_cancellation(ctx, &job_id);
      return Ok(IdleDecision::Exit);
    },
    PollOutcome::NetworkError(e) => {
      return Ok(match backoff_after_poll_error(ctx, &e, *backoff).await {
        Some(next) => {
          *backoff = next;
          IdleDecision::Continue
        },
        None => IdleDecision::Exit,
      });
    },
    control @ (PollOutcome::Skip { .. }
    | PollOutcome::Unknown { .. }
    | PollOutcome::UnsupportedControl { .. }
    | PollOutcome::Shutdown { .. }
    | PollOutcome::RefreshToken { .. }) => {
      let decision = handle_idle_control(ctx, control).await?;
      *backoff = POLL_BACKOFF_START;
      return Ok(decision);
    },
  }
  *backoff = POLL_BACKOFF_START;
  Ok(IdleDecision::Continue)
}

/// Handle a control envelope after the idle cursor has advanced.
async fn handle_idle_control(
  ctx: &mut SessionCtx,
  outcome: PollOutcome,
) -> Result<IdleDecision, RunnerError> {
  match outcome {
    PollOutcome::Skip { .. }
    | PollOutcome::Unknown { .. }
    | PollOutcome::UnsupportedControl { .. } => log_skipped_control(&outcome),
    PollOutcome::Shutdown { message_id } => {
      tracing::info!(message_id, "broker requested runner shutdown");
      ctx.cancel.cancel();
      return Ok(IdleDecision::Exit);
    },
    PollOutcome::RefreshToken { message_id } => return refresh_idle_token(ctx, message_id).await,
    PollOutcome::NoWork
    | PollOutcome::Migrated { .. }
    | PollOutcome::Job(_)
    | PollOutcome::Cancel { .. }
    | PollOutcome::NetworkError(_)
    | PollOutcome::Cancelled => {},
  }
  Ok(IdleDecision::Continue)
}

/// Publish a replacement token only after a complete successful exchange.
async fn refresh_idle_token(
  ctx: &mut SessionCtx,
  message_id: i64,
) -> Result<IdleDecision, RunnerError> {
  match refresh_access_token(ctx).await {
    Ok(next) => {
      ctx.token = next;
      tracing::info!(message_id, "broker token refreshed");
      Ok(IdleDecision::Continue)
    },
    Err(RunnerError::Cancelled) => Ok(IdleDecision::Exit),
    Err(e) => Err(e),
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
  let jittered = crate::retry::jittered_backoff(backoff);
  if sleep_or_cancel(ctx, jittered).await {
    return None;
  }
  Some(backoff.saturating_mul(2).min(POLL_BACKOFF_MAX))
}

async fn poll_once(ctx: &SessionCtx, token: &str, last_message_id: i64) -> PollOutcome {
  let params = PollParams {
    client: &ctx.client,
    server_url_v2: &ctx.broker_url,
    token,
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
      Ok(Some(msg)) => classify_received_message(ctx, msg),
      Err(e) => PollOutcome::NetworkError(e),
    },
  }
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

/// Extract the `SystemVssConnection` `AccessToken` from job message endpoints.
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
        .map_or("<none>", |a| a.scheme.as_str()),
      auth_keys = ?auth_keys,
      "SystemVssConnection AccessToken not found"
    );
  }
  token
}

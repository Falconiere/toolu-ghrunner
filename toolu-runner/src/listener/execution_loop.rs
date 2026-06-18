use std::collections::HashMap;

use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::SessionCtx;
use super::helpers::{
  RenewalParams, ResultsCtx, StepMetaMap, report_step_to_results, resolve_backend_ids,
  spawn_renewal,
};
use super::log_uploader::StreamerConfig;
use super::setup_step::report_setup_step;
use super::step_reporter::StepCollector;
use crate::Runner;
use crate::reporting::live_log::LiveLogLine;
use shared::{AgentJobRequestMessage, Conclusion, ListenerEvent, RunnerEvent};

pub(super) async fn execute_with_renewal(
  ctx: &SessionCtx,
  run_service_url: &str,
  rs_token: &str,
  plan_id: &str,
  job_msg: &AgentJobRequestMessage,
  live_log_tx: Option<tokio::sync::mpsc::Sender<LiveLogLine>>,
) -> (Conclusion, Vec<crate::reporting::StepResult>) {
  let renewal_cancel = CancellationToken::new();
  let renewal_handle = start_renewal(
    ctx,
    run_service_url,
    rs_token,
    plan_id,
    job_msg,
    &renewal_cancel,
  );

  // Report "Set up job" as step 1 (matches C# runner order).
  // Real workflow steps start at number 2+.
  let (setup_result, setup_lines) =
    report_setup_step(rs_token, plan_id, job_msg, &ctx.client).await;

  let collector = StepCollector::new();
  if let Some(result) = setup_result {
    collector.push_result(result).await;
  }
  let results_url = job_msg
    .variables
    .get("system.github.results_endpoint")
    .map(|v| v.value.trim_end_matches('/').to_owned());
  let (run_backend_id, job_backend_id) = resolve_backend_ids(job_msg, plan_id);

  // The engine owns its own event channel; we get a receiver back and
  // hand it to the forwarder, which derives the conclusion and signals
  // back via the oneshot.
  let runner = Runner::new(ctx.config.clone());
  let engine_rx = runner
    .execute_job(job_msg.clone(), ctx.cancel.clone())
    .await;

  let (conclusion_tx, conclusion_rx) = oneshot::channel::<Conclusion>();
  let fwd_handle = spawn_event_forwarder(
    engine_rx,
    collector.clone(),
    ctx.tx.clone(),
    FwdConfig {
      results_url: results_url.clone(),
      results_client: ctx.client.clone(),
      results_token: rs_token.to_owned(),
      run_backend_id: run_backend_id.clone(),
      job_backend_id: job_backend_id.clone(),
      setup_lines,
      live_log_tx,
    },
    conclusion_tx,
  );

  let conclusion = if let Ok(c) = conclusion_rx.await {
    c
  } else {
    tracing::error!("event forwarder dropped the conclusion sender");
    Conclusion::Failure
  };
  renewal_cancel.cancel();
  let _ = renewal_handle.await;
  let _ = fwd_handle.await;

  let step_results = collector.collected_results().await;
  (conclusion, step_results)
}

fn start_renewal(
  ctx: &SessionCtx,
  run_service_url: &str,
  rs_token: &str,
  plan_id: &str,
  job_msg: &AgentJobRequestMessage,
  cancel: &CancellationToken,
) -> tokio::task::JoinHandle<()> {
  let params = RenewalParams {
    client: ctx.client.clone(),
    token: rs_token.to_owned(),
    run_service_url: run_service_url.to_owned(),
    plan_id: plan_id.to_owned(),
    job_id: job_msg.job_id.clone(),
    tx: ctx.tx.clone(),
  };
  spawn_renewal(params, cancel.clone())
}

struct FwdConfig {
  results_url: Option<String>,
  results_client: reqwest::Client,
  results_token: String,
  run_backend_id: String,
  job_backend_id: String,
  setup_lines: Vec<String>,
  live_log_tx: Option<tokio::sync::mpsc::Sender<LiveLogLine>>,
}

fn spawn_event_forwarder(
  mut events_rx: mpsc::Receiver<RunnerEvent>,
  fwd_collector: StepCollector,
  fwd_tx: mpsc::Sender<ListenerEvent>,
  cfg: FwdConfig,
  conclusion_tx: oneshot::Sender<Conclusion>,
) -> tokio::task::JoinHandle<()> {
  tokio::spawn(async move {
    let mut change_order = 1_u64;
    let mut step_meta: StepMetaMap = StepMetaMap::new();
    let mut uploaders: HashMap<String, mpsc::Sender<String>> = HashMap::new();
    let mut upload_tasks: tokio::task::JoinSet<Option<(String, String, u64)>> =
      tokio::task::JoinSet::new();
    // Accumulate all log lines for combined job-level log upload.
    // Seed with setup step lines so job log includes "Set up job" output.
    let mut all_job_lines: Vec<String> = cfg.setup_lines;
    let mut conclusion: Option<Conclusion> = None;

    while let Some(event) = events_rx.recv().await {
      if let RunnerEvent::JobCompleted { conclusion: c, .. } = &event {
        conclusion = Some(*c);
      }
      fwd_collector.record(&event).await;

      match &event {
        RunnerEvent::StepStarted {
          step_id, step_name, ..
        } => {
          if let Some(ref url) = cfg.results_url {
            let (tx, handle) = super::log_uploader::spawn(StreamerConfig {
              client: cfg.results_client.clone(),
              results_url: url.clone(),
              token: cfg.results_token.clone(),
              run_backend_id: cfg.run_backend_id.clone(),
              job_backend_id: cfg.job_backend_id.clone(),
              step_backend_id: step_id.clone(),
              step_name: step_name.clone(),
            });
            uploaders.insert(step_id.clone(), tx);
            let sid = step_id.clone();
            upload_tasks.spawn(async move {
              handle
                .await
                .ok()
                .flatten()
                .map(|(url, count)| (sid, url, count))
            });
          }
        },
        RunnerEvent::Log { step_id, line, .. } => {
          all_job_lines.push(line.clone());
          if let Some(tx) = uploaders.get(step_id) {
            let _ = tx.send(line.clone()).await;
          }
          // Send to live log WebSocket for real-time UI updates
          if let Some(ref live_tx) = cfg.live_log_tx {
            let _ = live_tx
              .send(LiveLogLine {
                step_id: step_id.clone(),
                line: line.clone(),
              })
              .await;
          }
        },
        RunnerEvent::StepCompleted { step_id, .. } => {
          uploaders.remove(step_id);
        },
        RunnerEvent::JobStarted { .. }
        | RunnerEvent::JobCompleted { .. }
        | RunnerEvent::StepSkipped { .. }
        | RunnerEvent::LogGroup { .. }
        | RunnerEvent::Annotation { .. } => {},
      }

      if let Some(ref url) = cfg.results_url {
        let rctx = ResultsCtx {
          client: &cfg.results_client,
          results_url: url,
          token: &cfg.results_token,
          run_backend_id: &cfg.run_backend_id,
          job_backend_id: &cfg.job_backend_id,
        };
        report_step_to_results(&rctx, &event, &mut change_order, &mut step_meta).await;
      }

      if fwd_tx.send(ListenerEvent::Runner(event)).await.is_err() {
        break;
      }
    }

    // Drain step-level uploads and backfill log URLs.
    while let Some(Ok(Some((step_id, log_url, line_count)))) = upload_tasks.join_next().await {
      fwd_collector
        .set_log_url(&step_id, log_url, line_count)
        .await;
    }

    // Upload combined job-level log blob.
    if let Some(ref url) = cfg.results_url {
      let rctx = ResultsCtx {
        client: &cfg.results_client,
        results_url: url,
        token: &cfg.results_token,
        run_backend_id: &cfg.run_backend_id,
        job_backend_id: &cfg.job_backend_id,
      };
      if let Some(count) = super::log_uploader::upload_job_logs(&rctx, &all_job_lines).await {
        tracing::info!(line_count = count, "job log uploaded");
      }
    }

    let final_conclusion = conclusion.unwrap_or_else(|| {
      tracing::error!("forwarder drained engine without seeing JobCompleted");
      Conclusion::Failure
    });
    let _ = conclusion_tx.send(final_conclusion);
  })
}

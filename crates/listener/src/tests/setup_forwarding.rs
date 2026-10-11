//! Production Runner and listener replay of the sanitized #68 acquisition.
//! Remote actions are removed and the captured script is replaced with a shell probe.

use super::*;
use std::error::Error;

fn captured() -> Result<AgentJobRequestMessage, Box<dyn Error>> {
  let mut raw: serde_json::Value = serde_json::from_str(include_str!(
    "../../../execution/tests/incoming_contexts_matrix_0.json"
  ))?;
  let steps = raw
    .get_mut("steps")
    .and_then(serde_json::Value::as_array_mut)
    .ok_or("steps")?;
  let mut step = steps
    .iter()
    .find(|step| {
      step
        .get("reference")
        .and_then(|reference| reference.get("type"))
        .and_then(serde_json::Value::as_str)
        == Some("script")
    })
    .ok_or("script")?
    .clone();
  *step.get_mut("inputs").ok_or("inputs")? = serde_json::json!({"type":2,"map":[{"Key":{"type":0,"lit":"script"},"Value":{"type":0,"lit":"printf 'setup-shell-probe\\n'"}}]});
  *step.get_mut("condition").ok_or("condition")? = serde_json::json!("success()");
  *steps = vec![step];
  let mut msg: AgentJobRequestMessage = serde_json::from_value(raw)?;
  msg.variables.remove("system.github.results_endpoint");
  for endpoint in &mut msg.resources.endpoints {
    endpoint.data.remove("FeedStreamUrl");
  }
  msg.defaults.clear();
  msg.job_outputs = None;
  Ok(msg)
}

async fn replay(
  fail_workspace: bool,
  cancelled: bool,
  hook: Option<&str>,
) -> Result<
  (
    Vec<wire::reporting::StepResult>,
    Vec<LiveLogLine>,
    Vec<ListenerEvent>,
  ),
  Box<dyn Error>,
> {
  let mut msg = captured()?;
  // The existing secret fixture is placed in an observable metadata field to
  // check registration BEFORE the engine begins, including setup failure.
  let secret = msg
    .variables
    .get("system.github.token")
    .ok_or("token")?
    .value
    .clone();
  msg
    .variables
    .get_mut("system.runnerGroupName")
    .ok_or("group")?
    .value
    .clone_from(&secret);
  let temp = tempfile::tempdir()?;
  let config = shared::RunnerConfig {
    data_dir: temp.path().join("data"),
    workspace_root: temp.path().join("work"),
    workspace_gc_hours: 0,
    ..shared::RunnerConfig::default()
  };
  if fail_workspace {
    std::fs::write(&config.workspace_root, b"real filesystem obstruction")?;
  }
  if let Some(source) = hook {
    let path = temp.path().join("setup-hook.sh");
    std::fs::write(&path, source)?;
    msg.variables.insert(
      "ACTIONS_RUNNER_HOOK_JOB_STARTED".to_owned(),
      shared::VariableValue {
        value: path.to_string_lossy().into_owned(),
        is_secret: false,
      },
    );
  }
  let (journal_tx, mut journal_rx) = mpsc::channel(256);
  let ctx = SessionCtx {
    runner_name: Some(hostname::get()?.to_string_lossy().into_owned()),
    client: reqwest::Client::new(),
    token: String::new(),
    broker_url: String::new(),
    session_id: String::new(),
    config,
    masker: Arc::new(Mutex::new(SecretMasker::new())),
    cancel: CancellationToken::new(),
    tx: journal_tx,
    encryption_key: None,
    use_fips_encryption: false,
    rsa_private_key_der: Vec::new(),
    refresh_auth: None,
    live_log: None,
    job_log_upload: None,
    watchdog: crate::helpers::WatchdogConfig::default(),
  };
  let (setup, lines, _, handle) = connect_and_report_setup(&ctx, &msg, "", &msg.plan.plan_id).await;
  assert!(handle.is_none());
  let (live_tx, mut live_rx) = mpsc::channel(256);
  let mut cfg = build_fwd_config(&ctx, "", &msg.plan.plan_id, &msg, lines, Some(live_tx));
  cfg.setup_id = Some(setup);
  let cancel = CancellationToken::new();
  if cancelled {
    cancel.cancel();
  }
  let collector = StepCollector::new();
  tokio::time::timeout(
    std::time::Duration::from_secs(20),
    run_forwarded_job(&ctx, &msg, &collector, cfg, &cancel),
  )
  .await?;
  drop(ctx);
  let mut live = Vec::new();
  while let Some(line) = live_rx.recv().await {
    assert!(!line.line.contains(&secret));
    live.push(line);
  }
  let mut journal = Vec::new();
  while let Some(event) = journal_rx.recv().await {
    journal.push(event);
  }
  Ok((collector.collected_results().await, live, journal))
}

#[tokio::test]
async fn setup_production_replay_streams_metadata_and_completes_before_shell()
-> Result<(), Box<dyn Error>> {
  let (results, live, events) = replay(false, false, None).await?;
  let setup = results
    .iter()
    .find(|step| step.number == 1)
    .ok_or("setup result")?;
  assert_eq!(setup.name, "Set up job");
  assert_eq!(
    setup.conclusion,
    wire::reporting::run_service::JobConclusion::Succeeded
  );
  let lines: Vec<_> = live
    .iter()
    .filter(|line| line.step_id == setup.external_id)
    .map(|line| line.line.as_str())
    .collect();
  assert!(lines.contains(&"Runner group name: '***'"));
  assert!(lines.contains(&"##[group]GITHUB_TOKEN Permissions"));
  assert!(lines.contains(&"Contents: read"));
  assert!(lines.contains(&"Secret source: Actions"));
  let name = format!("Runner name: '{}'", hostname::get()?.to_string_lossy());
  assert!(lines.contains(&name.as_str()));
  assert!(live.iter().any(|line| line.line == "setup-shell-probe"));
  let complete = events.iter().position(|event| matches!(event, ListenerEvent::Runner(RunnerEvent::StepCompleted { step_id, .. }) if step_id == &setup.external_id)).ok_or("setup completion")?;
  let shell = events.iter().position(|event| matches!(event, ListenerEvent::Runner(RunnerEvent::StepStarted { step_number, .. }) if *step_number > 1)).ok_or("shell start")?;
  assert!(complete < shell);
  Ok(())
}

#[tokio::test]
async fn setup_production_failure_preserves_partial_masked_live_log() -> Result<(), Box<dyn Error>>
{
  let (results, live, _) = replay(true, false, None).await?;
  let setup = results
    .iter()
    .find(|step| step.number == 1)
    .ok_or("setup result")?;
  assert_eq!(
    setup.conclusion,
    wire::reporting::run_service::JobConclusion::Failed
  );
  assert!(
    live
      .iter()
      .any(|line| line.step_id == setup.external_id && line.line.starts_with("##[error]"))
  );
  assert!(
    live
      .iter()
      .any(|line| line.line == "Secret source: Actions")
  );
  Ok(())
}

#[tokio::test]
async fn setup_production_cancellation_preserves_metadata() -> Result<(), Box<dyn Error>> {
  let (results, live, _) = replay(false, true, None).await?;
  let setup = results
    .iter()
    .find(|step| step.number == 1)
    .ok_or("setup result")?;
  assert_eq!(
    setup.conclusion,
    wire::reporting::run_service::JobConclusion::Canceled
  );
  assert!(
    live
      .iter()
      .any(|line| line.line == "Secret source: Actions")
  );
  assert!(!live.iter().any(|line| line.line == "setup-shell-probe"));
  Ok(())
}

#[tokio::test]
async fn setup_deferred_action_log_retains_upload_and_live_identity() -> Result<(), Box<dyn Error>>
{
  let msg = captured()?;
  let (id, lines) =
    crate::setup_step::report_setup_step("", &msg.plan.plan_id, &msg, &reqwest::Client::new())
      .await;
  let (live_tx, mut live_rx) = mpsc::channel(64);
  let cfg = FwdConfig {
    setup_id: Some(id.clone()),
    setup_cancel: CancellationToken::new(),
    setup_lines: Vec::new(),
    results_url: None,
    results_client: reqwest::Client::new(),
    results_token: String::new(),
    run_backend_id: String::new(),
    job_backend_id: String::new(),
    live_log_tx: Some(live_tx),
    masker: Arc::new(Mutex::new(SecretMasker::new())),
  };
  let mut state = ForwarderState::new(Vec::new(), &cfg);
  let collector = StepCollector::new();
  let (journal_tx, _journal_rx) = mpsc::channel(64);
  setup_forwarding::start(&mut state, &cfg, &collector, &journal_tx, lines).await;
  let completed = crate::step_report_queue::build_step_entry(
    &RunnerEvent::StepCompleted {
      step_id: id.clone(),
      conclusion: Conclusion::Success,
      outputs: HashMap::new(),
    },
    &mut state.step_meta,
  )
  .ok_or("setup completion entry")?;
  let payload = serde_json::to_value(completed)?;
  assert_eq!(payload.get("number"), Some(&serde_json::json!(1)));
  assert_eq!(payload.get("name"), Some(&serde_json::json!("Set up job")));
  assert_eq!(payload.get("external_id"), Some(&serde_json::json!(id)));
  assert!(
    payload
      .get("started_at")
      .is_some_and(serde_json::Value::is_string)
  );
  // A real upload channel is the streamer's interface; no backend response is mocked.
  let (upload_tx, mut upload_rx) = mpsc::channel(4);
  state.uploaders.insert(id.clone(), upload_tx);
  setup_forwarding::finish(
    &mut state,
    &cfg,
    &collector,
    &journal_tx,
    Conclusion::Success,
  )
  .await;
  let revision: serde_json::Value = serde_json::from_str(include_str!(
    "../../../execution/tests/setup_action_commit.json"
  ))?;
  let sha = revision
    .get("sha")
    .and_then(serde_json::Value::as_str)
    .ok_or("captured SHA")?;
  let line = format!("Prepare action 'actions/hello-world-docker-action@main' (SHA:{sha})");
  let mut event = RunnerEvent::Log {
    step_id: shared::SETUP_STEP_ID.to_owned(),
    line: line.clone(),
    stream: shared::LogStream::Stdout,
  };
  setup_forwarding::before_event(&mut state, &cfg, &collector, &journal_tx, &mut event).await;
  handle_event_arm(&mut state, &cfg, &event).await;
  assert_eq!(upload_rx.try_recv()?, line);
  assert_eq!(state.all_job_lines.last(), Some(&line));
  let mut last = None;
  while let Ok(item) = live_rx.try_recv() {
    last = Some(item);
  }
  let last = last.ok_or("live line")?;
  assert_eq!(last.step_id, id);
  assert_eq!(last.line, line);
  assert_eq!(collector.collected_results().await.len(), 1);
  Ok(())
}

#[tokio::test]
async fn setup_failed_real_hook_keeps_named_preparation_output() -> Result<(), Box<dyn Error>> {
  let (results, live, _) =
    replay(false, false, Some("printf 'setup-hook-probe\\n'\nexit 1\n")).await?;
  let setup = results
    .iter()
    .find(|result| result.number == 1)
    .ok_or("setup")?;
  assert_eq!(
    setup.conclusion,
    wire::reporting::run_service::JobConclusion::Failed
  );
  assert!(
    live
      .iter()
      .any(|line| line.step_id == setup.external_id && line.line == "setup-hook-probe")
  );
  assert!(!live.iter().any(|line| line.line == "setup-shell-probe"));
  Ok(())
}

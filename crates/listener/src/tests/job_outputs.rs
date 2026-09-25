//! The event forwarder carries outputs from a real acquired engine run.

use super::*;
use std::error::Error;

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;
const CAPTURE: &str = include_str!("../../../execution/tests/job_outputs_message.json");

#[tokio::test]
async fn forwarder_keeps_real_job_outputs_for_completion() -> TestResult {
  let message: AgentJobRequestMessage = serde_json::from_str(CAPTURE)?;
  let dir = tempfile::tempdir()?;
  let config = shared::RunnerConfig {
    data_dir: dir.path().join("data"),
    workspace_root: dir.path().join("work"),
    ..shared::RunnerConfig::default()
  };
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let runner = Runner::new(config, Arc::clone(&masker));
  let engine_rx = runner.execute_job(message, CancellationToken::new());
  let cfg = FwdConfig {
    results_url: None,
    results_client: reqwest::Client::new(),
    results_token: String::new(),
    run_backend_id: String::new(),
    job_backend_id: String::new(),
    setup_lines: Vec::new(),
    live_log_tx: None,
    masker,
  };
  let (fwd_tx, mut fwd_rx) = mpsc::channel(128);
  let (outcome_tx, outcome_rx) = oneshot::channel();
  let handle = spawn_event_forwarder(engine_rx, StepCollector::new(), fwd_tx, cfg, outcome_tx);
  let outcome = tokio::time::timeout(std::time::Duration::from_secs(30), outcome_rx).await??;
  handle.await?;
  assert_eq!(outcome.conclusion, Conclusion::Success);
  assert_eq!(
    outcome.outputs.get("value").map(String::as_str),
    Some("hello-output")
  );
  let mut completed = None;
  while let Some(event) = fwd_rx.recv().await {
    if let ListenerEvent::Runner(RunnerEvent::JobCompleted { outputs, .. }) = event {
      completed = Some(outputs);
    }
  }
  assert_eq!(completed, Some(outcome.outputs));
  Ok(())
}

//! #251: the pre-job phase and post dispatch keep the no-host-fallback rule.
//! When the job's declared container never started, a repository action's
//! `pre` row and its post row fail before any dispatch, and nothing of the
//! action runs on the host (its pre/main/post would append to the marker).

use std::path::Path;
use std::sync::{Arc, Mutex};

use shared::{ActionStep, Conclusion, RunnerConfig, RunnerEvent, SecretMasker};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::execution::actions::prefetch::ActionFetcher;
use crate::execution::context::ExecutionContext;
use crate::execution::job_spec::JobSpec;
use crate::execution::steps_runner::{JobRun, run_steps};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const FIXTURE: &str = concat!(
  env!("CARGO_MANIFEST_DIR"),
  "/../toolu-runner/tests/fixtures/prepost_action"
);

#[tokio::test]
async fn a_missing_job_container_runs_no_pre_or_post_on_the_host() -> TestResult {
  let dir = tempfile::tempdir()?;
  let workspace = dir.path().join("work");
  let action = workspace.join("act-a");
  std::fs::create_dir_all(&action)?;
  for name in ["action.yml", "pre.js", "main.js", "post.js"] {
    std::fs::copy(Path::new(FIXTURE).join(name), action.join(name))?;
  }
  let marker = dir.path().join("markers.txt");
  std::fs::write(&marker, "")?;
  let config = RunnerConfig {
    data_dir: dir.path().join("data"),
    workspace_root: workspace.clone(),
    ..RunnerConfig::default()
  };
  let mut ctx = ExecutionContext::with_masker(Arc::new(Mutex::new(SecretMasker::new())));
  ctx.set_env("MARKER_FILE", &marker.to_string_lossy());
  ctx.job_container_missing = true;
  let mut step = ActionStep::with_ref_type("a", "repository");
  step.reference.name = Some("./act-a".to_owned());

  let (tx, mut rx) = mpsc::channel::<RunnerEvent>(1024);
  let collector = tokio::spawn(async move {
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
      events.push(event);
    }
    events
  });
  let spec = JobSpec::default();
  let http = reqwest::Client::new();
  let fetcher = ActionFetcher::new(&shared::NetworkConfig::default())?;
  let run = JobRun {
    workspace: &workspace,
    config: &config,
    spec: &spec,
    shadow: None,
    http: &http,
    fetcher: &fetcher,
  };
  let conclusion = run_steps(&[step], &mut ctx, &tx, CancellationToken::new(), &run).await?;
  drop(tx);
  let events = collector.await?;

  assert_eq!(
    std::fs::read_to_string(&marker)?,
    "",
    "a stage ran on the host"
  );
  assert_eq!(conclusion, Conclusion::Failure);
  let rows = |prefix: &str| -> Vec<Conclusion> {
    let mut ids = Vec::new();
    let mut found = Vec::new();
    for event in &events {
      if let RunnerEvent::StepStarted {
        step_id, step_name, ..
      } = event
        && step_name.starts_with(prefix)
      {
        ids.push(step_id.clone());
      } else if let RunnerEvent::StepCompleted {
        step_id,
        conclusion,
        ..
      } = event
        && ids.contains(step_id)
      {
        found.push(*conclusion);
      }
    }
    found
  };
  assert_eq!(rows("Pre "), [Conclusion::Failure], "{events:?}");
  assert_eq!(rows("Post "), [Conclusion::Failure], "{events:?}");
  Ok(())
}

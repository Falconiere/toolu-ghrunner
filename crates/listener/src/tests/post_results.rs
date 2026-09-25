//! Captured job replay through the collector and complete-job wire payload.

use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use execution::Runner;
use execution::node::runtime::{node_binary_path, node_cache_dir, node_version_for};
use shared::{AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerEvent, SecretMasker};
use tokio_util::sync::CancellationToken;
use wire::reporting::run_service::CompleteJobRequest;

use crate::helpers::map_conclusion;
use crate::step_reporter::StepCollector;

#[tokio::test]
async fn post_results_reach_distinct_completion_records() -> Result<(), Box<dyn Error>> {
  let dir = tempfile::tempdir()?;
  let config = RunnerConfig {
    data_dir: dir.path().join("data"),
    workspace_root: dir.path().join("work"),
    workspace_gc_hours: 0,
    ..RunnerConfig::default()
  };
  let mut msg: AgentJobRequestMessage = serde_json::from_str(include_str!(
    "../../../execution/tests/incoming_contexts_matrix_0.json"
  ))?;
  msg
    .steps
    .retain(|step| step.reference.repository_type.as_deref() == Some("self"));
  msg.steps.truncate(1);
  let step = msg
    .steps
    .first_mut()
    .ok_or("captured local action missing")?;
  step.reference.path = Some("./.github/actions/post-probe".to_owned());
  let main_id = step.id.clone();
  let action = config
    .workspace_root
    .join(&msg.job_id)
    .join(".github/actions/post-probe");
  std::fs::create_dir_all(&action)?;
  std::fs::write(
    action.join("action.yml"),
    include_str!("../../../execution/tests/post_results_action.yml"),
  )?;
  std::fs::write(
    action.join("main.js"),
    include_str!("../../../execution/tests/post_results_main.js"),
  )?;
  std::fs::write(
    action.join("post.js"),
    include_str!("../../../execution/tests/post_results_post.js"),
  )?;
  let node = std::process::Command::new("node")
    .args(["-e", "process.stdout.write(process.execPath)"])
    .output()?;
  assert!(node.status.success());
  let binary = node_binary_path(&node_cache_dir(&config.data_dir, node_version_for(20)));
  std::fs::create_dir_all(binary.parent().ok_or("node cache parent missing")?)?;
  #[cfg(unix)]
  std::os::unix::fs::symlink(String::from_utf8(node.stdout)?.trim(), &binary)?;
  #[cfg(not(unix))]
  std::fs::copy(String::from_utf8(node.stdout)?.trim(), &binary)?;

  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let collector = StepCollector::new();
  let cancellation = CancellationToken::new();
  let mut receiver = runner.execute_job(msg.clone(), cancellation.clone());
  let completion = tokio::time::timeout(Duration::from_secs(30), async {
    let mut conclusion = None;
    let mut step_completions = Vec::new();
    while let Some(event) = receiver.recv().await {
      collector.record(&event).await;
      if let RunnerEvent::StepCompleted {
        step_id,
        conclusion: result,
        ..
      } = &event
      {
        step_completions.push((step_id.clone(), *result));
      }
      if let RunnerEvent::JobCompleted {
        conclusion: result, ..
      } = event
      {
        conclusion = Some(result);
      }
    }
    (conclusion, step_completions)
  })
  .await;
  let (conclusion, step_completions) = match completion {
    Ok(conclusion) => conclusion,
    Err(error) => {
      cancellation.cancel();
      return Err(error.into());
    },
  };
  let conclusion = conclusion.ok_or("missing JobCompleted event")?;
  assert_eq!(step_completions.len(), 2);
  let (reported_main_id, main_conclusion) =
    step_completions.first().ok_or("main completion missing")?;
  let (reported_post_id, post_conclusion) =
    step_completions.get(1).ok_or("post completion missing")?;
  assert_eq!(reported_main_id, &main_id);
  assert_eq!(*main_conclusion, Conclusion::Success);
  assert_ne!(reported_post_id, &main_id);
  assert_eq!(*post_conclusion, Conclusion::Failure);
  assert_eq!(conclusion, Conclusion::Failure);
  let request = CompleteJobRequest {
    plan_id: msg.plan.plan_id,
    job_id: msg.job_id,
    request_id: msg.request_id,
    conclusion: map_conclusion(conclusion),
    outputs: serde_json::json!({}),
    step_results: collector.collected_results().await,
    annotations: Vec::new(),
  };
  let json = serde_json::to_value(&request)?;
  let steps = json
    .get("stepResults")
    .and_then(serde_json::Value::as_array)
    .ok_or("missing stepResults")?;
  assert_eq!(
    json.get("conclusion").and_then(serde_json::Value::as_u64),
    Some(3)
  );
  assert_eq!(steps.len(), 2);
  let main = steps.first().ok_or("main step missing")?;
  let post = steps.get(1).ok_or("post step missing")?;
  assert_eq!(
    main.get("externalId").and_then(serde_json::Value::as_str),
    Some(main_id.as_str())
  );
  assert_ne!(
    post.get("externalId").and_then(serde_json::Value::as_str),
    Some(main_id.as_str())
  );
  assert_eq!(
    main.get("conclusion").and_then(serde_json::Value::as_u64),
    Some(2)
  );
  assert_eq!(
    post.get("conclusion").and_then(serde_json::Value::as_u64),
    Some(3)
  );
  assert_eq!(
    post.get("number").and_then(serde_json::Value::as_u64),
    Some(3)
  );
  assert_eq!(
    post.get("name").and_then(serde_json::Value::as_str),
    Some("Post Workflow input passed to action")
  );
  Ok(())
}

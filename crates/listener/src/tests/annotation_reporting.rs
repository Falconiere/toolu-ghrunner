//! Captured acquired job and real shell/composite output through completion POST.

use std::collections::HashMap;
use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::Method;
use execution::Runner;
use shared::{AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerEvent, SecretMasker};
use tokio_util::sync::CancellationToken;
use wire::reporting::run_service::{CompleteJobRequest, complete_job};

use crate::helpers::map_conclusion;
use crate::step_reporter::StepCollector;
use crate::support::RecordingServer;

type TestResult<T> = Result<T, Box<dyn Error>>;

fn captured_probe_job() -> TestResult<AgentJobRequestMessage> {
  let mut raw: serde_json::Value = serde_json::from_str(include_str!(
    "../../../execution/tests/incoming_contexts_matrix_0.json"
  ))?;
  let steps = raw
    .get_mut("steps")
    .and_then(serde_json::Value::as_array_mut)
    .ok_or("captured steps missing")?;
  let mut script = steps.get(1).ok_or("captured script missing")?.clone();
  let mut composite = steps.get(2).ok_or("captured second step missing")?.clone();
  let probe = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("../execution/tests/command_annotation_probe.sh");
  let script_token = script
    .get_mut("inputs")
    .and_then(|v| v.get_mut("map"))
    .and_then(serde_json::Value::as_array_mut)
    .and_then(|items| items.first_mut())
    .and_then(|item| item.get_mut("Value"))
    .ok_or("captured script token missing")?;
  script_token["lit"] = serde_json::Value::String(format!("bash '{}'", probe.display()));
  let composite_map = composite
    .as_object_mut()
    .ok_or("captured composite object missing")?;
  composite_map.insert(
    "reference".to_owned(),
    serde_json::json!({
      "type": "repository",
      "repositoryType": "self",
      "path": "./.github/actions/annotation-82-probe"
    }),
  );
  composite_map.remove("inputs");
  *steps = vec![script, composite];
  Ok(serde_json::from_value(raw)?)
}

fn run_config(root: &std::path::Path) -> RunnerConfig {
  RunnerConfig {
    data_dir: root.join("data"),
    workspace_root: root.join("work"),
    workspace_gc_hours: 0,
    ..RunnerConfig::default()
  }
}

async fn posted_completion() -> TestResult<(serde_json::Value, HashMap<String, u32>)> {
  let dir = tempfile::tempdir()?;
  let config = run_config(dir.path());
  let msg = captured_probe_job()?;
  let action = config
    .workspace_root
    .join(&msg.job_id)
    .join(".github/actions/annotation-82-probe");
  std::fs::create_dir_all(&action)?;
  std::fs::write(
    action.join("action.yml"),
    include_str!("../../tests/annotation-82-action.yml"),
  )?;
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let collector = StepCollector::new();
  let cancellation = CancellationToken::new();
  let mut rx = runner.execute_job(msg.clone(), cancellation.clone());
  let (conclusion, numbers) = tokio::time::timeout(Duration::from_secs(45), async {
    let mut conclusion = None;
    let mut numbers = HashMap::new();
    while let Some(event) = rx.recv().await {
      collector.record(&event).await;
      if let RunnerEvent::StepStarted {
        step_id,
        step_number,
        ..
      } = &event
      {
        numbers.insert(step_id.clone(), *step_number);
      }
      if let RunnerEvent::JobCompleted {
        conclusion: result, ..
      } = event
      {
        conclusion = Some(result);
      }
    }
    (conclusion, numbers)
  })
  .await?;
  cancellation.cancel();
  let conclusion = conclusion.ok_or("captured job did not complete")?;
  assert_eq!(conclusion, Conclusion::Success);
  let request = CompleteJobRequest {
    plan_id: msg.plan.plan_id,
    job_id: msg.job_id,
    request_id: msg.request_id,
    conclusion: map_conclusion(conclusion),
    outputs: serde_json::json!({}),
    step_results: collector.collected_results().await,
    annotations: Vec::new(),
  };
  let server = RecordingServer::start(|_| HashMap::new()).await;
  complete_job(
    &reqwest::Client::new(),
    &server.base_url(),
    "disposable-token",
    &request,
  )
  .await?;
  let requests = server.requests_for("/completejob");
  let [posted] = requests.as_slice() else {
    return Err(format!("expected one completejob POST, got {}", requests.len()).into());
  };
  assert_eq!(posted.method, Method::POST);
  assert!(
    !posted
      .body
      .windows(b"probe-mask-82".len())
      .any(|window| window == b"probe-mask-82")
  );
  Ok((serde_json::from_slice(&posted.body)?, numbers))
}

fn annotation_for<'a>(
  body: &'a serde_json::Value,
  step_id: &str,
) -> TestResult<&'a [serde_json::Value]> {
  let steps = body
    .get("stepResults")
    .and_then(serde_json::Value::as_array)
    .ok_or("stepResults missing")?;
  let result = steps
    .iter()
    .find(|step| step.get("externalId").and_then(serde_json::Value::as_str) == Some(step_id))
    .ok_or("expected step result missing")?;
  Ok(
    result
      .get("annotations")
      .and_then(serde_json::Value::as_array)
      .ok_or_else(|| format!("step annotations missing for {step_id}; result: {result}"))?
      .as_slice(),
  )
}

#[tokio::test]
async fn annotation_reporting_posts_upstream_shape_on_correct_steps() -> TestResult<()> {
  let msg = captured_probe_job()?;
  let script_id = msg.steps.first().ok_or("script id missing")?.id.as_str();
  let composite_id = msg.steps.get(1).ok_or("composite id missing")?.id.as_str();
  let (body, numbers) = posted_completion().await?;
  let script = annotation_for(&body, script_id)?;
  assert_eq!(script.len(), 10);
  let full = script.first().ok_or("first annotation missing")?;
  assert_eq!(full.get("level"), Some(&serde_json::json!(3)));
  assert_eq!(full.get("message"), Some(&serde_json::json!("boom\n***")));
  assert_eq!(
    full.get("title"),
    Some(&serde_json::json!("Compile: detail"))
  );
  assert_eq!(full.get("path"), Some(&serde_json::json!("src,sample.rs")));
  assert_eq!(full.get("startLine"), Some(&serde_json::json!(4)));
  assert_eq!(full.get("endLine"), Some(&serde_json::json!(4)));
  assert_eq!(full.get("startColumn"), Some(&serde_json::json!(2)));
  assert_eq!(full.get("endColumn"), Some(&serde_json::json!(8)));
  assert_eq!(
    full.get("stepNumber"),
    numbers
      .get(script_id)
      .map(|n| serde_json::json!(n))
      .as_ref()
  );
  for old in ["annotationType", "file", "line", "col"] {
    assert!(
      full.get(old).is_none(),
      "legacy key {old} reached Run Service"
    );
  }
  assert_eq!(
    script.get(1).and_then(|a| a.get("level")),
    Some(&serde_json::json!(2))
  );
  assert_eq!(
    script.get(2).and_then(|a| a.get("level")),
    Some(&serde_json::json!(1))
  );
  let notice = script.get(2).ok_or("notice annotation missing")?;
  assert!(notice.get("path").is_none());
  assert!(notice.get("startLine").is_none());
  assert!(notice.get("endLine").is_none());
  let end_only = script.get(3).ok_or("end-only annotation missing")?;
  assert_eq!(end_only.get("startLine"), Some(&serde_json::json!(7)));
  assert_eq!(end_only.get("endLine"), Some(&serde_json::json!(7)));
  let descending = script.get(4).ok_or("descending annotation missing")?;
  assert!(descending.get("startLine").is_none());
  assert!(descending.get("endLine").is_none());
  let multiline = script.get(5).ok_or("multiline annotation missing")?;
  assert_eq!(multiline.get("startLine"), Some(&serde_json::json!(4)));
  assert_eq!(multiline.get("endLine"), Some(&serde_json::json!(5)));
  assert!(multiline.get("startColumn").is_none());
  assert!(multiline.get("endColumn").is_none());
  assert!(
    script
      .iter()
      .all(|annotation| annotation.get("rawDetails").is_none())
  );
  assert!(
    script
      .iter()
      .all(|annotation| annotation.get("isInfrastructureIssue").is_none())
  );
  let composite = annotation_for(&body, composite_id)?;
  assert_eq!(
    composite.len(),
    2,
    "both nested commands must attach to their parent step"
  );
  assert_eq!(
    composite.first().and_then(|a| a.get("title")),
    Some(&serde_json::json!("Nested: first"))
  );
  assert_eq!(
    composite.get(1).and_then(|a| a.get("message")),
    Some(&serde_json::json!("composite two ***"))
  );
  assert!(composite.iter().all(|a| {
    a.get("stepNumber")
      == numbers
        .get(composite_id)
        .map(|n| serde_json::json!(n))
        .as_ref()
  }));
  assert_eq!(body.get("annotations"), Some(&serde_json::json!([])));
  Ok(())
}

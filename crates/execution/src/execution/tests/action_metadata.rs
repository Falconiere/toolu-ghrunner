//! Captured #68 job replay with real shell/Node probes; no action-service mocks.
//! Preserve acquired wire IDs/names/token types, replace scripts/local paths and
//! drop checkout (live remote resolution needs unavailable job credentials).

use std::error::Error;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use shared::{AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerEvent, SecretMasker};
use tokio_util::sync::CancellationToken;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;
const CAPTURE: &str = include_str!("../../../tests/incoming_contexts_matrix_0.json");
const PROBES: &str = concat!(
  env!("CARGO_MANIFEST_DIR"),
  "/../../.github/actions/action-metadata-probe"
);
const SCRIPT: &str = r#"
printf 'run|%s|%s|%s|%s\n' "$GITHUB_ACTION" "$GITHUB_ACTION_REPOSITORY" "$GITHUB_ACTION_REF" "$RUNNER_ENVIRONMENT" >> metadata.log
test '${{ github.action }}' = "$GITHUB_ACTION"
test '${{ github.action_repository }}' = "$GITHUB_ACTION_REPOSITORY"
test '${{ github.action_ref }}' = "$GITHUB_ACTION_REF"
test '${{ runner.environment }}' = "$RUNNER_ENVIRONMENT"
"#;

fn job() -> TestResult<AgentJobRequestMessage> {
  let mut value: serde_json::Value = serde_json::from_str(CAPTURE)?;
  let steps = value
    .get_mut("steps")
    .and_then(serde_json::Value::as_array_mut)
    .ok_or("steps absent")?;
  steps.retain(|step| {
    step.get("contextName").and_then(serde_json::Value::as_str) != Some("__actions_checkout")
  });
  for step in steps {
    if step
      .get("reference")
      .and_then(|reference| reference.get("type"))
      .and_then(serde_json::Value::as_str)
      == Some("script")
    {
      step.as_object_mut().ok_or("step absent")?.insert(
        "inputs".to_owned(),
        serde_json::json!({"type":2,"map":[
          {"key":{"type":0,"lit":"script"},"value":{"type":0,"lit":SCRIPT}}
        ]}),
      );
    } else {
      step
        .get_mut("reference")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or("reference absent")?
        .insert(
          "path".to_owned(),
          "./.github/actions/action-metadata-probe/parent".into(),
        );
    }
    step
      .as_object_mut()
      .ok_or("step absent")?
      .insert("condition".to_owned(), "always()".into());
  }
  Ok(serde_json::from_value(value)?)
}

fn copy_probes(workspace: &Path) -> TestResult {
  for name in ["parent", "child", "node"] {
    let dest = workspace
      .join(".github/actions/action-metadata-probe")
      .join(name);
    std::fs::create_dir_all(&dest)?;
    for entry in std::fs::read_dir(Path::new(PROBES).join(name))? {
      let entry = entry?;
      std::fs::copy(entry.path(), dest.join(entry.file_name()))?;
    }
  }
  Ok(())
}

async fn replay(msg: AgentJobRequestMessage) -> TestResult<(String, Vec<RunnerEvent>)> {
  let temp = tempfile::tempdir()?;
  let config = RunnerConfig {
    data_dir: temp.path().join("data"),
    workspace_root: temp.path().join("work"),
    workspace_gc_hours: 0,
    ..RunnerConfig::default()
  };
  let workspace = config.workspace_root.join(&msg.job_id);
  copy_probes(&workspace)?;
  let runner = crate::Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let mut receiver = runner.execute_job(msg, CancellationToken::new());
  let events = tokio::time::timeout(Duration::from_secs(120), async {
    let mut events = Vec::new();
    while let Some(event) = receiver.recv().await {
      events.push(event);
    }
    events
  })
  .await?;
  assert!(
    events.iter().any(|event| matches!(
      event,
      RunnerEvent::JobCompleted {
        conclusion: Conclusion::Success,
        ..
      }
    )),
    "{events:?}"
  );
  Ok((
    std::fs::read_to_string(workspace.join("metadata.log"))?,
    events,
  ))
}

#[tokio::test]
async fn action_metadata_captured_names_nested_stages_and_restoration() -> TestResult {
  let (actual, _) = replay(job()?).await?;
  let mut expected = vec![
    "run|__run|||self-hosted".to_owned(),
    "run|__run_2|||self-hosted".to_owned(),
  ];
  for name in ["__self", "__self_2"] {
    for _ in 0..2 {
      for stage in ["pre", "main", "child"] {
        expected.push(format!("{stage}|{name}|||self-hosted"));
      }
    }
    expected.push(format!("parent|{name}|||self-hosted"));
  }
  expected.push("run|__run_3|||self-hosted".to_owned());
  for name in ["__self_2", "__self"] {
    for _ in 0..2 {
      expected.push(format!("post|{name}|||self-hosted"));
    }
  }
  assert_eq!(actual.lines().collect::<Vec<_>>(), expected);
  Ok(())
}

#[tokio::test]
async fn action_metadata_wire_name_named_skipped_and_legacy_absence() -> TestResult {
  let mut msg = job()?;
  msg.steps.retain(shared::ActionStep::is_run_step);
  let first = msg.steps.first_mut().ok_or("first step absent")?;
  first.name = Some("explicit_name".to_owned());
  first.context_name = Some("output_scope".to_owned());
  let second = msg.steps.get_mut(1).ok_or("second step absent")?;
  second.name = Some("skipped_name".to_owned());
  second.condition = Some("false".to_owned());
  let last = msg.steps.last_mut().ok_or("last step absent")?;
  last.name = None;
  last.context_name = None;
  let (actual, events) = replay(msg).await?;
  assert_eq!(
    actual,
    "run|explicit_name|||self-hosted\nrun||||self-hosted\n"
  );
  assert!(
    events
      .iter()
      .any(|event| matches!(event, RunnerEvent::StepSkipped { .. }))
  );
  Ok(())
}

#[tokio::test]
async fn action_metadata_empty_wire_name_uses_legacy_context_name() -> TestResult {
  let mut msg = job()?;
  msg.steps.retain(shared::ActionStep::is_run_step);
  msg.steps.truncate(1);
  let step = msg.steps.first_mut().ok_or("step absent")?;
  step.name = Some(String::new());
  step.context_name = Some("legacy".to_owned());
  assert_eq!(replay(msg).await?.0, "run|legacy|||self-hosted\n");
  Ok(())
}

/// Execute real committed action probes at the resolved-action boundary using
/// the captured remote reference. This verifies metadata dispatch, not live
/// action-service resolution; no service response or HTTP client is mocked.
#[tokio::test]
async fn action_metadata_remote_dispatch_repeated_refs_and_parent_restore() -> TestResult {
  use super::{ActionEnv, ResolvedStep, dispatch_action};
  use crate::execution::action_metadata::set_name;
  use crate::execution::actions::prefetch::ActionFetcher;
  use crate::execution::depth_tracker::DepthTracker;
  use crate::execution::job_context::build_context;
  use crate::execution::step_timeout::StepBounds;

  let msg: AgentJobRequestMessage = serde_json::from_str(CAPTURE)?;
  let mut step = msg.steps.first().ok_or("remote step absent")?.clone();
  let temp = tempfile::tempdir()?;
  let config = RunnerConfig {
    data_dir: temp.path().join("data"),
    ..RunnerConfig::default()
  };
  let workspace = temp.path().join("workspace");
  copy_probes(&workspace)?;
  let mut ctx = build_context(&msg, &config, Arc::new(Mutex::new(SecretMasker::new())));
  let client = reqwest::Client::new();
  let fetcher = ActionFetcher::new();
  let bounds = StepBounds::nested(None, None, CancellationToken::new());
  let (events, _receiver) = tokio::sync::mpsc::channel(256);
  let env = ActionEnv {
    events: &events,
    workspace: &workspace,
    config: &config,
    bounds: &bounds,
    http: &client,
    fetcher: &fetcher,
    log_step_id: &step.id,
  };
  // The probe stands in for user action code after resolution, never for the
  // metadata implementation. A subpath must not enter GITHUB_ACTION_REPOSITORY.
  step.reference.path = Some("probe/subpath".to_owned());
  ctx.set_github_context("action_repository", "parent/repository");
  ctx.set_github_context("action_ref", "parent-ref");
  for (name, git_ref, action) in [
    ("__actions_checkout", "v4", "node"),
    ("__actions_checkout_2", "topic/branch", "node"),
    ("__actions_checkout_3", "v4", "parent"),
  ] {
    step.name = Some(name.to_owned());
    step.reference.git_ref = Some(git_ref.to_owned());
    set_name(&mut ctx, &step);
    let action_dir = workspace
      .join(".github/actions/action-metadata-probe")
      .join(action);
    let resolved = ResolvedStep {
      client: client.clone(),
      image_cache_key: None,
      manifest: super::read_manifest(&action_dir)?,
      action_dir,
    };
    let result =
      dispatch_action(&step, &mut ctx, &env, &resolved, &mut DepthTracker::new()).await?;
    assert_eq!(result.conclusion, Conclusion::Success);
    assert_eq!(
      ctx.github_context("action_repository"),
      Some("parent/repository")
    );
    assert_eq!(ctx.github_context("action_ref"), Some("parent-ref"));
    if action == "parent" {
      assert_eq!(
        result.outputs.get("metadata").map(String::as_str),
        Some("__actions_checkout_3|actions/checkout|v4")
      );
    }
  }
  let actual = std::fs::read_to_string(workspace.join("metadata.log"))?;
  assert_eq!(
    actual,
    concat!(
      "pre|__actions_checkout|actions/checkout|v4|self-hosted\n",
      "main|__actions_checkout|actions/checkout|v4|self-hosted\n",
      "pre|__actions_checkout_2|actions/checkout|topic/branch|self-hosted\n",
      "main|__actions_checkout_2|actions/checkout|topic/branch|self-hosted\n",
      "pre|__actions_checkout_3|||self-hosted\nmain|__actions_checkout_3|||self-hosted\n",
      "child|__actions_checkout_3|||self-hosted\n",
      "pre|__actions_checkout_3|||self-hosted\nmain|__actions_checkout_3|||self-hosted\n",
      "child|__actions_checkout_3|||self-hosted\nparent|__actions_checkout_3|||self-hosted\n"
    )
  );
  let action_dir = workspace.join(".github/actions/action-metadata-probe/node");
  let mut manifest = super::read_manifest(&action_dir)?;
  manifest.runs.pre = Some("missing-script.js".to_owned());
  let broken = ResolvedStep {
    client: client.clone(),
    image_cache_key: None,
    action_dir,
    manifest,
  };
  assert!(
    dispatch_action(&step, &mut ctx, &env, &broken, &mut DepthTracker::new())
      .await
      .is_err()
  );
  assert_eq!(
    ctx.github_context("action_repository"),
    Some("parent/repository")
  );
  assert_eq!(ctx.github_context("action_ref"), Some("parent-ref"));
  Ok(())
}

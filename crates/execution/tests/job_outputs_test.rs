//! Replay a sanitized real acquisition through the job engine and its shell step.

use std::error::Error;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use execution::Runner;
use execution::node::runtime::{node_binary_path, node_cache_dir, node_version_for};
use shared::{
  AgentJobRequestMessage, AnnotationLevel, Conclusion, RunnerConfig, RunnerEvent, SecretMasker,
};
use tokio_util::sync::CancellationToken;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;
const CAPTURE: &str = include_str!("job_outputs_message.json");

fn captured() -> TestResult<AgentJobRequestMessage> {
  Ok(serde_json::from_str(CAPTURE)?)
}

fn set_script(message: &mut AgentJobRequestMessage, script: &str) -> TestResult {
  let step = message.steps.first_mut().ok_or("captured step missing")?;
  let input = step
    .inputs
    .d
    .as_mut()
    .ok_or("captured inputs missing")?
    .iter_mut()
    .find(|entry| entry.key.to_string_value() == Some("script"))
    .ok_or("captured script missing")?;
  input.value.lit = Some(script.to_owned());
  Ok(())
}

fn add_output(message: &mut AgentJobRequestMessage, name: &str, expr: &str) -> TestResult {
  let entries = message
    .job_outputs
    .as_mut()
    .ok_or("captured jobOutputs missing")?
    .d
    .as_mut()
    .ok_or("captured output map missing")?;
  let mut entry = entries
    .first()
    .ok_or("captured output entry missing")?
    .clone();
  entry.key.lit = Some(name.to_owned());
  entry.value.expr = Some(expr.to_owned());
  entries.push(entry);
  Ok(())
}

async fn replay(message: AgentJobRequestMessage) -> TestResult<Vec<RunnerEvent>> {
  let dir = tempfile::tempdir()?;
  let config = RunnerConfig {
    data_dir: dir.path().join("data"),
    workspace_root: dir.path().join("work"),
    ..RunnerConfig::default()
  };
  run_with_config(message, config, CancellationToken::new()).await
}

async fn run_with_config(
  message: AgentJobRequestMessage,
  config: RunnerConfig,
  cancel: CancellationToken,
) -> TestResult<Vec<RunnerEvent>> {
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let mut stream = runner.execute_job(message, cancel);
  let events = tokio::time::timeout(Duration::from_secs(60), async {
    let mut events = Vec::new();
    while let Some(event) = stream.recv().await {
      events.push(event);
    }
    events
  })
  .await?;
  Ok(events)
}

async fn seed_node(data: &Path) -> TestResult {
  let output = tokio::process::Command::new("node")
    .args(["-e", "process.stdout.write(process.execPath)"])
    .output()
    .await?;
  assert!(output.status.success(), "Node is required for this test");
  let host_binary = String::from_utf8(output.stdout)?;
  let target = node_binary_path(&node_cache_dir(data, node_version_for(20)));
  std::fs::create_dir_all(target.parent().ok_or("node cache parent missing")?)?;
  #[cfg(unix)]
  std::os::unix::fs::symlink(host_binary.trim(), target)?;
  #[cfg(not(unix))]
  std::fs::copy(host_binary.trim(), target)?;
  Ok(())
}

fn completed(
  events: &[RunnerEvent],
) -> TestResult<(Conclusion, &std::collections::HashMap<String, String>)> {
  events
    .iter()
    .find_map(|event| match event {
      RunnerEvent::JobCompleted {
        conclusion,
        outputs,
        ..
      } => Some((*conclusion, outputs)),
      RunnerEvent::JobStarted { .. }
      | RunnerEvent::StepStarted { .. }
      | RunnerEvent::StepCompleted { .. }
      | RunnerEvent::StepSkipped { .. }
      | RunnerEvent::Log { .. }
      | RunnerEvent::LogGroup { .. }
      | RunnerEvent::Annotation { .. } => None,
    })
    .ok_or_else(|| "JobCompleted missing".into())
}

#[tokio::test]
async fn captured_output_is_evaluated_after_real_github_output_step() -> TestResult {
  let message = captured()?;
  let events = replay(message).await?;
  let (conclusion, outputs) = completed(&events)?;
  assert_eq!(conclusion, Conclusion::Success, "{events:?}");
  assert_eq!(outputs.len(), 1, "{events:?}");
  assert_eq!(
    outputs.get("value").map(String::as_str),
    Some("hello-output")
  );
  Ok(())
}

#[tokio::test]
async fn skipped_producer_step_omits_its_job_output() -> TestResult {
  let mut message = captured()?;
  let step = message.steps.first_mut().ok_or("captured step missing")?;
  let step_id = step.id.clone();
  step.condition = Some("false".to_owned());
  let events = replay(message).await?;
  let (conclusion, outputs) = completed(&events)?;
  assert_eq!(conclusion, Conclusion::Success, "{events:?}");
  assert!(outputs.is_empty(), "{events:?}");
  assert!(events.iter().any(|event| {
    matches!(event, RunnerEvent::StepSkipped { step_id: skipped, .. } if skipped == &step_id)
  }));
  Ok(())
}

#[tokio::test]
async fn empty_multiline_and_unicode_values_follow_github_output_file() -> TestResult {
  let mut message = captured()?;
  set_script(
    &mut message,
    "printf 'value<<EOF\\nfirst\\ncafé 🌍\\nEOF\\nempty=\\nunicode=水🌊\\n' >> \"$GITHUB_OUTPUT\"",
  )?;
  add_output(&mut message, "empty", "steps.produce.outputs.empty")?;
  add_output(&mut message, "unicode", "steps.produce.outputs.unicode")?;
  let events = replay(message).await?;
  let (conclusion, outputs) = completed(&events)?;
  assert_eq!(conclusion, Conclusion::Success, "{events:?}");
  assert_eq!(outputs.len(), 2, "{events:?}");
  assert_eq!(
    outputs.get("value").map(String::as_str),
    Some("first\ncafé 🌍")
  );
  assert_eq!(outputs.get("unicode").map(String::as_str), Some("水🌊"));
  assert!(!outputs.contains_key("empty"));
  Ok(())
}

#[tokio::test]
async fn message_secret_and_dynamic_add_mask_are_skipped_with_warnings() -> TestResult {
  let mut message = captured()?;
  let secret = message
    .variables
    .get("github_token")
    .ok_or("captured secret variable missing")?
    .value
    .clone();
  set_script(
    &mut message,
    &format!(
      "printf 'static={secret}\\nsafe=clear\\ndynamic=snowfox-70\\n' >> \"$GITHUB_OUTPUT\"; printf '::add-mask::snowfox-70\\n'"
    ),
  )?;
  add_output(&mut message, "static", "steps.produce.outputs.static")?;
  add_output(&mut message, "safe", "steps.produce.outputs.safe")?;
  add_output(&mut message, "dynamic", "steps.produce.outputs.dynamic")?;
  let events = replay(message).await?;
  let (conclusion, outputs) = completed(&events)?;
  assert_eq!(conclusion, Conclusion::Success, "{events:?}");
  assert_eq!(outputs.get("safe").map(String::as_str), Some("clear"));
  assert!(!outputs.contains_key("static"));
  assert!(!outputs.contains_key("dynamic"));
  let warnings = events
    .iter()
    .filter(|event| {
      matches!(event, RunnerEvent::Annotation {
      level: AnnotationLevel::Warning, message, ..
    } if message.contains("secret"))
    })
    .count();
  assert_eq!(warnings, 2, "{events:?}");
  Ok(())
}

#[tokio::test]
async fn failed_step_keeps_output_and_later_expression_error_keeps_prior_output() -> TestResult {
  let mut message = captured()?;
  set_script(
    &mut message,
    "printf 'value=before-failure\\n' >> \"$GITHUB_OUTPUT\"; exit 3",
  )?;
  add_output(&mut message, "bad", "unknownFunction()")?;
  let events = replay(message).await?;
  let (conclusion, outputs) = completed(&events)?;
  assert_eq!(conclusion, Conclusion::Failure, "{events:?}");
  assert_eq!(outputs.len(), 1, "{events:?}");
  assert_eq!(
    outputs.get("value").map(String::as_str),
    Some("before-failure")
  );
  Ok(())
}

#[tokio::test]
async fn captured_matrix_and_strategy_are_visible_at_job_output_time() -> TestResult {
  let mut message = captured()?;
  let matrix_capture: AgentJobRequestMessage =
    serde_json::from_str(include_str!("incoming_contexts_matrix_0.json"))?;
  for name in ["matrix", "strategy"] {
    let value = matrix_capture
      .context_data
      .get(name)
      .ok_or("captured matrix context missing")?
      .clone();
    message.context_data.insert(name.to_owned(), value);
  }
  add_output(&mut message, "matrix_tag", "matrix.tag")?;
  add_output(&mut message, "job_index", "strategy['job-index']")?;
  let events = replay(message).await?;
  let (conclusion, outputs) = completed(&events)?;
  assert_eq!(conclusion, Conclusion::Success, "{events:?}");
  assert_eq!(outputs.get("matrix_tag").map(String::as_str), Some("alpha"));
  assert_eq!(outputs.get("job_index").map(String::as_str), Some("0"));
  Ok(())
}

#[tokio::test]
async fn job_output_hash_waits_for_real_node_post_step() -> TestResult {
  let mut message = captured()?;
  let action_capture: AgentJobRequestMessage =
    serde_json::from_str(include_str!("incoming_contexts_matrix_0.json"))?;
  let mut action = action_capture
    .steps
    .into_iter()
    .find(|step| step.reference.repository_type.as_deref() == Some("self"))
    .ok_or("captured local action missing")?;
  action.reference.path = Some("./.github/actions/post-a".to_owned());
  message.steps.push(action);
  add_output(&mut message, "post_hash", "hashFiles('post-markers.txt')")?;

  let dir = tempfile::tempdir()?;
  let config = RunnerConfig {
    data_dir: dir.path().join("data"),
    workspace_root: dir.path().join("work"),
    ..RunnerConfig::default()
  };
  let workspace = config.workspace_root.join(&message.job_id);
  let root = workspace.join(".github/actions/post-a");
  std::fs::create_dir_all(&root)?;
  std::fs::write(
    root.join("action.yml"),
    include_str!("post_results_action.yml").replace("default: 'true'", "default: 'false'"),
  )?;
  std::fs::write(root.join("main.js"), include_str!("post_results_main.js"))?;
  std::fs::write(root.join("post.js"), include_str!("post_results_post.js"))?;
  seed_node(&config.data_dir).await?;

  let events = run_with_config(message, config, CancellationToken::new()).await?;
  let (conclusion, outputs) = completed(&events)?;
  assert_eq!(conclusion, Conclusion::Success, "{events:?}");
  assert_eq!(
    std::fs::read_to_string(workspace.join("post-markers.txt"))?,
    "A:main\nA:post:STATE_k=A-state:INPUT_MARKER=A:GITHUB_ACTION=__self\n"
  );
  let expected = expressions::functions::hash_files(&workspace, &["post-markers.txt".to_owned()])?;
  assert_eq!(outputs.get("post_hash"), Some(&expected));
  let post_finished = events
    .iter()
    .position(|event| {
      matches!(event, RunnerEvent::StepCompleted {
      step_id, ..
    } if step_id != "a811e451-eda4-42c0-95d3-e04f5bf630c9")
    })
    .ok_or("post completion missing")?;
  let job_finished = events
    .iter()
    .position(|event| matches!(event, RunnerEvent::JobCompleted { .. }))
    .ok_or("job completion missing")?;
  assert!(post_finished < job_finished);
  Ok(())
}

#[tokio::test]
async fn post_action_env_is_available_to_final_job_output() -> TestResult {
  let mut message = captured()?;
  let action_capture: AgentJobRequestMessage =
    serde_json::from_str(include_str!("incoming_contexts_matrix_0.json"))?;
  let mut action = action_capture
    .steps
    .into_iter()
    .find(|step| step.reference.repository_type.as_deref() == Some("self"))
    .ok_or("captured local action missing")?;
  action.reference.path = Some("./.github/actions/job-outputs-70-post".to_owned());
  message.steps.push(action);
  add_output(&mut message, "post_value", "env.POST_VALUE")?;

  let dir = tempfile::tempdir()?;
  let config = RunnerConfig {
    data_dir: dir.path().join("data"),
    workspace_root: dir.path().join("work"),
    ..RunnerConfig::default()
  };
  let root = config
    .workspace_root
    .join(&message.job_id)
    .join(".github/actions/job-outputs-70-post");
  std::fs::create_dir_all(&root)?;
  std::fs::write(
    root.join("action.yml"),
    include_str!("../../../.github/actions/job-outputs-70-post/action.yml"),
  )?;
  std::fs::write(
    root.join("main.js"),
    include_str!("../../../.github/actions/job-outputs-70-post/main.js"),
  )?;
  std::fs::write(
    root.join("post.js"),
    include_str!("../../../.github/actions/job-outputs-70-post/post.js"),
  )?;
  seed_node(&config.data_dir).await?;
  let events = run_with_config(message, config, CancellationToken::new()).await?;
  let (conclusion, outputs) = completed(&events)?;
  assert_eq!(conclusion, Conclusion::Success, "{events:?}");
  assert_eq!(
    outputs.get("post_value").map(String::as_str),
    Some("after-post")
  );
  Ok(())
}

#[tokio::test]
async fn cancelled_job_evaluates_output_written_before_cleanup() -> TestResult {
  let mut message = captured()?;
  set_script(
    &mut message,
    "printf 'value=before-cancel\\n' >> \"$GITHUB_OUTPUT\"; touch ready-to-cancel; sleep 30",
  )?;
  let dir = tempfile::tempdir()?;
  let config = RunnerConfig {
    data_dir: dir.path().join("data"),
    workspace_root: dir.path().join("work"),
    ..RunnerConfig::default()
  };
  let marker = config
    .workspace_root
    .join(&message.job_id)
    .join("ready-to-cancel");
  let cancel = CancellationToken::new();
  let watcher_cancel = cancel.clone();
  let watcher = tokio::spawn(async move {
    for _ in 0..500 {
      if marker.exists() {
        watcher_cancel.cancel();
        return true;
      }
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
    watcher_cancel.cancel();
    false
  });
  let events = run_with_config(message, config, cancel).await?;
  assert!(
    watcher.await?,
    "step never reached its cancellation marker: {events:?}"
  );
  let (conclusion, outputs) = completed(&events)?;
  assert_eq!(conclusion, Conclusion::Cancelled, "{events:?}");
  assert_eq!(
    outputs.get("value").map(String::as_str),
    Some("before-cancel")
  );
  Ok(())
}

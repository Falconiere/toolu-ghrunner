//! Production step-loop regressions for captured wire IDs and expression names.
//!
//! The job message is a sanitized acquired payload. These tests retain its
//! UUID-shaped step IDs and run real `bash` children. The named producer and
//! consumer scripts are derived from that capture until a named-step capture
//! is available; they are not live-service acceptance evidence.

use std::error::Error;
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use execution::Runner;
use execution::execution::actions::prefetch::ActionFetcher;
use execution::execution::context::ExecutionContext;
use execution::execution::job_spec::{JobSpec, evaluate_job_outputs};
use execution::execution::steps_runner::{JobRun, run_steps};
use execution::node::runtime::{node_binary_path, node_cache_dir, node_version_for};
use shared::{
  ActionStep, AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerEvent, SecretMasker,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

type TestResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

const CAPTURED_JOB: &str = include_str!("../../toolu-runner/tests/fixtures/job_message.json");
static CAPTURED_MESSAGE: LazyLock<Result<AgentJobRequestMessage, serde_json::Error>> =
  LazyLock::new(|| serde_json::from_str(CAPTURED_JOB));

fn captured_message() -> TestResult<AgentJobRequestMessage> {
  let message = CAPTURED_MESSAGE
    .as_ref()
    .map_err(|error| std::io::Error::other(error.to_string()))?;
  Ok(message.clone())
}

fn captured_script(
  index: usize,
  name: Option<&str>,
  script: &str,
  condition: &str,
) -> TestResult<ActionStep> {
  let message = captured_message()?;
  captured_script_from_message(&message, index, name, script, condition)
}

fn captured_script_from_message(
  message: &AgentJobRequestMessage,
  index: usize,
  name: Option<&str>,
  script: &str,
  condition: &str,
) -> TestResult<ActionStep> {
  let captured = message.steps.get(index).ok_or("captured step missing")?;
  let mut step = ActionStep::script(&captured.id, script, condition);
  step.context_name = name.map(str::to_owned);
  Ok(step)
}

fn captured_action(index: usize, name: &str, path: &str) -> TestResult<ActionStep> {
  let message = captured_message()?;
  let captured = message.steps.get(index).ok_or("captured step missing")?;
  let mut step = ActionStep::with_ref_type(&captured.id, "repository");
  step.context_name = Some(name.to_owned());
  step.reference.name = Some(format!("./{path}"));
  Ok(step)
}

async fn execute_with(
  steps: Vec<ActionStep>,
  setup: impl FnOnce(&Path, &Path) -> TestResult<()> + Send + 'static,
) -> TestResult<(ExecutionContext, Vec<RunnerEvent>)> {
  let (dir, workspace, config) = tokio::task::spawn_blocking(move || {
    let dir = tempfile::tempdir()?;
    let workspace = dir.path().join("work");
    std::fs::create_dir_all(&workspace)?;
    let config = RunnerConfig {
      data_dir: dir.path().join("data"),
      workspace_root: workspace.clone(),
      ..RunnerConfig::default()
    };
    std::fs::create_dir_all(&config.data_dir)?;
    setup(&workspace, &config.data_dir)?;
    Ok::<_, Box<dyn Error + Send + Sync>>((dir, workspace, config))
  })
  .await??;
  let mut ctx = ExecutionContext::with_masker(Arc::new(Mutex::new(SecretMasker::new())));
  let (tx, mut rx) = mpsc::channel::<RunnerEvent>(256);
  let collector = tokio::spawn(async move {
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
      events.push(event);
    }
    events
  });
  let http = reqwest::Client::new();
  let fetcher = ActionFetcher::new();
  let spec = JobSpec::default();
  let conclusion = run_steps(
    &steps,
    &mut ctx,
    &tx,
    CancellationToken::new(),
    &JobRun {
      workspace: &workspace,
      config: &config,
      spec: &spec,
      shadow: None,
      http: &http,
      fetcher: &fetcher,
    },
  )
  .await?;
  drop(tx);
  let events = collector.await?;
  assert_eq!(conclusion, Conclusion::Success, "events: {events:?}");
  drop(dir);
  Ok((ctx, events))
}

async fn execute(steps: Vec<ActionStep>) -> TestResult<(ExecutionContext, Vec<RunnerEvent>)> {
  execute_with(steps, |_, _| Ok(())).await
}

fn copy_action(from: &Path, workspace: &Path, name: &str, files: &[&str]) -> TestResult<()> {
  let target = workspace.join(name);
  std::fs::create_dir_all(&target)?;
  for file in files {
    std::fs::copy(from.join(file), target.join(file))?;
  }
  Ok(())
}

fn seed_installed_node(data_dir: &Path) -> TestResult<()> {
  let output = std::process::Command::new("node")
    .args(["-e", "process.stdout.write(process.execPath)"])
    .output()?;
  if !output.status.success() {
    return Err("installed node did not report its executable path".into());
  }
  let node = String::from_utf8(output.stdout)?;
  let binary = node_binary_path(&node_cache_dir(data_dir, node_version_for(20)));
  let parent = binary.parent().ok_or("node cache binary has no parent")?;
  std::fs::create_dir_all(parent)?;
  #[cfg(unix)]
  std::os::unix::fs::symlink(node.trim(), &binary)?;
  #[cfg(not(unix))]
  std::fs::copy(node.trim(), &binary)?;
  Ok(())
}

fn log_lines<'a>(events: &'a [RunnerEvent], step_id: &str) -> Vec<&'a str> {
  events
    .iter()
    .filter_map(|event| {
      if let RunnerEvent::Log {
        step_id: id, line, ..
      } = event
      {
        (id == step_id).then_some(line.as_str())
      } else {
        None
      }
    })
    .collect()
}

#[tokio::test]
async fn named_output_uses_context_name_while_reporting_uses_wire_uuid() -> TestResult<()> {
  let producer = captured_script(
    0,
    Some("build"),
    "echo 'value=hello' >> \"$GITHUB_OUTPUT\"; echo '::set-output name=legacy::old'",
    "success()",
  )?;
  let consumer = captured_script(
    1,
    Some("consume"),
    "echo \"${{ steps.build.outputs.value }}:${{ steps.build.outputs.legacy }}\"",
    "success()",
  )?;
  let producer_id = producer.id.clone();
  let consumer_id = consumer.id.clone();
  assert_ne!(producer_id, "build");
  let (ctx, events) = execute(vec![producer, consumer]).await?;
  assert_eq!(
    ctx
      .evaluate_expression("steps.build.outputs.value")?
      .coerce_to_string(),
    "hello"
  );
  assert_eq!(
    ctx
      .evaluate_expression("steps.build.outcome")?
      .coerce_to_string(),
    "success"
  );
  assert_eq!(
    ctx
      .evaluate_expression("steps.build.conclusion")?
      .coerce_to_string(),
    "success"
  );
  assert_eq!(
    ctx
      .evaluate_expression("steps.build.outputs.legacy")?
      .coerce_to_string(),
    "old"
  );
  assert!(log_lines(&events, &consumer_id).contains(&"hello:old"));
  assert!(events.iter().any(
    |event| matches!(event, RunnerEvent::StepCompleted { step_id, .. } if step_id == &producer_id)
  ));
  let spec = JobSpec {
    outputs: std::collections::HashMap::from([(
      "artifact".to_owned(),
      "${{ steps.build.outputs.value }}".to_owned(),
    )]),
    ..JobSpec::default()
  };
  assert_eq!(
    evaluate_job_outputs(&spec, &ctx)?.get("artifact"),
    Some(&"hello".to_owned())
  );
  Ok(())
}

#[tokio::test]
async fn skipped_named_step_has_skipped_results_and_empty_outputs() -> TestResult<()> {
  let skipped = captured_script(0, Some("build"), "echo must-not-run", "false")?;
  let consumer = captured_script(
    1,
    Some("inspect"),
    "echo \"${{ steps.build.outcome }}:${{ steps.build.conclusion }}:${{ steps.build.outputs.value }}\"",
    "always()",
  )?;
  let skipped_id = skipped.id.clone();
  let consumer_id = consumer.id.clone();
  let (ctx, events) = execute(vec![skipped, consumer]).await?;
  assert_eq!(
    ctx
      .evaluate_expression("steps.build.outcome")?
      .coerce_to_string(),
    "skipped"
  );
  assert_eq!(
    ctx
      .evaluate_expression("steps.build.conclusion")?
      .coerce_to_string(),
    "skipped"
  );
  assert_eq!(
    ctx
      .evaluate_expression("steps.build.outputs.value")?
      .coerce_to_string(),
    ""
  );
  assert!(log_lines(&events, &consumer_id).contains(&"skipped:skipped:"));
  assert!(events.iter().any(
    |event| matches!(event, RunnerEvent::StepSkipped { step_id, .. } if step_id == &skipped_id)
  ));
  assert!(
    events
      .iter()
      .any(|event| matches!(event, RunnerEvent::StepCompleted {
    step_id,
    conclusion: Conclusion::Skipped,
    ..
  } if step_id == &skipped_id))
  );
  assert!(log_lines(&events, &skipped_id).is_empty());
  Ok(())
}

#[tokio::test]
async fn generated_context_name_stays_absent_from_steps_context() -> TestResult<()> {
  let generated = captured_script(
    0,
    Some("__run"),
    "echo 'value=private' >> \"$GITHUB_OUTPUT\"",
    "success()",
  )?;
  let (ctx, events) = execute(vec![generated]).await?;
  assert!(
    ctx
      .evaluate_expression("steps.__run")?
      .coerce_to_string()
      .is_empty()
  );
  assert!(
    events
      .iter()
      .any(|event| matches!(event, RunnerEvent::StepCompleted { .. }))
  );
  Ok(())
}

#[tokio::test]
async fn continue_on_error_keeps_real_outcome_under_context_name() -> TestResult<()> {
  let mut failed = captured_script(0, Some("build"), "exit 1", "success()")?;
  failed.set_continue_on_error(true);
  let consumer = captured_script(
    1,
    Some("inspect"),
    "echo \"${{ steps.build.outcome }}:${{ steps.build.conclusion }}\"",
    "success()",
  )?;
  let consumer_id = consumer.id.clone();
  let (ctx, events) = execute(vec![failed, consumer]).await?;
  assert_eq!(
    ctx
      .evaluate_expression("steps.build.outcome")?
      .coerce_to_string(),
    "failure"
  );
  assert_eq!(
    ctx
      .evaluate_expression("steps.build.conclusion")?
      .coerce_to_string(),
    "success"
  );
  assert!(log_lines(&events, &consumer_id).contains(&"failure:success"));
  Ok(())
}

#[tokio::test]
async fn post_stage_keeps_main_output_and_uses_distinct_report_identity() -> TestResult<()> {
  let action = captured_action(0, "action", "prepost_action")?;
  let action_id = action.id.clone();
  let consumer = captured_script(
    1,
    Some("inspect"),
    "echo \"${{ steps.action.outputs.phase }}\"",
    "success()",
  )?;
  let consumer_id = consumer.id.clone();
  let source =
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../toolu-runner/tests/fixtures/prepost_action");
  let (ctx, events) = execute_with(vec![action, consumer], move |workspace, data_dir| {
    copy_action(
      &source,
      workspace,
      "prepost_action",
      &["action.yml", "pre.js", "main.js", "post.js"],
    )?;
    seed_installed_node(data_dir)
  })
  .await?;
  assert_eq!(
    ctx
      .evaluate_expression("steps.action.outputs.phase")?
      .coerce_to_string(),
    "main"
  );
  assert_eq!(
    ctx
      .evaluate_expression("steps.action.outcome")?
      .coerce_to_string(),
    "success"
  );
  let completed: Vec<&str> = events
    .iter()
    .filter_map(|event| {
      if let RunnerEvent::StepCompleted { step_id, .. } = event {
        Some(step_id.as_str())
      } else {
        None
      }
    })
    .collect();
  assert_eq!(completed.iter().filter(|id| **id == action_id).count(), 1);
  assert_eq!(
    completed.len(),
    4,
    "pre, main, consumer, and post should report separately"
  );
  let distinct: std::collections::HashSet<&str> = completed.iter().copied().collect();
  assert_eq!(distinct.len(), completed.len());
  let post_id = completed.iter().copied().find(|id| {
    *id != action_id && *id != consumer_id && log_lines(&events, id).contains(&"post-state=X-state")
  });
  assert!(post_id.is_some(), "post must read main's private STATE_k");
  Ok(())
}

#[tokio::test]
async fn skipped_post_reports_skipped_without_rewriting_main_result() -> TestResult<()> {
  let action = captured_action(0, "action", "prepost_action")?;
  let main_id = action.id.clone();
  let source =
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../toolu-runner/tests/fixtures/prepost_action");
  let (ctx, events) = execute_with(vec![action], move |workspace, data_dir| {
    copy_action(
      &source,
      workspace,
      "prepost_action",
      &["action.yml", "pre.js", "main.js", "post.js"],
    )?;
    let manifest = workspace.join("prepost_action/action.yml");
    let original = std::fs::read_to_string(&manifest)?;
    let with_post_if =
      original.replace("  post: 'post.js'", "  post: 'post.js'\n  post-if: 'false'");
    assert_ne!(original, with_post_if);
    std::fs::write(manifest, with_post_if)?;
    seed_installed_node(data_dir)
  })
  .await?;
  let skipped_id = events.iter().find_map(|event| {
    if let RunnerEvent::StepSkipped { step_id, .. } = event {
      Some(step_id.as_str())
    } else {
      None
    }
  });
  let skipped_id = skipped_id.ok_or("missing post skip event")?;
  assert_ne!(skipped_id, main_id);
  assert!(events.iter().any(|event| matches!(event,
    RunnerEvent::StepCompleted { step_id, conclusion: Conclusion::Skipped, .. }
      if step_id == skipped_id
  )));
  assert_eq!(
    ctx
      .evaluate_expression("steps.action.outputs.phase")?
      .coerce_to_string(),
    "main"
  );
  Ok(())
}

#[tokio::test]
async fn repeated_nested_composites_do_not_leak_inner_step_outputs() -> TestResult<()> {
  let parent = captured_action(0, "parent", "composite-output-parent")?;
  let consumer = captured_script(
    1,
    Some("inspect"),
    "echo \"${{ steps.parent.outputs.first }}:${{ steps.parent.outputs.second }}\"",
    "success()",
  )?;
  let consumer_id = consumer.id.clone();
  let source =
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../toolu-runner/tests/fixtures/local_actions");
  let (ctx, events) = execute_with(vec![parent, consumer], move |workspace, _| {
    for name in ["composite-output-child", "composite-output-parent"] {
      copy_action(&source.join(name), workspace, name, &["action.yml"])?;
    }
    Ok(())
  })
  .await?;
  assert_eq!(
    ctx
      .evaluate_expression("steps.parent.outputs.first")?
      .coerce_to_string(),
    "one"
  );
  assert_eq!(
    ctx
      .evaluate_expression("steps.parent.outputs.second")?
      .coerce_to_string(),
    "two"
  );
  assert!(log_lines(&events, &consumer_id).contains(&"one:two"));
  assert_eq!(
    ctx
      .evaluate_expression("steps.make.outputs.value")?
      .coerce_to_string(),
    ""
  );
  Ok(())
}

#[tokio::test]
async fn acquired_message_replay_keeps_wire_ids_through_job_completion() -> TestResult<()> {
  let mut message = captured_message()?;
  let producer = captured_script_from_message(
    &message,
    0,
    Some("build"),
    "echo 'value=hello' >> \"$GITHUB_OUTPUT\"",
    "success()",
  )?;
  let consumer = captured_script_from_message(
    &message,
    1,
    Some("inspect"),
    "echo \"${{ steps.build.outputs.value }}\"",
    "success()",
  )?;
  let ids = [producer.id.clone(), consumer.id.clone()];
  message.steps = vec![producer, consumer];
  let dir = tokio::task::spawn_blocking(tempfile::tempdir).await??;
  let config = RunnerConfig {
    data_dir: dir.path().join("data"),
    workspace_root: dir.path().join("work"),
    workspace_gc_hours: 0,
    ..RunnerConfig::default()
  };
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let mut events = runner.execute_job(message, CancellationToken::new());
  let collected = tokio::time::timeout(Duration::from_secs(30), async {
    let mut collected = Vec::new();
    while let Some(event) = events.recv().await {
      collected.push(event);
    }
    collected
  })
  .await?;
  for id in &ids {
    assert!(collected.iter().any(|event| matches!(event,
      RunnerEvent::StepCompleted { step_id, conclusion: Conclusion::Success, .. } if step_id == id
    )));
  }
  assert!(log_lines(&collected, &ids[1]).contains(&"hello"));
  assert!(collected.iter().any(|event| matches!(
    event,
    RunnerEvent::JobCompleted {
      conclusion: Conclusion::Success,
      ..
    }
  )));
  Ok(())
}

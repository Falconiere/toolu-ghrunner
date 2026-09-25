//! Captured GitHub job messages drive real Node actions through the production job path.

use std::error::Error;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use execution::Runner;
use execution::node::runtime::{node_binary_path, node_cache_dir, node_version_for};
use shared::{
  ActionStep, AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerEvent, SecretMasker,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

type TestResult = Result<(), Box<dyn Error>>;
const CAPTURE: &str = include_str!("incoming_contexts_matrix_0.json");

#[tokio::test]
async fn post_failure_reaches_job_completed() -> TestResult {
  let dir = tempfile::tempdir()?;
  let config = test_config(&dir);
  let mut msg = local_action_message()?;
  msg.steps.truncate(1);
  let step = msg
    .steps
    .first_mut()
    .ok_or("captured local action missing")?;
  step.reference.path = Some("./.github/actions/post-a".to_owned());
  let main_id = step.id.clone();
  let workspace = config.workspace_root.join(&msg.job_id);
  seed_action(&workspace, "post-a", "A", true, false)?;
  let post_script = workspace.join(".github/actions/post-a/post.js");
  let post_code = std::fs::read_to_string(&post_script)?;
  std::fs::write(
    post_script,
    format!("{post_code}\nconsole.log('POST_LOG:A');\n"),
  )?;
  seed_node(&config.data_dir).await?;

  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let cancel = CancellationToken::new();
  let mut receiver = runner.execute_job(msg, cancel.clone());
  let events = tokio::time::timeout(Duration::from_secs(30), collect_events(&mut receiver)).await?;
  cancel.cancel();

  let completed: Vec<_> = events
    .iter()
    .filter_map(|event| {
      if let RunnerEvent::StepCompleted {
        step_id,
        conclusion,
        ..
      } = event
      {
        Some((step_id.as_str(), *conclusion))
      } else {
        None
      }
    })
    .collect();
  let job = events.iter().find_map(|event| {
    if let RunnerEvent::JobCompleted { conclusion, .. } = event {
      Some(*conclusion)
    } else {
      None
    }
  });
  assert_eq!(job, Some(Conclusion::Failure), "{events:#?}");
  assert_eq!(completed.len(), 2, "{events:#?}");
  let main = completed.first().ok_or("main completion missing")?;
  let post = completed.get(1).ok_or("post completion missing")?;
  assert_eq!(*main, (main_id.as_str(), Conclusion::Success));
  assert_eq!(post.1, Conclusion::Failure);
  assert_ne!(post.0, main_id, "post needs its own report ID");
  assert!(events.iter().any(|event| matches!(
    event,
    RunnerEvent::Log { step_id, line, .. }
      if step_id == post.0 && line.contains("POST_LOG:A")
  )));
  let post_started = events.iter().find_map(|event| {
    if let RunnerEvent::StepStarted {
      step_id,
      step_name,
      step_number,
    } = event
    {
      (step_id != &main_id).then_some((step_id.as_str(), step_name.as_str(), *step_number))
    } else {
      None
    }
  });
  assert_eq!(
    post_started,
    Some((post.0, "Post Workflow input passed to action", 3))
  );
  assert_eq!(
    std::fs::read_to_string(workspace.join("post-markers.txt"))?,
    "A:main\nA:post:STATE_k=A-state:INPUT_MARKER=A:GITHUB_ACTION=__self\n"
  );
  Ok(())
}

#[tokio::test]
async fn repeated_posts_keep_state_and_report_identity() -> TestResult {
  let dir = tempfile::tempdir()?;
  let config = test_config(&dir);
  let mut msg = local_action_message()?;
  assert_eq!(msg.steps.len(), 2, "capture must have two local actions");
  let main_ids: Vec<_> = msg.steps.iter().map(|step| step.id.clone()).collect();
  for (step, name) in msg.steps.iter_mut().zip(["post-a", "post-b"]) {
    step.reference.path = Some(format!("./.github/actions/{name}"));
  }
  let workspace = config.workspace_root.join(&msg.job_id);
  seed_action(&workspace, "post-a", "A", false, false)?;
  seed_action(&workspace, "post-b", "B", false, false)?;
  seed_node(&config.data_dir).await?;

  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let cancel = CancellationToken::new();
  let mut receiver = runner.execute_job(msg, cancel.clone());
  let events = tokio::time::timeout(Duration::from_secs(30), collect_events(&mut receiver)).await?;
  cancel.cancel();

  assert_eq!(
    std::fs::read_to_string(workspace.join("post-markers.txt"))?,
    "A:main\nB:main\nB:post:STATE_k=B-state:INPUT_MARKER=B:GITHUB_ACTION=__self_2\nA:post:STATE_k=A-state:INPUT_MARKER=A:GITHUB_ACTION=__self\n"
  );
  let completed_ids: Vec<_> = events
    .iter()
    .filter_map(|event| {
      if let RunnerEvent::StepCompleted { step_id, .. } = event {
        Some(step_id.clone())
      } else {
        None
      }
    })
    .collect();
  assert_eq!(completed_ids.len(), 4, "{events:#?}");
  assert_eq!(completed_ids.get(..2).ok_or("main ids missing")?, main_ids);
  let first_post = completed_ids.get(2).ok_or("first post missing")?;
  let second_post = completed_ids.get(3).ok_or("second post missing")?;
  assert_ne!(first_post, second_post);
  assert!(!main_ids.contains(first_post));
  assert!(!main_ids.contains(second_post));
  Ok(())
}

#[tokio::test]
async fn hard_post_error_keeps_draining_lifo() -> TestResult {
  let dir = tempfile::tempdir()?;
  let config = test_config(&dir);
  let mut msg = local_action_message()?;
  assert_eq!(msg.steps.len(), 2);
  for (step, name) in msg.steps.iter_mut().zip(["post-a", "post-b"]) {
    step.reference.path = Some(format!("./.github/actions/{name}"));
  }
  let workspace = config.workspace_root.join(&msg.job_id);
  seed_action(&workspace, "post-a", "A", false, false)?;
  seed_action(&workspace, "post-b", "B", false, true)?;
  seed_node(&config.data_dir).await?;

  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let mut receiver = runner.execute_job(msg, CancellationToken::new());
  let events = tokio::time::timeout(Duration::from_secs(30), collect_events(&mut receiver)).await?;
  assert_eq!(
    std::fs::read_to_string(workspace.join("post-markers.txt"))?,
    "A:main\nB:main\nA:post:STATE_k=A-state:INPUT_MARKER=A:GITHUB_ACTION=__self\n"
  );
  let completed: Vec<_> = events
    .iter()
    .filter_map(|event| {
      if let RunnerEvent::StepCompleted { conclusion, .. } = event {
        Some(*conclusion)
      } else {
        None
      }
    })
    .collect();
  assert_eq!(
    completed,
    [
      Conclusion::Success,
      Conclusion::Success,
      Conclusion::Failure,
      Conclusion::Success
    ]
  );
  assert!(events.iter().any(
    |event| matches!(event, RunnerEvent::Log { line, .. } if line.contains("post script not found"))
  ));
  assert!(events.iter().any(|event| matches!(
    event,
    RunnerEvent::JobCompleted {
      conclusion: Conclusion::Failure,
      ..
    }
  )));
  Ok(())
}

#[tokio::test]
async fn post_conditions_follow_live_status() -> TestResult {
  let (events, markers) = run_condition_case("A", true, "failure()", false).await?;
  assert!(markers.contains("A:post:STATE_k=A-state"), "{markers}");
  assert!(events.iter().any(|event| matches!(
    event,
    RunnerEvent::JobCompleted {
      conclusion: Conclusion::Failure,
      ..
    }
  )));

  let (events, markers) = run_condition_case("A", true, "success()", false).await?;
  assert!(!markers.contains("A:post:STATE_k=A-state"), "{markers}");
  assert!(events.iter().any(|event| matches!(
    event,
    RunnerEvent::StepCompleted {
      conclusion: Conclusion::Skipped,
      ..
    }
  )));

  let (events, markers) = run_condition_case("MAINFAIL", false, "failure()", false).await?;
  assert!(markers.contains("MAINFAIL:main\n"), "{markers}");
  assert!(markers.contains("B:post:STATE_k=B-state"), "{markers}");
  assert!(events.iter().any(|event| matches!(
    event,
    RunnerEvent::JobCompleted {
      conclusion: Conclusion::Failure,
      ..
    }
  )));
  let (events, markers) = run_condition_case("MAINFAIL", false, "success()", false).await?;
  assert!(!markers.contains("B:post:STATE_k=B-state"), "{markers}");
  assert!(events.iter().any(|event| matches!(
    event,
    RunnerEvent::StepCompleted {
      conclusion: Conclusion::Skipped,
      ..
    }
  )));
  Ok(())
}

#[tokio::test]
async fn hard_main_error_still_completes_after_post_cleanup() -> TestResult {
  let (events, markers) = run_condition_case("A", false, "failure()", true).await?;
  assert!(markers.contains("A:post:STATE_k=A-state"), "{markers}");
  assert!(events.iter().any(|event| matches!(
    event,
    RunnerEvent::JobCompleted {
      conclusion: Conclusion::Failure,
      ..
    }
  )));
  Ok(())
}

#[tokio::test]
async fn cancelled_posts_retest_always_and_complete_job() -> TestResult {
  let dir = tempfile::tempdir()?;
  let config = test_config(&dir);
  let mut msg = local_action_message()?;
  for (step, name) in msg.steps.iter_mut().zip(["post-a", "post-b"]) {
    step.reference.path = Some(format!("./.github/actions/{name}"));
  }
  let workspace = config.workspace_root.join(&msg.job_id);
  seed_action(&workspace, "post-a", "A", false, false)?;
  seed_action(&workspace, "post-b", "B", false, false)?;
  let marker = workspace.join("post-markers.txt");
  std::fs::write(&marker, "")?;
  let a_manifest = workspace.join(".github/actions/post-a/action.yml");
  let a_text = std::fs::read_to_string(&a_manifest)?;
  std::fs::write(
    a_manifest,
    a_text
      .replace("post-if: always()", "post-if: cancelled()")
      .replace("default: '0'", "default: '300'"),
  )?;
  let b_manifest = workspace.join(".github/actions/post-b/action.yml");
  let b_text = std::fs::read_to_string(&b_manifest)?;
  std::fs::write(
    b_manifest,
    b_text.replace("default: '0'", "default: '1000'"),
  )?;
  seed_node(&config.data_dir).await?;
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let cancel = CancellationToken::new();
  let mut receiver = runner.execute_job(msg, cancel.clone());
  tokio::time::timeout(Duration::from_secs(5), async {
    loop {
      if std::fs::read_to_string(&marker)?.contains("B:post-start") {
        break;
      }
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Ok::<(), std::io::Error>(())
  })
  .await??;
  let cancelled_at = std::time::Instant::now();
  cancel.cancel();
  let events = tokio::time::timeout(Duration::from_secs(6), collect_events(&mut receiver)).await?;
  assert!(cancelled_at.elapsed() < Duration::from_secs(5));
  let text = std::fs::read_to_string(marker)?;
  assert!(text.contains("A:post:STATE_k=A-state"), "{text}");
  assert!(text.contains("A:post-finished"), "{text}");
  assert!(text.contains("B:post-finished"), "{text}");
  assert!(events.iter().any(|event| matches!(
    event,
    RunnerEvent::JobCompleted {
      conclusion: Conclusion::Cancelled,
      ..
    }
  )));
  Ok(())
}

#[tokio::test]
async fn cancelled_main_keeps_failure_post_and_final_cancelled() -> TestResult {
  let dir = tempfile::tempdir()?;
  let config = test_config(&dir);
  let mut msg = local_action_message()?;
  assert_eq!(msg.steps.len(), 2, "capture must have two local actions");
  for (step, name) in msg.steps.iter_mut().zip(["post-a", "post-b"]) {
    step.reference.path = Some(format!("./.github/actions/{name}"));
  }
  msg.steps.push(ActionStep::script(
    "cancel-main",
    "echo MAIN_STARTED; sleep 30",
    "",
  ));
  let workspace = config.workspace_root.join(&msg.job_id);
  seed_action(&workspace, "post-a", "A", true, false)?;
  seed_action(&workspace, "post-b", "B", false, false)?;
  let manifest_path = workspace.join(".github/actions/post-a/action.yml");
  let manifest = std::fs::read_to_string(&manifest_path)?;
  std::fs::write(
    manifest_path,
    manifest.replace("post-if: always()", "post-if: cancelled()"),
  )?;
  let skipped_manifest = workspace.join(".github/actions/post-b/action.yml");
  let skipped_text = std::fs::read_to_string(&skipped_manifest)?;
  std::fs::write(
    skipped_manifest,
    skipped_text.replace("post-if: always()", "post-if: success()"),
  )?;
  seed_node(&config.data_dir).await?;
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let cancel = CancellationToken::new();
  let mut receiver = runner.execute_job(msg, cancel.clone());
  let mut events = Vec::new();
  let main_started = tokio::time::timeout(Duration::from_secs(5), async {
    while let Some(event) = receiver.recv().await {
      let started =
        matches!(&event, RunnerEvent::Log { line, .. } if line.contains("MAIN_STARTED"));
      events.push(event);
      if started {
        return true;
      }
    }
    false
  })
  .await?;
  assert!(main_started, "main script did not start: {events:#?}");
  cancel.cancel();
  tokio::time::timeout(Duration::from_secs(5), async {
    while let Some(event) = receiver.recv().await {
      events.push(event);
    }
  })
  .await?;
  let markers = std::fs::read_to_string(workspace.join("post-markers.txt"))?;
  assert!(markers.contains("A:post:STATE_k=A-state"), "{markers}");
  assert!(markers.contains("B:main\n"), "{markers}");
  assert!(!markers.contains("B:post:STATE_k=B-state"), "{markers}");
  assert!(events.windows(2).any(|pair| matches!(
    pair,
    [RunnerEvent::StepSkipped { step_id, reason }, RunnerEvent::StepCompleted {
      step_id: done_id,
      conclusion: Conclusion::Skipped,
      ..
    }] if reason.contains("post-if 'success()' evaluated to false") && step_id == done_id
  )));
  assert!(events.iter().any(|event| matches!(
    event,
    RunnerEvent::StepCompleted {
      conclusion: Conclusion::Failure,
      ..
    }
  )));
  assert!(events.iter().any(|event| matches!(
    event,
    RunnerEvent::JobCompleted {
      conclusion: Conclusion::Cancelled,
      ..
    }
  )));
  Ok(())
}

async fn run_condition_case(
  first_marker: &str,
  second_post_fails: bool,
  condition: &str,
  hard_second_main: bool,
) -> Result<(Vec<RunnerEvent>, String), Box<dyn Error>> {
  let dir = tempfile::tempdir()?;
  let config = test_config(&dir);
  let mut msg = local_action_message()?;
  for (step, name) in msg.steps.iter_mut().zip(["post-a", "post-b"]) {
    step.reference.path = Some(format!("./.github/actions/{name}"));
  }
  let workspace = config.workspace_root.join(&msg.job_id);
  seed_action(&workspace, "post-a", first_marker, false, false)?;
  if first_marker == "MAINFAIL" {
    let second = msg.steps.get_mut(1).ok_or("second local action missing")?;
    second.condition = Some("always()".to_owned());
  }
  seed_action(&workspace, "post-b", "B", second_post_fails, false)?;
  if hard_second_main {
    std::fs::remove_file(workspace.join(".github/actions/post-b/main.js"))?;
  }
  let conditional_action = if first_marker == "MAINFAIL" {
    "post-b"
  } else {
    "post-a"
  };
  let manifest_path = workspace
    .join(".github/actions")
    .join(conditional_action)
    .join("action.yml");
  let manifest = std::fs::read_to_string(&manifest_path)?;
  std::fs::write(
    manifest_path,
    manifest.replace("post-if: always()", &format!("post-if: {condition}")),
  )?;
  seed_node(&config.data_dir).await?;
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let mut receiver = runner.execute_job(msg, CancellationToken::new());
  let events = tokio::time::timeout(Duration::from_secs(30), collect_events(&mut receiver)).await?;
  let markers = std::fs::read_to_string(workspace.join("post-markers.txt"))?;
  Ok((events, markers))
}

fn test_config(dir: &tempfile::TempDir) -> RunnerConfig {
  RunnerConfig {
    data_dir: dir.path().join("data"),
    workspace_root: dir.path().join("work"),
    workspace_gc_hours: 0,
    ..RunnerConfig::default()
  }
}

fn local_action_message() -> Result<AgentJobRequestMessage, serde_json::Error> {
  let mut msg: AgentJobRequestMessage = serde_json::from_str(CAPTURE)?;
  msg
    .steps
    .retain(|step| step.reference.repository_type.as_deref() == Some("self"));
  Ok(msg)
}

async fn collect_events(receiver: &mut mpsc::Receiver<RunnerEvent>) -> Vec<RunnerEvent> {
  let mut events = Vec::new();
  while let Some(event) = receiver.recv().await {
    events.push(event);
  }
  events
}

fn seed_action(
  workspace: &Path,
  name: &str,
  marker: &str,
  fail: bool,
  remove_post: bool,
) -> TestResult {
  let root = workspace.join(".github/actions").join(name);
  std::fs::create_dir_all(&root)?;
  let manifest = include_str!("post_results_action.yml")
    .replace("default: A", &format!("default: {marker}"))
    .replace(
      "default: 'true'",
      if fail {
        "default: 'true'"
      } else {
        "default: 'false'"
      },
    )
    .replace(
      "Remove own post script after main to exercise a hard drain error\n    default: 'false'",
      &format!("Remove own post script after main to exercise a hard drain error\n    default: '{remove_post}'"),
    );
  std::fs::write(root.join("action.yml"), manifest)?;
  std::fs::write(root.join("main.js"), include_str!("post_results_main.js"))?;
  std::fs::write(root.join("post.js"), include_str!("post_results_post.js"))?;
  Ok(())
}

async fn seed_node(data: &Path) -> TestResult {
  let output = tokio::process::Command::new("node")
    .args(["-e", "process.stdout.write(process.execPath)"])
    .output()
    .await?;
  assert!(output.status.success(), "Node is required for this test");
  let path = String::from_utf8(output.stdout)?;
  let binary = node_binary_path(&node_cache_dir(data, node_version_for(20)));
  std::fs::create_dir_all(binary.parent().ok_or("node cache parent missing")?)?;
  #[cfg(unix)]
  std::os::unix::fs::symlink(path.trim(), &binary)?;
  #[cfg(not(unix))]
  std::fs::copy(path.trim(), &binary)?;
  Ok(())
}

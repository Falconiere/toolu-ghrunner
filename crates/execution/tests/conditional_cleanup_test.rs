//! Issue #100 replays the sanitized #68 acquisition with explicit probe substitutions.
//! Scripts/conditions/env replace captured tokens; report UUIDs and context names
//! remain distinct. All execution uses real shells/actions through Runner.

use std::error::Error;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use execution::Runner;
use serde_json::{Value, json};
use shared::{AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerEvent, SecretMasker};
use tokio_util::sync::CancellationToken;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;
const CAPTURE: &str = include_str!("incoming_contexts_matrix_0.json");

fn token_map(entries: &[(&str, &str)]) -> Value {
  json!({"type": 2, "map": entries.iter().map(|(key, value)| {
    json!({"Key": {"type": 0, "lit": key}, "Value": {"type": 0, "lit": value}})
  }).collect::<Vec<_>>()})
}

fn probe_message(kind: &str, continued: bool) -> TestResult<AgentJobRequestMessage> {
  let mut capture: Value = serde_json::from_str(CAPTURE)?;
  let steps = capture
    .get_mut("steps")
    .and_then(Value::as_array_mut)
    .ok_or("steps")?;
  for (index, step) in steps.iter_mut().enumerate() {
    step["reference"] = json!({"type": "script"});
    step["environment"] = Value::Null;
    step["condition"] = json!("success()");
    let script = if index == 0 {
      "exit 7".to_owned()
    } else {
      format!("echo marker-{index}")
    };
    step["inputs"] = token_map(&[("script", &script)]);
  }
  let first = steps.first_mut().ok_or("first step")?;
  first["contextName"] = json!("probe");
  first["continueOnError"] = json!(continued);
  match kind {
    "cwd" => {
      first["inputs"] = token_map(&[
        ("script", "echo unreachable"),
        ("workingDirectory", "missing-directory"),
      ]);
    },
    "shell" => first["environment"] = token_map(&[("PATH", "/nonexistent/issue-100")]),
    "action" => {
      first["reference"] =
        json!({"type":"repository", "repositoryType":"self", "path":"./missing-action"});
    },
    "condition" => first["condition"] = json!("broken("),
    "env" => {
      first["environment"] = json!({"type":2,"map":[{"Key":{"type":0,"lit":"BAD"},"Value":{"type":3,"expr":"broken("}}]});
    },
    _ => {},
  }
  for (step, condition) in steps.iter_mut().skip(1).zip([
    "success()",
    "failure()",
    "always()",
    "!cancelled()",
    "cancelled()",
  ]) {
    step["condition"] = json!(condition);
  }
  // Read raw outcome/effective conclusion from the runtime context, through a shell.
  let last = steps.get_mut(3).ok_or("always step")?;
  last["inputs"] = token_map(&[(
    "script",
    "echo marker-3\necho result=${{ steps.probe.outcome }}/${{ steps.probe.conclusion }}",
  )]);
  // The capture has no job defaults; retain all server contexts and IDs.
  Ok(serde_json::from_value(capture)?)
}

fn runner_for(msg: &AgentJobRequestMessage) -> TestResult<(tempfile::TempDir, PathBuf, Runner)> {
  let temp = tempfile::tempdir()?;
  let config = RunnerConfig {
    data_dir: temp.path().join("data"),
    workspace_root: temp.path().join("work"),
    workspace_gc_hours: 0,
    ..RunnerConfig::default()
  };
  let workspace = config.workspace_root.join(&msg.job_id);
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  Ok((temp, workspace, runner))
}

async fn collect(
  mut receiver: tokio::sync::mpsc::Receiver<RunnerEvent>,
) -> TestResult<Vec<RunnerEvent>> {
  Ok(
    tokio::time::timeout(Duration::from_secs(30), async {
      let mut events = Vec::new();
      while let Some(event) = receiver.recv().await {
        events.push(event);
      }
      events
    })
    .await?,
  )
}

// Collect only later-step markers and context results, not probe synchronization logs.
fn markers(events: &[RunnerEvent]) -> Vec<&str> {
  events
    .iter()
    .filter_map(|event| {
      if let RunnerEvent::Log { line, .. } = event
        && (line.starts_with("marker-") || line.starts_with("result="))
      {
        Some(line.as_str())
      } else {
        None
      }
    })
    .collect()
}

fn assert_completions(msg: &AgentJobRequestMessage, events: &[RunnerEvent], job: Conclusion) {
  for step in &msg.steps {
    assert_eq!(
      events
        .iter()
        .filter(
          |event| matches!(event, RunnerEvent::StepCompleted {step_id, ..} if step_id == &step.id)
        )
        .count(),
      1,
      "{}: {events:?}",
      step.id
    );
  }
  let jobs: Vec<_> = events
    .iter()
    .filter_map(|event| {
      if let RunnerEvent::JobCompleted { conclusion, .. } = event {
        Some(*conclusion)
      } else {
        None
      }
    })
    .collect();
  assert_eq!(jobs, [job]);
}

#[tokio::test]
async fn execution_errors_follow_exit_failure_policy_and_continue_on_error() -> TestResult {
  for kind in ["exit", "cwd", "shell", "action"] {
    for continued in [false, true] {
      let msg = probe_message(kind, continued)?;
      let (_temp, _workspace, runner) = runner_for(&msg)?;
      let events = collect(runner.execute_job(msg.clone(), CancellationToken::new())).await?;
      let expected = if continued {
        vec!["marker-1", "marker-3", "result=failure/success", "marker-4"]
      } else {
        vec!["marker-2", "marker-3", "result=failure/failure", "marker-4"]
      };
      assert_eq!(
        markers(&events),
        expected,
        "kind={kind}, continued={continued}: {events:?}"
      );
      assert_completions(
        &msg,
        &events,
        if continued {
          Conclusion::Success
        } else {
          Conclusion::Failure
        },
      );
      if kind != "exit" {
        let id = &msg.steps.first().ok_or("first")?.id;
        assert!(events.iter().any(|e| matches!(e, RunnerEvent::Log {step_id, line, ..} if step_id == id && line.starts_with("##[error]"))));
      }
    }
  }
  Ok(())
}

#[tokio::test]
async fn condition_and_env_errors_fail_without_continue_adjustment_and_allow_cleanup() -> TestResult
{
  for kind in ["condition", "env"] {
    let msg = probe_message(kind, true)?;
    let (_temp, _workspace, runner) = runner_for(&msg)?;
    let events = collect(runner.execute_job(msg.clone(), CancellationToken::new())).await?;
    assert_eq!(
      markers(&events),
      ["marker-2", "marker-3", "result=failure/failure", "marker-4"],
      "{kind}: {events:?}"
    );
    assert_completions(&msg, &events, Conclusion::Failure);
  }
  Ok(())
}

fn set_probe_script(msg: &mut AgentJobRequestMessage, script: &str, condition: &str) -> TestResult {
  let first = msg.steps.first_mut().ok_or("first")?;
  first.inputs = serde_json::from_value(token_map(&[("script", script)]))?;
  first.condition = Some(condition.to_owned());
  Ok(())
}

#[tokio::test]
async fn cancellation_before_first_step_runs_only_eligible_cleanup() -> TestResult {
  let msg = probe_message("exit", false)?;
  let (_temp, _workspace, runner) = runner_for(&msg)?;
  let cancel = CancellationToken::new();
  cancel.cancel();
  let events = collect(runner.execute_job(msg.clone(), cancel)).await?;
  assert_eq!(
    markers(&events),
    ["marker-3", "result=skipped/skipped", "marker-5"],
    "{events:?}"
  );
  assert_completions(&msg, &events, Conclusion::Cancelled);
  Ok(())
}

#[tokio::test]
async fn observed_start_cancel_retests_current_condition_and_runs_cleanup() -> TestResult {
  for condition in ["success()", "!cancelled()", "always()"] {
    let mut msg = probe_message("exit", false)?;
    set_probe_script(
      &mut msg,
      "echo started\nwhile [ ! -f release ]; do sleep 0.05; done\necho retained",
      condition,
    )?;
    let (_temp, workspace, runner) = runner_for(&msg)?;
    let cancel = CancellationToken::new();
    let mut rx = runner.execute_job(msg.clone(), cancel.clone());
    let events = tokio::time::timeout(Duration::from_secs(15), async {
      let mut events = Vec::new();
      while let Some(event) = rx.recv().await {
        if matches!(&event, RunnerEvent::Log {line, ..} if line == "started") {
          cancel.cancel();
          if condition == "always()" {
            std::fs::write(workspace.join("release"), "go")?;
          }
        }
        events.push(event);
      }
      Ok::<_, Box<dyn Error>>(events)
    })
    .await??;
    let retained = condition == "always()";
    assert_eq!(
      events
        .iter()
        .any(|e| matches!(e, RunnerEvent::Log {line, ..} if line == "retained")),
      retained,
      "{condition}: {events:?}"
    );
    let result = if retained {
      "result=success/success"
    } else {
      "result=cancelled/cancelled"
    };
    assert_eq!(
      markers(&events),
      ["marker-3", result, "marker-5"],
      "{condition}: {events:?}"
    );
    assert_completions(&msg, &events, Conclusion::Cancelled);
  }
  Ok(())
}

#[tokio::test]
async fn shutdown_interrupts_always_and_skips_later_user_steps() -> TestResult {
  let mut msg = probe_message("exit", false)?;
  set_probe_script(
    &mut msg,
    "echo started\nsleep 300\necho retained",
    "always()",
  )?;
  let (_temp, _workspace, runner) = runner_for(&msg)?;
  let shutdown = CancellationToken::new();
  let mut rx =
    runner.execute_job_with_shutdown(msg.clone(), CancellationToken::new(), shutdown.clone());
  let events = tokio::time::timeout(Duration::from_secs(15), async {
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
      if matches!(&event, RunnerEvent::Log {line, ..} if line == "started") {
        shutdown.cancel();
      }
      events.push(event);
    }
    events
  })
  .await?;
  assert!(
    events
      .iter()
      .any(|e| matches!(e, RunnerEvent::Log {line, ..} if line == "started")),
    "shutdown must follow the observed probe start: {events:?}"
  );
  assert!(markers(&events).is_empty(), "{events:?}");
  assert!(
    !events
      .iter()
      .any(|e| matches!(e, RunnerEvent::Log {line, ..} if line == "retained"))
  );
  assert_completions(&msg, &events, Conclusion::Failure);
  Ok(())
}

#[cfg(unix)]
fn process_alive(pid: &str) -> TestResult<bool> {
  let result = std::process::Command::new("ps")
    .args(["-p", pid.trim(), "-o", "stat="])
    .output()?;
  let state = String::from_utf8(result.stdout)?;
  Ok(result.status.success() && !state.trim().is_empty() && !state.trim().starts_with('Z'))
}

#[cfg(unix)]
#[tokio::test]
async fn cancellation_kills_owned_child_and_grandchild_but_not_unrelated_process() -> TestResult {
  let mut unrelated = tokio::process::Command::new("sleep")
    .arg("300")
    .kill_on_drop(true)
    .spawn()?;
  let mut msg = probe_message("exit", false)?;
  set_probe_script(
    &mut msg,
    "echo $$ > parent.pid\nsh -c 'sleep 300 & echo $! > grandchild.pid; wait' &\necho $! > child.pid\nwhile [ ! -s grandchild.pid ]; do sleep 0.01; done\necho family-started\nwait",
    "success()",
  )?;
  let (_temp, workspace, runner) = runner_for(&msg)?;
  let cancel = CancellationToken::new();
  let mut rx = runner.execute_job(msg.clone(), cancel.clone());
  let events = tokio::time::timeout(Duration::from_secs(15), async {
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
      if matches!(&event, RunnerEvent::Log {line, ..} if line == "family-started") {
        cancel.cancel();
      }
      events.push(event);
    }
    events
  })
  .await?;
  for file in ["parent.pid", "child.pid", "grandchild.pid"] {
    let pid = std::fs::read_to_string(workspace.join(file))?;
    assert!(!process_alive(&pid)?, "owned {file} still alive: {pid}");
  }
  assert!(
    unrelated.try_wait()?.is_none(),
    "unrelated process was killed"
  );
  unrelated.kill().await?;
  assert_completions(&msg, &events, Conclusion::Cancelled);
  Ok(())
}

#[tokio::test]
async fn workspace_setup_failure_never_runs_user_steps_and_completes_once() -> TestResult {
  let msg = probe_message("exit", true)?;
  let temp = tempfile::tempdir()?;
  let blocked = temp.path().join("workspace-file");
  std::fs::write(&blocked, "not a directory")?;
  let runner = Runner::new(
    RunnerConfig {
      data_dir: temp.path().join("data"),
      workspace_root: blocked,
      ..RunnerConfig::default()
    },
    Arc::new(Mutex::new(SecretMasker::new())),
  );
  let events = collect(runner.execute_job(msg, CancellationToken::new())).await?;
  assert!(
    !events
      .iter()
      .any(|e| matches!(e, RunnerEvent::StepStarted { .. }))
  );
  assert_eq!(
    events
      .iter()
      .filter(|e| matches!(
        e,
        RunnerEvent::JobCompleted {
          conclusion: Conclusion::Failure,
          ..
        }
      ))
      .count(),
    1
  );
  Ok(())
}

#[tokio::test]
async fn real_step_timeout_is_failure_then_conditional_cleanup_runs() -> TestResult {
  let mut msg = probe_message("exit", false)?;
  set_probe_script(
    &mut msg,
    "echo timeout-started\nsleep 300\necho unreachable",
    "success()",
  )?;
  msg.steps.first_mut().ok_or("first")?.timeout_in_minutes = Some(1);
  let (_temp, _workspace, runner) = runner_for(&msg)?;
  let mut rx = runner.execute_job(msg.clone(), CancellationToken::new());
  let events = tokio::time::timeout(Duration::from_secs(80), async {
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
      events.push(event);
    }
    events
  })
  .await?;
  assert_eq!(
    markers(&events),
    ["marker-2", "marker-3", "result=failure/failure", "marker-4"]
  );
  assert!(
    events
      .iter()
      .any(|e| matches!(e, RunnerEvent::Log {line, ..} if line.contains("exceeded its timeout")))
  );
  assert_completions(&msg, &events, Conclusion::Failure);
  Ok(())
}

#[tokio::test]
async fn cancellation_during_real_action_resolution_runs_later_cleanup() -> TestResult {
  use tokio::io::AsyncReadExt;
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
  let url = format!("http://{}", listener.local_addr()?);
  let (sent, mut requests) = tokio::sync::mpsc::channel(16);
  // Deliberate unresponsive service tests a transport failure boundary, never
  // substitutes a mocked successful GitHub response.
  let server = tokio::spawn(async move {
    let mut connections = Vec::new();
    while let Ok((mut socket, _)) = listener.accept().await {
      let mut bytes = [0; 4096];
      let read = socket.read(&mut bytes).await?;
      if read > 0 {
        let _ = sent.send(()).await;
      }
      connections.push(socket);
    }
    Ok::<(), std::io::Error>(())
  });
  let mut msg = probe_message("exit", false)?;
  let captured: AgentJobRequestMessage = serde_json::from_str(CAPTURE)?;
  msg.steps.first_mut().ok_or("first")?.reference = captured
    .steps
    .first()
    .ok_or("captured remote")?
    .reference
    .clone();
  msg.variables.insert(
    "system.github.launch_endpoint".to_owned(),
    serde_json::from_value(json!({"value":url,"isSecret":false}))?,
  );
  let (_temp, _workspace, runner) = runner_for(&msg)?;
  let cancel = CancellationToken::new();
  let mut rx = runner.execute_job(msg.clone(), cancel.clone());
  let first_event = tokio::time::timeout(Duration::from_secs(5), async {
    while let Some(event) = rx.recv().await {
      if matches!(&event, RunnerEvent::StepStarted { .. }) {
        return Some(event);
      }
    }
    None
  })
  .await?;
  assert!(first_event.is_some());
  tokio::time::timeout(Duration::from_secs(5), requests.recv())
    .await?
    .ok_or("no resolver request")?;
  cancel.cancel();
  let events = collect(rx).await?;
  server.abort();
  let _ = server.await;
  assert_eq!(
    markers(&events),
    ["marker-3", "result=cancelled/cancelled", "marker-5"],
    "{events:?}"
  );
  assert_completions(&msg, &events, Conclusion::Cancelled);
  Ok(())
}

//! Real shell summary regressions for issue 83.
use std::error::Error;

use super::file_commands::FileCommandManager;

#[tokio::test]
async fn step_summary_deleted_file_preserves_other_file_commands() -> Result<(), Box<dyn Error>> {
  let dir = tempfile::tempdir()?;
  let (files, env) = FileCommandManager::create(dir.path()).await?;
  let status = std::process::Command::new("bash")
    .args([
      "-c",
      "printf 'answer=42\\n' >> \"$GITHUB_OUTPUT\"; rm \"$GITHUB_STEP_SUMMARY\"",
    ])
    .envs(env)
    .status()?;
  assert!(status.success());
  let result = files.process().await?;
  assert_eq!(result.outputs.get("answer").map(String::as_str), Some("42"));
  Ok(())
}

use shared::{AgentJobRequestMessage, Conclusion, RunnerConfig, RunnerEvent, SecretMasker};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

fn message(scripts: &[&str]) -> Result<AgentJobRequestMessage, Box<dyn Error>> {
  // Existing #68 GitHub acquisition; preserve wire token types and replace only
  // step bodies/count/identities for the labelled summary probes.
  let mut raw: serde_json::Value = serde_json::from_str(include_str!(
    "../../../tests/incoming_contexts_matrix_0.json"
  ))?;
  let template = raw.pointer("/steps/1").ok_or("script template")?.clone();
  let mut steps = Vec::new();
  for (index, script) in scripts.iter().enumerate() {
    let mut step = template.clone();
    let obj = step.as_object_mut().ok_or("step object")?;
    obj.insert(
      "id".to_owned(),
      serde_json::json!(uuid::Uuid::new_v4().to_string()),
    );
    obj.insert(
      "contextName".to_owned(),
      serde_json::json!(format!("summary_{index}")),
    );
    obj.insert(
      "inputs".to_owned(),
      serde_json::json!({"type":2,"map":[
        {"Key":{"type":0,"lit":"script"},"Value":{"type":0,"lit":script}},
        {"Key":{"type":0,"lit":"shell"},"Value":{"type":0,"lit":"bash"}}
      ]}),
    );
    steps.push(step);
  }
  raw
    .as_object_mut()
    .ok_or("job object")?
    .insert("steps".to_owned(), serde_json::Value::Array(steps));
  Ok(serde_json::from_value(raw)?)
}

async fn replay(
  root: &Path,
  msg: AgentJobRequestMessage,
) -> Result<Vec<RunnerEvent>, Box<dyn Error>> {
  let runner = crate::Runner::new(
    RunnerConfig {
      data_dir: root.join("data"),
      workspace_root: root.join("work"),
      workspace_gc_hours: 0,
      ..RunnerConfig::default()
    },
    Arc::new(Mutex::new(SecretMasker::new())),
  );
  let mut rx = runner.execute_job(msg, CancellationToken::new());
  Ok(
    tokio::time::timeout(Duration::from_secs(60), async {
      let mut out = Vec::new();
      while let Some(event) = rx.recv().await {
        out.push(event);
      }
      out
    })
    .await?,
  )
}

#[tokio::test]
async fn step_summary_real_shell_produces_document_without_changing_conclusion()
-> Result<(), Box<dyn Error>> {
  let dir = tempfile::tempdir()?;
  let events = replay(
    dir.path(),
    message(&["printf '# summary-83\\n' >> \"$GITHUB_STEP_SUMMARY\""])?,
  )
  .await?;
  assert!(events.iter().any(|event| matches!(
    event,
    RunnerEvent::JobCompleted {
      conclusion: Conclusion::Success,
      ..
    }
  )));
  // Initially assert the public event stream without referencing the not-yet
  // implemented variant, so the red test demonstrates missing behavior.
  assert!(
    events
      .iter()
      .any(|event| format!("{event:?}").starts_with("StepSummary {")),
    "no summary event"
  );
  Ok(())
}

fn summaries(events: &[RunnerEvent]) -> Vec<(&str, &str)> {
  events
    .iter()
    .filter_map(|event| match event {
      RunnerEvent::StepSummary { step_id, content } => Some((step_id.as_str(), content.as_str())),
      RunnerEvent::JobStarted { .. }
      | RunnerEvent::StepStarted { .. }
      | RunnerEvent::StepCompleted { .. }
      | RunnerEvent::StepSkipped { .. }
      | RunnerEvent::Log { .. }
      | RunnerEvent::LogGroup { .. }
      | RunnerEvent::Annotation { .. }
      | RunnerEvent::JobCompleted { .. } => None,
    })
    .collect()
}

/// Expected summary text: the engine ends every line with the platform newline.
fn platform_lines(text: &str) -> String {
  if cfg!(windows) {
    text.replace('\n', "\r\n")
  } else {
    text.to_owned()
  }
}

#[tokio::test]
async fn step_summary_append_overwrite_absent_deleted_and_failure() -> Result<(), Box<dyn Error>> {
  let dir = tempfile::tempdir()?;
  let events = replay(dir.path(), message(&[
    "printf 'old' > \"$GITHUB_STEP_SUMMARY\"; printf '# first\\n' > \"$GITHUB_STEP_SUMMARY\"; printf 'appended\\n' >> \"$GITHUB_STEP_SUMMARY\"",
    "true",
    "rm \"$GITHUB_STEP_SUMMARY\"; printf 'answer=42\\n' >> \"$GITHUB_OUTPUT\"",
    "printf '# last\\n' > \"$GITHUB_STEP_SUMMARY\"; exit 1",
  ])?).await?;
  let docs = summaries(&events);
  assert_eq!(
    docs.iter().map(|(_, text)| *text).collect::<Vec<_>>(),
    vec![
      platform_lines("# first\nappended\n"),
      platform_lines("# last\n")
    ]
  );
  assert_ne!(
    docs.first().map(|(id, _)| id),
    docs.last().map(|(id, _)| id)
  );
  assert!(events.iter().any(|event| matches!(event, RunnerEvent::StepCompleted { outputs, .. } if outputs.get("answer").map(String::as_str) == Some("42"))));
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
async fn step_summary_utf8_byte_limit_rejects_without_failing_job() -> Result<(), Box<dyn Error>> {
  let dir = tempfile::tempdir()?;
  let events = replay(dir.path(), message(&[
    "python3 -c 'import os; open(os.environ[\"GITHUB_STEP_SUMMARY\"], \"wb\").write(\"é\".encode()*524287+b\"!\\n\")'",
    "python3 -c 'import os; open(os.environ[\"GITHUB_STEP_SUMMARY\"], \"wb\").write(\"é\".encode()*524288+b\"!\")'",
  ])?).await?;
  let docs = summaries(&events);
  assert_eq!(docs.len(), 1);
  let body = docs.first().ok_or("missing boundary summary")?.1;
  assert_eq!(body, platform_lines(&format!("{}!\n", "é".repeat(524_287))));
  assert!(events.iter().any(|event| matches!(event, RunnerEvent::Annotation { level: shared::AnnotationLevel::Error, message, .. } if message.starts_with("$GITHUB_STEP_SUMMARY upload aborted, supports content up to a size of 1024k, got 1024k."))));
  assert!(events.iter().any(|event| matches!(
    event,
    RunnerEvent::JobCompleted {
      conclusion: Conclusion::Success,
      ..
    }
  )));
  Ok(())
}

#[tokio::test]
async fn step_summary_dynamic_masks_and_line_normalization() -> Result<(), Box<dyn Error>> {
  let dir = tempfile::tempdir()?;
  let events = replay(dir.path(), message(&[
    "printf '::add-mask::summary-dynamic-value\\n'; printf '\\357\\273\\277first\\r\\nsummary-dynamic-value\\rlast' > \"$GITHUB_STEP_SUMMARY\"",
  ])?).await?;
  assert_eq!(
    summaries(&events)
      .iter()
      .map(|(_, text)| *text)
      .collect::<Vec<_>>(),
    vec![platform_lines("first\n***\nlast\n")]
  );
  Ok(())
}

#[tokio::test]
async fn step_summary_real_node_and_composite_keep_distinct_documents() -> Result<(), Box<dyn Error>>
{
  use crate::node::runtime::{node_binary_path, node_cache_dir, node_version_for};
  let root = tempfile::tempdir()?;
  let mut raw: serde_json::Value = serde_json::from_str(include_str!(
    "../../../tests/incoming_contexts_matrix_0.json"
  ))?;
  let template = raw.pointer("/steps/3").ok_or("captured action")?.clone();
  let mut steps = Vec::new();
  for action in ["step-summary-83-node", "step-summary-83-composite"] {
    let mut step = template.clone();
    let obj = step.as_object_mut().ok_or("action object")?;
    obj.insert(
      "id".to_owned(),
      serde_json::json!(uuid::Uuid::new_v4().to_string()),
    );
    obj.insert("reference".to_owned(), serde_json::json!({"type":"repository", "repositoryType":"self", "path":format!("./.github/actions/{action}")}));
    steps.push(step);
  }
  raw
    .as_object_mut()
    .ok_or("job object")?
    .insert("steps".to_owned(), steps.into());
  let job: AgentJobRequestMessage = serde_json::from_value(raw)?;
  let workspace = root.path().join("work").join(&job.job_id);
  let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/actions");
  for action in ["step-summary-83-node", "step-summary-83-composite"] {
    let target = workspace.join(".github/actions").join(action);
    std::fs::create_dir_all(&target)?;
    for entry in std::fs::read_dir(source.join(action))? {
      let entry = entry?;
      std::fs::copy(entry.path(), target.join(entry.file_name()))?;
    }
  }
  let node = std::process::Command::new("node")
    .args(["-e", "process.stdout.write(process.execPath)"])
    .output()?;
  assert!(
    node.status.success(),
    "Node must be available for acceptance"
  );
  let binary = node_binary_path(&node_cache_dir(
    &root.path().join("data"),
    node_version_for(20),
  ));
  std::fs::create_dir_all(binary.parent().ok_or("node cache parent")?)?;
  #[cfg(unix)]
  std::os::unix::fs::symlink(String::from_utf8(node.stdout)?.trim(), binary)?;
  #[cfg(not(unix))]
  std::fs::copy(String::from_utf8(node.stdout)?.trim(), binary)?;
  let events = replay(root.path(), job).await?;
  let docs = summaries(&events);
  assert_eq!(
    docs.iter().map(|(_, text)| *text).collect::<Vec<_>>(),
    [
      "# Node pre\n",
      "# Node main\n***\n",
      "# Composite first\nappended\n",
      "# Composite second\n",
      "# Node pre\n",
      "# Node main\n***\n",
      "# Node post\n",
      "# Node post\n",
    ]
    .map(platform_lines)
  );
  let ids: std::collections::HashSet<_> = docs.iter().map(|(id, _)| *id).collect();
  assert_eq!(ids.len(), docs.len());
  for id in ids {
    uuid::Uuid::parse_str(id)?;
  }
  assert!(events.iter().any(|event| matches!(
    event,
    RunnerEvent::JobCompleted {
      conclusion: Conclusion::Success,
      ..
    }
  )));
  Ok(())
}

#[tokio::test]
async fn step_summary_registered_secret_is_masked() -> Result<(), Box<dyn Error>> {
  let dir = tempfile::tempdir()?;
  let mut job =
    message(&["printf '%s\\n' \"${{ secrets.SUMMARY_PROBE }}\" > \"$GITHUB_STEP_SUMMARY\""])?;
  job.variables.insert(
    "SUMMARY_PROBE".to_owned(),
    shared::VariableValue {
      value: "registered-summary-probe".to_owned(),
      is_secret: true,
    },
  );
  let events = replay(dir.path(), job).await?;
  assert_eq!(
    summaries(&events)
      .iter()
      .map(|(_, text)| *text)
      .collect::<Vec<_>>(),
    vec![platform_lines("***\n")]
  );
  Ok(())
}

#[tokio::test]
async fn step_summary_completed_documents_survive_cancellation() -> Result<(), Box<dyn Error>> {
  let root = tempfile::tempdir()?;
  let runner = crate::Runner::new(
    RunnerConfig {
      data_dir: root.path().join("data"),
      workspace_root: root.path().join("work"),
      workspace_gc_hours: 0,
      ..RunnerConfig::default()
    },
    Arc::new(Mutex::new(SecretMasker::new())),
  );
  let cancel = CancellationToken::new();
  let mut rx = runner.execute_job(message(&[
    "printf '# completed\\n' > \"$GITHUB_STEP_SUMMARY\"",
    "printf '# interrupted\\n' > \"$GITHUB_STEP_SUMMARY\"; printf 'summary-cancel-ready\\n'; sleep 60",
  ])?, cancel.clone());
  let events = tokio::time::timeout(Duration::from_secs(30), async {
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
      if matches!(&event, RunnerEvent::Log { line, .. } if line == "summary-cancel-ready") {
        cancel.cancel();
      }
      events.push(event);
    }
    events
  })
  .await?;
  assert_eq!(
    summaries(&events)
      .iter()
      .map(|(_, text)| *text)
      .collect::<Vec<_>>(),
    vec![
      platform_lines("# completed\n"),
      platform_lines("# interrupted\n")
    ]
  );
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
async fn step_summary_read_error_preserves_outputs_and_result() -> Result<(), Box<dyn Error>> {
  let dir = tempfile::tempdir()?;
  let events = replay(dir.path(), message(&[
    "rm \"$GITHUB_STEP_SUMMARY\"; mkdir \"$GITHUB_STEP_SUMMARY\"; printf 'answer=42\\n' > \"$GITHUB_OUTPUT\"",
  ])?).await?;
  assert!(summaries(&events).is_empty());
  assert!(events.iter().any(|event| matches!(event, RunnerEvent::Annotation { level: shared::AnnotationLevel::Error, message, .. } if message.contains("unable to read summary file"))));
  assert!(events.iter().any(|event| matches!(event, RunnerEvent::StepCompleted { conclusion: Conclusion::Success, outputs, .. } if outputs.get("answer").map(String::as_str) == Some("42"))));
  Ok(())
}

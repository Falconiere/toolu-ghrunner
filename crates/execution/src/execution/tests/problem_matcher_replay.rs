//! Real Bash replay of captured acquisition #68 with pinned setup-node matchers.
use shared::{
  AgentJobRequestMessage, AnnotationLevel, Conclusion, RunnerConfig, RunnerEvent, SecretMasker,
};
use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;
const MESSAGE: &str = include_str!("../../../tests/incoming_contexts_matrix_0.json");
const TSC: &str = include_str!("problem_matcher_tsc.json");
const ESLINT: &str = include_str!("problem_matcher_eslint.json");

async fn replay(scripts: &[&str], nested: bool) -> TestResult<Vec<RunnerEvent>> {
  let mut message: AgentJobRequestMessage = serde_json::from_str(MESSAGE)?;
  let mut selected = Vec::new();
  for (step, script) in message
    .steps
    .iter()
    .filter(|step| step.reference.ref_type.as_deref() == Some("script"))
    .zip(scripts)
  {
    let mut step = step.clone();
    let entries = step.inputs.d.as_mut().ok_or("no input map")?;
    entries.retain(|entry| entry.key.to_string_value() == Some("script"));
    let token = &mut entries.first_mut().ok_or("no script")?.value;
    token.token_type = 0;
    token.expr = None;
    token.lit = Some((*script).to_owned());
    selected.push(step);
  }
  assert_eq!(selected.len(), scripts.len());
  if nested {
    let mut step = message
      .steps
      .iter()
      .find(|step| step.context_name.as_deref() == Some("__self"))
      .ok_or("no action")?
      .clone();
    step.reference.path = Some("./.github/actions/matcher-84".to_owned());
    selected.push(step);
  }
  message.steps = selected;
  message.defaults.clear();
  let temp = tempfile::tempdir()?;
  let config = RunnerConfig {
    data_dir: temp.path().join("data"),
    workspace_root: temp.path().join("work"),
    workspace_gc_hours: 0,
    ..RunnerConfig::default()
  };
  let workspace = config.workspace_root.join(&message.job_id);
  std::fs::create_dir_all(workspace.join("src"))?;
  std::fs::create_dir_all(workspace.join(".git"))?;
  std::fs::write(
    workspace.join(".git/config"),
    "[remote \"origin\"]\n url = https://github.com/Falconiere/toolu-ghrunner\n",
  )?;
  std::fs::write(
    workspace.join("src/example.ts"),
    "const value: string = 42;\n",
  )?;
  std::fs::write(
    workspace.join("src/example.js"),
    "var unused = 1;\nconsole.log(missing);\n",
  )?;
  std::fs::write(workspace.join("tsc.json"), TSC)?;
  let mut from_path: serde_json::Value = serde_json::from_str(TSC)?;
  from_path
    .get_mut("problemMatcher")
    .and_then(|matchers| matchers.get_mut(0))
    .and_then(serde_json::Value::as_object_mut)
    .ok_or("missing captured matcher")?
    .insert("fromPath".to_owned(), "package/tsconfig.json".into());
  std::fs::write(
    workspace.join("from-path.json"),
    serde_json::to_vec(&from_path)?,
  )?;
  std::fs::create_dir_all(workspace.join("package/src"))?;
  std::fs::copy(
    workspace.join("src/example.ts"),
    workspace.join("package/src/example.ts"),
  )?;
  std::fs::write(workspace.join("eslint.json"), ESLINT)?;
  std::fs::write(
    workspace.join("tsc.txt"),
    include_str!("problem_matcher_tsc.txt"),
  )?;
  std::fs::write(
    workspace.join("eslint.txt"),
    include_str!("problem_matcher_eslint.txt").replace("@WORKSPACE@", &workspace.to_string_lossy()),
  )?;
  std::fs::write(workspace.join("invalid.json"), "{invalid")?;
  if nested {
    let action = workspace.join(".github/actions/matcher-84");
    std::fs::create_dir_all(&action)?;
    std::fs::write(
      action.join("action.yml"),
      "name: Matcher probe\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      run: cat tsc.txt >&2\n",
    )?;
  }
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  let runner = crate::Runner::new(config, masker);
  let mut rx = runner.execute_job(message, CancellationToken::new());
  Ok(
    tokio::time::timeout(Duration::from_secs(30), async {
      let mut events = Vec::new();
      while let Some(event) = rx.recv().await {
        events.push(event);
      }
      events
    })
    .await?,
  )
}

#[tokio::test]
async fn problem_matcher_real_tsc_stderr_and_nested_scope() -> TestResult {
  let events = replay(&["echo '::add-matcher::tsc.json'", "cat tsc.txt >&2"], true).await?;
  let annotations: Vec<_> = events
    .iter()
    .filter(|event| matches!(event, RunnerEvent::Annotation { .. }))
    .collect();
  assert_eq!(annotations.len(), 2, "{events:?}");
  for (annotation, expected_id) in annotations.iter().zip([
    "543c9d5a-7af8-40ad-a1e9-90062b03e0f9",
    "f8bcd552-f40d-4cdc-94de-8f9fbf4dd191",
  ]) {
    assert!(
      matches!(annotation, RunnerEvent::Annotation { step_id, level: AnnotationLevel::Error, message, file, line: Some(1), col: Some(7), .. } if step_id == expected_id && message == "Type 'number' is not assignable to type 'string'." && file.as_deref() == Some("src/example.ts")),
      "{annotation:?}"
    );
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
async fn problem_matcher_real_eslint_loop_and_remove() -> TestResult {
  let events = replay(
    &[
      "echo '::add-matcher::eslint.json'",
      "cat eslint.txt; echo '::remove-matcher owner=ESLINT-STYLISH::'; cat eslint.txt",
    ],
    false,
  )
  .await?;
  let annotations: Vec<_> = events
    .iter()
    .filter(|event| matches!(event, RunnerEvent::Annotation { .. }))
    .collect();
  assert_eq!(annotations.len(), 2, "{events:?}");
  assert!(
    matches!(annotations.first(), Some(RunnerEvent::Annotation { level: AnnotationLevel::Warning, file, line: Some(1), col: Some(5), .. }) if file.as_deref() == Some("src/example.js"))
  );
  assert!(
    matches!(annotations.get(1), Some(RunnerEvent::Annotation { level: AnnotationLevel::Error, file, line: Some(2), col: Some(13), .. }) if file.as_deref() == Some("src/example.js"))
  );
  Ok(())
}

#[tokio::test]
async fn problem_matcher_bad_json_fails_command_step() -> TestResult {
  let events = replay(&["echo '::add-matcher::invalid.json'"], false).await?;
  assert!(
    events.iter().any(|event| matches!(
      event,
      RunnerEvent::JobCompleted {
        conclusion: Conclusion::Failure,
        ..
      }
    )),
    "{events:?}"
  );
  assert!(events.iter().any(|event| matches!(
    event,
    RunnerEvent::Annotation {
      level: AnnotationLevel::Error,
      ..
    }
  )));
  Ok(())
}

#[tokio::test]
async fn problem_matcher_from_path_ignores_process_cwd() -> TestResult {
  let events = replay(
    &["echo '::add-matcher::from-path.json'; cd src; cat ../tsc.txt"],
    false,
  )
  .await?;
  assert!(events.iter().any(|event| matches!(event, RunnerEvent::Annotation { file, .. } if file.as_deref() == Some("package/src/example.ts"))), "{events:?}");
  Ok(())
}

#[tokio::test]
async fn problem_matcher_masks_real_diagnostic_and_file_removal() -> TestResult {
  let events = replay(&["echo '::add-matcher::tsc.json'; echo '::add-mask::number'; cat tsc.txt; echo '::remove-matcher::tsc.json'; cat tsc.txt"], false).await?;
  let annotations: Vec<_> = events
    .iter()
    .filter_map(|event| {
      if let RunnerEvent::Annotation { message, .. } = event {
        Some(message)
      } else {
        None
      }
    })
    .collect();
  assert_eq!(
    annotations,
    ["Type '***' is not assignable to type 'string'."]
  );
  Ok(())
}

#[tokio::test]
async fn problem_matcher_partial_loop_does_not_cross_steps() -> TestResult {
  let events = replay(
    &[
      "echo '::add-matcher::eslint.json'; head -n 2 eslint.txt",
      "head -n 3 eslint.txt | tail -n 1; cat eslint.txt",
    ],
    false,
  )
  .await?;
  assert_eq!(
    events
      .iter()
      .filter(|event| matches!(event, RunnerEvent::Annotation { .. }))
      .count(),
    2,
    "{events:?}"
  );
  Ok(())
}

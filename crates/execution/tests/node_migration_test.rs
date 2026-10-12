//! Production replay of upstream's node20 → node24 migration (#95).
//!
//! The captured github.com job message `job_outputs_message.json` carries
//! GitHub's live flags (`actions.runner.requirenode24 = "true"`,
//! `usenode24bydefault = "true"`, `warnonnode20 = "true"`). Its steps are
//! replaced by the committed `node20` probe `.github/actions/node-runtime-95`
//! (also run by the official-runner lane), which records `process.execPath`;
//! everything else stays as captured. The real host `node` is copied into the
//! cache dir of the pin that should be selected, so `execPath` names the
//! runtime the engine chose.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use execution::Runner;
use execution::node::runtime::{node_binary_path, node_cache_dir, node_version_for};
use shared::{
  ActionStep, AgentJobRequestMessage, AnnotationLevel, Conclusion, RunnerConfig, RunnerEvent,
  SecretMasker, TemplateToken, VariableValue,
};
use tokio_util::sync::CancellationToken;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const CAPTURE: &str = include_str!("job_outputs_message.json");
/// The same probe the official-runner lane runs (`.github/workflows/node-runtime-95.yml`).
const ACTION_YML: &str = include_str!("../../../.github/actions/node-runtime-95/action.yml");
const ACTION_JS: &str = include_str!("../../../.github/actions/node-runtime-95/index.js");
const URL: &str =
  "https://github.blog/changelog/2025-09-19-deprecation-of-node-20-on-github-actions-runners/";

struct Replay {
  events: Vec<RunnerEvent>,
  exec_path: PathBuf,
  data: PathBuf,
  _dir: tempfile::TempDir,
}

fn host_node() -> TestResult<PathBuf> {
  let output = std::process::Command::new("node")
    .args(["-e", "process.stdout.write(process.execPath)"])
    .output()?;
  if !output.status.success() {
    return Err("a real Node.js on PATH is required".into());
  }
  Ok(PathBuf::from(String::from_utf8(output.stdout)?))
}

/// Copy the real node into `real` and symlink it into `other`: only the
/// copy reports its own path as `process.execPath`.
fn seed(data: &Path, real: u8, other: u8) -> TestResult {
  let node = host_node()?;
  for (major, copy) in [(real, true), (other, false)] {
    let target = node_binary_path(&node_cache_dir(data, node_version_for(major)));
    std::fs::create_dir_all(target.parent().ok_or("cache parent")?)?;
    if copy {
      std::fs::copy(&node, &target)?;
    } else {
      #[cfg(unix)]
      std::os::unix::fs::symlink(&node, &target)?;
      #[cfg(not(unix))]
      std::fs::copy(&node, &target)?;
    }
  }
  Ok(())
}

fn probe_step(environment: Option<TemplateToken>) -> ActionStep {
  let mut step = ActionStep::with_ref_type("node_probe", "repository");
  step.reference.path = Some("./node action".to_owned());
  step.reference.repository_type = Some("self".to_owned());
  step.environment = environment;
  step
}

async fn replay(message: AgentJobRequestMessage, selected: u8, other: u8) -> TestResult<Replay> {
  let dir = tempfile::tempdir()?;
  let data = dir.path().join("data");
  let config = RunnerConfig {
    data_dir: data.clone(),
    workspace_root: dir.path().join("work"),
    workspace: shared::WorkspaceConfig {
      mode: shared::WorkspaceMode::Fresh,
      stable_gc_hours: 0,
    },
    ..RunnerConfig::default()
  };
  seed(&data, selected, other)?;
  let workspace = config.workspace_root.join(&message.job_id);
  let action = workspace.join("node action");
  std::fs::create_dir_all(&action)?;
  std::fs::write(action.join("action.yml"), ACTION_YML)?;
  std::fs::write(action.join("index.js"), ACTION_JS)?;
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let mut stream = runner.execute_job(message, CancellationToken::new());
  let events = tokio::time::timeout(Duration::from_secs(120), async {
    let mut events = Vec::new();
    while let Some(event) = stream.recv().await {
      events.push(event);
    }
    events
  })
  .await?;
  let observed: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
    workspace.join("node-runtime-95.json"),
  )?)?;
  let exec_path = PathBuf::from(
    observed
      .get("execPath")
      .and_then(serde_json::Value::as_str)
      .ok_or("probe recorded no execPath")?,
  );
  Ok(Replay {
    events,
    exec_path,
    data,
    _dir: dir,
  })
}

fn conclusion(events: &[RunnerEvent]) -> Option<Conclusion> {
  events.iter().find_map(|event| {
    if let RunnerEvent::JobCompleted { conclusion, .. } = event {
      Some(*conclusion)
    } else {
      None
    }
  })
}

fn warnings(events: &[RunnerEvent]) -> Vec<&str> {
  events
    .iter()
    .filter_map(|event| {
      if let RunnerEvent::Annotation {
        level: AnnotationLevel::Warning,
        message,
        ..
      } = event
      {
        Some(message.as_str())
      } else {
        None
      }
    })
    .collect()
}

fn ran_on(replay: &Replay, major: u8) -> TestResult<bool> {
  let expected = node_binary_path(&node_cache_dir(&replay.data, node_version_for(major)));
  Ok(replay.exec_path.canonicalize()? == expected.canonicalize()?)
}

#[tokio::test]
async fn captured_require_node24_runs_a_node20_action_on_node24() -> TestResult {
  let mut message: AgentJobRequestMessage = serde_json::from_str(CAPTURE)?;
  message.steps = vec![probe_step(None)];
  let replay = replay(message, 24, 20).await?;
  assert_eq!(
    conclusion(&replay.events),
    Some(Conclusion::Success),
    "{:?}",
    replay.events
  );
  assert!(ran_on(&replay, 24)?, "ran {}", replay.exec_path.display());
  assert_eq!(
    warnings(&replay.events),
    vec![format!(
      "Node.js 20 is deprecated. The following actions target Node.js 20 but are being forced to \
       run on Node.js 24: ./node action. For more information see: {URL}"
    )]
  );
  Ok(())
}

#[tokio::test]
async fn phase_two_with_workflow_opt_out_stays_on_node20_and_warns_deprecation() -> TestResult {
  let mut message: AgentJobRequestMessage = serde_json::from_str(CAPTURE)?;
  // Same captured wire structure; only the server phase is moved back to 2.
  message.variables.insert(
    "actions.runner.requirenode24".to_owned(),
    VariableValue {
      value: "false".to_owned(),
      is_secret: false,
    },
  );
  let opt_out: TemplateToken = serde_json::from_value(serde_json::json!({
    "type": 2,
    "map": [{
      "key": { "type": 0, "lit": "ACTIONS_ALLOW_USE_UNSECURE_NODE_VERSION" },
      "value": { "type": 0, "lit": "true" }
    }]
  }))?;
  message.steps = vec![probe_step(Some(opt_out))];
  let replay = replay(message, 20, 24).await?;
  assert_eq!(
    conclusion(&replay.events),
    Some(Conclusion::Success),
    "{:?}",
    replay.events
  );
  assert!(ran_on(&replay, 20)?, "ran {}", replay.exec_path.display());
  assert_eq!(
    warnings(&replay.events),
    vec![format!(
      "Node.js 20 actions are deprecated. The following actions are running on Node.js 20 and \
       may not work as expected: ./node action. Actions will be forced to run with Node.js 24 by \
       default starting June 16th, 2026. Node.js 20 will be removed from the runner on \
       September 23rd, 2026. Please check if updated versions of these actions are available \
       that support Node.js 24. To opt into Node.js 24 now, set the \
       FORCE_JAVASCRIPT_ACTIONS_TO_NODE24=true environment variable on the runner or in your \
       workflow file. Once Node.js 24 becomes the default, you can temporarily opt out by \
       setting ACTIONS_ALLOW_USE_UNSECURE_NODE_VERSION=true. For more information see: {URL}"
    )]
  );
  Ok(())
}

#[tokio::test]
async fn conflicting_step_switches_warn_on_the_step_and_print_the_phase_two_notice() -> TestResult {
  let mut message: AgentJobRequestMessage = serde_json::from_str(CAPTURE)?;
  // Phase 2 (captured `usenode24bydefault = "true"`, `requirenode24` moved back).
  message.variables.insert(
    "actions.runner.requirenode24".to_owned(),
    VariableValue {
      value: "false".to_owned(),
      is_secret: false,
    },
  );
  let both: TemplateToken = serde_json::from_value(serde_json::json!({
    "type": 2,
    "map": [
      {
        "key": { "type": 0, "lit": "FORCE_JAVASCRIPT_ACTIONS_TO_NODE24" },
        "value": { "type": 0, "lit": "true" }
      },
      {
        "key": { "type": 0, "lit": "ACTIONS_ALLOW_USE_UNSECURE_NODE_VERSION" },
        "value": { "type": 0, "lit": "true" }
      }
    ]
  }))?;
  let step = probe_step(Some(both));
  let step_id = step.id.clone();
  message.steps = vec![step];
  let replay = replay(message, 24, 20).await?;
  assert_eq!(
    conclusion(&replay.events),
    Some(Conclusion::Success),
    "{:?}",
    replay.events
  );
  assert!(ran_on(&replay, 24)?, "ran {}", replay.exec_path.display());
  let conflict = "Both FORCE_JAVASCRIPT_ACTIONS_TO_NODE24 and ACTIONS_ALLOW_USE_UNSECURE_NODE_VERSION \
                  environment variables are set to true in the workflow environment. This is \
                  likely a configuration error. Using the default Node version: node24.";
  let step_warnings: Vec<&str> = replay
    .events
    .iter()
    .filter_map(|event| {
      if let RunnerEvent::Annotation {
        step_id: id,
        level: AnnotationLevel::Warning,
        message,
        ..
      } = event
        && id == &step_id
      {
        Some(message.as_str())
      } else {
        None
      }
    })
    .collect();
  assert_eq!(step_warnings, vec![conflict]);
  let step_logs: Vec<&str> = replay
    .events
    .iter()
    .filter_map(|event| {
      if let RunnerEvent::Log {
        step_id: id, line, ..
      } = event
        && id == &step_id
      {
        Some(line.as_str())
      } else {
        None
      }
    })
    .collect();
  assert!(
    step_logs.contains(&format!("##[warning]{conflict}").as_str()),
    "{step_logs:#?}"
  );
  let notice = format!(
    "Node 20 is being deprecated. This workflow is running with Node 24 by default. If you need \
     to temporarily use Node 20, you can set the ACTIONS_ALLOW_USE_UNSECURE_NODE_VERSION=true \
     environment variable. For more information see: {URL}"
  );
  assert!(step_logs.contains(&notice.as_str()), "{step_logs:#?}");
  Ok(())
}

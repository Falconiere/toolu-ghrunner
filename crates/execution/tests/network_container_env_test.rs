//! #94 AC-7: the proxy export reaches a real job container (and the `run:`
//! steps exec'd into it), a real service container and real Docker actions —
//! each keeping its own value when its `env` sets one, and none receiving
//! `NODE_EXTRA_CA_CERTS` (a host path). Docker lane: needs Linux, real Docker
//! and a daemon-shared `TOOLU_CONTAINER_TEST_ROOT`.
#![cfg(target_os = "linux")]

#[path = "../../network/tests/helpers/network_fixtures.rs"]
mod network_fixtures;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use execution::Runner;
use network_fixtures::{FixtureResult, Pki};
use shared::{
  ActionStep, AgentJobRequestMessage, Conclusion, DictEntry, NetworkConfig, RunnerConfig,
  RunnerEvent, SecretMasker, TemplateToken,
};
use tokio_util::sync::CancellationToken;

const CAPTURE: &str = include_str!("../../toolu-runner/tests/fixtures/job_container_message.json");
const ALPINE: &str =
  "alpine@sha256:ce64758a109eb420d874a118f87920e625e12d3634e03b4a5573fd9f6e5d3507";
const NGINX: &str =
  "nginx@sha256:65645c7bb6a0661892a8b03b89d0743208a18dd2f3f17a54ef4b76fb8e2f2a10";
const PROXY: &str = "http://proxy.corp.test:3128";

fn literal(value: &str) -> TemplateToken {
  TemplateToken {
    token_type: 0,
    lit: Some(value.to_owned()),
    ..TemplateToken::default()
  }
}

fn token_map(entries: &[(&str, &str)]) -> TemplateToken {
  TemplateToken {
    token_type: 2,
    d: Some(
      entries
        .iter()
        .map(|(key, value)| DictEntry {
          key: literal(key),
          value: literal(value),
        })
        .collect(),
    ),
    ..TemplateToken::default()
  }
}

/// The captured job-container acquisition with `HTTP_PROXY` added to the
/// job container's own `env` (it must win over the export).
fn captured_job() -> FixtureResult<AgentJobRequestMessage> {
  let mut raw: serde_json::Value = serde_json::from_str(CAPTURE)?;
  let env = raw
    .pointer_mut("/jobContainer/map")
    .and_then(serde_json::Value::as_array_mut)
    .and_then(|entries| {
      entries
        .iter_mut()
        .find(|entry| entry.pointer("/Key/lit").and_then(|v| v.as_str()) == Some("env"))
    })
    .and_then(|entry| entry.pointer_mut("/Value/map"))
    .and_then(serde_json::Value::as_array_mut)
    .ok_or("captured job container env map absent")?;
  env.push(serde_json::json!({
    "Key": {"type": 0, "lit": "HTTP_PROXY"},
    "Value": {"type": 0, "lit": "http://container-own.test:1"}
  }));
  Ok(serde_json::from_value(raw)?)
}

/// One service whose health check passes only when its env carries the
/// export (both casings), keeps its own `NO_PROXY`, and lacks
/// `NODE_EXTRA_CA_CERTS` — a failed check fails the job.
fn service_token() -> FixtureResult<TemplateToken> {
  let health = format!(
    "--health-cmd 'test \"$HTTPS_PROXY\" = {PROXY} && test \"$https_proxy\" = {PROXY} && test \"$NO_PROXY\" = service-own && test -z \"$NODE_EXTRA_CA_CERTS\"' --health-interval 1s --health-retries 5"
  );
  Ok(serde_json::from_value(serde_json::json!({"type":2,"map":[
    {"key":{"type":0,"lit":"web"},"value":{"type":2,"map":[
      {"key":{"type":0,"lit":"image"},"value":{"type":0,"lit":NGINX}},
      {"key":{"type":0,"lit":"env"},"value":{"type":2,"map":[
        {"key":{"type":0,"lit":"NO_PROXY"},"value":{"type":0,"lit":"service-own"}}
      ]}},
      {"key":{"type":0,"lit":"options"},"value":{"type":0,"lit":health}}
    ]}}
  ]}))?)
}

fn docker_env_step(id: &str) -> ActionStep {
  let mut step = ActionStep::with_ref_type(id, "containerRegistry");
  step.reference.image = Some(format!("docker://{ALPINE}"));
  step.inputs = token_map(&[("entrypoint", "env"), ("args", "")]);
  step
}

fn linux_root() -> FixtureResult<tempfile::TempDir> {
  let base = std::env::var_os("TOOLU_CONTAINER_TEST_ROOT")
    .map(PathBuf::from)
    .ok_or("TOOLU_CONTAINER_TEST_ROOT must be shared with the Docker daemon")?;
  Ok(tempfile::Builder::new().prefix("net94 ").tempdir_in(base)?)
}

async fn collect_lines(
  config: RunnerConfig,
  job: AgentJobRequestMessage,
) -> FixtureResult<(Option<Conclusion>, Vec<String>)> {
  let runner = Runner::new(config, Arc::new(Mutex::new(SecretMasker::new())));
  let mut receiver = runner.execute_job(job, CancellationToken::new());
  let mut conclusion = None;
  let mut lines = Vec::new();
  tokio::time::timeout(Duration::from_secs(600), async {
    while let Some(event) = receiver.recv().await {
      if let RunnerEvent::Log { line, .. } = &event {
        lines.push(line.clone());
      } else if let RunnerEvent::JobCompleted { conclusion: c, .. } = &event {
        conclusion = Some(*c);
      }
    }
  })
  .await?;
  Ok((conclusion, lines))
}

fn has(lines: &[String], wanted: &str) -> bool {
  lines.iter().any(|line| line == wanted)
}

#[tokio::test]
#[ignore = "requires Linux, real Docker, and a daemon-shared test root"]
async fn containers_receive_the_proxy_export_unless_they_set_it() -> FixtureResult<()> {
  let root = linux_root()?;
  let pki = Pki::new()?;
  let config = RunnerConfig {
    data_dir: root.path().join("data"),
    workspace_root: root.path().join("work"),
    workspace_gc_hours: 0,
    workspace: shared::WorkspaceConfig {
      mode: shared::WorkspaceMode::Fresh,
      stable_gc_hours: 0,
    },
    network: NetworkConfig {
      proxy: Some(PROXY.to_owned()),
      no_proxy: vec!["internal.corp".to_owned()],
      ca_bundle: Some(pki.write_bundle(root.path())?),
    },
    ..RunnerConfig::default()
  };
  let mut job = captured_job()?;
  job.job_service_containers = Some(service_token()?);
  let mut overridden = docker_env_step("docker_override");
  overridden.environment = Some(token_map(&[("HTTPS_PROXY", "http://step-own.test:2")]));
  job.steps = vec![
    ActionStep::script(
      "in_container",
      "for v in HTTPS_PROXY https_proxy HTTP_PROXY http_proxy NO_PROXY NODE_EXTRA_CA_CERTS; do printf 'NET94|container|%s|%s\\n' \"$v\" \"${!v-unset}\"; done",
      "success()",
    ),
    docker_env_step("docker_plain"),
    overridden,
  ];
  let (conclusion, lines) = collect_lines(config, job).await?;
  assert_eq!(conclusion, Some(Conclusion::Success), "{lines:?}");

  let bypass = "internal.corp,localhost,127.0.0.1,::1";
  for (name, value) in [
    ("HTTPS_PROXY", PROXY),
    ("https_proxy", PROXY),
    ("HTTP_PROXY", "http://container-own.test:1"),
    ("http_proxy", PROXY),
    ("NO_PROXY", bypass),
    ("NODE_EXTRA_CA_CERTS", "unset"),
  ] {
    assert!(
      has(&lines, &format!("NET94|container|{name}|{value}")),
      "job container {name}={value}: {lines:?}"
    );
  }
  assert!(has(&lines, &format!("HTTPS_PROXY={PROXY}")), "{lines:?}");
  assert!(has(&lines, "HTTPS_PROXY=http://step-own.test:2"), "{lines:?}");
  assert!(has(&lines, &format!("https_proxy={PROXY}")), "{lines:?}");
  assert!(has(&lines, &format!("no_proxy={bypass}")), "{lines:?}");
  assert!(
    !lines.iter().any(|line| line.starts_with("NODE_EXTRA_CA_CERTS=")),
    "{lines:?}"
  );
  Ok(())
}

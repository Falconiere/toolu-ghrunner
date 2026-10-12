//! #94 AC-5 / AC-6 / AC-9: the job's `[network]` export reaches real host
//! steps through the production engine (`Runner::execute_job`) — `run:`
//! steps, a job hook, a local Node action's main and post stages, and Node's
//! TLS trust — under workflow env, with the proxy password masked.

#[path = "../../network/tests/helpers/tls_fixtures.rs"]
mod tls_fixtures;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use execution::Runner;
use execution::node::runtime::{node_binary_path, node_cache_dir, node_version_for};
use shared::{
  AgentJobRequestMessage, Conclusion, DictEntry, NetworkConfig, RunnerConfig, RunnerEvent,
  SecretMasker, TemplateToken, VariableValue,
};
use tls_fixtures::{FixtureResult, Leaf, Pki, TlsTarget};
use tokio_util::sync::CancellationToken;

/// The sanitized #68 GitHub.com acquisition (a `run:` step and a local action).
const CAPTURE: &str = include_str!("incoming_contexts_matrix_0.json");
/// Variables every test controls; the host's own values must not leak in.
const AMBIENT: [&str; 10] = [
  "HTTPS_PROXY",
  "https_proxy",
  "HTTP_PROXY",
  "http_proxy",
  "ALL_PROXY",
  "all_proxy",
  "NO_PROXY",
  "no_proxy",
  "NODE_EXTRA_CA_CERTS",
  "TOOLU_RUNNER_PROBE",
];
/// What the probes print, in order.
const PROBED: [&str; 8] = [
  "HTTPS_PROXY",
  "https_proxy",
  "HTTP_PROXY",
  "http_proxy",
  "NO_PROXY",
  "no_proxy",
  "NODE_EXTRA_CA_CERTS",
  "TOOLU_RUNNER_PROBE",
];

fn literal(value: &str) -> TemplateToken {
  TemplateToken {
    token_type: 0,
    lit: Some(value.to_owned()),
    ..TemplateToken::default()
  }
}

fn mapping(values: &[(&str, &str)]) -> TemplateToken {
  TemplateToken {
    token_type: 2,
    d: Some(
      values
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

/// The captured job reduced to its `run:` step (and, with `node`, its local
/// action step pointed at `./.github/actions/net-94`).
fn job(script: &str, node: bool) -> FixtureResult<AgentJobRequestMessage> {
  let mut job: AgentJobRequestMessage = serde_json::from_str(CAPTURE)?;
  job.steps.retain(|step| {
    matches!(step.context_name.as_deref(), Some("__run"))
      || (node && matches!(step.context_name.as_deref(), Some("__self")))
  });
  let run = job.steps.first_mut().ok_or("captured run step absent")?;
  let entry = run
    .inputs
    .d
    .as_mut()
    .and_then(|entries| {
      entries
        .iter_mut()
        .find(|entry| entry.key.to_string_value() == Some("script"))
    })
    .ok_or("captured script token absent")?;
  entry.value = literal(script);
  if node {
    let action = job.steps.get_mut(1).ok_or("captured action step absent")?;
    action.reference.path = Some("./.github/actions/net-94".to_owned());
  }
  Ok(job)
}

/// A bash probe printing `NET94|<tag>|<name>|<value or unset>` per variable.
fn probe(tag: &str) -> String {
  PROBED
    .iter()
    .map(|name| format!("printf 'NET94|{tag}|{name}|%s\\n' \"${{{name}-unset}}\""))
    .collect::<Vec<_>>()
    .join("\n")
}

struct Harness {
  root: tempfile::TempDir,
  config: RunnerConfig,
  masker: Arc<Mutex<SecretMasker>>,
}

impl Harness {
  async fn new(network: NetworkConfig) -> FixtureResult<Self> {
    let root = tempfile::tempdir()?;
    let config = RunnerConfig {
      data_dir: root.path().join("data"),
      workspace_root: root.path().join("work"),
      workspace_gc_hours: 0,
      workspace: shared::WorkspaceConfig {
        mode: shared::WorkspaceMode::Fresh,
        stable_gc_hours: 0,
      },
      network,
      ..RunnerConfig::default()
    };
    seed_node(&config.data_dir).await?;
    Ok(Self {
      root,
      config,
      masker: Arc::new(Mutex::new(SecretMasker::new())),
    })
  }

  fn path(&self, name: &str) -> PathBuf {
    self.root.path().join(name)
  }

  /// Install the local Node action whose main and post print `HTTPS_PROXY`.
  fn install_node_action(&self, job: &AgentJobRequestMessage) -> FixtureResult<()> {
    let dir = self
      .config
      .workspace_root
      .join(&job.job_id)
      .join(".github/actions/net-94");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
      dir.join("action.yml"),
      "name: net-94\ndescription: prints the proxy export\nruns:\n  using: node20\n  main: main.js\n  post: post.js\n",
    )?;
    for stage in ["main", "post"] {
      std::fs::write(
        dir.join(format!("{stage}.js")),
        format!(
          "console.log('NET94|{stage}|HTTPS_PROXY|' + (process.env.HTTPS_PROXY ?? 'unset'));\n"
        ),
      )?;
    }
    Ok(())
  }

  /// Install the local composite action whose single `run:` step prints the
  /// full [`probe`] under the `comp` tag.
  fn install_composite_action(&self, job: &AgentJobRequestMessage) -> FixtureResult<()> {
    let dir = self
      .config
      .workspace_root
      .join(&job.job_id)
      .join(".github/actions/net-94");
    std::fs::create_dir_all(&dir)?;
    let body = probe("comp")
      .lines()
      .map(|line| format!("        {line}"))
      .collect::<Vec<_>>()
      .join("\n");
    std::fs::write(
      dir.join("action.yml"),
      format!(
        "name: net-94\ndescription: prints the proxy export\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      run: |\n{body}\n"
      ),
    )?;
    Ok(())
  }

  /// Run `job` with exactly `env` set among [`AMBIENT`]; returns its log lines.
  async fn run(
    &self,
    job: AgentJobRequestMessage,
    env: &[(&'static str, &'static str)],
  ) -> FixtureResult<(Option<Conclusion>, Vec<String>)> {
    let vars: Vec<(&str, Option<&str>)> = AMBIENT
      .iter()
      .map(|name| (*name, env.iter().find(|(k, _)| k == name).map(|(_, v)| *v)))
      .collect();
    let runner = Runner::new(self.config.clone(), Arc::clone(&self.masker));
    temp_env::async_with_vars(vars, async move {
      let mut stream = runner.execute_job(job, CancellationToken::new());
      let mut conclusion = None;
      let mut lines = Vec::new();
      tokio::time::timeout(Duration::from_secs(120), async {
        while let Some(event) = stream.recv().await {
          if let RunnerEvent::Log { line, .. } = &event {
            lines.push(line.clone());
          } else if let RunnerEvent::JobCompleted { conclusion: c, .. } = &event {
            conclusion = Some(*c);
          }
        }
      })
      .await?;
      Ok((conclusion, lines))
    })
    .await
  }
}

async fn seed_node(data: &Path) -> FixtureResult<()> {
  let output = tokio::process::Command::new("node")
    .args(["-e", "process.stdout.write(process.execPath)"])
    .output()
    .await?;
  assert!(output.status.success(), "Node is required for this test");
  let host_binary = String::from_utf8(output.stdout)?;
  // The captured message requires node24 (#95 migrates node20 actions), so
  // both cache slots point at the host binary.
  for major in [20, 24] {
    let target = node_binary_path(&node_cache_dir(data, node_version_for(major)));
    std::fs::create_dir_all(target.parent().ok_or("node cache parent missing")?)?;
    std::os::unix::fs::symlink(host_binary.trim(), target)?;
  }
  Ok(())
}

/// The value a `NET94|<tag>|<name>|<value>` line reported.
fn reported<'a>(lines: &'a [String], tag: &str, name: &str) -> Option<&'a str> {
  let prefix = format!("NET94|{tag}|{name}|");
  lines.iter().find_map(|line| line.strip_prefix(&prefix))
}

const PROXY: &str = "http://proxy.corp.test:3128";

#[tokio::test]
async fn host_steps_and_hooks_get_the_config_export_over_the_runner_env() -> FixtureResult<()> {
  let pki = Pki::new()?;
  let dir = tempfile::tempdir()?;
  let bundle = pki.write_bundle(dir.path())?;
  let h = Harness::new(NetworkConfig {
    proxy: Some(PROXY.to_owned()),
    no_proxy: vec!["internal.corp".to_owned()],
    ca_bundle: Some(bundle.clone()),
  })
  .await?;
  let hook = h.path("hook.sh");
  std::fs::write(&hook, format!("#!/bin/bash\n{}\n", probe("hook")))?;
  let mut job = job(&probe("run"), false)?;
  job.variables.insert(
    "ACTIONS_RUNNER_HOOK_JOB_STARTED".to_owned(),
    VariableValue {
      value: hook.to_string_lossy().into_owned(),
      is_secret: false,
    },
  );
  let (conclusion, lines) = h
    .run(
      job,
      &[
        ("HTTPS_PROXY", "http://ambient.test:9"),
        ("NO_PROXY", "ambient-only"),
        ("TOOLU_RUNNER_PROBE", "must-not-leak"),
      ],
    )
    .await?;
  assert_eq!(conclusion, Some(Conclusion::Success), "{lines:?}");
  let bypass = "internal.corp,localhost,127.0.0.1,::1";
  let bundle = bundle.display().to_string();
  for tag in ["run", "hook"] {
    for name in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"] {
      assert_eq!(
        reported(&lines, tag, name),
        Some(PROXY),
        "{tag} {name}: {lines:?}"
      );
    }
    assert_eq!(reported(&lines, tag, "NO_PROXY"), Some(bypass), "{tag}");
    assert_eq!(reported(&lines, tag, "no_proxy"), Some(bypass), "{tag}");
    assert_eq!(
      reported(&lines, tag, "NODE_EXTRA_CA_CERTS"),
      Some(bundle.as_str())
    );
    assert_eq!(
      reported(&lines, tag, "TOOLU_RUNNER_PROBE"),
      Some("unset"),
      "{tag}"
    );
  }
  Ok(())
}

#[tokio::test]
async fn a_step_env_value_beats_the_export() -> FixtureResult<()> {
  let h = Harness::new(NetworkConfig {
    proxy: Some(PROXY.to_owned()),
    ..NetworkConfig::default()
  })
  .await?;
  let mut job = job(&probe("run"), false)?;
  job.steps.first_mut().ok_or("run step")?.environment =
    Some(mapping(&[("HTTPS_PROXY", "http://workflow.test:1")]));
  let (conclusion, lines) = h.run(job, &[]).await?;
  assert_eq!(conclusion, Some(Conclusion::Success), "{lines:?}");
  assert_eq!(
    reported(&lines, "run", "HTTPS_PROXY"),
    Some("http://workflow.test:1")
  );
  assert_eq!(reported(&lines, "run", "https_proxy"), Some(PROXY));
  Ok(())
}

#[tokio::test]
async fn env_mode_reexports_the_runner_proxy_in_both_casings() -> FixtureResult<()> {
  let h = Harness::new(NetworkConfig::default()).await?;
  let (conclusion, lines) = h
    .run(
      job(&probe("run"), false)?,
      &[("HTTPS_PROXY", "http://ambient.test:9")],
    )
    .await?;
  assert_eq!(conclusion, Some(Conclusion::Success), "{lines:?}");
  assert_eq!(
    reported(&lines, "run", "HTTPS_PROXY"),
    Some("http://ambient.test:9")
  );
  assert_eq!(
    reported(&lines, "run", "https_proxy"),
    Some("http://ambient.test:9")
  );
  assert_eq!(reported(&lines, "run", "HTTP_PROXY"), Some("unset"));
  assert_eq!(
    reported(&lines, "run", "NODE_EXTRA_CA_CERTS"),
    Some("unset")
  );
  Ok(())
}

#[tokio::test]
async fn node_action_main_and_post_see_the_export() -> FixtureResult<()> {
  let h = Harness::new(NetworkConfig {
    proxy: Some(PROXY.to_owned()),
    ..NetworkConfig::default()
  })
  .await?;
  let job = job("true", true)?;
  h.install_node_action(&job)?;
  let (conclusion, lines) = h.run(job, &[]).await?;
  assert_eq!(conclusion, Some(Conclusion::Success), "{lines:?}");
  assert_eq!(
    reported(&lines, "main", "HTTPS_PROXY"),
    Some(PROXY),
    "{lines:?}"
  );
  assert_eq!(
    reported(&lines, "post", "HTTPS_PROXY"),
    Some(PROXY),
    "{lines:?}"
  );
  Ok(())
}

/// A Node `run:` step fetching from a server signed by the generated CA.
fn node_fetch(port: u16) -> String {
  format!(
    "node -e \"require('https').get('https://127.0.0.1:{port}/', r => console.log('NET94|node|status|' + r.statusCode)).on('error', e => console.log('NET94|node|status|' + e.code))\""
  )
}

#[tokio::test]
async fn node_trusts_the_ca_bundle_only_when_configured() -> FixtureResult<()> {
  let pki = Pki::new()?;
  let dir = tempfile::tempdir()?;
  let target = TlsTarget::serve(
    pki.server_config(&["127.0.0.1"], Leaf::Valid)?,
    b"ok".to_vec(),
  )
  .await?;
  let trusted = Harness::new(NetworkConfig {
    ca_bundle: Some(pki.write_bundle(dir.path())?),
    ..NetworkConfig::default()
  })
  .await?;
  let (_, lines) = trusted
    .run(job(&node_fetch(target.port()), false)?, &[])
    .await?;
  assert_eq!(reported(&lines, "node", "status"), Some("200"), "{lines:?}");
  assert_eq!(target.requests().len(), 1, "{:?}", target.requests());

  let untrusted = Harness::new(NetworkConfig::default()).await?;
  let (_, lines) = untrusted
    .run(job(&node_fetch(target.port()), false)?, &[])
    .await?;
  let status = reported(&lines, "node", "status").unwrap_or_default();
  assert_ne!(status, "200", "{lines:?}");
  assert!(
    status.contains("CERT") || status.contains("SIGNATURE") || status.contains("ISSUER"),
    "expected a certificate error, got {status}: {lines:?}"
  );
  assert_eq!(
    target.requests().len(),
    1,
    "untrusted node must not reach the server"
  );
  Ok(())
}

#[tokio::test]
async fn an_echoed_proxy_password_is_masked() -> FixtureResult<()> {
  let h = Harness::new(NetworkConfig {
    proxy: Some("http://u:s3cr3tpw@proxy.corp.test:3128".to_owned()),
    ..NetworkConfig::default()
  })
  .await?;
  let (conclusion, lines) = h
    .run(
      job("echo \"NET94|echo|HTTPS_PROXY|$HTTPS_PROXY\"", false)?,
      &[],
    )
    .await?;
  assert_eq!(conclusion, Some(Conclusion::Success), "{lines:?}");
  let raw = reported(&lines, "echo", "HTTPS_PROXY").ok_or("echo line missing")?;
  assert!(
    raw.contains("s3cr3tpw"),
    "the event stream is unmasked by design"
  );
  let masker = h.masker.lock().map_err(|e| e.to_string())?;
  let shown = masker.mask(raw);
  assert!(!shown.contains("s3cr3tpw"), "{shown}");
  assert_eq!(shown, "http://u:***@proxy.corp.test:3128");
  Ok(())
}

#[tokio::test]
async fn composite_action_sees_the_same_export_as_a_run_step() -> FixtureResult<()> {
  let pki = Pki::new()?;
  let dir = tempfile::tempdir()?;
  let bundle = pki.write_bundle(dir.path())?;
  let h = Harness::new(NetworkConfig {
    proxy: Some(PROXY.to_owned()),
    no_proxy: vec!["internal.corp".to_owned()],
    ca_bundle: Some(bundle),
  })
  .await?;
  let job = job(&probe("run"), true)?;
  h.install_composite_action(&job)?;
  let (conclusion, lines) = h.run(job, &[("NO_PROXY", "ambient-only")]).await?;
  assert_eq!(conclusion, Some(Conclusion::Success), "{lines:?}");
  for name in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"] {
    assert_eq!(
      reported(&lines, "comp", name),
      Some(PROXY),
      "comp {name}: {lines:?}"
    );
  }
  let bypass = "internal.corp,localhost,127.0.0.1,::1";
  assert_eq!(reported(&lines, "comp", "NO_PROXY"), Some(bypass));
  assert_eq!(reported(&lines, "comp", "no_proxy"), Some(bypass));
  for name in PROBED {
    let in_run = reported(&lines, "run", name);
    assert!(in_run.is_some(), "run {name}: {lines:?}");
    assert_eq!(
      reported(&lines, "comp", name),
      in_run,
      "composite and run step disagree on {name}: {lines:?}"
    );
  }
  Ok(())
}

#[tokio::test]
async fn node_action_sees_the_workflow_env_over_the_export() -> FixtureResult<()> {
  let h = Harness::new(NetworkConfig {
    proxy: Some(PROXY.to_owned()),
    ..NetworkConfig::default()
  })
  .await?;
  let mut job = job("true", true)?;
  job
    .environment_variables
    .push(serde_json::to_value(mapping(&[(
      "HTTPS_PROXY",
      "http://workflow.test:1",
    )]))?);
  h.install_node_action(&job)?;
  let (conclusion, lines) = h.run(job, &[]).await?;
  assert_eq!(conclusion, Some(Conclusion::Success), "{lines:?}");
  for stage in ["main", "post"] {
    assert_eq!(
      reported(&lines, stage, "HTTPS_PROXY"),
      Some("http://workflow.test:1"),
      "{stage}: {lines:?}"
    );
  }
  Ok(())
}

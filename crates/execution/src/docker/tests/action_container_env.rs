//! `build_env`: the `[network]` proxy export for Docker-action containers
//! (#94). Real mounts and config; no daemon is contacted.

use std::path::PathBuf;

use super::*;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const PROXY: &str = "http://runner:pw-94@proxy.corp.test:3128";

fn network_config(root: &Path) -> RunnerConfig {
  RunnerConfig {
    data_dir: root.join("data"),
    workspace_root: root.join("work"),
    network: shared::NetworkConfig {
      proxy: Some(PROXY.to_owned()),
      no_proxy: vec!["internal.test".to_owned()],
      ca_bundle: Some(PathBuf::from("/etc/ssl/corp-ca.pem")),
    },
    ..RunnerConfig::default()
  }
}

/// Run `build_env` for a step whose own env is `step_env`.
fn env_for(step_env: &HashMap<String, String>) -> TestResult<Vec<String>> {
  let root = tempfile::tempdir()?;
  let config = network_config(root.path());
  let workspace = root.path().join("work").join("repo");
  let action_dir = root.path().join("action");
  let socket = root.path().join("docker.sock");
  let mounts = ActionMounts::create(&config, &workspace, &action_dir, &socket)?;
  let cancel = CancellationToken::new();
  let params = ActionContainerParams {
    image: "alpine",
    entrypoint: None,
    args: None,
    env: step_env,
    path_additions: &[],
    config: &config,
    workspace: &workspace,
    action_dir: &action_dir,
    network: None,
    step_id: "step-env",
    timeout: None,
    cancel: &cancel,
  };
  Ok(build_env(&params, &mounts, None))
}

fn value_of<'a>(env: &'a [String], name: &str) -> Option<&'a str> {
  env.iter().find_map(|entry| {
    entry
      .strip_prefix(name)
      .and_then(|rest| rest.strip_prefix('='))
  })
}

#[test]
fn exports_proxy_vars_in_both_casings_without_the_host_ca_path() -> TestResult {
  let env = env_for(&HashMap::new())?;
  for name in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"] {
    assert_eq!(value_of(&env, name), Some(PROXY), "{name} in {env:?}");
  }
  for name in ["NO_PROXY", "no_proxy"] {
    let value = value_of(&env, name).ok_or("no_proxy variable missing")?;
    assert!(value.contains("internal.test"), "{name}={value}");
  }
  assert_eq!(
    value_of(&env, "NODE_EXTRA_CA_CERTS"),
    None,
    "the bundle's host path does not exist in the container: {env:?}"
  );
  Ok(())
}

#[test]
fn step_env_proxy_wins_over_the_export() -> TestResult {
  let step_env = HashMap::from([("HTTPS_PROXY".to_owned(), "http://step.test:1".to_owned())]);
  let env = env_for(&step_env)?;
  assert_eq!(value_of(&env, "HTTPS_PROXY"), Some("http://step.test:1"));
  assert_eq!(
    value_of(&env, "https_proxy"),
    Some(PROXY),
    "only the key the step set is kept; the other casing is still exported"
  );
  assert_eq!(value_of(&env, "NODE_EXTRA_CA_CERTS"), None);
  Ok(())
}

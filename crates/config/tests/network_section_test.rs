//! #94 AC-1: the `[network]` section parses, maps to `NetworkConfig`, and is
//! omitted on save when empty.

use std::path::PathBuf;

use config::config::{RunnerRegistrationConfig, load_config, save_config};
use shared::NetworkConfig;
use tempfile::TempDir;

/// The README's registration example, extended per case.
const BASE: &str = r#"
runner_url   = "https://github.com/owner/repo"
runner_name  = "my-runner"
runner_id    = 12345
auth_token   = "ghs_EXAMPLE"
labels       = ["self-hosted", "Linux", "X64"]
runner_group = "Default"

[runtime]
jit_config       = "e30="
work_dir         = "~/.toolu-runner/_work"
data_dir         = "~/.toolu-runner/runners/owner/repo"
protocol_version = "v2"

[services]
mode = "forwarder"
"#;

fn load(extra: &str) -> Result<RunnerRegistrationConfig, Box<dyn std::error::Error>> {
  let dir = TempDir::new()?;
  let path = dir.path().join("config.toml");
  std::fs::write(&path, format!("{BASE}{extra}"))?;
  Ok(load_config(&path)?)
}

#[test]
fn network_section_maps_trims_and_expands() {
  let cfg = load(
    r#"
[network]
proxy     = "http://user:pass@proxy.corp:3128"
no_proxy  = ["internal.corp", "  ", " .svc.local ", ""]
ca_bundle = "~/certs/corp-root.pem"
"#,
  )
  .expect("load config");
  let network = cfg.network_config();
  assert_eq!(
    network.proxy.as_deref(),
    Some("http://user:pass@proxy.corp:3128")
  );
  assert_eq!(network.no_proxy, vec!["internal.corp", ".svc.local"]);
  let expected = shared::paths::expand_tilde(&PathBuf::from("~/certs/corp-root.pem"));
  assert!(expected.is_absolute(), "{}", expected.display());
  assert_eq!(network.ca_bundle, Some(expected));
  let debug = format!("{:?}", cfg.network);
  assert!(debug.contains(r#"proxy: Some("<set>")"#), "{debug}");
  assert!(
    !debug.contains("pass"),
    "Debug must not print the proxy: {debug}"
  );
}

/// A blank `ca_bundle` resolves to unset. (A blank `proxy` passes through
/// `resolve` unchanged; `network::configured_proxy` treats it as env mode.)
#[test]
fn blank_ca_bundle_is_unset() {
  for blank in ["", "   ", "\t"] {
    let cfg = load(&format!("\n[network]\nca_bundle = \"{blank}\"\n")).expect("load config");
    assert_eq!(cfg.network_config().ca_bundle, None, "{blank:?}");
  }
}

#[test]
fn absent_section_is_env_mode_and_is_not_written_back() {
  let cfg = load("").expect("load config");
  assert_eq!(cfg.network_config(), NetworkConfig::default());
  let dir = TempDir::new().expect("temp dir");
  let path = dir.path().join("config.toml");
  save_config(&path, &cfg).expect("save");
  let text = std::fs::read_to_string(&path).expect("read back");
  assert!(!text.contains("[network]"), "{text}");
}

#[test]
fn configured_section_round_trips() {
  let cfg = load(
    r#"
[network]
ca_bundle = "/etc/corp/ca.pem"
"#,
  )
  .expect("load config");
  let dir = TempDir::new().expect("temp dir");
  let path = dir.path().join("config.toml");
  save_config(&path, &cfg).expect("save");
  let text = std::fs::read_to_string(&path).expect("read back");
  assert!(
    text.contains("[network]") && text.contains("ca_bundle = \"/etc/corp/ca.pem\""),
    "{text}"
  );
  assert!(!text.contains("proxy"), "unset keys stay unset: {text}");
  assert_eq!(load_config(&path).expect("reload"), cfg);
}

/// A misspelled key fails `load_config` naming it, instead of being
/// ignored (which would silently drop the CA or proxy).
#[test]
fn unknown_network_key_is_rejected() {
  let err = match load(
    r#"
[network]
ca_bundel = "/etc/corp/ca.pem"
"#,
  ) {
    Ok(cfg) => format!("loaded: {:?}", cfg.network),
    Err(e) => e.to_string(),
  };
  assert!(err.contains("ca_bundel"), "{err}");
  assert!(err.contains("unknown field"), "{err}");
}

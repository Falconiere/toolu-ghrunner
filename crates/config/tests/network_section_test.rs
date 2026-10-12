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

fn load(extra: &str) -> RunnerRegistrationConfig {
  let dir = TempDir::new().expect("temp dir");
  let path = dir.path().join("config.toml");
  std::fs::write(&path, format!("{BASE}{extra}")).expect("write config");
  load_config(&path).expect("load config")
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
  );
  let network = cfg.network_config();
  assert_eq!(network.proxy.as_deref(), Some("http://user:pass@proxy.corp:3128"));
  assert_eq!(network.no_proxy, vec!["internal.corp", ".svc.local"]);
  let expected = shared::paths::expand_tilde(&PathBuf::from("~/certs/corp-root.pem"));
  assert!(expected.is_absolute(), "{}", expected.display());
  assert_eq!(network.ca_bundle, Some(expected));
  assert!(!format!("{:?}", cfg.network).contains("pass@"), "Debug must redact");
}

#[test]
fn absent_section_is_env_mode_and_is_not_written_back() {
  let cfg = load("");
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
  );
  let dir = TempDir::new().expect("temp dir");
  let path = dir.path().join("config.toml");
  save_config(&path, &cfg).expect("save");
  let text = std::fs::read_to_string(&path).expect("read back");
  assert!(text.contains("[network]") && text.contains("ca_bundle = \"/etc/corp/ca.pem\""), "{text}");
  assert!(!text.contains("proxy"), "unset keys stay unset: {text}");
  assert_eq!(load_config(&path).expect("reload"), cfg);
}

//! #206 AC-8: the `[provider]` section of `config.toml`.
//!
//! Real files only: registrations are written with `save_config` into a temp
//! dir and read back with `load_config`. [`PRE_206_GITHUB`] is the exact text
//! `save_config` wrote for [`github`] before #206 (captured from `main` at
//! 71db588), so a GitHub registration's file is pinned byte for byte.

use config::config::{
  CacheSection, RunnerRegistrationConfig, RuntimeConfig, ServicesSection, ShadowSection,
  WorkspaceSection, load_config, save_config,
};
use config::network::NetworkSection;
use config::provider::ProviderSection;
use config::registration_auth::AuthSection;
use tempfile::TempDir;

/// What `save_config` wrote for [`github`] before #206.
const PRE_206_GITHUB: &str = r#"runner_url = "https://github.com/octo/demo"
runner_name = "runner-206"
runner_id = 7
auth_token = "Iv1.client"
labels = [
    "self-hosted",
    "linux",
]
runner_group = "Default"
ephemeral = false

[runtime]
jit_config = "eyJqaXQiOiB0cnVlfQ=="
work_dir = "~/.toolu-runner/_work"
data_dir = "~/.toolu-runner/runners/octo/demo"
protocol_version = "v2"

[services]
mode = "forwarder"
bind = "0.0.0.0"

[cache]
max_bytes = 107374182400
entry_ttl_days = 7
protected_branches = [
    "main",
    "master",
]
chunk_avg_bytes = 65536
fsync_chunks = false

[cache.l2]
enabled = false
bucket = ""
endpoint = ""
region = ""

[workspace]
mode = "stable"
gc_after_hours = 24
stable_gc_after_hours = 0

[shadow]
enabled = false
"#;

/// A toolu.sh machine's `config.toml` as #207 writes it: no GitHub fields.
const TOOLU: &str = r#"labels = ["gpu"]

[provider]
kind = "toolu"
api_url = "https://console.toolu.sh"
host_id = "83dca27b-4052-420a-a767-04c5cd758654"
org = "org_123"

[runtime]
work_dir = "~/.toolu-runner/_work"
data_dir = "~/.toolu-runner/toolu/org_123"
"#;

fn github() -> RunnerRegistrationConfig {
  RunnerRegistrationConfig {
    runner_url: "https://github.com/octo/demo".to_owned(),
    runner_name: "runner-206".to_owned(),
    runner_id: 7,
    auth_token: "Iv1.client".to_owned(),
    labels: vec!["self-hosted".to_owned(), "linux".to_owned()],
    runner_group: "Default".to_owned(),
    ephemeral: false,
    runtime: RuntimeConfig {
      jit_config: "eyJqaXQiOiB0cnVlfQ==".to_owned(),
      work_dir: "~/.toolu-runner/_work".to_owned(),
      data_dir: "~/.toolu-runner/runners/octo/demo".to_owned(),
      protocol_version: "v2".to_owned(),
    },
    services: ServicesSection::default(),
    cache: CacheSection::default(),
    workspace: WorkspaceSection::default(),
    shadow: ShadowSection::default(),
    network: NetworkSection::default(),
    auth: AuthSection::Token,
    provider: ProviderSection::Github,
  }
}

fn toolu_provider() -> ProviderSection {
  ProviderSection::Toolu {
    api_url: "https://console.toolu.sh".to_owned(),
    host_id: "83dca27b-4052-420a-a767-04c5cd758654".to_owned(),
    org: "org_123".to_owned(),
  }
}

/// Write `text` as `config.toml` in a fresh temp dir and load it.
fn load_text(
  text: &str,
) -> Result<Result<RunnerRegistrationConfig, shared::RunnerError>, Box<dyn std::error::Error>> {
  let dir = TempDir::new()?;
  let path = dir.path().join("config.toml");
  std::fs::write(&path, text)?;
  Ok(load_config(&path))
}

#[test]
fn a_github_registration_is_saved_byte_identical_to_before_206() {
  let dir = TempDir::new().expect("temp dir");
  let path = dir.path().join("config.toml");
  save_config(&path, &github()).expect("save");
  let text = std::fs::read_to_string(&path).expect("read");
  assert_eq!(text, PRE_206_GITHUB);
  assert_eq!(
    load_config(&path).expect("load").provider,
    ProviderSection::Github
  );
}

#[test]
fn a_pre_206_github_file_loads_as_the_github_provider() {
  let cfg = load_text(PRE_206_GITHUB).expect("setup").expect("loads");
  assert_eq!(cfg, github());
}

#[test]
fn a_toolu_machine_config_without_github_fields_loads() {
  let cfg = load_text(TOOLU).expect("setup").expect("loads");
  assert_eq!(cfg.provider, toolu_provider());
  assert_eq!(cfg.labels, vec!["gpu".to_owned()]);
  assert_eq!(cfg.runtime.data_dir, "~/.toolu-runner/toolu/org_123");
  assert!(cfg.runner_url.is_empty() && cfg.runtime.jit_config.is_empty());
}

#[test]
fn a_toolu_machine_config_round_trips_without_github_fields() {
  let dir = TempDir::new().expect("temp dir");
  let path = dir.path().join("config.toml");
  let original = load_text(TOOLU).expect("setup").expect("loads");
  save_config(&path, &original).expect("save");
  let text = std::fs::read_to_string(&path).expect("read");
  assert!(text.contains("[provider]\nkind = \"toolu\""), "{text}");
  for github_only in [
    "runner_url",
    "runner_name",
    "runner_id",
    "auth_token",
    "runner_group",
    "jit_config",
    "protocol_version",
  ] {
    assert!(
      !text.contains(github_only),
      "{github_only} written:\n{text}"
    );
  }
  assert_eq!(load_config(&path).expect("reload"), original);
}

#[test]
fn an_unknown_provider_kind_or_key_is_rejected() {
  let err = load_text(&TOOLU.replace("kind = \"toolu\"", "kind = \"gitlab\""))
    .expect("setup")
    .expect_err("unknown kind");
  assert!(err.to_string().contains("gitlab"), "{err}");
  let err = load_text(&TOOLU.replace("org = \"org_123\"", "org = \"org_123\"\nslot = 2"))
    .expect("setup")
    .expect_err("a typo'd key must not be ignored");
  assert!(err.to_string().contains("slot"), "{err}");
}

#[test]
fn a_github_config_without_runner_url_is_rejected() {
  let text = PRE_206_GITHUB.replace("runner_url = \"https://github.com/octo/demo\"\n", "");
  let err = load_text(&text)
    .expect("setup")
    .expect_err("a GitHub registration names its runner_url");
  assert!(err.to_string().contains("runner_url"), "{err}");
}

#[test]
fn a_toolu_provider_without_its_fields_is_rejected() {
  let err = load_text(&TOOLU.replace("host_id = \"83dca27b-4052-420a-a767-04c5cd758654\"\n", ""))
    .expect("setup")
    .expect_err("host_id is required");
  assert!(err.to_string().contains("host_id"), "{err}");
}

//! Integration tests for the `~/.toolu-runner/` storage layout (step 9).
//!
//! Covers the spec's storage layout: `config.toml` (0600),
//! `credentials.json` (0600), roundtrip through the
//! `RunnerRegistrationConfig` / `CredentialsFile` structs, JIT endpoint
//! derivation, and tilde expansion via [`shared::paths::expand_tilde`].
//!
//! All tests use real temp directories; nothing mocks the filesystem.

use std::path::{Path, PathBuf};

use shared::paths::expand_tilde;
use toolu_runner::config::{
  CredentialsFile, RunnerRegistrationConfig, RuntimeConfig, jit_endpoint_for_host,
  load_config as load_reg_config, load_credentials, resolve_data_dir, resolve_work_dir,
  save_config as save_reg_config, save_credentials,
};

fn temp_dir(label: &str) -> PathBuf {
  let dir = std::env::temp_dir().join(format!(
    "toolu-runner-storage-{label}-{}",
    std::process::id()
  ));
  let _ = std::fs::remove_dir_all(&dir);
  // `allow-expect-in-tests` only applies inside `#[test]` fns —
  // swallow the create error in this helper.
  std::fs::create_dir_all(&dir).ok();
  dir
}

fn sample_config() -> RunnerRegistrationConfig {
  RunnerRegistrationConfig {
    runner_url: "https://github.com/Falconiere/toolu-ghrunner".to_owned(),
    runner_name: "storage-test-runner".to_owned(),
    runner_id: 67890,
    auth_token: "ghs_storage_test_token".to_owned(),
    labels: vec![
      "self-hosted".to_owned(),
      "linux".to_owned(),
      "x64".to_owned(),
    ],
    runner_group: "Default".to_owned(),
    runtime: RuntimeConfig {
      jit_config: "<base64-jit-blob>".to_owned(),
      work_dir: "~/.toolu-runner/_work".to_owned(),
      data_dir: "~/.toolu-runner".to_owned(),
      protocol_version: "v2".to_owned(),
    },
  }
}

fn sample_credentials() -> CredentialsFile {
  CredentialsFile {
    access_token: "ghs_storage_test_token".to_owned(),
    issued_at: "2026-06-18T10:00:00Z".to_owned(),
    expires_at: Some("2027-06-18T10:00:00Z".to_owned()),
  }
}

// ─── Roundtrip ──────────────────────────────────────────────────────

#[test]
fn config_roundtrips_through_toml() {
  let dir = temp_dir("config-roundtrip");
  let path = dir.join("config.toml");
  let cfg = sample_config();

  save_reg_config(&path, &cfg).expect("save config");
  let loaded = load_reg_config(&path).expect("load config");
  assert_eq!(loaded, cfg, "roundtrip must be lossless");
}

#[test]
fn credentials_roundtrip_through_json() {
  let dir = temp_dir("creds-roundtrip");
  let path = dir.join("credentials.json");
  let creds = sample_credentials();

  save_credentials(&path, &creds).expect("save creds");
  let loaded = load_credentials(&path).expect("load creds");
  assert_eq!(loaded, creds);
}

#[test]
fn credentials_roundtrip_without_expires_at() {
  let dir = temp_dir("creds-no-expiry");
  let path = dir.join("credentials.json");
  let creds = CredentialsFile {
    access_token: "ghs_no_expiry".to_owned(),
    issued_at: "2026-06-18T10:00:00Z".to_owned(),
    expires_at: None,
  };
  save_credentials(&path, &creds).expect("save");
  let loaded = load_credentials(&path).expect("load");
  assert_eq!(loaded, creds);
  assert!(loaded.expires_at.is_none());
}

#[test]
fn config_creates_parent_directories() {
  let dir = temp_dir("config-mkdir");
  let nested = dir.join("deep/nested/path/config.toml");
  save_reg_config(&nested, &sample_config()).expect("save");
  assert!(nested.exists());
  let loaded = load_reg_config(&nested).expect("load");
  assert_eq!(loaded.runner_name, "storage-test-runner");
}

// ─── File mode (Unix only) ──────────────────────────────────────────

#[cfg(unix)]
#[test]
fn config_file_mode_is_0600() {
  use std::os::unix::fs::PermissionsExt;
  let dir = temp_dir("config-mode");
  let path = dir.join("config.toml");
  save_reg_config(&path, &sample_config()).expect("save");
  let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
  assert_eq!(mode, 0o600, "config.toml must be 0600; got {mode:o}");
}

#[cfg(unix)]
#[test]
fn credentials_file_mode_is_0600() {
  use std::os::unix::fs::PermissionsExt;
  let dir = temp_dir("creds-mode");
  let path = dir.join("credentials.json");
  save_credentials(&path, &sample_credentials()).expect("save");
  let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
  assert_eq!(mode, 0o600, "credentials.json must be 0600; got {mode:o}");
}

// ─── `expand_tilde` (shared::paths) ────────────────────────────────

#[test]
fn expand_tilde_handles_absolute_path() {
  let p = expand_tilde(Path::new("/etc/hosts"));
  assert_eq!(p, PathBuf::from("/etc/hosts"));
}

#[test]
fn expand_tilde_handles_relative_path() {
  let p = expand_tilde(Path::new("relative/path"));
  assert_eq!(p, PathBuf::from("relative/path"));
}

#[test]
fn expand_tilde_expands_bare_tilde() {
  let home = home_dir();
  let p = expand_tilde(Path::new("~"));
  assert_eq!(p, home);
}

#[test]
fn expand_tilde_expands_tilde_with_subpath() {
  let home = home_dir();
  let p = expand_tilde(Path::new("~/.toolu-runner"));
  assert_eq!(p, home.join(".toolu-runner"));
}

#[test]
fn expand_tilde_does_not_expand_other_users() {
  // Only the current user's `~` is expanded; `~user/...` is passed through.
  let p = expand_tilde(Path::new("~nobody/secret"));
  assert_eq!(p, PathBuf::from("~nobody/secret"));
}

// ─── Helpers ────────────────────────────────────────────────────────

fn home_dir() -> PathBuf {
  std::env::var_os("HOME")
    .or_else(|| std::env::var_os("USERPROFILE"))
    .map(PathBuf::from)
    .unwrap_or_else(|| PathBuf::from("/tmp"))
}

// ─── `resolve_data_dir` + `resolve_work_dir` ───────────────────────

#[test]
fn resolve_data_dir_creates_and_returns_absolute() {
  let dir = temp_dir("resolve-data-dir");
  let nested = dir.join("nope/not/yet/.toolu-runner");
  let resolved = resolve_data_dir(nested.to_str().expect("utf8")).expect("resolve");
  assert!(resolved.is_absolute(), "got: {resolved:?}");
  assert!(resolved.exists());
}

#[test]
fn resolve_work_dir_does_not_create() {
  let dir = temp_dir("resolve-work-dir");
  // Pass a non-tilde path so we don't depend on HOME in this test.
  let target = dir.join("work");
  let resolved = resolve_work_dir(target.to_str().expect("utf8"));
  assert_eq!(resolved, target);
  assert!(!resolved.exists(), "resolve_work_dir must not create");
}

#[test]
fn resolve_data_dir_expands_tilde() {
  let resolved = resolve_data_dir("~/toolu-runner-tilde-test-target").expect("resolve");
  assert!(
    resolved
      .to_string_lossy()
      .contains("toolu-runner-tilde-test-target")
  );
  assert!(resolved.exists());
  let _ = std::fs::remove_dir_all(&resolved);
}

// ─── JIT endpoint ──────────────────────────────────────────────────

#[test]
fn jit_endpoint_handles_github_com_case_insensitively() {
  assert_eq!(
    jit_endpoint_for_host("github.com"),
    "https://pipelinesgh.azureedge.net"
  );
  assert_eq!(
    jit_endpoint_for_host("GitHub.com"),
    "https://pipelinesgh.azureedge.net"
  );
  assert_eq!(
    jit_endpoint_for_host("GITHUB.COM"),
    "https://pipelinesgh.azureedge.net"
  );
}

#[test]
fn jit_endpoint_for_ghes_uses_subdomain_convention() {
  assert_eq!(
    jit_endpoint_for_host("ghes.example.com"),
    "https://pipelines.ghes.example.com"
  );
}

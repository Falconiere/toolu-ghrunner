//! Integration tests for `config::remint` (always-online AC-9).
//!
//! Real data only: a non-default `RunnerRegistrationConfig` is written to a
//! real `tempfile` `config.toml`, loaded back, re-minted, re-persisted, and
//! loaded again — proving the user-editable sections survive a full TOML
//! round-trip while only the three mint-derived fields change. No mocks.

use config::config::{
  CacheSection, L2Section, RunnerRegistrationConfig, RuntimeConfig, ServicesSection, ShadowSection,
  WorkspaceSection, load_config, save_config,
};
use config::remint::merge_reminted_config;
use tempfile::TempDir;

/// A registration whose `[services]`/`[cache]`/`[workspace]`/`[shadow]`
/// sections are all set to NON-default values, so a re-mint that clobbered
/// any of them would be caught.
fn non_default_config() -> RunnerRegistrationConfig {
  RunnerRegistrationConfig {
    runner_url: "https://github.com/octocat/hello-world".to_owned(),
    runner_name: "runner-01".to_owned(),
    runner_id: 42,
    auth_token: "ghs_original_token".to_owned(),
    labels: vec![
      "self-hosted".to_owned(),
      "linux".to_owned(),
      "x64".to_owned(),
    ],
    runner_group: "Production".to_owned(),
    runtime: RuntimeConfig {
      jit_config: "eyJvcmlnaW5hbCI6IHRydWV9".to_owned(),
      work_dir: "~/.toolu-runner/runners/octocat/hello-world/_work".to_owned(),
      data_dir: "~/.toolu-runner/runners/octocat/hello-world".to_owned(),
      protocol_version: "v2".to_owned(),
    },
    services: ServicesSection {
      mode: "accelerated".to_owned(),
      bind: "0.0.0.0:9000".to_owned(),
    },
    cache: CacheSection {
      max_bytes: 1_234_567_890,
      entry_ttl_days: 14,
      protected_branches: vec!["main".to_owned(), "release/*".to_owned()],
      chunk_avg_bytes: 262_144,
      l2: L2Section {
        enabled: true,
        bucket: "my-cache".to_owned(),
        endpoint: "https://s3.example.com".to_owned(),
        region: "us-east-1".to_owned(),
      },
    },
    workspace: WorkspaceSection { gc_after_hours: 72 },
    shadow: ShadowSection { enabled: true },
  }
}

#[test]
fn remint_preserves_sections_through_a_full_toml_round_trip() {
  let home = TempDir::new().expect("temp home");
  let original = non_default_config();

  // Persist + reload the prior config (the state the loop reads each pass).
  let prior_path = home.path().join("config.toml");
  save_config(&prior_path, &original).expect("save prior");
  let prior = load_config(&prior_path).expect("load prior");
  assert_eq!(prior, original, "sanity: the prior config round-trips");

  // Re-mint with fresh JIT/id/client, then persist + reload the result.
  let merged = merge_reminted_config(
    &prior,
    "eyJyZW1pbnRlZCI6IHRydWV9".to_owned(),
    99,
    "Iv1.newclientid".to_owned(),
  );
  let merged_path = home.path().join("config-remint.toml");
  save_config(&merged_path, &merged).expect("save merged");
  let reloaded = load_config(&merged_path).expect("load merged");

  // The three mint-derived fields changed.
  assert_eq!(reloaded.runner_id, 99);
  assert_eq!(reloaded.auth_token, "Iv1.newclientid");
  assert_eq!(reloaded.runtime.jit_config, "eyJyZW1pbnRlZCI6IHRydWV9");

  // Every user-editable section survived byte-for-byte (TOML-value equal).
  assert_eq!(reloaded.services, original.services);
  assert_eq!(reloaded.cache, original.cache);
  assert_eq!(reloaded.workspace, original.workspace);
  assert_eq!(reloaded.shadow, original.shadow);
  assert_eq!(reloaded.runner_url, original.runner_url);
  assert_eq!(reloaded.runner_name, original.runner_name);
  assert_eq!(reloaded.labels, original.labels);
  assert_eq!(reloaded.runner_group, original.runner_group);
  assert_eq!(reloaded.runtime.work_dir, original.runtime.work_dir);
  assert_eq!(reloaded.runtime.data_dir, original.runtime.data_dir);
  assert_eq!(
    reloaded.runtime.protocol_version,
    original.runtime.protocol_version
  );
}

#[test]
fn remint_equals_prior_with_only_three_fields_swapped() {
  let prior = non_default_config();
  let merged = merge_reminted_config(&prior, "NEWJIT".to_owned(), 7, "cid".to_owned());

  // `prior` is unused after the mint, so build `expected` by moving it.
  let mut expected = prior;
  expected.runner_id = 7;
  expected.auth_token = "cid".to_owned();
  expected.runtime.jit_config = "NEWJIT".to_owned();

  assert_eq!(
    merged, expected,
    "re-mint must touch only runner_id/auth_token/runtime.jit_config"
  );
}

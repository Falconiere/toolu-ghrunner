//! `[workspace]` section parsing (issue #90 AC-7).
//!
//! Real `config.toml` files on disk through `load_config` / `save_config`,
//! including a pre-#90 registration that has no `mode` key.

use config::config::{load_config, save_config};
use shared::{WorkspaceConfig, WorkspaceMode};
use tempfile::TempDir;

/// A registration as `register` wrote it before #90: `[workspace]` carries
/// only `gc_after_hours`.
const PRE_90_CONFIG: &str = r#"runner_url = "https://github.com/octocat/hello-world"
runner_name = "runner-01"
runner_id = 42
auth_token = "client-id"
labels = ["self-hosted", "linux"]
runner_group = "Default"

[runtime]
jit_config = "e30="
work_dir = "~/.toolu-runner/_work"
data_dir = "~/.toolu-runner/runners/octocat/hello-world"
protocol_version = "v2"

[workspace]
gc_after_hours = 48
"#;

fn write(dir: &TempDir, body: &str) -> std::io::Result<std::path::PathBuf> {
  let path = dir.path().join("config.toml");
  std::fs::write(&path, body)?;
  Ok(path)
}

#[test]
fn pre_90_config_loads_as_stable_and_never_prunes_stable_workspaces() {
  let dir = TempDir::new().expect("tempdir");
  let cfg = load_config(&write(&dir, PRE_90_CONFIG).expect("write config.toml"))
    .expect("pre-#90 config loads");
  assert_eq!(cfg.workspace_gc_hours(), 48);
  assert_eq!(
    cfg.workspace_config(),
    WorkspaceConfig {
      mode: WorkspaceMode::Stable,
      stable_gc_hours: 0,
    }
  );
}

#[test]
fn fresh_mode_and_stable_gc_age_round_trip() {
  let dir = TempDir::new().expect("tempdir");
  let body = PRE_90_CONFIG.replace(
    "gc_after_hours = 48\n",
    "mode = \"fresh\"\ngc_after_hours = 48\nstable_gc_after_hours = 72\n",
  );
  let path = write(&dir, &body).expect("write config.toml");
  let cfg = load_config(&path).expect("fresh config loads");
  assert_eq!(
    cfg.workspace_config(),
    WorkspaceConfig {
      mode: WorkspaceMode::Fresh,
      stable_gc_hours: 72,
    }
  );
  save_config(&path, &cfg).expect("save");
  let saved = std::fs::read_to_string(&path).expect("read back");
  assert!(saved.contains("mode = \"fresh\""), "{saved}");
  assert!(saved.contains("stable_gc_after_hours = 72"), "{saved}");
  assert_eq!(load_config(&path).expect("reload"), cfg);
}

#[test]
fn unknown_mode_fails_loading_and_names_the_file() {
  let dir = TempDir::new().expect("tempdir");
  let body = PRE_90_CONFIG.replace("gc_after_hours = 48\n", "mode = \"per-job\"\n");
  let path = write(&dir, &body).expect("write config.toml");
  let error = load_config(&path)
    .expect_err("unknown mode is rejected")
    .to_string();
  assert!(error.contains(&path.display().to_string()), "{error}");
  assert!(error.contains("per-job"), "{error}");
}

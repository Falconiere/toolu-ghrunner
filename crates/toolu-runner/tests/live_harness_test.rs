//! Hermetic checks of the live harness itself (#236): where it finds the built
//! binary, which labels it registers, and whether cleanup reports its failures.
//! These need no credentials; CI runs them from the `test` job.

#![cfg(feature = "live")]

use std::path::Path;

use crate::harness::{self, LiveHarness};

#[test]
fn default_binary_path_is_under_the_workspace_target() {
  let root = harness::workspace_root();
  assert!(root.join("Cargo.lock").is_file(), "{}", root.display());
  assert_eq!(
    harness::binary_path_for(None),
    root.join("target").join("debug").join("toolu-runner")
  );
}

#[test]
fn target_dir_resolves_the_binary_cargo_built() {
  let built = Path::new(env!("CARGO_BIN_EXE_toolu-runner"));
  assert!(built.is_file(), "{}", built.display());
  let target = built
    .ancestors()
    .nth(2)
    .expect("<target>/debug/toolu-runner has a target dir");
  assert_eq!(harness::binary_path_for(Some(target.to_path_buf())), built);
}

#[test]
fn labels_name_this_host() {
  let labels = harness::live_labels();
  let labels: Vec<&str> = labels.split(',').collect();
  assert!(labels.contains(&"toolu-runner-v1"), "{labels:?}");
  assert!(
    labels.contains(&shared::platform::runner_os()),
    "{labels:?}"
  );
  assert!(
    labels.contains(&shared::platform::runner_arch()),
    "{labels:?}"
  );
}

fn harness_with_binary(binary: &Path) -> Result<LiveHarness, Box<dyn std::error::Error>> {
  LiveHarness::for_binary(binary.to_path_buf(), "owner/repo".to_owned(), String::new())
}

#[tokio::test]
async fn cleanup_reports_a_failed_remove() {
  let missing = tempfile::tempdir().expect("temp dir");
  let harness = harness_with_binary(&missing.path().join("no-such-binary")).expect("harness");
  std::fs::write(harness.config_path(), "").expect("seed config");

  let error = harness
    .cleanup(&[])
    .await
    .expect_err("a failed remove must be reported");
  assert!(error.to_string().starts_with("remove: "), "{error}");
}

#[tokio::test]
async fn cleanup_reports_a_remove_the_real_binary_rejects() {
  let harness =
    harness_with_binary(Path::new(env!("CARGO_BIN_EXE_toolu-runner"))).expect("harness");
  std::fs::write(harness.config_path(), "not toml =").expect("seed config");

  let error = harness
    .cleanup(&[])
    .await
    .expect_err("a rejected remove must be reported");
  // Not the spawn failure above: the binary ran and exited non-zero.
  assert!(
    error.to_string().starts_with("remove: remove failed: "),
    "{error}"
  );
}

#[tokio::test]
async fn cleanup_with_nothing_to_do_succeeds() {
  let harness =
    harness_with_binary(Path::new(env!("CARGO_BIN_EXE_toolu-runner"))).expect("harness");
  harness.cleanup(&[]).await.expect("nothing to clean");
}

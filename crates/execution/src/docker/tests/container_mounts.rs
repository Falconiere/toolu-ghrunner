//! Tests for `container_mounts`: the operator's `TOOLU_RUNNER_NODE_DIR` is
//! bind-mounted read-only at `/github/externals` and translates a preseeded
//! runtime path; a missing directory is neither created nor mounted; a mount
//! source that cannot be made absolute fails naming the path.

use super::{ContainerMounts, mount};
use crate::docker::path_translator::PathTranslator;
use shared::RunnerConfig;
use std::path::{Path, PathBuf};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn config(root: &Path, node_dir: Option<PathBuf>) -> RunnerConfig {
  RunnerConfig {
    data_dir: root.join("data"),
    node_dir,
    ..RunnerConfig::default()
  }
}

#[test]
fn preseed_dir_is_mounted_read_only_and_translates_its_runtimes() -> TestResult {
  let root = tempfile::tempdir()?;
  let externals = root.path().join("externals");
  std::fs::create_dir_all(externals.join("node24_alpine/bin"))?;
  let mounts = ContainerMounts::create(
    &config(root.path(), Some(externals.clone())),
    &root.path().join("work"),
  )?;
  assert!(
    mounts
      .volumes
      .contains(&format!("{}:/github/externals:ro", externals.display())),
    "{:?}",
    mounts.volumes
  );
  assert_eq!(
    mounts
      .translator
      .to_container(&externals.join("node24_alpine/bin/node")),
    Path::new("/github/externals/node24_alpine/bin/node")
  );
  Ok(())
}

#[test]
fn a_missing_preseed_dir_is_not_created_or_mounted() -> TestResult {
  let root = tempfile::tempdir()?;
  let missing = root.path().join("typo-externals");
  let mounts = ContainerMounts::create(
    &config(root.path(), Some(missing.clone())),
    &root.path().join("work"),
  )?;
  assert!(
    !missing.exists(),
    "the runner must not create the preseed dir"
  );
  assert!(
    !mounts
      .volumes
      .iter()
      .any(|volume| volume.contains("/github/externals")),
    "{:?}",
    mounts.volumes
  );
  Ok(())
}

#[test]
fn an_unresolvable_mount_source_names_the_path_in_the_error() -> TestResult {
  let mut translator = PathTranslator::new(PathBuf::from("/w"), PathBuf::from("/t"));
  let mut volumes = Vec::new();
  let error = mount(&mut translator, &mut volumes, Path::new(""), "/x", true)
    .err()
    .ok_or("an empty path cannot be made absolute")?;
  assert!(
    error
      .to_string()
      .contains("cannot resolve job mount source"),
    "{error}"
  );
  assert!(volumes.is_empty(), "{volumes:?}");
  Ok(())
}

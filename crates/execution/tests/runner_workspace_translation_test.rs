//! `RUNNER_WORKSPACE` / `runner.workspace` in job containers (#90).
//!
//! A stable workspace is `<work>/<repo>/<repo>` and its parent
//! `<work>/<repo>` is `runner.workspace`. Only the workspace is mounted (at
//! `/github/workspace`), so the parent must become the mount's container-side
//! parent, `/github`. The translator is built from real host directories with
//! the same mappings `ContainerMounts::create` adds.

use std::path::{Path, PathBuf};

use execution::docker::path_translator::PathTranslator;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The translator a job container builds for `workspace`.
fn translator(root: &Path, workspace: &Path) -> std::io::Result<PathTranslator> {
  let temp = root.join("runner data/_temp");
  std::fs::create_dir_all(workspace)?;
  std::fs::create_dir_all(&temp)?;
  let mut translator = PathTranslator::new(workspace.to_path_buf(), temp.clone());
  for (host, target) in [
    (workspace.to_path_buf(), "/github/workspace"),
    (temp, "/github/runner_temp"),
    (root.join("runner data/_tool"), "/github/tool_cache"),
  ] {
    translator.add_mapping(std::path::absolute(host)?, PathBuf::from(target));
  }
  Ok(translator)
}

#[test]
fn stable_pipeline_dir_maps_to_the_workspace_mount_parent() -> TestResult {
  let root = tempfile::tempdir()?;
  let pipeline = root.path().join("work/hello-world");
  let workspace = pipeline.join("hello-world");
  let translator = translator(root.path(), &workspace)?;

  assert_eq!(
    translator.runner_workspace_to_container(&pipeline),
    PathBuf::from("/github")
  );
  // The workspace itself and paths inside it keep the mount translation.
  assert_eq!(
    translator.runner_workspace_to_container(&workspace),
    PathBuf::from("/github/workspace")
  );
  assert_eq!(
    translator.to_container(&workspace.join("src")),
    PathBuf::from("/github/workspace/src")
  );
  // `to_container` is unchanged: the pipeline dir is not itself a mount.
  assert_eq!(translator.to_container(&pipeline), pipeline);
  Ok(())
}

#[test]
fn fresh_workspace_is_its_own_runner_workspace() -> TestResult {
  // Fresh mode records the job directory as both values.
  let root = tempfile::tempdir()?;
  let workspace = root.path().join("work/1234");
  let translator = translator(root.path(), &workspace)?;

  assert_eq!(
    translator.runner_workspace_to_container(&workspace),
    PathBuf::from("/github/workspace")
  );
  Ok(())
}

#[test]
fn unrelated_paths_are_not_rewritten() -> TestResult {
  let root = tempfile::tempdir()?;
  let workspace = root.path().join("work/hello-world/hello-world");
  let translator = translator(root.path(), &workspace)?;
  let other = root.path().join("work");

  assert_eq!(translator.runner_workspace_to_container(&other), other);
  assert_eq!(
    translator.runner_workspace_to_container(&root.path().join("runner data/_tool/node")),
    PathBuf::from("/github/tool_cache/node")
  );
  Ok(())
}

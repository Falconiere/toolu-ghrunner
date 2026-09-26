//! Standard bind mounts and path translation for Docker action containers.

use std::path::{Path, PathBuf};

use shared::{RunnerConfig, RunnerError};

use super::path_translator::PathTranslator;

/// Materialized mounts retained until the owned action container is removed.
pub(super) struct ActionMounts {
  pub(super) translator: PathTranslator,
  pub(super) volumes: Vec<String>,
}

impl ActionMounts {
  /// Create the standard action mounts and their shared translator.
  pub(super) fn create(
    config: &RunnerConfig,
    workspace: &Path,
    action_dir: &Path,
    docker_socket: &Path,
  ) -> Result<Self, RunnerError> {
    let stable = stable_mappings(config, workspace, action_dir);
    let translator = translator_for(config, workspace, action_dir);
    let mut volumes = Vec::new();
    for (host, target, readonly) in stable {
      add_mount(&mut volumes, &host, target, readonly)?;
    }
    add_mount(&mut volumes, docker_socket, "/var/run/docker.sock", false)?;
    Ok(Self {
      translator,
      volumes,
    })
  }

  /// Translate an environment value into container coordinates.
  pub(super) fn translate_env(&self, key: &str, value: &str) -> String {
    if key == "PATH" {
      return value
        .split(':')
        .map(|part| {
          self
            .translator
            .to_container(Path::new(part))
            .to_string_lossy()
            .into_owned()
        })
        .collect::<Vec<_>>()
        .join(":");
    }
    self
      .translator
      .to_container(Path::new(value))
      .to_string_lossy()
      .into_owned()
  }
}

/// Translate one container-emitted action path back to its stable host path.
pub(crate) fn action_path_to_host(
  config: &RunnerConfig,
  workspace: &Path,
  action_dir: &Path,
  value: &str,
) -> PathBuf {
  translator_for(config, workspace, action_dir).to_host(Path::new(value))
}

fn translator_for(config: &RunnerConfig, workspace: &Path, action_dir: &Path) -> PathTranslator {
  let mut translator = PathTranslator::new(workspace.to_path_buf(), config.data_dir.join("_temp"));
  for (host, target, _readonly) in stable_mappings(config, workspace, action_dir) {
    translator.add_mapping(host, PathBuf::from(target));
  }
  translator
}

fn stable_mappings(
  config: &RunnerConfig,
  workspace: &Path,
  action_dir: &Path,
) -> Vec<(PathBuf, &'static str, bool)> {
  let mut mappings = vec![
    (workspace.to_path_buf(), "/github/workspace", false),
    (config.data_dir.join("_temp"), "/github/runner_temp", false),
    (config.data_dir.join("tmp"), "/github/file_commands", false),
    (config.data_dir.join("_tool"), "/github/tool_cache", false),
    (config.data_dir.join("events"), "/github/workflow", true),
    (shared_home(config, workspace), "/github/home", false),
  ];
  if action_dir != workspace {
    mappings.push((action_dir.to_path_buf(), "/github/action", true));
  }
  mappings
}

fn add_mount(
  volumes: &mut Vec<String>,
  host: &Path,
  target: &str,
  readonly: bool,
) -> Result<(), RunnerError> {
  if target != "/var/run/docker.sock" {
    std::fs::create_dir_all(host)?;
  }
  let host = std::path::absolute(host)?;
  if host.to_string_lossy().contains(':') {
    return Err(RunnerError::Docker(
      "action mount source paths cannot contain ':'".to_owned(),
    ));
  }
  volumes.push(format!(
    "{}:{target}:{}",
    host.display(),
    if readonly { "ro" } else { "rw" }
  ));
  Ok(())
}

fn shared_home(config: &RunnerConfig, workspace: &Path) -> PathBuf {
  let identity = blake3::hash(workspace.to_string_lossy().as_bytes());
  config
    .data_dir
    .join("_temp/action-home")
    .join(identity.to_hex().as_str())
}

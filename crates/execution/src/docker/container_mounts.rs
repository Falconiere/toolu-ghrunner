//! Individual job bind mounts; registration and authentication files stay private.

use std::path::{Component, Path, PathBuf};

use shared::{RunnerConfig, RunnerError};

use super::path_translator::PathTranslator;

/// Materialized mounts and path mappings for a job.
pub(crate) struct ContainerMounts {
  pub(crate) translator: PathTranslator,
  pub(crate) volumes: Vec<String>,
  pub(crate) temp: PathBuf,
  // Keeps the private job HOME alive until container removal.
  _home: tempfile::TempDir,
}

impl ContainerMounts {
  pub(crate) fn create(config: &RunnerConfig, workspace: &Path) -> Result<Self, RunnerError> {
    let temp = config.data_dir.join("_temp");
    std::fs::create_dir_all(&temp)?;
    let home = tempfile::Builder::new()
      .prefix("container-home-")
      .tempdir_in(&temp)?;
    let mappings = [
      (workspace.to_path_buf(), "/github/workspace", false),
      (temp.clone(), "/github/runner_temp", false),
      (config.data_dir.join("tmp"), "/github/file_commands", false),
      (config.data_dir.join("_tool"), "/github/tool_cache", false),
      (config.data_dir.join("actions"), "/github/actions", false),
      (config.data_dir.join("node"), "/github/node", true),
      (config.data_dir.join("events"), "/github/events", true),
      (home.path().to_path_buf(), "/github/home", false),
    ];
    let mut translator = PathTranslator::new(workspace.to_path_buf(), temp.clone());
    let mut volumes = Vec::new();
    for (host, target, readonly) in mappings {
      std::fs::create_dir_all(&host)?;
      let host = std::path::absolute(host)?;
      if host.to_string_lossy().contains(':') {
        return Err(RunnerError::Docker(
          "job mount source paths cannot contain ':'".to_owned(),
        ));
      }
      translator.add_mapping(host.clone(), PathBuf::from(target));
      volumes.push(format!(
        "{}:{target}:{}",
        host.display(),
        if readonly { "ro" } else { "rw" }
      ));
    }
    Ok(Self {
      translator,
      volumes,
      temp,
      _home: home,
    })
  }
}

/// Reject user mounts that obscure runner-owned paths or use ambiguous syntax.
pub(crate) fn validate_volume(volume: &str) -> Result<(), RunnerError> {
  let pieces: Vec<_> = volume.split(':').collect();
  let destination = match pieces.as_slice() {
    [source, destination] if !source.is_empty() => *destination,
    [source, destination, "ro" | "rw"] if !source.is_empty() => *destination,
    _ => {
      return Err(RunnerError::Docker(
        "container volume must be source:/absolute/destination[:ro|rw]".to_owned(),
      ));
    },
  };
  let path = Path::new(destination);
  if !path.is_absolute()
    || path == Path::new("/")
    || path.starts_with("/github")
    || path.components().any(|c| matches!(c, Component::ParentDir))
  {
    return Err(RunnerError::Docker(
      "container volume destination must be absolute and outside runner-owned /github paths"
        .to_owned(),
    ));
  }
  Ok(())
}

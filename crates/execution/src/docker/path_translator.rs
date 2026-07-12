use std::path::{Path, PathBuf};

/// Translates paths between host and container coordinate systems.
///
/// When steps run inside a job container, paths like `GITHUB_WORKSPACE`
/// must be translated from host paths to container paths.
#[derive(Debug, Clone)]
pub struct PathTranslator {
  host_workspace: PathBuf,
  container_workspace: PathBuf,
  host_temp: PathBuf,
  container_temp: PathBuf,
}

impl PathTranslator {
  /// Create a new translator with the standard GitHub Actions path mapping.
  pub fn new(host_workspace: PathBuf, host_temp: PathBuf) -> Self {
    Self {
      host_workspace,
      container_workspace: PathBuf::from("/github/workspace"),
      host_temp,
      container_temp: PathBuf::from("/github/runner_temp"),
    }
  }

  /// Translate a host path to its container equivalent.
  pub fn to_container(&self, host_path: &Path) -> PathBuf {
    if let Ok(relative) = host_path.strip_prefix(&self.host_workspace) {
      return self.container_workspace.join(relative);
    }
    if let Ok(relative) = host_path.strip_prefix(&self.host_temp) {
      return self.container_temp.join(relative);
    }
    host_path.to_owned()
  }

  /// Translate a container path to its host equivalent.
  pub fn to_host(&self, container_path: &Path) -> PathBuf {
    if let Ok(relative) = container_path.strip_prefix(&self.container_workspace) {
      return self.host_workspace.join(relative);
    }
    if let Ok(relative) = container_path.strip_prefix(&self.container_temp) {
      return self.host_temp.join(relative);
    }
    container_path.to_owned()
  }
}

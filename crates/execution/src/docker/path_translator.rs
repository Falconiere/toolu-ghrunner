//! Component-aware host and container path mappings.

use std::path::{Path, PathBuf};

/// Translates paths between host and container coordinate systems.
///
/// When steps run inside a job container, paths like `GITHUB_WORKSPACE`
/// must be translated from host paths to container paths.
#[derive(Debug, Clone)]
pub struct PathTranslator {
  mappings: Vec<(PathBuf, PathBuf)>,
}

impl PathTranslator {
  /// Create a new translator with the standard GitHub Actions path mapping.
  pub fn new(host_workspace: PathBuf, host_temp: PathBuf) -> Self {
    Self {
      mappings: vec![
        (host_workspace, PathBuf::from("/github/workspace")),
        (host_temp, PathBuf::from("/github/runner_temp")),
      ],
    }
  }

  /// Translate a host path to its container equivalent.
  pub fn to_container(&self, host_path: &Path) -> PathBuf {
    for (host, container) in &self.mappings {
      if let Ok(relative) = host_path.strip_prefix(host) {
        if relative.as_os_str().is_empty() {
          return container.clone();
        }
        return container.join(relative);
      }
    }
    host_path.to_owned()
  }

  /// Translate a container path to its host equivalent.
  pub fn to_host(&self, container_path: &Path) -> PathBuf {
    for (host, container) in &self.mappings {
      if let Ok(relative) = container_path.strip_prefix(container) {
        if relative.as_os_str().is_empty() {
          return host.clone();
        }
        return host.join(relative);
      }
    }
    container_path.to_owned()
  }

  /// Add a mount mapping, preferring the longest host prefix for nested mounts.
  pub fn add_mapping(&mut self, host: PathBuf, container: PathBuf) {
    self.mappings.retain(|(existing, _)| existing != &host);
    self.mappings.push((host, container));
    self
      .mappings
      .sort_by_key(|(host, _)| std::cmp::Reverse(host.components().count()));
  }
}

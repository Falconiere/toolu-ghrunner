//! Job, step, and host-hook environment assembly.

use std::collections::HashMap;

use super::ExecutionContext;

impl ExecutionContext {
  /// Explicit workflow PATH and additions, excluding the host or job image default.
  pub(crate) fn docker_action_path(&self) -> (Option<String>, Vec<String>) {
    (
      self.visible_env().get("PATH").cloned(),
      self.path_additions.clone(),
    )
  }

  /// Merge global env + step env + PATH additions into a full env map.
  pub fn build_step_env(&self, step_env: &HashMap<String, String>) -> HashMap<String, String> {
    let mut base = self.visible_env();
    if let Some(container) = &self.container {
      base
        .entry("PATH".to_owned())
        .or_insert_with(|| container.image_path().to_owned());
      base
        .entry("HOME".to_owned())
        .or_insert_with(|| "/github/home".to_owned());
    }
    self.merge_step_env(base, step_env)
  }

  /// Environment for host hooks, without container HOME or image PATH defaults.
  pub fn build_host_step_env(&self) -> HashMap<String, String> {
    let path_additions = self
      .path_additions
      .iter()
      .map(|path| {
        self.container.as_ref().map_or_else(
          || path.clone(),
          |container| {
            container
              .translator()
              .to_host(std::path::Path::new(path))
              .to_string_lossy()
              .into_owned()
          },
        )
      })
      .collect::<Vec<_>>();
    Self::merge_step_env_with_paths(self.visible_env(), &HashMap::new(), &path_additions)
  }

  fn merge_step_env(
    &self,
    result: HashMap<String, String>,
    step_env: &HashMap<String, String>,
  ) -> HashMap<String, String> {
    Self::merge_step_env_with_paths(result, step_env, &self.path_additions)
  }

  fn merge_step_env_with_paths(
    mut result: HashMap<String, String>,
    step_env: &HashMap<String, String>,
    path_additions: &[String],
  ) -> HashMap<String, String> {
    result.extend(step_env.clone());
    if !path_additions.is_empty() {
      let existing = result
        .get("PATH")
        .cloned()
        .or_else(crate::config::path)
        .unwrap_or_default();
      let mut new_path: Vec<&str> = path_additions.iter().rev().map(String::as_str).collect();
      if !existing.is_empty() {
        new_path.push(&existing);
      }
      result.insert("PATH".to_owned(), new_path.join(":"));
    }
    result
  }
}

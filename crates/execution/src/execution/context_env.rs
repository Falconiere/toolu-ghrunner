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

  /// Set the host-step network export (proxy variables and
  /// `NODE_EXTRA_CA_CERTS`) from `network::step_env(..).host` (#94).
  pub(crate) fn set_network_env(&mut self, env: Vec<(String, String)>) {
    self.network_env = env;
  }

  /// The env a HOST child inherits: the runner process env without its
  /// private `TOOLU_RUNNER_*` namespace ([`super::safe_process_env_vars`]),
  /// overlaid by the job's network export. Callers fold it with `or_insert`
  /// after workflow env, so the export beats the runner's own env but never a
  /// workflow, job, `GITHUB_ENV` or step value.
  pub(crate) fn inherited_process_env(&self) -> Vec<(String, String)> {
    let mut inherited: HashMap<String, String> = super::safe_process_env_vars().collect();
    inherited.extend(self.network_env.iter().cloned());
    inherited.into_iter().collect()
  }

  /// The network export alone, for Node stages whose delta env sits on top
  /// of the inherited process env (`node_exec`).
  pub(crate) fn network_env(&self) -> &[(String, String)] {
    &self.network_env
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
    let mut env = self.merge_step_env(base, step_env);
    super::super::action_metadata::export(self, &mut env);
    env
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

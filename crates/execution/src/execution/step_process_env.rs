//! GitHub Actions process flags applied immediately before a step child starts.

use std::collections::HashMap;

/// Apply the runner's process flags after job and step env precedence is settled.
///
/// `container_ci` is the job container's retained base value. A child that
/// supplies its own `CI` keeps it, including an empty string.
pub(crate) fn apply(env: &mut HashMap<String, String>, container_ci: Option<&str>) {
  env.insert("GITHUB_ACTIONS".to_owned(), "true".to_owned());
  env.entry("CI".to_owned()).or_insert_with(|| {
    container_ci
      .map(str::to_owned)
      .or_else(|| crate::config::var("CI"))
      .unwrap_or_else(|| "true".to_owned())
  });
}

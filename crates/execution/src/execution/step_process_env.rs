//! GitHub Actions process flags applied immediately before a step child starts.

use std::collections::HashMap;

const GITHUB_ACTIONS_ENABLED: &str = "true";

/// Force GitHub Actions mode, default absent `CI`, and tag host children with
/// the job's `RUNNER_TRACKING_ID` after child env is built.
///
/// `container_ci` is the job container's retained base value. A child that
/// supplies its own `CI` keeps it, including an empty string. `tracking` is the
/// host spawn's tracking value (`None` for container and Docker-action execs,
/// which upstream never tags); a workflow-provided `RUNNER_TRACKING_ID` —
/// including the empty-string opt-out — wins over it.
pub(crate) fn apply(
  env: &mut HashMap<String, String>,
  container_ci: Option<&str>,
  tracking: Option<&str>,
) {
  if let Some(id) = tracking {
    env
      .entry(super::orphan_cleanup::TRACKING_ENV.to_owned())
      .or_insert_with(|| id.to_owned());
  }
  env.insert(
    "GITHUB_ACTIONS".to_owned(),
    GITHUB_ACTIONS_ENABLED.to_owned(),
  );
  env.entry("CI".to_owned()).or_insert_with(|| {
    container_ci
      .map(str::to_owned)
      .or_else(|| crate::config::var("CI"))
      .unwrap_or_else(|| "true".to_owned())
  });
}

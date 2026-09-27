//! Captured GitHub.com job metadata, including absent-endpoint diagnostics.

use super::*;

#[tokio::test]
async fn setup_capture_keeps_metadata_without_results_endpoint() {
  let mut msg: AgentJobRequestMessage = serde_json::from_str(include_str!(
    "../../../execution/tests/incoming_contexts_matrix_0.json"
  ))
  .expect("sanitized acquired message");
  msg.variables.remove("system.github.results_endpoint");
  let (_, lines) = report_setup_step("", &msg.plan.plan_id, &msg, &reqwest::Client::new()).await;
  assert!(
    lines
      .iter()
      .any(|line| line == "##[group]GITHUB_TOKEN Permissions")
  );
  let group = lines
    .iter()
    .position(|line| line == "##[group]GITHUB_TOKEN Permissions")
    .expect("permissions group");
  assert_eq!(
    lines.get(group..group + 4),
    Some(
      [
        "##[group]GITHUB_TOKEN Permissions".to_owned(),
        "Contents: read".to_owned(),
        "Metadata: read".to_owned(),
        "##[endgroup]".to_owned(),
      ]
      .as_slice()
    )
  );
  assert!(
    lines
      .iter()
      .any(|line| line == "Runner group name: 'Default'")
  );
  assert!(lines.iter().any(|line| line == "Secret source: Actions"));
}

#[test]
fn setup_capture_has_actual_host_identity_and_explicit_absence() {
  let mut msg: AgentJobRequestMessage = serde_json::from_str(include_str!(
    "../../../execution/tests/incoming_contexts_matrix_0.json"
  ))
  .expect("capture");
  msg.variables.remove("system.runnerGroupName");
  msg.variables.remove("system.github.token.permissions");
  msg.context_data.remove("github");
  let lines = metadata_lines(&msg);
  assert!(lines.contains(&format!(
    "Toolu runner version: '{}'",
    env!("CARGO_PKG_VERSION")
  )));
  assert!(lines.contains(&format!(
    "Operating system: '{}'",
    shared::platform::runner_os()
  )));
  assert!(lines.contains(&format!(
    "Architecture: '{}'",
    shared::platform::runner_arch()
  )));
  assert!(lines.contains(&format!(
    "Machine name: '{}'",
    hostname::get().expect("host").to_string_lossy()
  )));
  assert!(lines.contains(&"Runner group name: 'unavailable'".to_owned()));
  assert!(
    !lines
      .iter()
      .any(|line| line.contains("GITHUB_TOKEN") || line.starts_with("Secret source:"))
  );
  assert_eq!(available(None), "unavailable");
  assert_eq!(available(Some("")), "unavailable");
}

#[test]
fn setup_capture_malformed_permissions_are_not_reflected() {
  let mut msg: AgentJobRequestMessage = serde_json::from_str(include_str!(
    "../../../execution/tests/incoming_contexts_matrix_0.json"
  ))
  .expect("capture");
  let raw = msg
    .variables
    .get("system.github.token.permissions")
    .expect("permissions")
    .value
    .clone();
  let truncated = raw.trim_end_matches('}');
  msg
    .variables
    .get_mut("system.github.token.permissions")
    .expect("permissions")
    .value = truncated.to_owned();
  let lines = metadata_lines(&msg);
  assert!(lines.contains(&"Unable to parse GITHUB_TOKEN permissions metadata.".to_owned()));
  assert!(!lines.iter().any(|line| line.contains(truncated)));
  assert!(!lines.iter().any(|line| line.starts_with("##[group]")));
}

#[test]
fn setup_empty_permissions_are_explicitly_optional() {
  let mut msg: AgentJobRequestMessage = serde_json::from_str(include_str!(
    "../../../execution/tests/incoming_contexts_matrix_0.json"
  ))
  .expect("capture");
  msg
    .variables
    .get_mut("system.github.token.permissions")
    .expect("permissions")
    .value
    .clear();
  assert!(
    !metadata_lines(&msg)
      .iter()
      .any(|line| line.contains("GITHUB_TOKEN"))
  );
  msg
    .variables
    .get_mut("system.github.token.permissions")
    .expect("permissions")
    .value = "{}".to_owned();
  let lines = metadata_lines(&msg);
  let group = lines
    .iter()
    .position(|line| line == "##[group]GITHUB_TOKEN Permissions")
    .expect("empty group");
  assert_eq!(
    lines.get(group + 1).map(String::as_str),
    Some("##[endgroup]")
  );
}

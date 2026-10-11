//! `action_metadata` against the references captured in
//! `completejob-88.yml` run 38098611227 and upstream's mapping rules.

use shared::ActionStepDefinitionReference;

use super::action_metadata;
use crate::execution::actions::manifest::RunsUsing;

type TestResult = Result<(), serde_json::Error>;

fn reference(json: serde_json::Value) -> Result<ActionStepDefinitionReference, serde_json::Error> {
  serde_json::from_value(json)
}

fn triple(
  kind: &str,
  action: &str,
  git_ref: Option<&str>,
) -> (String, Option<String>, Option<String>) {
  (
    kind.to_owned(),
    Some(action.to_owned()),
    git_ref.map(str::to_owned),
  )
}

#[test]
fn captured_remote_action_reports_name_ref_and_runtime() -> TestResult {
  let checkout = reference(serde_json::json!({
    "type": "repository", "name": "actions/checkout", "ref": "v4", "repositoryType": "GitHub"
  }))?;
  assert_eq!(
    action_metadata(&checkout, &RunsUsing::Node { major: 20 }),
    triple("node20", "actions/checkout", Some("v4"))
  );
  Ok(())
}

#[test]
fn remote_subpath_action_joins_name_and_path() -> TestResult {
  let nested = reference(serde_json::json!({
    "type": "repository", "name": "github/codeql-action", "path": "init",
    "ref": "v3", "repositoryType": "GitHub"
  }))?;
  assert_eq!(
    action_metadata(&nested, &RunsUsing::Composite),
    triple("composite", "github/codeql-action/init", Some("v3"))
  );
  Ok(())
}

#[test]
fn self_action_reports_only_its_path() -> TestResult {
  let local = reference(serde_json::json!({
    "type": "repository", "repositoryType": "self",
    "path": "./.github/actions/completejob-88-docker"
  }))?;
  assert_eq!(
    action_metadata(&local, &RunsUsing::Docker),
    triple(
      "Dockerfile",
      "./.github/actions/completejob-88-docker",
      None
    )
  );
  Ok(())
}

#[test]
fn registry_step_is_dockerhub_with_its_image() -> TestResult {
  let registry = reference(serde_json::json!({
    "type": "containerRegistry", "image": "alpine:3.20"
  }))?;
  assert_eq!(
    action_metadata(&registry, &RunsUsing::Docker),
    triple("DockerHub", "alpine:3.20", None)
  );
  let prefixed = reference(serde_json::json!({
    "type": "containerRegistry", "image": "docker://alpine:3.20"
  }))?;
  assert_eq!(
    action_metadata(&prefixed, &RunsUsing::Docker),
    triple("DockerHub", "alpine:3.20", None)
  );
  Ok(())
}

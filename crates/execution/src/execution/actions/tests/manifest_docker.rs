//! Docker fields from the committed executable acceptance action.

use super::parse_action_manifest;

#[test]
fn preserves_docker_stage_and_argument_contract() -> Result<(), Box<dyn std::error::Error>> {
  let action = parse_action_manifest(include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../.github/actions/docker-action-probe/action.yml"
  )))?;
  assert_eq!(action.runs.image.as_deref(), Some("Dockerfile"));
  assert_eq!(action.runs.entrypoint.as_deref(), Some("/probe/main.sh"));
  assert_eq!(action.runs.pre_entrypoint.as_deref(), Some("/probe/pre.sh"));
  assert_eq!(
    action.runs.post_entrypoint.as_deref(),
    Some("/probe/post.sh")
  );
  assert_eq!(
    action.runs.args,
    Some(vec![
      String::new(),
      "two words".to_owned(),
      "${{ inputs.marker }}".to_owned()
    ])
  );
  assert_eq!(
    action.runs.env.get("DEFAULT_ONLY").map(String::as_str),
    Some("manifest-default")
  );
  assert_eq!(action.runs.pre_if.as_deref(), Some("runner.os == 'Linux'"));
  assert_eq!(action.runs.post_if.as_deref(), Some("always()"));
  let absent = parse_action_manifest(include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../.github/actions/docker-action-probe/action-absent-args.yml"
  )))?;
  let empty = parse_action_manifest(include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../.github/actions/docker-action-probe/action-empty-args.yml"
  )))?;
  assert_eq!(absent.runs.args, None);
  assert_eq!(empty.runs.args, Some(vec![]));
  Ok(())
}

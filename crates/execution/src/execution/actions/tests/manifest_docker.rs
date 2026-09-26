//! Docker fields from the committed executable acceptance action.
//!
//! These exact files also drive `Runner::execute_job` in
//! `crates/execution/tests/docker_action_linux_test.rs`:
//! - `action.yml`: `local_actions_preserve_argv_env_commands_state_and_lifo_posts`;
//! - `action-absent-args.yml` and `action-empty-args.yml`:
//!   `registry_and_manifest_arg_variants_execute_exact_argv`.
//! That suite's `seed_probe` copies each selected manifest verbatim to the local
//! action's `action.yml`; renaming the variants does not change their contents.

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

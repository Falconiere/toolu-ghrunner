//! Docker fields from the committed executable acceptance action.
//!
//! These exact files also drive `Runner::execute_job` in
//! `crates/execution/tests/docker_action_linux_test.rs`:
//! - `action.yml`: `local_actions_preserve_argv_env_commands_state_and_lifo_posts`;
//! - `action-absent-args.yml` and `action-empty-args.yml`:
//!   `registry_and_manifest_arg_variants_execute_exact_argv`.
//!
//! That suite's `seed_probe` copies each selected manifest verbatim to the local
//! action's `action.yml`; renaming the variants does not change their contents.

use super::{DockerStage, stage_values};
use crate::execution::actions::manifest::parse_action_manifest;

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

  // Consume the parsed fixtures through the same argument/environment builder
  // that run_docker_stage uses immediately before constructing container params.
  let job: shared::AgentJobRequestMessage = serde_json::from_str(include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../toolu-runner/tests/fixtures/job_container_message.json"
  )))?;
  let step = job.steps.first().ok_or("captured job has no steps")?;
  let mut ctx = crate::execution::context::ExecutionContext::new_for_test();
  ctx.set_env("PATH", "/probe");
  let config = shared::RunnerConfig::default();
  let action_dir = std::path::Path::new(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../.github/actions/docker-action-probe"
  ));
  let bounds = crate::execution::step_timeout::StepBounds {
    deadline: None,
    cancel: tokio_util::sync::CancellationToken::new(),
  };
  let (events, _receiver) = tokio::sync::mpsc::channel(1);
  let inputs = std::collections::HashMap::from([("marker".to_owned(), action.name.clone())]);
  let stage = DockerStage {
    step,
    events: &events,
    workspace: action_dir,
    config: &config,
    action_dir,
    manifest: &action,
    bounds: &bounds,
    stage: "main",
    log_step_id: &step.id,
    inputs: Some(&inputs),
  };
  for (name, expected_entrypoint) in [
    ("pre", "/probe/pre.sh"),
    ("main", "/probe/main.sh"),
    ("post", "/probe/post.sh"),
  ] {
    let (env, args, entrypoint) = stage_values(
      &DockerStage {
        stage: name,
        ..stage
      },
      &ctx,
      &inputs,
    )?;
    assert_eq!(entrypoint.as_deref(), Some(expected_entrypoint));
    assert_eq!(
      args,
      Some(vec![
        String::new(),
        "two words".to_owned(),
        action.name.clone()
      ])
    );
    assert_eq!(env.get("INPUT_MARKER"), Some(&action.name));
    assert_eq!(env.get("PATH").map(String::as_str), Some("/probe"));
    assert_eq!(
      env.get("DEFAULT_ONLY").map(String::as_str),
      Some("manifest-default")
    );
  }
  for (manifest, expected_args) in [(&absent, None), (&empty, Some(vec![]))] {
    let (_, args, entrypoint) = stage_values(&DockerStage { manifest, ..stage }, &ctx, &inputs)?;
    assert_eq!(args, expected_args);
    assert_eq!(entrypoint.as_deref(), Some("/probe/main.sh"));
  }
  Ok(())
}

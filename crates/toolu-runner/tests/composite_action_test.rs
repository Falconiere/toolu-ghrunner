//! Real-data tests for composite action handling (AC #11).
//!
//! Covers:
//! - parsing a real-shape `action.yml` with `runs.using: composite`
//!   (the shape GitHub ships for composite actions).
//! - `prepare_composite` builds a scoped execution context and
//!   increments the depth tracker.
//! - `evaluate_outputs` resolves `${{ steps.X.outputs.Y }}` references
//!   against the child step's outputs.
//! - `parse_action_manifest` rejects unsupported `runs.using` values.
//! - handler dispatch (`resolve_handler`) classifies composite steps.

use std::collections::HashMap;

use execution::execution::actions::manifest::{self, RunsUsing};
use execution::execution::depth_tracker::DepthTracker;
use execution::execution::handlers::composite::prepare_composite;
use execution::execution::handlers::resolve::{HandlerKind, resolve_handler};
use execution::plugin::PluginRegistry;
use shared::ActionStep;

const COMPOSITE_ACTION_YML: &str = r#"
name: 'My Composite'
description: 'Run a few steps in order'
inputs:
  who-to-greet:
    description: 'Who to greet'
    required: true
    default: 'World'
outputs:
  greeting:
    description: 'the greeting we generated'
    value: ${{ steps.greet.outputs.greeting }}
runs:
  using: 'composite'
  steps:
    - id: greet
      name: Set greeting
      run: echo "::set-output name=greeting::hello ${{ inputs.who-to-greet }}"
      shell: bash
    - id: done
      name: Mark done
      run: echo "done"
"#;

#[test]
fn parse_action_manifest_composite_yml_extracts_fields() {
  let def = manifest::parse_action_manifest(COMPOSITE_ACTION_YML).expect("parse");
  assert_eq!(def.name, "My Composite");
  assert_eq!(def.description, "Run a few steps in order");
  assert!(matches!(def.runs.using, RunsUsing::Composite));
  let steps = def.runs.steps;
  assert_eq!(steps.len(), 2);
  let greet = steps.first().expect("first step present");
  assert_eq!(greet.id.as_deref(), Some("greet"));
  assert_eq!(
    greet.run.as_deref(),
    Some("echo \"::set-output name=greeting::hello ${{ inputs.who-to-greet }}\""),
  );
  assert_eq!(greet.shell.as_deref(), Some("bash"));
  let done = steps.get(1).expect("second step present");
  assert_eq!(done.id.as_deref(), Some("done"));
}

#[test]
fn parse_action_manifest_composite_collects_outputs_with_expressions() {
  let def = manifest::parse_action_manifest(COMPOSITE_ACTION_YML).expect("parse");
  let out = def.outputs.get("greeting").expect("greeting output");
  assert_eq!(
    out.value.as_deref(),
    Some("${{ steps.greet.outputs.greeting }}"),
  );
}

#[test]
fn parse_action_manifest_node20_uses_runs_using_node() {
  let yaml = r#"
name: 'Node Action'
description: 'Does the thing'
runs:
  using: 'node20'
  main: 'dist/index.js'
"#;
  let def = manifest::parse_action_manifest(yaml).expect("parse node");
  let is_node = matches!(def.runs.using, RunsUsing::Node { .. });
  assert!(is_node, "expected Node, got {:?}", def.runs.using);
  if let RunsUsing::Node { major } = def.runs.using {
    assert_eq!(major, 20);
  }
  assert_eq!(def.runs.main.as_deref(), Some("dist/index.js"));
}

#[test]
fn parse_action_manifest_deprecated_node16_redirects_to_20() {
  let yaml = r#"
name: 'Node16 Action'
description: 'Old'
runs:
  using: 'node16'
  main: 'dist/index.js'
"#;
  let def = manifest::parse_action_manifest(yaml).expect("parse node16");
  let is_node = matches!(def.runs.using, RunsUsing::Node { .. });
  assert!(is_node, "expected Node, got {:?}", def.runs.using);
  if let RunsUsing::Node { major } = def.runs.using {
    assert_eq!(major, 20, "node16 should redirect to node20");
  }
}

#[test]
fn parse_action_manifest_unsupported_using_returns_error() {
  let yaml = r#"
name: 'Bad'
description: 'Unsupported using'
runs:
  using: 'fortran77'
  main: 'src/main.f'
"#;
  let err = manifest::parse_action_manifest(yaml).expect_err("unsupported");
  let msg = format!("{err}");
  assert!(msg.contains("unsupported runs.using"), "got: {msg}");
}

#[test]
fn prepare_composite_creates_scope_and_increments_depth() {
  let def = manifest::parse_action_manifest(COMPOSITE_ACTION_YML).expect("parse");
  let mut depth = DepthTracker::new();
  let initial_depth = depth.current();
  let execution = prepare_composite(&def, "step-1", &mut depth).expect("prepare");
  assert_eq!(execution.step_id, "step-1");
  assert_eq!(depth.current(), initial_depth + 1, "depth should increment");
  let exprs = execution.outputs.expressions();
  assert!(exprs.contains_key("greeting"));
  assert_eq!(
    exprs.get("greeting").map(String::as_str),
    Some("${{ steps.greet.outputs.greeting }}"),
  );
}

#[test]
fn prepare_composite_rejects_when_depth_limit_exceeded() {
  let def = manifest::parse_action_manifest(COMPOSITE_ACTION_YML).expect("parse");
  let mut depth = DepthTracker::new();
  for _ in 0..10 {
    depth.enter().expect("enter level");
  }
  let result = prepare_composite(&def, "step-deep", &mut depth);
  assert!(result.is_err(), "expected depth limit error");
}

#[test]
fn evaluate_outputs_resolves_step_output_references() {
  let def = manifest::parse_action_manifest(COMPOSITE_ACTION_YML).expect("parse");
  let mut depth = DepthTracker::new();
  let execution = prepare_composite(&def, "step-1", &mut depth).expect("prepare");

  let mut child_outputs: HashMap<String, HashMap<String, String>> = HashMap::new();
  let mut greet = HashMap::new();
  greet.insert("greeting".to_owned(), "hello World".to_owned());
  child_outputs.insert("greet".to_owned(), greet);

  let outputs = execution.evaluate_outputs(&child_outputs);
  assert_eq!(
    outputs.get("greeting").map(String::as_str),
    Some("hello World")
  );
}

#[test]
fn evaluate_outputs_skips_unresolvable_references() {
  let def = manifest::parse_action_manifest(COMPOSITE_ACTION_YML).expect("parse");
  let mut depth = DepthTracker::new();
  let execution = prepare_composite(&def, "step-1", &mut depth).expect("prepare");

  let outputs = execution.evaluate_outputs(&HashMap::new());
  assert!(!outputs.contains_key("greeting"), "got: {outputs:?}");
}

#[test]
fn resolve_handler_routes_composite_using_to_composite() {
  let step = ActionStep::with_ref_type("my-composite", "composite");
  let plugins = PluginRegistry::new();
  let handler = resolve_handler(&step, &plugins);
  assert_eq!(handler, HandlerKind::Composite);
}

#[test]
fn resolve_handler_runs_to_script_for_run_step() {
  let step = ActionStep::script("echo-step", "echo hi", "");
  let plugins = PluginRegistry::new();
  let handler = resolve_handler(&step, &plugins);
  assert_eq!(handler, HandlerKind::Script);
}

#[test]
fn resolve_handler_routes_node_runs_to_node_handler() {
  let step = ActionStep::with_ref_type("checkout", "node20");
  let plugins = PluginRegistry::new();
  let handler = resolve_handler(&step, &plugins);
  assert_eq!(handler, HandlerKind::Node);
}

#[test]
fn resolve_handler_routes_docker_to_docker_handler() {
  let step = ActionStep::with_ref_type("alpine", "docker");
  let plugins = PluginRegistry::new();
  let handler = resolve_handler(&step, &plugins);
  assert_eq!(handler, HandlerKind::Docker);
}

#[test]
fn resolve_handler_routes_unknown_using_to_unknown_variant() {
  let step = ActionStep::with_ref_type("weird", "telepathy");
  let plugins = PluginRegistry::new();
  let handler = resolve_handler(&step, &plugins);
  let is_unknown = matches!(handler, HandlerKind::Unknown(_));
  assert!(is_unknown, "expected Unknown, got {handler:?}");
  if let HandlerKind::Unknown(name) = handler {
    assert_eq!(name, "telepathy");
  }
}

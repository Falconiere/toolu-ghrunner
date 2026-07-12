//! Real-data tests for reusable workflow resolution (AC #10).
//!
//! Covers:
//! - `parse_reusable_ref` for `owner/repo/path@ref` shape (the format
//!   that appears in a real workflow file's `uses:`).
//! - input validation (`validate_inputs`) and resolution
//!   (`resolve_inputs` — caller values win over defaults).
//! - secret validation (`validate_secrets`) for both `inherit` and
//!   explicit modes.
//! - output resolution (`resolve_outputs`) from a job output map.
//! - depth / circular-reference checks against the spec's limits.

use std::collections::HashMap;

use execution::execution::workflow::reusable::{
  self, InputDef, OutputDef, ResolveContext, ReusableWorkflowDef, SecretDef, SecretMode,
  build_caller_context, check_nesting_depth, parse_reusable_ref, resolve_reusable_invocation,
};

fn str_map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
  let mut m = HashMap::new();
  for (k, v) in pairs {
    m.insert((*k).to_owned(), (*v).to_owned());
  }
  m
}

fn def_with_inputs(inputs: &[(&str, bool, Option<&str>)]) -> ReusableWorkflowDef {
  let mut m = HashMap::new();
  for (name, required, default) in inputs {
    m.insert(
      (*name).to_owned(),
      InputDef {
        required: *required,
        default: default.map(str::to_owned),
      },
    );
  }
  ReusableWorkflowDef {
    inputs: m,
    outputs: HashMap::new(),
    secrets: HashMap::new(),
  }
}

#[test]
fn parse_reusable_ref_with_simple_path() {
  let r = parse_reusable_ref("Falconiere/toolu-ghrunner/.github/workflows/reusable.yml@v1")
    .expect("parse");
  assert_eq!(r.owner, "Falconiere");
  assert_eq!(r.repo, "toolu-ghrunner");
  assert_eq!(r.path, ".github/workflows/reusable.yml");
  assert_eq!(r.git_ref, "v1");
}

#[test]
fn parse_reusable_ref_with_nested_path() {
  let r = parse_reusable_ref("org/repo/.github/workflows/sub/dir/workflow.yml@main")
    .expect("parse nested");
  assert_eq!(r.path, ".github/workflows/sub/dir/workflow.yml");
  assert_eq!(r.git_ref, "main");
}

#[test]
fn parse_reusable_ref_with_sha() {
  let r =
    parse_reusable_ref("Falconiere/toolu-ghrunner/.github/workflows/build.yml@abc1234567890abcdef")
      .expect("parse sha");
  assert_eq!(r.git_ref, "abc1234567890abcdef");
}

#[test]
fn parse_reusable_ref_rejects_missing_at_sign() {
  let err = parse_reusable_ref("Falconiere/toolu-ghrunner/path/to/workflow.yml").expect_err("no @");
  let msg = format!("{err}");
  assert!(msg.contains("missing @ref"), "got: {msg}");
}

#[test]
fn parse_reusable_ref_rejects_empty_ref() {
  let err = parse_reusable_ref("Falconiere/toolu-ghrunner/.github/workflows/reusable.yml@")
    .expect_err("empty @");
  let msg = format!("{err}");
  assert!(
    msg.contains("empty ref") || msg.contains("missing @ref"),
    "got: {msg}"
  );
}

#[test]
fn parse_reusable_ref_rejects_missing_path() {
  let err = parse_reusable_ref("Falconiere/toolu-ghrunner@main").expect_err("no path");
  let msg = format!("{err}");
  assert!(
    msg.contains("missing workflow path") || msg.contains("missing"),
    "got: {msg}"
  );
}

#[test]
fn validate_inputs_passes_when_required_inputs_provided() {
  let def = def_with_inputs(&[("name", true, None), ("region", false, Some("us-east-1"))]);
  let provided = str_map(&[("name", "alice")]);
  reusable::validate_inputs(&def.inputs, &provided).expect("should validate");
}

#[test]
fn validate_inputs_fails_when_required_input_missing() {
  let def = def_with_inputs(&[("name", true, None)]);
  let provided = str_map(&[]);
  let err = reusable::validate_inputs(&def.inputs, &provided).expect_err("missing required");
  let msg = format!("{err}");
  assert!(msg.contains("required input 'name'"), "got: {msg}");
}

#[test]
fn validate_inputs_passes_when_required_has_default() {
  let def = def_with_inputs(&[("region", true, Some("us-east-1"))]);
  let provided = str_map(&[]);
  reusable::validate_inputs(&def.inputs, &provided).expect("default satisfies required");
}

#[test]
fn resolve_inputs_uses_caller_value_over_default() {
  let def = def_with_inputs(&[("region", false, Some("us-east-1"))]);
  let provided = str_map(&[("region", "eu-west-2")]);
  let resolved = reusable::resolve_inputs(&def.inputs, &provided);
  assert_eq!(
    resolved.get("region").map(String::as_str),
    Some("eu-west-2")
  );
}

#[test]
fn resolve_inputs_uses_default_when_caller_omits() {
  let def = def_with_inputs(&[("region", false, Some("us-east-1"))]);
  let provided = str_map(&[]);
  let resolved = reusable::resolve_inputs(&def.inputs, &provided);
  assert_eq!(
    resolved.get("region").map(String::as_str),
    Some("us-east-1")
  );
}

#[test]
fn resolve_inputs_skips_inputs_neither_provided_nor_defaulted() {
  let def = def_with_inputs(&[("region", false, None)]);
  let provided = str_map(&[]);
  let resolved = reusable::resolve_inputs(&def.inputs, &provided);
  assert!(!resolved.contains_key("region"));
}

#[test]
fn validate_secrets_inherit_passes_through_all_caller_secrets() {
  let mut def = ReusableWorkflowDef {
    inputs: HashMap::new(),
    outputs: HashMap::new(),
    secrets: HashMap::new(),
  };
  def
    .secrets
    .insert("GH_TOKEN".to_owned(), SecretDef { required: true });
  let provided = str_map(&[("GH_TOKEN", "secret-value")]);
  let resolved =
    reusable::validate_secrets(&SecretMode::Inherit, &def.secrets, &provided).expect("inherit");
  assert_eq!(
    resolved.get("GH_TOKEN").map(String::as_str),
    Some("secret-value")
  );
}

#[test]
fn validate_secrets_explicit_fails_when_required_missing() {
  let mut def = ReusableWorkflowDef {
    inputs: HashMap::new(),
    outputs: HashMap::new(),
    secrets: HashMap::new(),
  };
  def
    .secrets
    .insert("GH_TOKEN".to_owned(), SecretDef { required: true });
  let provided = str_map(&[]);
  let mapping = HashMap::new();
  let err = reusable::validate_secrets(&SecretMode::Explicit(mapping), &def.secrets, &provided)
    .expect_err("missing required");
  let msg = format!("{err}");
  assert!(msg.contains("required secret 'GH_TOKEN'"), "got: {msg}");
}

#[test]
fn validate_secrets_explicit_passes_when_required_provided() {
  let mut def = ReusableWorkflowDef {
    inputs: HashMap::new(),
    outputs: HashMap::new(),
    secrets: HashMap::new(),
  };
  def
    .secrets
    .insert("GH_TOKEN".to_owned(), SecretDef { required: true });
  let mut mapping = HashMap::new();
  mapping.insert("GH_TOKEN".to_owned(), "secret-value".to_owned());
  let resolved =
    reusable::validate_secrets(&SecretMode::Explicit(mapping), &def.secrets, &str_map(&[]))
      .expect("explicit");
  assert_eq!(
    resolved.get("GH_TOKEN").map(String::as_str),
    Some("secret-value")
  );
}

#[test]
fn resolve_outputs_maps_simple_jobs_outputs_expression() {
  let mut outputs = HashMap::new();
  outputs.insert(
    "version".to_owned(),
    OutputDef {
      description: Some("the build version".to_owned()),
      value: "${{ jobs.build.outputs.version }}".to_owned(),
    },
  );

  let mut job_outputs: HashMap<String, HashMap<String, String>> = HashMap::new();
  let mut build = HashMap::new();
  build.insert("version".to_owned(), "1.2.3".to_owned());
  job_outputs.insert("build".to_owned(), build);

  let resolved = reusable::resolve_outputs(&outputs, &job_outputs);
  assert_eq!(resolved.get("version").map(String::as_str), Some("1.2.3"));
}

#[test]
fn resolve_outputs_returns_empty_string_for_missing_job_output() {
  let mut outputs = HashMap::new();
  outputs.insert(
    "version".to_owned(),
    OutputDef {
      description: None,
      value: "${{ jobs.build.outputs.version }}".to_owned(),
    },
  );
  let resolved = reusable::resolve_outputs(&outputs, &HashMap::new());
  assert_eq!(resolved.get("version").map(String::as_str), Some(""));
}

#[test]
fn check_nesting_depth_allows_four_levels() {
  // GitHub's documented limit is 4 levels of nesting.
  reusable::check_nesting_depth(4).expect("4 is allowed");
  reusable::check_nesting_depth(3).expect("3 is allowed");
}

#[test]
fn check_nesting_depth_rejects_five_levels() {
  let err = check_nesting_depth(5).expect_err("5 is too deep");
  let msg = format!("{err}");
  assert!(
    msg.contains("depth") || msg.contains("exceeded"),
    "got: {msg}"
  );
}

#[test]
fn check_circular_reference_passes_when_ref_not_in_stack() {
  let stack = vec![
    "org/repo/.github/workflows/a.yml@v1".to_owned(),
    "org/repo/.github/workflows/b.yml@v1".to_owned(),
  ];
  reusable::check_circular_reference(&stack, "org/repo/.github/workflows/c.yml@v1")
    .expect("no cycle");
}

#[test]
fn check_circular_reference_rejects_when_ref_already_in_stack() {
  let stack = vec![
    "org/repo/.github/workflows/a.yml@v1".to_owned(),
    "org/repo/.github/workflows/b.yml@v1".to_owned(),
  ];
  let err = reusable::check_circular_reference(&stack, "org/repo/.github/workflows/a.yml@v1")
    .expect_err("cycle");
  let msg = format!("{err}");
  assert!(msg.contains("circular"), "got: {msg}");
  assert!(msg.contains("a.yml"), "got: {msg}");
}

#[test]
fn resolve_reusable_invocation_happy_path() {
  let mut def = ReusableWorkflowDef {
    inputs: HashMap::new(),
    outputs: HashMap::new(),
    secrets: HashMap::new(),
  };
  def.inputs.insert(
    "version".to_owned(),
    InputDef {
      required: false,
      default: Some("1.0.0".to_owned()),
    },
  );
  def
    .secrets
    .insert("GH_TOKEN".to_owned(), SecretDef { required: true });

  let ctx = ResolveContext {
    call_stack: vec![],
    current_ref: "Falconiere/toolu-ghrunner/.github/workflows/lib.yml@v1".to_owned(),
    current_depth: 1,
  };
  let caller_inputs = str_map(&[("version", "2.0.0")]);
  let caller_secrets = str_map(&[("GH_TOKEN", "real-secret-value")]);
  let resolved = resolve_reusable_invocation(
    &def,
    &caller_inputs,
    &SecretMode::Inherit,
    &caller_secrets,
    &ctx,
  )
  .expect("resolve");
  assert_eq!(
    resolved.inputs.get("version").map(String::as_str),
    Some("2.0.0")
  );
  assert_eq!(
    resolved.secrets.get("GH_TOKEN").map(String::as_str),
    Some("real-secret-value"),
  );
}

#[test]
fn build_caller_context_clones_inputs_and_secrets() {
  let inputs = str_map(&[("k", "v")]);
  let secrets = str_map(&[("s", "t")]);
  let ctx = build_caller_context(&inputs, &secrets);
  assert_eq!(ctx.inputs.get("k").map(String::as_str), Some("v"));
  assert_eq!(ctx.secrets.get("s").map(String::as_str), Some("t"));
}

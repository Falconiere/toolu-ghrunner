//! Real-data tests for the `${{ }}` expression evaluator (AC #5).
//!
//! Covers the public surface of `execution::execution::expressions`:
//! - Literal evaluation
//! - Context / property / index access
//! - Function calls (`contains`, `startsWith`, `endsWith`, `format`, `join`,
//!   `toJson`, `fromJson`, `success`, `failure`, `always`, `cancelled`)
//! - Unary and binary operators with GitHub's loose-equality rules
//! - Wildcard expansion
//!
//! Inputs are real-shape GitHub Actions expressions; the contexts are
//! constructed from the same `HashMap` shape the engine feeds in.
//!
//! `ExprValue` intentionally does not implement `PartialEq` (it carries
//! `HashMap` and `Vec`, which do). Tests use `matches!` and pattern
//! matching against the discriminant for assertions.

use std::collections::HashMap;

use expressions::evaluator::{self, EvalContext, JobStatus};
use expressions::types::ExprValue;
use serde_json::Value;

fn object_from(pairs: &[(&str, ExprValue)]) -> ExprValue {
  let mut map = HashMap::new();
  for (k, v) in pairs {
    map.insert((*k).to_owned(), v.clone());
  }
  ExprValue::Object(map)
}

fn array_from(values: &[ExprValue]) -> ExprValue {
  ExprValue::Array(values.to_vec())
}

fn context(pairs: &[(&str, ExprValue)]) -> EvalContext {
  let mut map = HashMap::new();
  for (k, v) in pairs {
    map.insert((*k).to_owned(), v.clone());
  }
  EvalContext {
    contexts: map,
    job_status: JobStatus::Success,
    workspace: None,
  }
}

/// Assert `v` is `ExprValue::String(s)` where `s == expected`. The
/// "is string" check uses a runtime pattern match so the assert
/// never reads as a constant condition to clippy.
fn assert_string(v: &ExprValue, expected: &str) {
  let actual = if let ExprValue::String(s) = v {
    s.clone()
  } else {
    String::new()
  };
  let matched = matches!(v, ExprValue::String(_));
  assert!(matched, "expected String({expected}), got {v:?}");
  assert_eq!(actual, expected, "string mismatch: {actual}");
}

fn assert_bool(v: &ExprValue, expected: bool) {
  let actual = if let ExprValue::Bool(b) = v {
    Some(*b)
  } else {
    None
  };
  let matched = actual.is_some();
  let actual = actual.unwrap_or(false);
  assert!(matched, "expected Bool({expected}), got {v:?}");
  assert_eq!(actual, expected, "bool mismatch");
}

fn assert_number(v: &ExprValue, expected: f64) {
  let actual = if let ExprValue::Number(n) = v {
    Some(*n)
  } else {
    None
  };
  let matched = actual.is_some();
  let actual = actual.unwrap_or(0.0);
  assert!(matched, "expected Number({expected}), got {v:?}");
  assert_eq!(actual, expected, "number mismatch");
}

#[test]
fn literal_string_evaluates_to_string() {
  let ctx = context(&[]);
  let v = evaluator::evaluate("'hello world'", &ctx).expect("string literal");
  assert_string(&v, "hello world");
}

#[test]
fn literal_number_evaluates_to_number() {
  let ctx = context(&[]);
  let v = evaluator::evaluate("42", &ctx).expect("number literal");
  assert_number(&v, 42.0);
}

#[test]
fn literal_bool_true_and_false_evaluate() {
  let ctx = context(&[]);
  assert_bool(&evaluator::evaluate("true", &ctx).unwrap(), true);
  assert_bool(&evaluator::evaluate("false", &ctx).unwrap(), false);
}

#[test]
fn literal_null_evaluates_to_null() {
  let ctx = context(&[]);
  let v = evaluator::evaluate("null", &ctx).expect("null literal");
  let is_null = matches!(v, ExprValue::Null);
  assert!(is_null, "expected Null, got {v:?}");
}

#[test]
fn context_github_resolves_to_object() {
  let ctx = context(&[(
    "github",
    object_from(&[
      (
        "repository",
        ExprValue::String("Falconiere/toolu-ghrunner".to_owned()),
      ),
      ("sha", ExprValue::String("abc123".to_owned())),
    ]),
  )]);
  let v = evaluator::evaluate("github", &ctx).expect("github context");
  let is_object = matches!(v, ExprValue::Object(_));
  assert!(is_object, "expected Object, got {v:?}");
  if let ExprValue::Object(map) = v {
    assert_eq!(map.len(), 2);
    let has_repo = map.contains_key("repository");
    let has_sha = map.contains_key("sha");
    assert!(has_repo, "missing repository");
    assert!(has_sha, "missing sha");
  }
}

#[test]
fn property_access_github_repository_returns_string() {
  let ctx = context(&[(
    "github",
    object_from(&[(
      "repository",
      ExprValue::String("Falconiere/toolu-ghrunner".to_owned()),
    )]),
  )]);
  let v = evaluator::evaluate("github.repository", &ctx).expect("property access");
  assert_string(&v, "Falconiere/toolu-ghrunner");
}

#[test]
fn property_access_is_case_insensitive() {
  let ctx = context(&[(
    "GitHub",
    object_from(&[("Repository", ExprValue::String("owner/repo".to_owned()))]),
  )]);
  assert_string(
    &evaluator::evaluate("github.repository", &ctx).unwrap(),
    "owner/repo",
  );
  assert_string(
    &evaluator::evaluate("GITHUB.REPOSITORY", &ctx).unwrap(),
    "owner/repo",
  );
}

#[test]
fn property_access_missing_returns_null() {
  let ctx = context(&[(
    "github",
    object_from(&[("repository", ExprValue::String("owner/repo".to_owned()))]),
  )]);
  let v = evaluator::evaluate("github.nonexistent", &ctx).expect("missing property");
  let is_null = matches!(v, ExprValue::Null);
  assert!(is_null, "expected Null, got {v:?}");
}

#[test]
fn index_access_on_array_returns_element() {
  let ctx = context(&[(
    "matrix",
    array_from(&[
      ExprValue::String("a".to_owned()),
      ExprValue::String("b".to_owned()),
      ExprValue::String("c".to_owned()),
    ]),
  )]);
  assert_string(&evaluator::evaluate("matrix[0]", &ctx).unwrap(), "a");
  assert_string(&evaluator::evaluate("matrix[2]", &ctx).unwrap(), "c");
}

#[test]
fn index_access_out_of_bounds_returns_null() {
  let ctx = context(&[(
    "matrix",
    array_from(&[ExprValue::String("only".to_owned())]),
  )]);
  let v = evaluator::evaluate("matrix[5]", &ctx).expect("out of bounds");
  let is_null = matches!(v, ExprValue::Null);
  assert!(is_null, "expected Null, got {v:?}");
}

#[test]
fn index_access_on_object_uses_string_key() {
  let ctx = context(&[(
    "github",
    object_from(&[("event", ExprValue::String("push".to_owned()))]),
  )]);
  assert_string(
    &evaluator::evaluate("github['event']", &ctx).unwrap(),
    "push",
  );
}

#[test]
fn contains_function_matches_substring_case_insensitive() {
  let ctx = context(&[(
    "github",
    object_from(&[("event", ExprValue::String("pull_request".to_owned()))]),
  )]);
  assert_bool(
    &evaluator::evaluate("contains(github.event, 'PULL')", &ctx).unwrap(),
    true,
  );
  assert_bool(
    &evaluator::evaluate("contains(github.event, 'push')", &ctx).unwrap(),
    false,
  );
}

#[test]
fn starts_with_and_ends_with_functions() {
  let ctx = context(&[(
    "github",
    object_from(&[("ref", ExprValue::String("refs/heads/main".to_owned()))]),
  )]);
  assert_bool(
    &evaluator::evaluate("startsWith(github.ref, 'refs/heads/')", &ctx).unwrap(),
    true,
  );
  assert_bool(
    &evaluator::evaluate("endsWith(github.ref, 'main')", &ctx).unwrap(),
    true,
  );
  assert_bool(
    &evaluator::evaluate("endsWith(github.ref, 'dev')", &ctx).unwrap(),
    false,
  );
}

#[test]
fn format_function_substitutes_indexed_placeholders() {
  let ctx = context(&[
    (
      "github",
      object_from(&[("repository", ExprValue::String("owner/repo".to_owned()))]),
    ),
    (
      "runner",
      object_from(&[("os", ExprValue::String("Linux".to_owned()))]),
    ),
  ]);
  let v =
    evaluator::evaluate("format('{0}/{1}', github.repository, runner.os)", &ctx).expect("format");
  assert_string(&v, "owner/repo/Linux");
}

#[test]
fn format_function_doubles_braces_for_literal_braces() {
  let ctx = context(&[]);
  let v = evaluator::evaluate("format('{{hello}} {0}', 'world')", &ctx).expect("format escaped");
  assert_string(&v, "{hello} world");
}

#[test]
fn join_function_with_array_uses_explicit_separator() {
  let ctx = context(&[(
    "labels",
    array_from(&[
      ExprValue::String("self-hosted".to_owned()),
      ExprValue::String("linux".to_owned()),
      ExprValue::String("x64".to_owned()),
    ]),
  )]);
  let v = evaluator::evaluate("join(labels, ',')", &ctx).expect("join");
  assert_string(&v, "self-hosted,linux,x64");
}

#[test]
fn success_failure_always_cancelled_depend_on_job_status() {
  let mut ctx = context(&[]);
  ctx.job_status = JobStatus::Success;
  assert_bool(&evaluator::evaluate("success()", &ctx).unwrap(), true);
  assert_bool(&evaluator::evaluate("failure()", &ctx).unwrap(), false);
  assert_bool(&evaluator::evaluate("always()", &ctx).unwrap(), true);
  assert_bool(&evaluator::evaluate("cancelled()", &ctx).unwrap(), false);

  ctx.job_status = JobStatus::Failure;
  assert_bool(&evaluator::evaluate("success()", &ctx).unwrap(), false);
  assert_bool(&evaluator::evaluate("failure()", &ctx).unwrap(), true);

  ctx.job_status = JobStatus::Cancelled;
  assert_bool(&evaluator::evaluate("cancelled()", &ctx).unwrap(), true);
}

#[test]
fn to_json_serializes_object_to_json_string() {
  let ctx = context(&[(
    "github",
    object_from(&[
      ("repository", ExprValue::String("owner/repo".to_owned())),
      ("run_id", ExprValue::String("12345".to_owned())),
    ]),
  )]);
  let v = evaluator::evaluate("toJson(github)", &ctx).expect("toJson");
  let is_string = matches!(v, ExprValue::String(_));
  assert!(is_string, "expected String, got {v:?}");
  // Safe to unwrap because we just asserted `is_string`.
  let s = if let ExprValue::String(s) = &v {
    s.clone()
  } else {
    String::new()
  };
  let parsed: Value = serde_json::from_str(&s).expect("valid JSON");
  let repo = parsed
    .get("repository")
    .and_then(Value::as_str)
    .unwrap_or("");
  let run_id = parsed.get("run_id").and_then(Value::as_str).unwrap_or("");
  let has_repo = !repo.is_empty();
  let has_run_id = !run_id.is_empty();
  assert!(has_repo, "repository missing in {parsed}");
  assert!(has_run_id, "run_id missing in {parsed}");
  assert_eq!(repo, "owner/repo");
  assert_eq!(run_id, "12345");
}

#[test]
fn from_json_parses_string_into_object() {
  let ctx = context(&[(
    "payload",
    ExprValue::String(r#"{"action":"opened","number":42}"#.to_owned()),
  )]);
  let v = evaluator::evaluate("fromJson(payload).number", &ctx).expect("fromJson");
  assert_number(&v, 42.0);
}

#[test]
fn binary_eq_is_loose_and_case_insensitive_for_strings() {
  let ctx = context(&[(
    "github",
    object_from(&[("event", ExprValue::String("PUSH".to_owned()))]),
  )]);
  assert_bool(
    &evaluator::evaluate("github.event == 'push'", &ctx).unwrap(),
    true,
  );
}

#[test]
fn binary_eq_returns_false_for_distinct_values() {
  let ctx = context(&[(
    "github",
    object_from(&[("event", ExprValue::String("push".to_owned()))]),
  )]);
  assert_bool(
    &evaluator::evaluate("github.event == 'pull_request'", &ctx).unwrap(),
    false,
  );
}

#[test]
fn binary_and_uses_short_circuit() {
  let ctx = context(&[(
    "github",
    object_from(&[("event", ExprValue::String("push".to_owned()))]),
  )]);
  assert_bool(
    &evaluator::evaluate("github.event == 'push' && github.event == 'pull'", &ctx).unwrap(),
    false,
  );
  assert_bool(
    &evaluator::evaluate("github.event == 'push' && github.event == 'push'", &ctx).unwrap(),
    true,
  );
}

#[test]
fn binary_or_returns_left_when_truthy() {
  let ctx = context(&[(
    "github",
    object_from(&[("event", ExprValue::String("push".to_owned()))]),
  )]);
  // Push is truthy (non-empty string) — OR short-circuits and returns the LHS value.
  let v = evaluator::evaluate("github.event || 'fallback'", &ctx).expect("or");
  assert_string(&v, "push");
}

#[test]
fn binary_comparison_operators_compare_numbers() {
  let ctx = context(&[("n", ExprValue::Number(5.0))]);
  assert_bool(&evaluator::evaluate("n < 10", &ctx).unwrap(), true);
  assert_bool(&evaluator::evaluate("n <= 5", &ctx).unwrap(), true);
  assert_bool(&evaluator::evaluate("n > 1", &ctx).unwrap(), true);
  assert_bool(&evaluator::evaluate("n >= 5", &ctx).unwrap(), true);
  assert_bool(&evaluator::evaluate("n == 5", &ctx).unwrap(), true);
  assert_bool(&evaluator::evaluate("n != 5", &ctx).unwrap(), false);
}

#[test]
fn unary_not_inverts_truthiness() {
  let ctx = context(&[("empty", ExprValue::String(String::new()))]);
  assert_bool(&evaluator::evaluate("!empty", &ctx).unwrap(), true);
  let ctx = context(&[("nonempty", ExprValue::String("x".to_owned()))]);
  assert_bool(&evaluator::evaluate("!nonempty", &ctx).unwrap(), false);
}

#[test]
fn wildcard_on_object_returns_values_array() {
  let ctx = context(&[(
    "github",
    object_from(&[
      ("a", ExprValue::String("alpha".to_owned())),
      ("b", ExprValue::String("beta".to_owned())),
    ]),
  )]);
  let v = evaluator::evaluate("github.*", &ctx).expect("wildcard");
  let is_array = matches!(v, ExprValue::Array(_));
  assert!(is_array, "expected Array, got {v:?}");
  if let ExprValue::Array(items) = v {
    let mut strings: Vec<String> = items
      .iter()
      .filter_map(|e| {
        if let ExprValue::String(s) = e {
          Some(s.clone())
        } else {
          None
        }
      })
      .collect();
    strings.sort();
    let matches = strings == vec!["alpha".to_owned(), "beta".to_owned()];
    assert!(matches, "got {strings:?}");
  }
}

#[test]
fn unknown_function_returns_expression_error() {
  let ctx = context(&[]);
  let err = evaluator::evaluate("bogus_function('x')", &ctx).expect_err("unknown fn");
  let msg = format!("{err}");
  let has_msg = msg.contains("unknown function");
  assert!(has_msg, "got: {msg}");
}

#[test]
fn malformed_expression_returns_expression_error() {
  let ctx = context(&[]);
  let err = evaluator::evaluate("github.(event", &ctx).expect_err("parse error");
  let msg = format!("{err}");
  let has_msg = !msg.is_empty();
  assert!(has_msg, "expected error message, got empty");
}

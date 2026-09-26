//! Pinned actions/runner expression contracts; see expression-reference provenance.

use std::collections::HashMap;

use crate::evaluator::{EvalContext, JobStatus, evaluate};
use crate::types::ExprValue;

fn context() -> EvalContext {
  EvalContext {
    contexts: HashMap::new(),
    job_status: JobStatus::Success,
    workspace: None,
  }
}

#[test]
fn signed_literals_and_lazy_case_follow_pinned_runner() -> Result<(), shared::RunnerError> {
  let ctx = context();
  assert_eq!(evaluate("-1", &ctx)?.coerce_to_string(), "-1");
  assert_eq!(evaluate("0xff", &ctx)?.coerce_to_string(), "255");
  assert_eq!(
    evaluate("case(false, fromJSON('bad'), true, 42, 0)", &ctx)?.coerce_to_string(),
    "42"
  );
  assert!(evaluate("case(1, 'yes', 'no')", &ctx).is_err());
  Ok(())
}

#[test]
fn captured_malformed_literals_report_reason_and_byte_offset() {
  // These malformed expressions also appear in the pinned SDK corpus. Unlike
  // the cross-language corpus check, this pins our Rust diagnostic contract.
  for (expression, reason) in [
    ("0Xff", "invalid numeric syntax"),
    ("0x100000000", "number too large to fit in target type"),
    ("1e", "invalid float literal"),
  ] {
    for prefix in ["", "  "] {
      let error = evaluate(&format!("{prefix}{expression}"), &context())
        .expect_err("captured malformed literal must fail");
      let expected = shared::RunnerError::Expression(format!(
        "invalid number at byte {}: {reason}",
        prefix.len()
      ));
      assert_eq!(error.to_string(), expected.to_string());
    }
  }
}

#[test]
fn distinct_json_objects_are_not_equal() -> Result<(), shared::RunnerError> {
  assert!(matches!(
    evaluate("fromJSON('{}') == fromJSON('{}')", &context())?,
    ExprValue::Bool(false)
  ));
  Ok(())
}

#[test]
fn official_runner_corpus_matches_types_values_and_failures()
-> Result<(), Box<dyn std::error::Error>> {
  let mut ctx = context();
  let github = evaluate(
    r#"fromJSON('{"repository":"Falconiere/toolu-ghrunner","array":[1,2],"object":{"z":1,"a":2}}')"#,
    &ctx,
  )?;
  ctx.contexts.insert("github".to_owned(), github);
  ctx
    .contexts
    .insert("vars".to_owned(), evaluate("fromJSON('{}')", &context())?);
  let corpus: serde_json::Value = serde_json::from_str(include_str!("expression_reference.json"))?;
  let rows = corpus.as_array().ok_or("reference array missing")?;
  assert_eq!(rows.len(), 1402, "the complete pinned corpus must run");
  let mut failures = Vec::new();
  for row in rows {
    let expression = row
      .get("expression")
      .ok_or("expression missing")?
      .as_str()
      .ok_or("expression missing")?;
    let kind = row
      .get("kind")
      .ok_or("kind missing")?
      .as_str()
      .ok_or("kind missing")?;
    let expected = row
      .get("value")
      .ok_or("value missing")?
      .as_str()
      .ok_or("value missing")?;
    match evaluate(expression, &ctx) {
      Err(_) if kind == "error" => {},
      Err(err) => failures.push(format!(
        "{expression}: expected {kind} {expected:?}, got {err}"
      )),
      Ok(value) => {
        let actual_kind = match value {
          ExprValue::Bool(_) => "boolean",
          ExprValue::Null
          | ExprValue::Number(_)
          | ExprValue::String(_)
          | ExprValue::Array(_)
          | ExprValue::Object(_) => value.type_name(),
        };
        let actual = if value.is_primitive() {
          value.coerce_to_string()
        } else {
          evaluate(&format!("toJSON({expression})"), &ctx)?.coerce_to_string()
        };
        if actual_kind != kind || actual != expected {
          failures.push(format!(
            "{expression}: expected {kind} {expected:?}, got {actual_kind} {actual:?}"
          ));
        }
      },
    }
  }
  assert!(
    failures.is_empty(),
    "{} mismatches:\n{}",
    failures.len(),
    failures.join("\n")
  );
  Ok(())
}

#[test]
fn escaped_format_braces_survive_template_delimiters() -> Result<(), shared::RunnerError> {
  assert_eq!(
    crate::template::interpolate("value=${{ format('{{{0}}}', 'a') }}", &context())?,
    "value={a}"
  );
  Ok(())
}

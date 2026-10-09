//! Step attribute evaluation against tokens captured verbatim from a live
//! github.com job message (`step-attrs-99.yml`, run 37992025403). Expected
//! diagnostics are the reference runner's log lines from the same run.

use std::time::Duration;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn token(json: &str) -> Result<TemplateToken, serde_json::Error> {
  serde_json::from_str(json)
}

fn file_table() -> Vec<serde_json::Value> {
  vec![serde_json::Value::from(
    ".github/workflows/step-attrs-99.yml",
  )]
}

fn ctx_with_prior(outputs: &[(&str, &str)]) -> ExecutionContext {
  let mut ctx = ExecutionContext::new_for_test();
  for (key, value) in outputs {
    ctx.set_step_output("prior", key, value);
  }
  ctx
}

#[test]
fn literal_timeout_and_continue_on_error() -> TestResult {
  let ctx = ExecutionContext::new_for_test();
  let timeout = token(r#"{"col":26,"file":1,"line":47,"num":5,"type":6}"#)?;
  let coe = token(r#"{"bool":true,"col":28,"file":1,"line":46,"type":5}"#)?;
  assert_eq!(
    evaluate_timeout(Some(&timeout), &ctx),
    Ok(Some(Duration::from_secs(300)))
  );
  assert_eq!(evaluate_continue_on_error(Some(&coe), &ctx), Ok(true));
  Ok(())
}

#[test]
fn absent_and_null_tokens_use_defaults() -> TestResult {
  let ctx = ExecutionContext::new_for_test();
  let null = token("null")?;
  assert_eq!(evaluate_timeout(None, &ctx), Ok(None));
  assert_eq!(evaluate_timeout(Some(&null), &ctx), Ok(None));
  assert_eq!(evaluate_continue_on_error(None, &ctx), Ok(false));
  assert_eq!(evaluate_continue_on_error(Some(&null), &ctx), Ok(false));
  Ok(())
}

#[test]
fn zero_negative_and_fractional_timeouts_truncate() -> TestResult {
  let ctx = ExecutionContext::new_for_test();
  let zero = token(r#"{"col":26,"file":1,"line":64,"num":0,"type":6}"#)?;
  let negative = token(r#"{"col":26,"expr":"fromJSON('-1')","file":1,"line":69,"type":3}"#)?;
  let fractional = token(r#"{"col":26,"file":1,"line":106,"num":1.9,"type":6}"#)?;
  assert_eq!(evaluate_timeout(Some(&zero), &ctx), Ok(None));
  assert_eq!(evaluate_timeout(Some(&negative), &ctx), Ok(None));
  assert_eq!(
    evaluate_timeout(Some(&fractional), &ctx),
    Ok(Some(Duration::from_secs(60)))
  );
  Ok(())
}

#[test]
fn deferred_expressions_read_step_outputs() -> TestResult {
  let ctx = ctx_with_prior(&[("minutes", "1"), ("flag", "true")]);
  let timeout = token(
    r#"{"col":26,"expr":"fromJSON(steps.prior.outputs.minutes)","file":1,"line":89,"type":3}"#,
  )?;
  let coe =
    token(r#"{"col":28,"expr":"fromJSON(steps.prior.outputs.flag)","file":1,"line":52,"type":3}"#)?;
  assert_eq!(
    evaluate_timeout(Some(&timeout), &ctx),
    Ok(Some(Duration::from_secs(60)))
  );
  assert_eq!(evaluate_continue_on_error(Some(&coe), &ctx), Ok(true));
  Ok(())
}

#[test]
fn string_timeout_matches_reference_diagnostic() -> TestResult {
  let ctx = ctx_with_prior(&[("minutes", "1")]);
  let timeout =
    token(r#"{"col":26,"expr":"steps.prior.outputs.minutes","file":1,"line":118,"type":3}"#)?;
  let err = evaluate_timeout(Some(&timeout), &ctx)
    .err()
    .ok_or("string timeout must fail")?;
  assert_eq!(
    err.render(&file_table()),
    "The template is not valid. .github/workflows/step-attrs-99.yml (Line: 118, Col: 26): Unexpected value '1'"
  );
  Ok(())
}

#[test]
fn string_continue_on_error_matches_reference_diagnostic() -> TestResult {
  let ctx = ctx_with_prior(&[("flag", "true")]);
  let coe = token(r#"{"col":28,"expr":"steps.prior.outputs.flag","file":1,"line":185,"type":3}"#)?;
  let err = evaluate_continue_on_error(Some(&coe), &ctx)
    .err()
    .ok_or("string flag must fail")?;
  assert_eq!(
    err.render(&file_table()),
    "The template is not valid. .github/workflows/step-attrs-99.yml (Line: 185, Col: 28): Unexpected value 'true'"
  );
  Ok(())
}

#[test]
fn structured_and_failing_expressions_are_template_errors() -> TestResult {
  let ctx = ExecutionContext::new_for_test();
  let seq = token(r#"{"col":26,"expr":"fromJSON('[1]')","file":1,"line":3,"type":3}"#)?;
  let map = token(r#"{"col":28,"expr":"fromJSON('{}')","file":1,"line":4,"type":3}"#)?;
  let bad = token(r#"{"col":28,"expr":"fromJSON('nope')","file":1,"line":5,"type":3}"#)?;
  let rendered = |e: StepAttrError| e.render(&file_table());
  assert_eq!(
    evaluate_timeout(Some(&seq), &ctx).map_err(rendered),
    Err("The template is not valid. .github/workflows/step-attrs-99.yml (Line: 3, Col: 26): A sequence was not expected".to_owned())
  );
  assert_eq!(
    evaluate_continue_on_error(Some(&map), &ctx).map_err(rendered),
    Err("The template is not valid. .github/workflows/step-attrs-99.yml (Line: 4, Col: 28): A mapping was not expected".to_owned())
  );
  let err = evaluate_continue_on_error(Some(&bad), &ctx)
    .err()
    .ok_or("bad JSON must fail")?;
  assert!(err.render(&file_table()).starts_with(
    "The template is not valid. .github/workflows/step-attrs-99.yml (Line: 5, Col: 28): "
  ));
  Ok(())
}

#[test]
fn number_continue_on_error_is_not_coerced() -> TestResult {
  let ctx = ExecutionContext::new_for_test();
  let coe = token("1")?;
  assert_eq!(
    evaluate_continue_on_error(Some(&coe), &ctx).map_err(|e| e.render(&[])),
    Err("The template is not valid. Unexpected value '1'".to_owned())
  );
  Ok(())
}

#[test]
fn out_of_range_timeouts_are_rejected() -> TestResult {
  let ctx = ExecutionContext::new_for_test();
  let max = token("71582.9")?;
  let over = token("71583")?;
  let infinite = TemplateToken::number(f64::INFINITY);
  assert_eq!(
    evaluate_timeout(Some(&max), &ctx),
    Ok(Some(Duration::from_secs(71_582 * 60)))
  );
  let err = evaluate_timeout(Some(&over), &ctx)
    .err()
    .ok_or("71583 must be rejected")?;
  assert_eq!(
    err.render(&[]),
    "The step timeout of 71583 minutes is out of range; the maximum is 71582 minutes."
  );
  assert!(matches!(
    evaluate_timeout(Some(&infinite), &ctx),
    Err(StepAttrError::TimeoutOutOfRange(m)) if m.is_infinite()
  ));
  Ok(())
}

#[test]
fn positions_render_without_a_resolvable_file() {
  let err = StepAttrError::Template {
    file: Some(2),
    line: Some(7),
    col: Some(9),
    message: "m".to_owned(),
  };
  assert_eq!(
    err.render(&file_table()),
    "The template is not valid. (Line: 7, Col: 9): m"
  );
}

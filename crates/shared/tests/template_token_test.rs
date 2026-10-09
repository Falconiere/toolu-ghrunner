//! `TemplateToken` wire forms: GitHub's typed objects and bare scalars.
//!
//! Typed-object fragments are copied verbatim from the job message acquired in
//! run 37992025403 (workflow `step-attrs-99.yml`); bare scalars are the forms
//! the official runner's `TemplateTokenJsonConverter` writes for tokens without
//! a source position.

use shared::{DictEntry, TemplateToken};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn parse(json: &str) -> Result<TemplateToken, serde_json::Error> {
  serde_json::from_str(json)
}

#[test]
fn captured_typed_boolean_keeps_type_and_value() -> TestResult {
  let token = parse(r#"{"type":5,"file":1,"line":46,"col":28,"bool":true}"#)?;
  assert_eq!(token.token_type, 5);
  assert_eq!(token.bool_val, Some(true));
  assert_eq!(
    (token.file, token.line, token.col),
    (Some(1), Some(46), Some(28))
  );
  Ok(())
}

#[test]
fn bare_scalars_carry_no_position_and_round_trip_without_one() -> TestResult {
  let token = parse("true")?;
  assert_eq!((token.file, token.line, token.col), (None, None, None));
  let json = serde_json::to_value(&token)?;
  assert!(json.get("file").is_none() && json.get("line").is_none());
  Ok(())
}

#[test]
fn captured_typed_numbers_keep_integer_zero_and_fraction() -> TestResult {
  let five = parse(r#"{"type":6,"file":1,"line":47,"col":26,"num":5}"#)?;
  let zero = parse(r#"{"type":6,"file":1,"line":64,"col":26,"num":0}"#)?;
  let fraction = parse(r#"{"type":6,"file":1,"line":106,"col":26,"num":1.9}"#)?;
  assert_eq!((five.token_type, five.num_val), (6, Some(5.0)));
  assert_eq!((zero.token_type, zero.num_val), (6, Some(0.0)));
  assert_eq!((fraction.token_type, fraction.num_val), (6, Some(1.9)));
  Ok(())
}

#[test]
fn captured_expressions_keep_unevaluated_text() -> TestResult {
  let deferred =
    parse(r#"{"type":3,"file":1,"line":52,"col":28,"expr":"fromJSON(steps.prior.outputs.flag)"}"#)?;
  let name = parse(
    r#"{"type":3,"file":1,"line":50,"col":15,"expr":"format('Deferred {0}', steps.prior.outputs.label)"}"#,
  )?;
  assert_eq!(
    deferred.to_expr_string(),
    Some("fromJSON(steps.prior.outputs.flag)")
  );
  assert_eq!(
    name.to_expr_string(),
    Some("format('Deferred {0}', steps.prior.outputs.label)")
  );
  Ok(())
}

#[test]
fn captured_typed_literal_is_a_string() -> TestResult {
  let token = parse(r#"{"type":0,"file":1,"line":44,"col":15,"lit":"Literal continue-on-error"}"#)?;
  assert_eq!(token.to_string_value(), Some("Literal continue-on-error"));
  Ok(())
}

#[test]
fn bare_scalars_become_typed_tokens() -> TestResult {
  let text = parse(r#""hello""#)?;
  let flag = parse("true")?;
  let integer = parse("3")?;
  let float = parse("2.5")?;
  let null = parse("null")?;
  assert_eq!(text.to_string_value(), Some("hello"));
  assert_eq!((flag.token_type, flag.bool_val), (5, Some(true)));
  assert_eq!((integer.token_type, integer.num_val), (6, Some(3.0)));
  assert_eq!((float.token_type, float.num_val), (6, Some(2.5)));
  assert_eq!(null.token_type, 7);
  Ok(())
}

#[test]
fn object_without_type_is_a_string_token() -> TestResult {
  let token = parse(r#"{"lit":"untyped"}"#)?;
  assert_eq!(token.to_string_value(), Some("untyped"));
  Ok(())
}

#[test]
fn mapping_accepts_bare_keys_and_values() -> TestResult {
  let token =
    parse(r#"{"type":2,"map":[{"Key":"script","Value":"echo hi"},{"Key":"n","Value":1}]}"#)?;
  let map = token.to_map();
  assert_eq!(
    map.get("script").and_then(TemplateToken::to_string_value),
    Some("echo hi")
  );
  assert_eq!(map.get("n").and_then(|value| value.num_val), Some(1.0));
  Ok(())
}

#[test]
fn sequence_accepts_mixed_forms() -> TestResult {
  let token = parse(r#"{"type":1,"seq":["a",{"type":5,"bool":false},null]}"#)?;
  let items = token.seq.unwrap_or_default();
  let kinds: Vec<i32> = items.iter().map(|item| item.token_type).collect();
  assert_eq!(kinds, vec![0, 5, 7]);
  Ok(())
}

#[test]
fn bare_array_is_rejected() {
  assert!(parse("[1]").is_err());
}

#[test]
fn constructors_match_wire_types() {
  let entry = DictEntry {
    key: TemplateToken::literal("k"),
    value: TemplateToken::expression("matrix.coe"),
  };
  assert_eq!(entry.key.to_string_value(), Some("k"));
  assert_eq!(entry.value.to_expr_string(), Some("matrix.coe"));
  assert_eq!(TemplateToken::boolean(false).bool_val, Some(false));
  assert_eq!(TemplateToken::number(1.0).num_val, Some(1.0));
  assert_eq!(TemplateToken::null().token_type, 7);
}

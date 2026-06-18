//! JSON conversion functions (toJSON, fromJSON) and value converters.

use shared::RunnerError;

use super::super::types::ExprValue;

pub(super) fn fn_to_json(args: &[ExprValue]) -> Result<ExprValue, RunnerError> {
  let val = arg1("toJSON", args)?;
  let json = expr_value_to_json(val);
  let s = serde_json::to_string(&json).map_err(RunnerError::Json)?;
  Ok(ExprValue::String(s))
}

pub(super) fn fn_from_json(args: &[ExprValue]) -> Result<ExprValue, RunnerError> {
  let val = arg1("fromJSON", args)?;
  let s = val.coerce_to_string();
  if s.is_empty() {
    return Err(RunnerError::Expression(
      "fromJSON: empty string is not valid JSON".to_owned(),
    ));
  }
  let json: serde_json::Value = serde_json::from_str(&s).map_err(RunnerError::Json)?;
  Ok(json_to_expr_value(&json))
}

fn arg1<'a>(name: &str, args: &'a [ExprValue]) -> Result<&'a ExprValue, RunnerError> {
  if args.len() != 1 {
    return Err(RunnerError::Expression(format!(
      "{name} expects 1 arg, got {}",
      args.len()
    )));
  }
  args
    .first()
    .ok_or_else(|| RunnerError::Expression(format!("{name} expects 1 arg, got 0")))
}

fn expr_value_to_json(val: &ExprValue) -> serde_json::Value {
  match val {
    ExprValue::Null => serde_json::Value::Null,
    ExprValue::Bool(b) => serde_json::Value::Bool(*b),
    ExprValue::Number(n) => serde_json::Number::from_f64(*n)
      .map(serde_json::Value::Number)
      .unwrap_or(serde_json::Value::Null),
    ExprValue::String(s) => serde_json::Value::String(s.clone()),
    ExprValue::Array(arr) => serde_json::Value::Array(arr.iter().map(expr_value_to_json).collect()),
    ExprValue::Object(map) => {
      let obj = map
        .iter()
        .map(|(k, v)| (k.clone(), expr_value_to_json(v)))
        .collect();
      serde_json::Value::Object(obj)
    },
  }
}

fn json_to_expr_value(val: &serde_json::Value) -> ExprValue {
  match val {
    serde_json::Value::Null => ExprValue::Null,
    serde_json::Value::Bool(b) => ExprValue::Bool(*b),
    serde_json::Value::Number(n) => ExprValue::Number(n.as_f64().unwrap_or(0.0)),
    serde_json::Value::String(s) => ExprValue::String(s.clone()),
    serde_json::Value::Array(arr) => ExprValue::Array(arr.iter().map(json_to_expr_value).collect()),
    serde_json::Value::Object(map) => {
      let obj = map
        .iter()
        .map(|(k, v)| (k.clone(), json_to_expr_value(v)))
        .collect();
      ExprValue::Object(obj)
    },
  }
}

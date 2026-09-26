//! JSON conversion functions (toJSON, fromJSON) and value converters.

use shared::RunnerError;

use super::super::types::ExprValue;

pub(super) fn fn_to_json(args: &[ExprValue]) -> Result<ExprValue, RunnerError> {
  let val = arg1("toJSON", args)?;
  let mut output = String::new();
  write_json(val, 0, &mut output)?;
  Ok(ExprValue::String(output))
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

fn write_json(value: &ExprValue, depth: usize, out: &mut String) -> Result<(), RunnerError> {
  match value {
    ExprValue::Null => out.push_str("null"),
    ExprValue::Bool(_) | ExprValue::Number(_) => out.push_str(&value.coerce_to_string()),
    ExprValue::String(s) => out.push_str(&serde_json::to_string(s)?),
    ExprValue::Array(array) => {
      out.push('[');
      for (index, item) in array.iter().enumerate() {
        newline(out, depth + 1, index > 0);
        write_json(item, depth + 1, out)?;
      }
      if !array.is_empty() {
        newline(out, depth, false);
      }
      out.push(']');
    },
    ExprValue::Object(object) => {
      out.push('{');
      for (index, (key, item)) in object.iter().enumerate() {
        newline(out, depth + 1, index > 0);
        out.push_str(&serde_json::to_string(key)?);
        out.push_str(": ");
        write_json(item, depth + 1, out)?;
      }
      if !object.is_empty() {
        newline(out, depth, false);
      }
      out.push('}');
    },
  }
  Ok(())
}

fn newline(out: &mut String, depth: usize, comma: bool) {
  if comma {
    out.push(',');
  }
  out.push('\n');
  out.push_str(&"  ".repeat(depth));
}

fn json_to_expr_value(val: &serde_json::Value) -> ExprValue {
  match val {
    serde_json::Value::Null => ExprValue::Null,
    serde_json::Value::Bool(b) => ExprValue::Bool(*b),
    serde_json::Value::Number(n) => ExprValue::Number(n.as_f64().unwrap_or(0.0)),
    serde_json::Value::String(s) => ExprValue::String(s.clone()),
    serde_json::Value::Array(arr) => ExprValue::array(arr.iter().map(json_to_expr_value)),
    serde_json::Value::Object(map) => {
      let obj: crate::object::ExprObject = map
        .iter()
        .map(|(k, v)| (k.clone(), json_to_expr_value(v)))
        .collect();
      ExprValue::Object(obj)
    },
  }
}

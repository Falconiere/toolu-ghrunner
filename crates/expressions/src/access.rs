//! Property indexing and one-level projection of wildcard results.

use crate::array::ExprArray;
use crate::types::{ExprValue, ordinal_compare};

/// Apply an index, or a wildcard when the index is absent.
pub(crate) fn access(value: &ExprValue, index: Option<&ExprValue>) -> ExprValue {
  if let ExprValue::Array(array) = value
    && array.filtered
  {
    let mut result = Vec::new();
    for item in array {
      if let Some(index) = index {
        if let Some(value) = lookup(item, index) {
          result.push(value);
        }
      } else {
        append_values(item, &mut result);
      }
    }
    return ExprValue::Array(ExprArray::filtered(result));
  }
  if let Some(index) = index {
    lookup(value, index).unwrap_or(ExprValue::Null)
  } else {
    let mut result = Vec::new();
    append_values(value, &mut result);
    ExprValue::Array(ExprArray::filtered(result))
  }
}

fn lookup(value: &ExprValue, index: &ExprValue) -> Option<ExprValue> {
  match value {
    ExprValue::Object(object) if index.is_primitive() => {
      let key = index.coerce_to_string();
      object
        .iter()
        .find(|(k, _)| ordinal_compare(k, &key).is_eq())
        .map(|(_, v)| v.clone())
    },
    ExprValue::Array(array) => {
      let number = index.coerce_to_number();
      if !number.is_finite() || number < 0.0 || number.floor() > f64::from(i32::MAX) {
        return None;
      }
      let index = format!("{:.0}", number.floor().abs())
        .parse::<usize>()
        .ok()?;
      array.get(index).cloned()
    },
    ExprValue::Null
    | ExprValue::Bool(_)
    | ExprValue::Number(_)
    | ExprValue::String(_)
    | ExprValue::Object(_) => None,
  }
}

fn append_values(value: &ExprValue, out: &mut Vec<ExprValue>) {
  match value {
    ExprValue::Array(array) => out.extend(array.iter().cloned()),
    ExprValue::Object(object) => out.extend(object.values().cloned()),
    ExprValue::Null | ExprValue::Bool(_) | ExprValue::Number(_) | ExprValue::String(_) => {},
  }
}

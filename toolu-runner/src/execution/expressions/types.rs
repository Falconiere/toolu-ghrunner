use std::collections::HashMap;
use std::fmt;

/// Runtime value in the GitHub Actions expression language.
///
/// Follows GitHub's type coercion rules exactly:
/// - Null → false/0/""
/// - Bool → 0|1/"true"|"false"
/// - String comparisons are case-insensitive
#[derive(Debug, Clone)]
pub enum ExprValue {
  Null,
  Bool(bool),
  Number(f64),
  String(String),
  Array(Vec<ExprValue>),
  Object(HashMap<String, ExprValue>),
}

impl ExprValue {
  /// GitHub Actions truthiness rules.
  pub fn is_truthy(&self) -> bool {
    match self {
      Self::Null => false,
      Self::Bool(b) => *b,
      Self::Number(n) => *n != 0.0,
      Self::String(s) => !s.is_empty(),
      Self::Array(_) | Self::Object(_) => true,
    }
  }

  /// Coerce to string following GitHub's rules.
  pub fn coerce_to_string(&self) -> String {
    match self {
      Self::Bool(b) => if *b { "true" } else { "false" }.to_owned(),
      Self::Number(n) => format_number(*n),
      Self::String(s) => s.clone(),
      Self::Null | Self::Array(_) | Self::Object(_) => String::new(),
    }
  }

  /// Coerce to number following GitHub's rules.
  pub fn coerce_to_number(&self) -> f64 {
    match self {
      Self::Null => 0.0,
      Self::Bool(b) => {
        if *b {
          1.0
        } else {
          0.0
        }
      },
      Self::Number(n) => *n,
      Self::String(s) => parse_number_str(s),
      Self::Array(_) | Self::Object(_) => f64::NAN,
    }
  }

  /// GitHub Actions loose equality (`==`).
  ///
  /// When types differ, both sides are coerced to a number (null -> 0,
  /// false -> 0, "" -> 0, true -> 1). Strings compared case-insensitively.
  pub fn loose_eq(&self, other: &Self) -> bool {
    match (self, other) {
      (Self::Null, Self::Null) => true,
      (Self::Bool(a), Self::Bool(b)) => a == b,
      (Self::Number(a), Self::Number(b)) => float_eq(*a, *b),
      (Self::String(a), Self::String(b)) => a.eq_ignore_ascii_case(b),
      // Mixed types: coerce both to number
      _ => {
        let a = self.coerce_to_number();
        let b = other.coerce_to_number();
        float_eq(a, b)
      },
    }
  }

  /// Convert to a `serde_json::Value` for serialization (e.g. event.json).
  pub fn to_json_value(&self) -> serde_json::Value {
    match self {
      Self::Null => serde_json::Value::Null,
      Self::Bool(b) => serde_json::Value::Bool(*b),
      Self::Number(n) => serde_json::json!(*n),
      Self::String(s) => serde_json::Value::String(s.clone()),
      Self::Array(arr) => serde_json::Value::Array(arr.iter().map(Self::to_json_value).collect()),
      Self::Object(map) => {
        let obj = map
          .iter()
          .map(|(k, v)| (k.clone(), v.to_json_value()))
          .collect();
        serde_json::Value::Object(obj)
      },
    }
  }

  /// Type name for error messages.
  pub fn type_name(&self) -> &'static str {
    match self {
      Self::Null => "null",
      Self::Bool(_) => "bool",
      Self::Number(_) => "number",
      Self::String(_) => "string",
      Self::Array(_) => "array",
      Self::Object(_) => "object",
    }
  }
}

impl fmt::Display for ExprValue {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}", self.coerce_to_string())
  }
}

fn float_eq(a: f64, b: f64) -> bool {
  if a.is_nan() && b.is_nan() {
    return true;
  }
  (a - b).abs() < f64::EPSILON
}

fn format_number(n: f64) -> String {
  let s = format!("{n}");
  s.strip_suffix(".0").map(ToOwned::to_owned).unwrap_or(s)
}

fn parse_number_str(s: &str) -> f64 {
  let trimmed = s.trim();
  if trimmed.is_empty() {
    return 0.0;
  }
  trimmed.parse::<f64>().unwrap_or(f64::NAN)
}

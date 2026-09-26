//! Values and coercions of the Actions expression language.

use crate::array::ExprArray;
use crate::number::{format_number, parse_number};
use crate::object::ExprObject;
use std::fmt;

/// Runtime value in the GitHub Actions expression language.
///
/// Follows GitHub's type coercion rules exactly:
/// - Null → false/0/""
/// - Bool → 0|1/"true"|"false"
/// - String comparisons are case-insensitive
#[derive(Debug, Clone)]
pub enum ExprValue {
  /// The `null` value.
  Null,
  /// A boolean value.
  Bool(bool),
  /// A numeric value.
  Number(f64),
  /// A string value.
  String(String),
  /// An ordered list of values.
  Array(ExprArray),
  /// A map from string keys to values.
  Object(ExprObject),
}

impl ExprValue {
  /// Collect an array with new identity; cloning it preserves that identity.
  pub fn array(values: impl IntoIterator<Item = Self>) -> Self {
    Self::Array(values.into_iter().collect())
  }

  /// Collect an object in iteration order with new identity.
  pub fn object(values: impl IntoIterator<Item = (String, Self)>) -> Self {
    Self::Object(values.into_iter().collect())
  }

  /// Whether this value can be converted to an object index.
  pub(crate) fn is_primitive(&self) -> bool {
    !matches!(self, Self::Array(_) | Self::Object(_))
  }

  /// GitHub Actions truthiness rules.
  pub fn is_truthy(&self) -> bool {
    match self {
      Self::Null => false,
      Self::Bool(b) => *b,
      Self::Number(n) => *n != 0.0 && !n.is_nan(),
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
      Self::Null => String::new(),
      Self::Array(_) => "Array".to_owned(),
      Self::Object(_) => "Object".to_owned(),
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
      Self::String(s) => parse_number(s),
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
      (Self::String(a), Self::String(b)) => ordinal_compare(a, b).is_eq(),
      (Self::Array(a), Self::Array(b)) => a.same_reference(b),
      (Self::Object(a), Self::Object(b)) => a.same_reference(b),
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
  a.partial_cmp(&b) == Some(std::cmp::Ordering::Equal)
}

/// Compare strings using invariant ordinal-ignore-case UTF-16 ordering.
pub(crate) fn ordinal_compare(a: &str, b: &str) -> std::cmp::Ordering {
  ordinal_upper(a).cmp(&ordinal_upper(b))
}

/// Invariant simple uppercase units used by ordinal-ignore-case operations.
pub(crate) fn ordinal_upper(s: &str) -> Vec<u16> {
  s.chars()
    .map(|ch| {
      // .NET ordinal casing excludes the two non-ASCII-to-ASCII mappings.
      if matches!(ch, '\u{0131}' | '\u{017f}') {
        return ch;
      }
      let mut chars = ch.to_uppercase();
      let first = chars.next().unwrap_or(ch);
      if chars.next().is_some() { ch } else { first }
    })
    .collect::<String>()
    .encode_utf16()
    .collect()
}

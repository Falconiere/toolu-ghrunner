//! Template token types from the job message protocol.

use std::collections::HashMap;

use serde::Serialize;

use super::context_data::DictEntry;

/// A template token from the job message.
///
/// Type discriminator:
/// - 0 = literal string (field `lit`)
/// - 1 = sequence (field `seq`)
/// - 2 = mapping (field `map`)
/// - 3 = expression (field `expr`)
/// - 5 = boolean (field `bool`)
/// - 6 = number (field `num`)
/// - 7 = null
///
/// The `Deserialize` impl (see [`super::template_token_de`]) also accepts the
/// bare string/bool/number/null wire forms.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TemplateToken {
  /// The `type` discriminator (0=literal, 1=sequence, 2=mapping, 3=expression,
  /// 5=boolean, 6=number, 7=null).
  #[serde(rename = "type", default)]
  pub token_type: i32,
  /// The literal string value, present when `token_type` is 0.
  #[serde(default)]
  pub lit: Option<String>,
  /// The unevaluated `${{ }}` expression text, present when `token_type` is 3.
  #[serde(default)]
  pub expr: Option<String>,
  /// Wire field `bool`; the boolean value, present when `token_type` is 5.
  #[serde(default, rename = "bool")]
  pub bool_val: Option<bool>,
  /// Wire field `num`; the numeric value, present when `token_type` is 6.
  #[serde(default, rename = "num")]
  pub num_val: Option<f64>,
  /// The mapping entries (wire field `map`), present when `token_type` is 2.
  #[serde(default, alias = "map")]
  pub d: Option<Vec<DictEntry<TemplateToken>>>,
  /// The sequence elements, present when `token_type` is 1.
  #[serde(default)]
  pub seq: Option<Vec<TemplateToken>>,
  /// 1-based index into the job message's `fileTable`, when positioned.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub file: Option<i32>,
  /// Source line of the token, when positioned.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub line: Option<i32>,
  /// Source column of the token, when positioned.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub col: Option<i32>,
}

impl TemplateToken {
  /// Build a literal string token (type 0).
  pub fn literal(value: &str) -> Self {
    Self {
      token_type: 0,
      lit: Some(value.to_owned()),
      ..Self::default()
    }
  }

  /// Build an expression token (type 3) from its unwrapped expression text.
  pub fn expression(expr: &str) -> Self {
    Self {
      token_type: 3,
      expr: Some(expr.to_owned()),
      ..Self::default()
    }
  }

  /// Build a boolean token (type 5).
  pub fn boolean(value: bool) -> Self {
    Self {
      token_type: 5,
      bool_val: Some(value),
      ..Self::default()
    }
  }

  /// Build a number token (type 6).
  pub fn number(value: f64) -> Self {
    Self {
      token_type: 6,
      num_val: Some(value),
      ..Self::default()
    }
  }

  /// Build a null token (type 7).
  pub fn null() -> Self {
    Self {
      token_type: 7,
      ..Self::default()
    }
  }

  /// Extract the literal string value (for type 0 tokens).
  pub fn to_string_value(&self) -> Option<&str> {
    if self.token_type == 0 {
      self.lit.as_deref()
    } else {
      None
    }
  }

  /// Extract the expression string (for type 3 tokens).
  pub fn to_expr_string(&self) -> Option<&str> {
    if self.token_type == 3 {
      self.expr.as_deref()
    } else {
      None
    }
  }

  /// Convert a mapping token (type 2) into a `HashMap`.
  pub fn to_map(&self) -> HashMap<String, TemplateToken> {
    let mut result = HashMap::new();
    if let Some(entries) = &self.d {
      for entry in entries {
        if let Some(key) = entry.key.to_string_value() {
          result.insert(key.to_owned(), entry.value.clone());
        }
      }
    }
    result
  }
}

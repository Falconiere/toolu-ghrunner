//! Template token types from the job message protocol.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TemplateToken {
  #[serde(rename = "type", default)]
  pub token_type: i32,
  #[serde(default)]
  pub lit: Option<String>,
  #[serde(default)]
  pub expr: Option<String>,
  #[serde(default, rename = "bool")]
  pub bool_val: Option<bool>,
  #[serde(default, rename = "num")]
  pub num_val: Option<f64>,
  #[serde(default, alias = "map")]
  pub d: Option<Vec<DictEntry<TemplateToken>>>,
  #[serde(default)]
  pub seq: Option<Vec<TemplateToken>>,
}

impl TemplateToken {
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

  /// Convert a mapping token (type 2) into a HashMap.
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

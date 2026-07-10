//! Pipeline context data and dictionary entry types.

use serde::{Deserialize, Serialize};

/// Context data from the pipeline (github, env, etc.).
///
/// Uses a `type` integer discriminator:
/// - 0 = string (field `s`)
/// - 1 = array (field `a`)
/// - 2 = dictionary (field `d`)
/// - 3 = boolean (field `b`)
/// - 4 = number (field `n`)
/// - 5 = null
///
/// GitHub may also serialize dict keys as plain strings instead of the full
/// struct. The custom `Deserialize` impl (see [`super::context_data_de`]) handles
/// both forms.
#[derive(Debug, Clone, Serialize)]
pub struct PipelineContextData {
  #[serde(rename = "type", default)]
  pub data_type: i32,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub s: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub b: Option<bool>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub n: Option<f64>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub a: Option<Vec<PipelineContextData>>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub d: Option<Vec<DictEntry<PipelineContextData>>>,
}

/// A key-value pair in a dictionary context data.
/// GitHub uses `k`/`v` for context data and `Key`/`Value` for template tokens.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DictEntry<T> {
  #[serde(alias = "k", alias = "Key")]
  pub key: T,
  #[serde(alias = "v", alias = "Value")]
  pub value: T,
}

impl PipelineContextData {
  pub fn string(s: String) -> Self {
    Self {
      data_type: 0,
      s: Some(s),
      b: None,
      n: None,
      a: None,
      d: None,
    }
  }

  pub fn bool(v: bool) -> Self {
    Self {
      data_type: 3,
      s: None,
      b: Some(v),
      n: None,
      a: None,
      d: None,
    }
  }

  pub fn number(v: f64) -> Self {
    Self {
      data_type: 4,
      s: None,
      b: None,
      n: Some(v),
      a: None,
      d: None,
    }
  }

  pub fn null() -> Self {
    Self {
      data_type: 5,
      s: None,
      b: None,
      n: None,
      a: None,
      d: None,
    }
  }
}

impl Default for DictEntry<PipelineContextData> {
  fn default() -> Self {
    Self {
      key: PipelineContextData::null(),
      value: PipelineContextData::null(),
    }
  }
}

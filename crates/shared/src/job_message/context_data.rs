//! Pipeline context data and dictionary entry types.

use serde::{Deserialize, Serialize};

/// Context data from the pipeline (github, env, etc.), tagged by `data_type`
/// (0=string, 1=array, 2=dictionary, 3=boolean, 4=number, 5=null). The custom
/// `Deserialize` impl (see [`super::context_data_de`]) also accepts plain
/// string dict keys.
#[derive(Debug, Clone, Serialize)]
pub struct PipelineContextData {
  /// The `type` discriminator (0=string, 1=array, 2=dictionary, 3=boolean,
  /// 4=number, 5=null).
  #[serde(rename = "type", default)]
  pub data_type: i32,
  /// The string value, present when `data_type` is 0.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub s: Option<String>,
  /// The boolean value, present when `data_type` is 3.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub b: Option<bool>,
  /// The numeric value, present when `data_type` is 4.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub n: Option<f64>,
  /// The array elements, present when `data_type` is 1.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub a: Option<Vec<PipelineContextData>>,
  /// The dictionary entries, present when `data_type` is 2.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub d: Option<Vec<DictEntry<PipelineContextData>>>,
}

/// A key-value pair in a dictionary context data.
/// GitHub uses `k`/`v` for context data and `Key`/`Value` for template tokens.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DictEntry<T> {
  /// The entry's key (wire field `k` or `Key`).
  #[serde(alias = "k", alias = "Key")]
  pub key: T,
  /// The entry's value (wire field `v` or `Value`).
  #[serde(alias = "v", alias = "Value")]
  pub value: T,
}

impl PipelineContextData {
  /// Build a string-typed context data value.
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

  /// Build a boolean-typed context data value.
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

  /// Build a number-typed context data value.
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

  /// Build a null-typed context data value.
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

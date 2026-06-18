//! Convert `PipelineContextData` (from the job message) into `ExprValue` for
//! the expression evaluator.

use shared::DictEntry;
use shared::PipelineContextData;

use super::types::ExprValue;

/// Convert a `PipelineContextData` node into the expression evaluator's
/// generic value type. Prioritizes actual data over the `data_type` tag for
/// robustness — GitHub sometimes sends mismatched tags (e.g. `event` tagged
/// as 0/string but containing dict entries in `d`).
pub fn pipeline_data_to_expr_value(data: &PipelineContextData) -> ExprValue {
  if let Some(s) = &data.s {
    return ExprValue::String(s.clone());
  }
  if let Some(entries) = &data.d {
    let mut map = std::collections::HashMap::new();
    for entry in entries {
      let key = pipeline_data_to_expr_value(&entry.key).coerce_to_string();
      let value = pipeline_data_to_expr_value(&entry.value);
      map.insert(key, value);
    }
    return ExprValue::Object(map);
  }
  if let Some(arr) = &data.a {
    return ExprValue::Array(arr.iter().map(pipeline_data_to_expr_value).collect());
  }
  if let Some(b) = data.b {
    return ExprValue::Bool(b);
  }
  if let Some(n) = data.n {
    return ExprValue::Number(n);
  }
  ExprValue::Null
}

/// Helper for `DictEntry<PipelineContextData>` callers — exposes the value side.
pub fn dict_entry_value(entry: &DictEntry<PipelineContextData>) -> ExprValue {
  pipeline_data_to_expr_value(&entry.value)
}

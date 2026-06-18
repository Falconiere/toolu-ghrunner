use std::collections::HashMap;

/// Identifies the scope for output isolation in composite actions.
///
/// Each composite action gets its own scope so internal step outputs
/// are isolated from the parent context.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ScopeName(String);

impl ScopeName {
  /// Create a new scope from a step ID.
  pub fn new(step_id: &str) -> Self {
    Self(step_id.to_owned())
  }

  /// The scope identifier string.
  pub fn as_str(&self) -> &str {
    &self.0
  }
}

/// Outputs from a composite action that are mapped to the parent context.
#[derive(Debug, Clone, Default)]
pub struct CompositeOutputs {
  /// Map from output name to expression string (evaluated at completion).
  expressions: HashMap<String, String>,
}

impl CompositeOutputs {
  /// Create from the `outputs:` section of an action manifest.
  pub fn from_manifest(
    outputs: &HashMap<String, super::actions::manifest::ActionOutput>,
  ) -> Self {
    let expressions = outputs
      .iter()
      .filter_map(|(name, output)| {
        output
          .value
          .as_ref()
          .map(|expr| (name.clone(), expr.clone()))
      })
      .collect();
    Self { expressions }
  }

  /// Get all output expressions to evaluate.
  pub fn expressions(&self) -> &HashMap<String, String> {
    &self.expressions
  }
}

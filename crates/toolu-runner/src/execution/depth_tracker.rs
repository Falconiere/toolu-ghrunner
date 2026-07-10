use shared::RunnerError;

/// Maximum composite action nesting depth (matches GitHub's limit).
pub const MAX_COMPOSITE_DEPTH: u32 = 10;

/// Tracks composite action nesting depth to prevent infinite recursion.
#[derive(Debug, Clone)]
pub struct DepthTracker {
  current: u32,
  max: u32,
}

impl DepthTracker {
  /// Create a new tracker with the default max depth.
  pub fn new() -> Self {
    Self {
      current: 0,
      max: MAX_COMPOSITE_DEPTH,
    }
  }

  /// Current nesting depth.
  pub fn current(&self) -> u32 {
    self.current
  }

  /// Enter a new composite level. Returns error if max depth exceeded.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::StepExecution` if depth limit exceeded.
  pub fn enter(&mut self) -> Result<(), RunnerError> {
    self.current += 1;
    if self.current > self.max {
      return Err(RunnerError::StepExecution(format!(
        "composite action depth limit exceeded (max {})",
        self.max
      )));
    }
    Ok(())
  }

  /// Exit the current composite level.
  pub fn exit(&mut self) {
    self.current = self.current.saturating_sub(1);
  }
}

impl Default for DepthTracker {
  fn default() -> Self {
    Self::new()
  }
}

//! Environment-derived configuration for the execution engine.

/// Read the process `PATH`. `None` when unset or not valid unicode.
pub fn path() -> Option<String> {
  std::env::var("PATH").ok()
}

/// Generic passthrough read of an arbitrary environment variable `key`.
/// `None` when unset or not valid unicode.
pub fn var(key: &str) -> Option<String> {
  std::env::var(key).ok()
}

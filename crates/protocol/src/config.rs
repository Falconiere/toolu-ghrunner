//! Environment-derived configuration for the protocol layer.

/// Read the local hostname: use valid-Unicode `HOSTNAME`, then fall back to
/// valid-Unicode `COMPUTERNAME`. `None` when neither variable is usable.
pub fn hostname() -> Option<String> {
  std::env::var("HOSTNAME")
    .or_else(|_| std::env::var("COMPUTERNAME"))
    .ok()
}

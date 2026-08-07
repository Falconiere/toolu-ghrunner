//! Environment-derived configuration for the protocol layer.

/// Read the local hostname: tries `HOSTNAME`, then `COMPUTERNAME`. `None`
/// when neither is set.
pub fn hostname() -> Option<String> {
  std::env::var("HOSTNAME")
    .or_else(|_| std::env::var("COMPUTERNAME"))
    .ok()
}

//! Environment-derived configuration for the CLI bin.

/// Read `TOOLU_JITCONFIG`; invalid Unicode is intentionally treated as unset.
pub(crate) fn jit_config_raw() -> Option<String> {
  std::env::var("TOOLU_JITCONFIG").ok()
}

/// Read `TOOLU_DEADLINE`; invalid Unicode is intentionally treated as unset.
pub(crate) fn deadline_raw() -> Option<String> {
  std::env::var("TOOLU_DEADLINE").ok()
}

/// Read `TOOLU_RUNNER_TOKEN`; invalid Unicode is intentionally treated as unset.
pub(crate) fn runner_token() -> Option<String> {
  std::env::var("TOOLU_RUNNER_TOKEN").ok()
}

/// Read `TOOLU_RUNNER_CLIENT_ID`; invalid Unicode is intentionally treated as unset.
pub(crate) fn runner_client_id() -> Option<String> {
  std::env::var("TOOLU_RUNNER_CLIENT_ID").ok()
}

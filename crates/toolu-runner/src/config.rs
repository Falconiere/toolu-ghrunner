//! Environment-derived configuration for the CLI bin.

/// Read `TOOLU_JITCONFIG`; `None` when unset or not valid unicode.
pub fn jit_config_raw() -> Option<String> {
  std::env::var("TOOLU_JITCONFIG").ok()
}

/// Read `TOOLU_DEADLINE`; `None` when unset or not valid unicode.
pub fn deadline_raw() -> Option<String> {
  std::env::var("TOOLU_DEADLINE").ok()
}

/// Read `TOOLU_RUNNER_TOKEN`; `None` when unset or not valid unicode.
pub fn runner_token() -> Option<String> {
  std::env::var("TOOLU_RUNNER_TOKEN").ok()
}

/// Read `TOOLU_RUNNER_CLIENT_ID`; `None` when unset or not valid unicode.
pub fn runner_client_id() -> Option<String> {
  std::env::var("TOOLU_RUNNER_CLIENT_ID").ok()
}

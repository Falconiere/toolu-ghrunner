//! Environment-derived configuration for the CLI bin.

/// Read `TOOLU_JITCONFIG`; invalid Unicode is diagnosed and treated as unset.
pub(crate) fn jit_config_raw() -> Option<String> {
  read("TOOLU_JITCONFIG")
}

/// Read `TOOLU_DEADLINE`; invalid Unicode is diagnosed and treated as unset.
pub(crate) fn deadline_raw() -> Option<String> {
  read("TOOLU_DEADLINE")
}

/// Read `TOOLU_RUNNER_CLIENT_ID`; invalid Unicode is diagnosed and treated as unset.
pub(crate) fn runner_client_id() -> Option<String> {
  read("TOOLU_RUNNER_CLIENT_ID")
}

fn read(key: &str) -> Option<String> {
  match std::env::var(key) {
    Ok(value) => Some(value),
    Err(std::env::VarError::NotPresent) => None,
    Err(std::env::VarError::NotUnicode(_)) => {
      report_invalid_unicode(key);
      None
    },
  }
}

fn report_invalid_unicode(key: &str) {
  if tracing::dispatcher::has_been_set() {
    tracing::warn!(
      variable = key,
      "environment variable is not valid Unicode; treating as unset"
    );
  } else {
    eprintln!("toolu-runner: {key} is not valid Unicode; treating it as unset");
  }
}

#[cfg(test)]
#[path = "tests/config.rs"]
mod tests;

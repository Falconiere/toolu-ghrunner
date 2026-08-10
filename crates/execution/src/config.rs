//! Environment-derived configuration for the execution engine.

/// Read the process `PATH`. `None` when unset or not valid Unicode.
pub fn path() -> Option<String> {
  read("PATH")
}

/// Read `DOCKER_HOST` — the endpoint the Docker CLI and every Docker-compatible
/// daemon (Colima, `OrbStack`, Podman, Rancher Desktop) advertise themselves on.
/// `None` when unset or not valid Unicode.
pub fn docker_host() -> Option<String> {
  read("DOCKER_HOST")
}

/// Generic passthrough read of an arbitrary environment variable `key`.
/// `None` when unset or not valid Unicode.
pub fn var(key: &str) -> Option<String> {
  read(key)
}

fn read(key: &str) -> Option<String> {
  match std::env::var(key) {
    Ok(value) => Some(value),
    Err(std::env::VarError::NotPresent) => None,
    Err(std::env::VarError::NotUnicode(_)) => {
      tracing::warn!(
        variable = key,
        "environment variable is not valid Unicode; treating as unset"
      );
      None
    },
  }
}

#[cfg(test)]
#[path = "tests/config.rs"]
mod tests;

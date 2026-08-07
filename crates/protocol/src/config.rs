//! Environment-derived configuration for the protocol layer.

/// Read the local hostname: use valid-Unicode `HOSTNAME`, then fall back to
/// valid-Unicode `COMPUTERNAME`. `None` when neither variable is usable.
pub fn hostname() -> Option<String> {
  read("HOSTNAME").or_else(|| read("COMPUTERNAME"))
}

fn read(key: &str) -> Option<String> {
  match std::env::var(key) {
    Ok(value) => Some(value),
    Err(std::env::VarError::NotPresent) => None,
    Err(std::env::VarError::NotUnicode(_)) => {
      eprintln!("toolu-runner: {key} is not valid Unicode; treating it as unset");
      None
    },
  }
}

#[cfg(test)]
#[path = "tests/config.rs"]
mod tests;

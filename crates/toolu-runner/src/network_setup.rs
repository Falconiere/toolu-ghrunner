//! Per-lifecycle network setup shared by `run` and `boot` (#94): register the
//! proxy credentials with the masker, log the redacted settings, and build
//! the lifecycle's HTTP client. The client is rebuilt every pass, but the
//! secrets and the log line are only repeated when they changed.

use std::sync::Mutex;
use std::time::Duration;

use shared::{NetworkConfig, RunnerError, SecretMasker};

/// What the previous pass registered and logged, so a backoff-retry loop
/// does not re-add the same secrets (each add rebuilds the masker's
/// automaton) or repeat the same INFO line every second.
#[derive(Debug, Default)]
pub(crate) struct NetworkMemo {
  /// Secrets last registered with the masker.
  secrets: Option<Vec<String>>,
  /// The `network: …` line last logged.
  line: Option<String>,
}

impl NetworkMemo {
  /// Record `secrets`; true when they differ from the previous pass.
  fn secrets_changed(&mut self, secrets: &[String]) -> bool {
    if self.secrets.as_deref() == Some(secrets) {
      return false;
    }
    self.secrets = Some(secrets.to_vec());
    true
  }

  /// Record `line`; true when it differs from the previous pass.
  fn line_changed(&mut self, line: &str) -> bool {
    if self.line.as_deref() == Some(line) {
      return false;
    }
    self.line = Some(line.to_owned());
    true
  }
}

/// The client for one listener lifecycle pass, built from `network`: the
/// listener, the JIT re-mint and an ephemeral registration's unregister all
/// use it (`run`), as does `boot`'s single lifecycle.
///
/// `masker` is the process-wide masker, shared across passes; `memo` is the
/// caller's per-loop state and is updated in place.
///
/// Registers [`network::proxy_secrets`] with `masker` first (it is also the
/// tracing redactor, so no later line carries them), then logs
/// [`network::describe`] at INFO — upstream's "running behind proxy" line —
/// each only when it differs from what `memo` saw on the previous pass.
///
/// # Errors
///
/// `RunnerError::Config` for an unusable `[network]` value (fail closed —
/// `run`/`boot` exit before polling), or the TLS backend's build error.
pub(crate) fn lifecycle_client(
  network: &NetworkConfig,
  masker: &Mutex<SecretMasker>,
  timeout: Duration,
  memo: &mut NetworkMemo,
) -> Result<reqwest::Client, RunnerError> {
  let secrets = network::proxy_secrets(network);
  if !secrets.is_empty() && memo.secrets_changed(&secrets) {
    match masker.lock() {
      Ok(mut guard) => guard.add_secrets(&secrets),
      Err(poisoned) => poisoned.into_inner().add_secrets(&secrets),
    }
  }
  let line = network::describe(network)?;
  if memo.line_changed(&line) {
    tracing::info!("network: {line}");
  }
  network::client_builder(network)?
    .timeout(timeout)
    .build()
    .map_err(|e| RunnerError::Network(format!("HTTP client: {e}")))
}

#[cfg(test)]
#[path = "tests/network_setup.rs"]
mod tests;

//! Per-lifecycle network setup shared by `run` and `boot` (#94): register the
//! proxy credentials with the masker, log the redacted settings once, and
//! build the lifecycle's HTTP client.

use std::sync::Mutex;
use std::time::Duration;

use shared::{NetworkConfig, RunnerError, SecretMasker};

/// The listener / re-mint client for one lifecycle, built from `network`.
///
/// Registers [`network::proxy_secrets`] with `masker` first (it is also the
/// tracing redactor, so no later line carries them), then logs
/// [`network::describe`] at INFO — upstream's "running behind proxy" line.
///
/// # Errors
///
/// `RunnerError::Config` for an unusable `[network]` value (fail closed —
/// `run`/`boot` exit before polling), or the TLS backend's build error.
pub(crate) fn lifecycle_client(
  network: &NetworkConfig,
  masker: &Mutex<SecretMasker>,
  timeout: Duration,
) -> Result<reqwest::Client, RunnerError> {
  let secrets = network::proxy_secrets(network);
  if !secrets.is_empty() {
    match masker.lock() {
      Ok(mut guard) => guard.add_secrets(&secrets),
      Err(poisoned) => poisoned.into_inner().add_secrets(&secrets),
    }
  }
  tracing::info!("network: {}", network::describe(network)?);
  network::client_builder(network)?
    .timeout(timeout)
    .build()
    .map_err(|e| RunnerError::Network(format!("HTTP client: {e}")))
}

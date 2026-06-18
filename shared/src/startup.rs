//! Tracing initialization for toolu-runner.
//!
//! Replaces `yamless-shared::startup::init` with a slim version: no OTel,
//! no dotenvy walk, just `tracing-subscriber` + `EnvFilter`.
//!
//! In step 4e, this is extended to accept a `SecretMasker` and route all log
//! output through it before any sink.

use std::path::{Path, PathBuf};

use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Default data directory under the user's home.
fn default_data_dir() -> PathBuf {
  if let Some(home) = std::env::var_os("HOME") {
    return PathBuf::from(home).join(".toolu-runner");
  }
  if let Some(profile) = std::env::var_os("USERPROFILE") {
    return PathBuf::from(profile).join(".toolu-runner");
  }
  PathBuf::from("/var/lib/toolu-runner")
}

/// Diagnostics directory: `data_dir/_diag/`.
fn diag_dir(data_dir: &Path) -> PathBuf {
  data_dir.join("_diag")
}

/// Initialize tracing for the runner.
///
/// Loads `.env` from `manifest_dir` (no-op if the file is absent), resolves
/// the data dir, and attaches:
/// - a `tracing_subscriber::fmt` layer writing pretty lines to stderr,
/// - a `tracing_subscriber::fmt` JSON layer writing to
///   `data_dir/_diag/<service>.log` (created if absent).
///
/// `EnvFilter` is built from `RUST_LOG` first, then `TOOLU_RUNNER_LOG`,
/// falling back to `info`.
///
/// Uses `try_init` so duplicate calls (e.g. in tests) are silently ignored.
///
/// # Errors
///
/// Returns `RunnerError::Io` if the diagnostics directory cannot be created.
pub fn init(manifest_dir: &str, service: &str) -> Result<(), crate::RunnerError> {
  load_dotenv(Path::new(manifest_dir));

  let data_dir = default_data_dir();
  let diag = diag_dir(&data_dir);
  std::fs::create_dir_all(&diag).map_err(|source| crate::RunnerError::WorkspaceInit {
    path: diag.clone(),
    source,
  })?;

  let file_appender = tracing_appender::rolling::daily(&diag, format!("{service}.log"));

  let env_filter = EnvFilter::try_from_env("TOOLU_RUNNER_LOG")
    .or_else(|_| EnvFilter::try_from_env("RUST_LOG"))
    .unwrap_or_else(|_| EnvFilter::new("info"));

  let stderr_layer = tracing_subscriber::fmt::layer()
    .with_writer(std::io::stderr)
    .with_target(true)
    .with_thread_names(false)
    .compact();

  let file_layer = tracing_subscriber::fmt::layer()
    .with_writer(file_appender)
    .with_target(true)
    .with_thread_names(true)
    .json();

  tracing_subscriber::registry()
    .with(env_filter)
    .with(stderr_layer)
    .with(file_layer)
    .try_init()
    .ok();

  Ok(())
}

/// Load a `.env` file from `manifest_dir` (no-op if absent).
fn load_dotenv(manifest_dir: &Path) {
  let path = manifest_dir.join(".env");
  if !path.exists() {
    return;
  }
  match dotenvy::from_path(&path) {
    Ok(()) => tracing::info!(".env loaded from {}", path.display()),
    Err(e) => eprintln!("warning: could not load {}: {e}", path.display()),
  }
}

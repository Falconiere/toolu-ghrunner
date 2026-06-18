//! Persisted registration + runtime configuration.
//!
//! The runner's `register` subcommand writes a [`RunnerRegistrationConfig`]
//! to `~/.toolu-runner/config.toml` (mode 0600 on Unix) and a matching
//! [`CredentialsFile`] to `~/.toolu-runner/credentials.json` (mode 0600).
//! The `run` / `status` / `remove` subcommands read these back.
//!
//! ## Layout
//!
//! ```toml
//! runner_url   = "https://github.com/owner/repo"
//! runner_name  = "my-runner"
//! runner_id    = 12345
//! auth_token   = "ghs_..."
//! labels       = ["self-hosted", "linux", "x64"]
//! runner_group = "Default"
//!
//! [runtime]
//! jit_config        = "<base64 blob>"
//! work_dir          = "~/.toolu-runner/_work"
//! data_dir          = "~/.toolu-runner"
//! protocol_version  = "v2"   # or "v1" for GHES
//! ```
//!
//! Files are written with explicit 0600 mode on Unix via
//! `OpenOptions::mode(0o600)` so the default 0644 never applies.
//!
//! ## Tilde expansion
//!
//! `work_dir` and `data_dir` are stored with the `~` they came in with
//! (so a moved home dir keeps working). [`shared::paths::expand_tilde`]
//! resolves them at load time.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use shared::{RunnerError, paths};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

#[cfg(unix)]
const SECRET_FILE_MODE: u32 = 0o600;

/// Top-level registration data persisted in `config.toml`.
///
/// Loaded by `run`, `status`, and `remove`. Written by `register` after
/// the JIT endpoint is validated and the OAuth token is in hand.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerRegistrationConfig {
  /// The `--url` the user registered against, e.g.
  /// `https://github.com/Falconiere/toolu-ghrunner`.
  pub runner_url: String,
  /// Runner name — defaults to the hostname at `register` time.
  pub runner_name: String,
  /// Runner ID assigned by GH during registration.
  pub runner_id: i64,
  /// Long-lived OAuth token (the `ghs_…` form on github.com).
  pub auth_token: String,
  /// Labels the runner advertises.
  pub labels: Vec<String>,
  /// Runner group (defaults to `"Default"`).
  pub runner_group: String,
  /// Runtime section: paths + JIT config + protocol version.
  pub runtime: RuntimeConfig,
}

/// Runtime sub-section: paths, JIT config blob, protocol version.
///
/// `work_dir` and `data_dir` are stored as the user supplied them (with
/// `~` intact). Callers expand tilde via [`shared::paths::expand_tilde`]
/// at use time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeConfig {
  /// Base64-encoded JIT config from GH. Loaded by `run` and parsed into
  /// `protocol::JitConfig`.
  pub jit_config: String,
  /// Per-job workspace root.
  pub work_dir: String,
  /// Root for runner-internal state (logs, cache, lock, events).
  pub data_dir: String,
  /// `"v2"` for github.com, `"v1"` for GHES.
  pub protocol_version: String,
}

/// Long-lived OAuth credentials stored in `credentials.json`.
///
/// Kept separate from `config.toml` so it can be rotated without
/// rewriting the registration block, and so future keyring-based
/// credential storage can swap in without touching the TOML format.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CredentialsFile {
  /// OAuth access token (typically `ghs_…`).
  pub access_token: String,
  /// RFC3339 timestamp the token was issued.
  pub issued_at: String,
  /// RFC3339 timestamp the token expires. Optional — GH tokens are
  /// long-lived but the field is reserved for future short-lived flows.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub expires_at: Option<String>,
}

/// Load `RunnerRegistrationConfig` from a TOML file.
///
/// # Errors
///
/// Returns `RunnerError::Config` on missing/unparseable file, and
/// `RunnerError::Io` on filesystem errors.
pub fn load_config(path: &Path) -> Result<RunnerRegistrationConfig, RunnerError> {
  let raw = std::fs::read_to_string(path).map_err(|e| RunnerError::Config(format!(
    "read {}: {e}",
    path.display()
  )))?;
  toml::from_str(&raw).map_err(|e| RunnerError::Config(format!(
    "parse {}: {e}",
    path.display()
  )))
}

/// Persist `config` to `path` as TOML, mode 0600 on Unix.
///
/// Creates parent directories if missing. Truncates and rewrites the
/// file atomically (open-with-truncate).
///
/// # Errors
///
/// Returns `RunnerError::Config` on TOML encoding failures,
/// `RunnerError::Io` on filesystem errors (parent dir creation, file
/// open, write, sync).
pub fn save_config(path: &Path, config: &RunnerRegistrationConfig) -> Result<(), RunnerError> {
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent)?;
  }
  let body = toml::to_string_pretty(config)
    .map_err(|e| RunnerError::Config(format!("toml encode: {e}")))?;
  write_secret_file(path, body.as_bytes())
}

/// Load `CredentialsFile` from a JSON file.
///
/// # Errors
///
/// Returns `RunnerError::Config` on missing/unparseable file.
pub fn load_credentials(path: &Path) -> Result<CredentialsFile, RunnerError> {
  let raw = std::fs::read_to_string(path).map_err(|e| RunnerError::Config(format!(
    "read {}: {e}",
    path.display()
  )))?;
  serde_json::from_str(&raw).map_err(|e| RunnerError::Config(format!(
    "parse {}: {e}",
    path.display()
  )))
}

/// Persist `creds` to `path` as JSON, mode 0600 on Unix.
///
/// # Errors
///
/// Returns `RunnerError::Config` on JSON encoding failures,
/// `RunnerError::Io` on filesystem errors.
pub fn save_credentials(path: &Path, creds: &CredentialsFile) -> Result<(), RunnerError> {
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent)?;
  }
  let body = serde_json::to_string_pretty(creds)
    .map_err(|e| RunnerError::Config(format!("json encode: {e}")))?;
  write_secret_file(path, body.as_bytes())
}

/// Write `body` to `path` with explicit 0600 mode on Unix.
///
/// On non-Unix platforms the mode is best-effort (Windows inherits its
/// default ACL behavior — the runner logs a warning rather than failing).
fn write_secret_file(path: &Path, body: &[u8]) -> Result<(), RunnerError> {
  #[cfg(unix)]
  let mut opts = OpenOptions::new();
  #[cfg(unix)]
  {
    opts.create(true).write(true).truncate(true).mode(SECRET_FILE_MODE);
  }
  #[cfg(not(unix))]
  let mut opts = OpenOptions::new();
  #[cfg(not(unix))]
  {
    opts.create(true).write(true).truncate(true);
  }

  let mut f = opts.open(path)?;
  f.write_all(body)?;
  f.sync_all()?;
  Ok(())
}

/// JIT endpoint URL for a given GH host.
///
/// - `github.com` → `https://pipelinesgh.azureedge.net` (the canonical
///   JIT endpoint for github.com).
/// - Any other host → `https://pipelines.<host>` (GHES convention).
///
/// This is the URL `register` validates with a HEAD request before
/// accepting the registration.
pub fn jit_endpoint_for_host(host: &str) -> String {
  if host.eq_ignore_ascii_case("github.com") {
    "https://pipelinesgh.azureedge.net".to_owned()
  } else {
    format!("https://pipelines.{host}")
  }
}

/// Resolve `data_dir` from a stored config string, expanding `~` and
/// ensuring it exists.
///
/// # Errors
///
/// Returns `RunnerError::Io` if the resolved directory cannot be
/// created.
pub fn resolve_data_dir(stored: &str) -> Result<PathBuf, RunnerError> {
  let p = paths::expand_tilde(Path::new(stored));
  std::fs::create_dir_all(&p)?;
  Ok(p)
}

/// Resolve `work_dir` from a stored config string, expanding `~`.
/// Does NOT create the directory — the listener creates per-job
/// subdirs under it on demand.
pub fn resolve_work_dir(stored: &str) -> PathBuf {
  paths::expand_tilde(Path::new(stored))
}

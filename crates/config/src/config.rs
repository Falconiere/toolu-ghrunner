//! Persisted registration + runtime configuration.
//!
//! `register` writes [`RunnerRegistrationConfig`] to
//! `~/.toolu-runner/config.toml` and [`CredentialsFile`] to
//! `credentials.json` (both 0600 on Unix); `run`/`status`/`remove` read them
//! back. The `[runtime]` section holds paths + JIT blob + protocol version;
//! the optional `[services]` section selects forwarder vs offline mode.
//! `work_dir`/`data_dir` keep their `~` and resolve via
//! [`shared::paths::expand_tilde`] at load time.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use shared::{CacheConfig, L2Config, RunnerError, ServicesMode, paths};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

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
  /// `[services]` section: artifact/cache/OIDC serving mode.
  #[serde(default)]
  pub services: ServicesSection,
  /// `[cache]` section: content-addressed cache settings.
  #[serde(default)]
  pub cache: CacheSection,
  /// `[workspace]` section: per-job workspace GC.
  #[serde(default)]
  pub workspace: WorkspaceSection,
  /// `[shadow]` section: step-observation mode.
  #[serde(default)]
  pub shadow: ShadowSection,
}

impl RunnerRegistrationConfig {
  /// Resolve the artifact/cache/OIDC serving mode (`forwarder` default).
  pub fn services_mode(&self) -> ServicesMode {
    self.services.resolve()
  }

  /// Address the accelerated cache server binds (`0.0.0.0` default).
  pub fn service_bind(&self) -> String {
    self.services.bind.clone()
  }

  /// Resolve the `[cache]` section into the runtime [`CacheConfig`].
  pub fn cache_config(&self) -> CacheConfig {
    self.cache.resolve()
  }

  /// Age in hours after which a finished job's workspace is pruned.
  pub fn workspace_gc_hours(&self) -> u64 {
    self.workspace.gc_after_hours
  }

  /// Whether shadow-mode step observation is enabled.
  pub fn shadow_enabled(&self) -> bool {
    self.shadow.enabled
  }
}

/// `[services]` config section selecting how artifacts/cache/OIDC are served.
///
/// `mode = "forwarder"` (default) forwards real GitHub service URLs from the
/// job message; `mode = "offline"` hosts local services for hermetic runs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServicesSection {
  /// `"forwarder"` (default), `"offline"`, or `"accelerated"`.
  #[serde(default = "default_services_mode")]
  pub mode: String,
  /// Address the accelerated cache server binds (`0.0.0.0` default; must not
  /// be loopback — `docker-container` BuildKit reaches it across a netns).
  #[serde(default = "default_service_bind")]
  pub bind: String,
}

impl Default for ServicesSection {
  fn default() -> Self {
    Self {
      mode: default_services_mode(),
      bind: default_service_bind(),
    }
  }
}

impl ServicesSection {
  /// Map the `mode` string to [`ServicesMode`]; unknown values fall back to
  /// `forwarder` with a `WARN` (a typo must not silently host local services).
  fn resolve(&self) -> ServicesMode {
    match self.mode.trim().to_ascii_lowercase().as_str() {
      "offline" => ServicesMode::Offline,
      "accelerated" => ServicesMode::Accelerated,
      "forwarder" => ServicesMode::Forwarder,
      other => {
        tracing::warn!(mode = other, "unknown [services] mode; using forwarder");
        ServicesMode::Forwarder
      },
    }
  }
}

fn default_services_mode() -> String {
  "forwarder".to_owned()
}

fn default_service_bind() -> String {
  shared::RunnerConfig::DEFAULT_SERVICE_BIND.to_owned()
}

/// `[cache]` config section: content-addressed cache settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheSection {
  /// L1 eviction ceiling in bytes.
  #[serde(default = "default_cache_max_bytes")]
  pub max_bytes: u64,
  /// Entry TTL in days.
  #[serde(default = "default_entry_ttl_days")]
  pub entry_ttl_days: u64,
  /// Branches a `Trusted` job may write the shared scope for.
  #[serde(default = "default_protected_branches")]
  pub protected_branches: Vec<String>,
  /// FastCDC target average chunk size in bytes.
  #[serde(default = "default_chunk_avg_bytes")]
  pub chunk_avg_bytes: u32,
  /// `[cache.l2]` S3 cold tier.
  #[serde(default)]
  pub l2: L2Section,
}

impl Default for CacheSection {
  fn default() -> Self {
    Self {
      max_bytes: default_cache_max_bytes(),
      entry_ttl_days: default_entry_ttl_days(),
      protected_branches: default_protected_branches(),
      chunk_avg_bytes: default_chunk_avg_bytes(),
      l2: L2Section::default(),
    }
  }
}

impl CacheSection {
  /// Resolve into the runtime [`CacheConfig`]. L2 is `Some` only when enabled.
  fn resolve(&self) -> CacheConfig {
    CacheConfig {
      max_bytes: self.max_bytes,
      entry_ttl_days: self.entry_ttl_days,
      protected_branches: self.protected_branches.clone(),
      chunk_avg_bytes: self.chunk_avg_bytes,
      l2: self.l2.resolve(),
    }
  }
}

/// `[cache.l2]` config section: optional S3 cold tier.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct L2Section {
  /// Enable the S3 mirror.
  #[serde(default)]
  pub enabled: bool,
  /// S3 bucket.
  #[serde(default)]
  pub bucket: String,
  /// S3-compatible endpoint URL.
  #[serde(default)]
  pub endpoint: String,
  /// S3 region.
  #[serde(default)]
  pub region: String,
}

impl L2Section {
  /// `Some(L2Config)` when enabled, else `None`.
  fn resolve(&self) -> Option<L2Config> {
    if !self.enabled {
      return None;
    }
    Some(L2Config {
      bucket: self.bucket.clone(),
      endpoint: self.endpoint.clone(),
      region: self.region.clone(),
    })
  }
}

/// `[workspace]` config section: per-job workspace GC.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceSection {
  /// Age in hours after which a finished job's workspace is pruned.
  #[serde(default = "default_gc_after_hours")]
  pub gc_after_hours: u64,
}

impl Default for WorkspaceSection {
  fn default() -> Self {
    Self {
      gc_after_hours: default_gc_after_hours(),
    }
  }
}

/// `[shadow]` config section: step-observation mode.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShadowSection {
  /// Record would-hit / false-hit observations (never serves).
  #[serde(default)]
  pub enabled: bool,
}

fn default_cache_max_bytes() -> u64 {
  CacheConfig::DEFAULT_MAX_BYTES
}

fn default_entry_ttl_days() -> u64 {
  CacheConfig::DEFAULT_ENTRY_TTL_DAYS
}

fn default_protected_branches() -> Vec<String> {
  CacheConfig::DEFAULT_PROTECTED_BRANCHES
    .iter()
    .map(|b| (*b).to_owned())
    .collect()
}

fn default_chunk_avg_bytes() -> u32 {
  CacheConfig::DEFAULT_CHUNK_AVG_BYTES
}

fn default_gc_after_hours() -> u64 {
  shared::RunnerConfig::DEFAULT_WORKSPACE_GC_HOURS
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
  let raw = std::fs::read_to_string(path)
    .map_err(|e| RunnerError::Config(format!("read {}: {e}", path.display())))?;
  toml::from_str(&raw).map_err(|e| RunnerError::Config(format!("parse {}: {e}", path.display())))
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
  let body =
    toml::to_string_pretty(config).map_err(|e| RunnerError::Config(format!("toml encode: {e}")))?;
  write_secret_file(path, body.as_bytes())
}

/// Load `CredentialsFile` from a JSON file.
///
/// # Errors
///
/// Returns `RunnerError::Config` on missing/unparseable file.
pub fn load_credentials(path: &Path) -> Result<CredentialsFile, RunnerError> {
  let raw = std::fs::read_to_string(path)
    .map_err(|e| RunnerError::Config(format!("read {}: {e}", path.display())))?;
  serde_json::from_str(&raw)
    .map_err(|e| RunnerError::Config(format!("parse {}: {e}", path.display())))
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
/// Beyond `.mode()`, Unix adds `O_NOFOLLOW` (refuse a symlink pre-planted at
/// `path`) and an explicit `set_permissions(0600)` on the open handle —
/// `.mode()` only fires on create, so an already-existing target (the re-mint
/// loop rewrites `credentials.json` every job) would keep its looser mode.
/// Non-Unix mode is best-effort (Windows inherits its default ACL).
pub(crate) fn write_secret_file(path: &Path, body: &[u8]) -> Result<(), RunnerError> {
  #[cfg(unix)]
  let mut opts = OpenOptions::new();
  #[cfg(unix)]
  {
    opts
      .create(true)
      .write(true)
      .truncate(true)
      .mode(SECRET_FILE_MODE)
      // Unix-only, per-OS constant (0x100 macOS / 0o400000 Linux); taken
      // from libc so the value is correct on both targets.
      .custom_flags(libc::O_NOFOLLOW);
  }
  #[cfg(not(unix))]
  let mut opts = OpenOptions::new();
  #[cfg(not(unix))]
  {
    opts.create(true).write(true).truncate(true);
  }

  let mut f = opts.open(path)?;
  // Re-tighten an already-existing file to 0600 regardless of its prior mode
  // (see the doc comment above — `.mode()` only fires on create).
  #[cfg(unix)]
  f.set_permissions(std::fs::Permissions::from_mode(SECRET_FILE_MODE))?;
  f.write_all(body)?;
  f.sync_all()?;
  Ok(())
}

/// Best-effort `0700` on a freshly created directory (Unix only).
///
/// Mirrors `shared::startup`'s directory hardening so the runner home is not
/// left world-listable when `login` / `create-app` create it before any
/// command has run tracing init (the only other place the home is
/// tightened). WARN, not fatal — a chmod failure never blocks persisting the
/// token / app file, and it also supplies no worse a posture than the prior
/// behavior.
#[cfg(unix)]
pub(crate) fn harden_dir_perms(path: &Path) {
  if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)) {
    tracing::warn!(error = %e, path = %path.display(), "failed to set 0o700 on runner home");
  }
}

/// Non-Unix no-op (permissions are inherited from the default ACL).
#[cfg(not(unix))]
pub(crate) fn harden_dir_perms(_path: &Path) {}

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

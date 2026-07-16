//! CLI-login token persistence.
//!
//! Stores the `login` device-flow bearer per host: a `0600` JSON file under
//! `data_dir` by default, the OS keyring when `TOOLU_RUNNER_KEYRING` opts
//! in. File is the default because macOS Keychain ACLs bind to the binary's
//! code signature — every rebuild re-prompts — and `0600` already matches
//! the on-disk `credentials.json` (RSA key) threat model.
//!
//! Token resolution precedence (flag > env > stored) is factored into the
//! pure [`pick_bearer`] so it can be unit-tested without any I/O, with
//! [`resolve_bearer`] wiring the three real sources into it.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use shared::RunnerError;

/// Keyring service name; the `user` slot holds the GitHub host.
const KEYRING_SERVICE: &str = "toolu-runner";

/// Env var that forces the file backend and skips the OS keyring probe.
/// Kept for back-compat; overrides [`KEYRING_ENV`] when both are set.
const NO_KEYRING_ENV: &str = "TOOLU_RUNNER_NO_KEYRING";

/// Env var that opts in to the OS keyring backend (file is the default).
const KEYRING_ENV: &str = "TOOLU_RUNNER_KEYRING";

/// A persisted login token plus the metadata needed to reuse it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredToken {
  /// The bearer token GitHub issued (e.g. a `gho_…` device-flow token).
  pub access_token: String,
  /// OAuth scopes the token was granted.
  pub scope: String,
  /// GitHub host the token authenticates against (e.g. `github.com`).
  pub host: String,
  /// When the token was stored, RFC3339 (`chrono::Utc::now().to_rfc3339()`).
  pub issued_at: String,
}

/// Where login tokens are persisted.
///
/// [`AuthStore::new`] picks the variant: per-host `0600` JSON files by
/// default; the OS keyring only when `TOOLU_RUNNER_KEYRING` opts in and
/// the keyring is reachable (`TOOLU_RUNNER_NO_KEYRING` overrides).
pub enum AuthStore {
  /// The OS secure store (`keyring` crate: macOS Keychain, Windows
  /// Credential Manager, Linux kernel keyutils).
  Keyring,
  /// Fallback: per-host `token-<host>.json` files under this directory.
  File(PathBuf),
}

impl AuthStore {
  /// Choose a backend: `File(data_dir)` unless `TOOLU_RUNNER_KEYRING` opts
  /// in to the OS keyring (see [`keyring_opted_in`]) AND the READ-ONLY
  /// [`Self::keyring_reachable`] probe succeeds (probe failure WARNs once
  /// and falls back to `File`). `TOOLU_RUNNER_NO_KEYRING` (back-compat)
  /// forces `File` even when the opt-in is set, short-circuiting BEFORE any
  /// keyring call — which can block or prompt on macOS / a locked keyring.
  pub fn new(data_dir: &Path) -> Self {
    if no_keyring_forced(std::env::var_os(NO_KEYRING_ENV).as_deref())
      || !keyring_opted_in(std::env::var_os(KEYRING_ENV).as_deref())
    {
      return AuthStore::File(data_dir.to_path_buf());
    }
    match Self::keyring_reachable() {
      Ok(()) => AuthStore::Keyring,
      Err(err) => {
        tracing::warn!(
          error = %err,
          "OS keyring unavailable; storing login tokens as 0600 files under data_dir instead"
        );
        AuthStore::File(data_dir.to_path_buf())
      },
    }
  }

  /// Read-only keyring reachability probe. Builds the sentinel entry and
  /// reads it; `get_password` never creates an entry, so a missing sentinel
  /// (`NoEntry`) still proves the store is reachable. Any other error means
  /// the secure store is unavailable.
  fn keyring_reachable() -> Result<(), keyring::Error> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, "__probe__")?;
    match entry.get_password() {
      Ok(_) | Err(keyring::Error::NoEntry) => Ok(()),
      Err(err) => Err(err),
    }
  }

  /// Persist `token`, keyed by its `host`. Overwrites any existing entry.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Auth` on keyring failures, `RunnerError::Io` /
  /// `RunnerError::Json` on file-backend failures.
  pub fn save(&self, token: &StoredToken) -> Result<(), RunnerError> {
    match self {
      AuthStore::Keyring => {
        let json = serde_json::to_string(token)?;
        keyring_entry(&token.host)?
          .set_password(&json)
          .map_err(|e| RunnerError::Auth(format!("keyring write failed for {}: {e}", token.host)))
      },
      AuthStore::File(dir) => {
        std::fs::create_dir_all(dir)?;
        // `login` may create the runner home before any command runs tracing
        // init (which is the only other place the home is chmod 0700), so
        // tighten it here to keep it off the world-listable default umask.
        crate::config::harden_dir_perms(dir);
        let path = token_file_path(dir, &token.host);
        let body = serde_json::to_string_pretty(token)?;
        crate::config::write_secret_file(&path, body.as_bytes())
      },
    }
  }

  /// Load the token for `host`, or `None` when not logged in.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Auth` on keyring failures other than a missing
  /// entry, `RunnerError::Io` / `RunnerError::Json` on file-backend failures.
  pub fn load(&self, host: &str) -> Result<Option<StoredToken>, RunnerError> {
    match self {
      AuthStore::Keyring => match keyring_entry(host)?.get_password() {
        Ok(json) => Ok(Some(serde_json::from_str(&json)?)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(err) => Err(RunnerError::Auth(format!(
          "keyring read failed for {host}: {err}"
        ))),
      },
      AuthStore::File(dir) => {
        let path = token_file_path(dir, host);
        match std::fs::read(&path) {
          Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
          Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
          Err(e) => Err(RunnerError::Io(e)),
        }
      },
    }
  }

  /// Delete the token for `host`. Idempotent: a missing entry is `Ok(())`.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Auth` on keyring failures other than a missing
  /// entry, `RunnerError::Io` on file-backend failures other than a
  /// missing file.
  pub fn delete(&self, host: &str) -> Result<(), RunnerError> {
    match self {
      AuthStore::Keyring => match keyring_entry(host)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(err) => Err(RunnerError::Auth(format!(
          "keyring delete failed for {host}: {err}"
        ))),
      },
      AuthStore::File(dir) => {
        let path = token_file_path(dir, host);
        match std::fs::remove_file(&path) {
          Ok(()) => Ok(()),
          Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
          Err(e) => Err(RunnerError::Io(e)),
        }
      },
    }
  }
}

/// Whether `TOOLU_RUNNER_NO_KEYRING` forces the file backend: set to a
/// non-empty value other than `"0"`. Set-but-empty and the literal `"0"`
/// count as "not requested" — the comparison is exact, no trimming, so
/// `"0 "` or `"00"` count as set. Pure (takes the raw env value) so the
/// rule is unit-tested without mutating the environment or the keyring.
pub fn no_keyring_forced(value: Option<&OsStr>) -> bool {
  env_flag_set(value)
}

/// Whether `TOOLU_RUNNER_KEYRING` opts in to the OS keyring backend. Same
/// parse rule as [`no_keyring_forced`]: set, non-empty, not the literal
/// `"0"`. Pure for the same unit-testability reason.
pub fn keyring_opted_in(value: Option<&OsStr>) -> bool {
  env_flag_set(value)
}

/// Shared env-flag parse: set to a non-empty value other than `"0"`.
fn env_flag_set(value: Option<&OsStr>) -> bool {
  match value {
    Some(v) => !v.is_empty() && v != OsStr::new("0"),
    None => false,
  }
}

/// Build a keyring entry for `host`, mapping construction errors to `Auth`.
fn keyring_entry(host: &str) -> Result<keyring::Entry, RunnerError> {
  keyring::Entry::new(KEYRING_SERVICE, host)
    .map_err(|e| RunnerError::Auth(format!("keyring entry init failed for {host}: {e}")))
}

/// Path of the per-host token file: `<dir>/token-<host>.json`.
///
/// `host` is whitelist-sanitized: only `[A-Za-z0-9.-]` survive, every other
/// char (`:`, `/`, `\`, control chars, null, …) maps to `_`. The result can
/// never introduce a path separator, so it always stays a flat filename
/// directly under `dir` and cannot escape it on any platform. A sanitized
/// host that is empty or holds no alphanumeric (e.g. `..`, `::`) falls back
/// to `unknown-host`, so the filename is never `token-.json` / `token-...json`.
fn token_file_path(dir: &Path, host: &str) -> PathBuf {
  let safe: String = host
    .chars()
    .map(|c| {
      if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
        c
      } else {
        '_'
      }
    })
    .collect();
  let safe = if safe.is_empty() || !safe.chars().any(|c| c.is_ascii_alphanumeric()) {
    "unknown-host".to_owned()
  } else {
    safe
  };
  dir.join(format!("token-{safe}.json"))
}

/// Pure precedence resolver: `flag` > `env` > `stored`.
///
/// Isolated (no I/O) so the precedence rule is unit-testable.
pub fn pick_bearer(
  flag: Option<String>,
  env: Option<String>,
  stored: Option<String>,
) -> Option<String> {
  flag.or(env).or(stored)
}

/// Resolve the bearer token for `host`: `flag` > `TOOLU_RUNNER_TOKEN` env >
/// the stored token. Wires the three sources into [`pick_bearer`].
///
/// # Errors
///
/// Propagates a `RunnerError` from [`AuthStore::load`].
pub fn resolve_bearer(
  store: &AuthStore,
  host: &str,
  flag: Option<String>,
) -> Result<Option<String>, RunnerError> {
  let env = std::env::var("TOOLU_RUNNER_TOKEN").ok();
  let stored = store.load(host)?.map(|t| t.access_token);
  Ok(pick_bearer(flag, env, stored))
}

/// What `register` does about a bearer: the outcome of [`decide_bearer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BearerDecision {
  /// A token was resolved — use it (regardless of TTY).
  Use(String),
  /// No token, but interactive — start the inline device flow.
  StartDeviceFlow,
  /// No token and non-interactive — fail with this canonical message.
  Fail(String),
}

/// Pure TTY gate on the resolved bearer: token → [`BearerDecision::Use`];
/// none + TTY → [`BearerDecision::StartDeviceFlow`]; none + no TTY →
/// [`BearerDecision::Fail`] naming `--token`, `TOOLU_RUNNER_TOKEN`, and
/// `toolu-runner login`.
pub fn decide_bearer(resolved: Option<String>, is_tty: bool) -> BearerDecision {
  match (resolved, is_tty) {
    (Some(token), _) => BearerDecision::Use(token),
    (None, true) => BearerDecision::StartDeviceFlow,
    (None, false) => BearerDecision::Fail(
      "no GitHub token: pass --token, set TOOLU_RUNNER_TOKEN, or run 'toolu-runner login'"
        .to_owned(),
    ),
  }
}

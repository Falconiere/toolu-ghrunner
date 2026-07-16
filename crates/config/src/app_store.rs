//! GitHub App identity + secret persistence.
//!
//! The `create-github-app` flow mints a GitHub App (via the App-manifest
//! conversion) and hands back its identity plus its secrets: the private
//! key PEM, the OAuth client secret, and (optionally) the webhook secret.
//! This module persists that bundle to a single `0600` JSON file under the
//! runner home (`<home>/github-app.json`), shared by every repo — the App
//! is an account-level identity, not a per-repo registration.
//!
//! The `0600` write reuses [`crate::config::write_secret_file`] so the mode
//! logic lives in exactly one place; read / NotFound handling mirrors
//! [`crate::auth_store`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use shared::RunnerError;

/// The app store file name under the runner home.
const APP_STORE_FILE: &str = "github-app.json";

/// Persisted GitHub App identity + secrets. `<home>/github-app.json`, 0600.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct StoredApp {
  /// GitHub host the App lives on (e.g. `github.com`).
  pub host: String,
  /// Numeric App id GitHub assigned.
  pub app_id: i64,
  /// URL slug (e.g. `my-runner-app`); backs [`StoredApp::install_url`].
  pub slug: String,
  /// Account (user or org) that owns the App.
  pub owner: String,
  /// OAuth client id (public, but stored alongside the secret).
  pub client_id: String,
  /// OAuth client secret (sensitive).
  pub client_secret: String,
  /// Webhook signing secret, when one was configured (sensitive).
  pub webhook_secret: Option<String>,
  /// Private-key PEM used to mint installation tokens (sensitive).
  pub pem: String,
  /// The App's public GitHub URL.
  pub html_url: String,
  /// When the App was created, RFC3339.
  pub created_at: String,
}

/// Path of the app store file: `<home>/github-app.json`.
pub fn app_path(home: &Path) -> PathBuf {
  home.join(APP_STORE_FILE)
}

/// Write the app store 0600 (reuses [`crate::config::write_secret_file`]).
///
/// Creates the home directory if missing.
///
/// # Errors
///
/// Returns `RunnerError::Config` on JSON encoding failures,
/// `RunnerError::Io` on filesystem errors.
pub fn save_app(home: &Path, app: &StoredApp) -> Result<(), RunnerError> {
  std::fs::create_dir_all(home)?;
  // `create-app` may create the runner home before any command runs tracing
  // init (which is the only other place the home is chmod 0700), so tighten
  // it here to keep it off the world-listable default umask.
  crate::config::harden_dir_perms(home);
  let path = app_path(home);
  let body = serde_json::to_string_pretty(app)
    .map_err(|e| RunnerError::Config(format!("json encode: {e}")))?;
  crate::config::write_secret_file(&path, body.as_bytes())
}

/// Load the app store, or `None` when no App has been created.
///
/// # Errors
///
/// Returns `RunnerError::Io` on read failures other than a missing file,
/// `RunnerError::Config` on parse failures.
pub fn load_app(home: &Path) -> Result<Option<StoredApp>, RunnerError> {
  let path = app_path(home);
  match std::fs::read(&path) {
    Ok(bytes) => serde_json::from_slice(&bytes)
      .map(Some)
      .map_err(|e| RunnerError::Config(format!("parse {}: {e}", path.display()))),
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
    Err(e) => Err(RunnerError::Io(e)),
  }
}

impl StoredApp {
  /// The install URL to print after creation:
  /// `https://github.com/apps/{slug}/installations/new`.
  pub fn install_url(&self) -> String {
    format!("https://github.com/apps/{}/installations/new", self.slug)
  }

  /// A secret-free, human-readable summary for the terminal.
  ///
  /// Excludes the pem body, `client_secret`, `webhook_secret`, and
  /// `client_id` — only non-sensitive identity fields, the install URL,
  /// and where the secrets were saved.
  pub fn safe_summary(&self, save_path: &Path) -> String {
    format!(
      "GitHub App created:\n  \
       name:    {slug}\n  \
       app id:  {app_id}\n  \
       owner:   {owner}\n  \
       host:    {host}\n  \
       url:     {html_url}\n\n\
       Install it:\n  {install_url}\n\n\
       Secrets saved (0600): {save_path}",
      slug = self.slug,
      app_id = self.app_id,
      owner = self.owner,
      host = self.host,
      html_url = self.html_url,
      install_url = self.install_url(),
      save_path = save_path.display(),
    )
  }
}

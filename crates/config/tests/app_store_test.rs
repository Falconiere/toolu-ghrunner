//! Integration tests for `config::app_store` (create-github-app AC-3, AC-9).
//!
//! Real data only: a real `StoredApp` is persisted into a real `tempfile`
//! home and read back — no mocks. The dummy PEM is a phony-but-shaped key
//! string (never a live key), so the roundtrip and the secret-free summary
//! are exercised against the same bytes the real flow would write.

use std::path::Path;

use config::app_store::{self, StoredApp};
use tempfile::TempDir;

/// A phony PEM body — right shape, no live key material.
const DUMMY_PEM: &str =
  "-----BEGIN RSA PRIVATE KEY-----\nMIIphony\n-----END RSA PRIVATE KEY-----\n";
/// The OAuth client secret used across the secret-leak assertions.
const CLIENT_SECRET: &str = "cs_super_secret_value";
/// The webhook secret used across the secret-leak assertions.
const WEBHOOK_SECRET: &str = "wh_super_secret_value";

/// Build the canonical `StoredApp` the tests persist.
fn sample_app() -> StoredApp {
  StoredApp {
    host: "github.com".to_owned(),
    app_id: 123_456,
    slug: "my-runner-app".to_owned(),
    owner: "falconiere".to_owned(),
    client_id: "Iv1.abcdef1234567890".to_owned(),
    client_secret: CLIENT_SECRET.to_owned(),
    webhook_secret: Some(WEBHOOK_SECRET.to_owned()),
    pem: DUMMY_PEM.to_owned(),
    html_url: "https://github.com/apps/my-runner-app".to_owned(),
    created_at: "2026-07-13T00:00:00Z".to_owned(),
  }
}

// ── AC-3: save/load roundtrip ─────────────────────────────────────────

#[test]
fn save_then_load_roundtrips_the_whole_struct() {
  let home = TempDir::new().expect("temp home");
  let app = sample_app();

  app_store::save_app(home.path(), &app).expect("save_app");
  let loaded = app_store::load_app(home.path()).expect("load_app");

  assert_eq!(loaded, Some(app));
}

// ── AC-3: 0600 mode on Unix ───────────────────────────────────────────

#[cfg(unix)]
#[test]
fn saved_file_is_mode_0600() {
  use std::os::unix::fs::PermissionsExt;

  let home = TempDir::new().expect("temp home");
  app_store::save_app(home.path(), &sample_app()).expect("save_app");

  let path = app_store::app_path(home.path());
  let mode = std::fs::metadata(&path)
    .expect("metadata")
    .permissions()
    .mode();
  assert_eq!(mode & 0o777, 0o600, "github-app.json must be 0600");
}

// ── absent: no App created yet ────────────────────────────────────────

#[test]
fn load_on_fresh_home_is_none() {
  let home = TempDir::new().expect("temp home");
  assert_eq!(app_store::load_app(home.path()).expect("load_app"), None);
}

// ── AC-9: safe_summary leaks no secrets ───────────────────────────────

#[test]
fn safe_summary_excludes_secrets() {
  let home = TempDir::new().expect("temp home");
  let app = sample_app();
  app_store::save_app(home.path(), &app).expect("save_app");

  let save_path = app_store::app_path(home.path());
  let summary = app.safe_summary(&save_path);

  // No pem body, client secret, or webhook secret.
  assert!(!summary.contains("MIIphony"), "summary leaked the pem body");
  assert!(
    !summary.contains(CLIENT_SECRET),
    "summary leaked the client secret"
  );
  assert!(
    !summary.contains(WEBHOOK_SECRET),
    "summary leaked the webhook secret"
  );

  // It still carries the identity + install URL the operator needs.
  assert!(summary.contains(&app.slug), "summary should name the app");
  assert!(
    summary.contains(&app.app_id.to_string()),
    "summary should include the app id"
  );
  assert!(
    summary.contains(&app.install_url()),
    "summary should include the install url"
  );
  assert!(
    summary.contains(save_path_str(&save_path).as_str()),
    "summary should include the save path"
  );
}

/// The save path as it renders in the summary (`Path::display`).
fn save_path_str(path: &Path) -> String {
  path.display().to_string()
}

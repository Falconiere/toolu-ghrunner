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
    owner_id: Some(9_919),
    installation_id: Some(987_654),
  }
}

/// A `github-app.json` exactly as `create-app` wrote it before #202 (no
/// `owner_id` / `installation_id`).
const PRE_202_FILE: &str = r#"{
  "host": "github.com",
  "app_id": 123456,
  "slug": "my-runner-app",
  "owner": "falconiere",
  "client_id": "Iv1.abcdef1234567890",
  "client_secret": "cs_super_secret_value",
  "webhook_secret": null,
  "pem": "-----BEGIN RSA PRIVATE KEY-----\nMIIphony\n-----END RSA PRIVATE KEY-----\n",
  "html_url": "https://github.com/apps/my-runner-app",
  "created_at": "2026-07-13T00:00:00Z"
}"#;

// ── #202 AC-8: optional owner / installation ids ─────────────────────

#[test]
fn a_pre_202_file_loads_without_the_new_ids_and_keeps_the_plain_install_url() {
  let home = TempDir::new().expect("temp home");
  std::fs::write(app_store::app_path(home.path()), PRE_202_FILE).expect("write");
  let app = app_store::load_app(home.path())
    .expect("load")
    .expect("present");
  assert_eq!((app.owner_id, app.installation_id), (None, None));
  assert_eq!(
    app.install_url(),
    "https://github.com/apps/my-runner-app/installations/new"
  );
}

#[test]
fn a_known_owner_id_makes_the_install_url_skip_the_account_picker() {
  assert_eq!(
    sample_app().install_url(),
    "https://github.com/apps/my-runner-app/installations/new/permissions?target_id=9919"
  );
}

#[test]
fn unset_ids_are_left_out_of_the_saved_file() {
  let home = TempDir::new().expect("temp home");
  let app = StoredApp {
    owner_id: None,
    installation_id: None,
    ..sample_app()
  };
  app_store::save_app(home.path(), &app).expect("save");
  let text = std::fs::read_to_string(app_store::app_path(home.path())).expect("read");
  assert!(
    !text.contains("owner_id") && !text.contains("installation_id"),
    "{text}"
  );
  assert_eq!(app_store::load_app(home.path()).expect("load"), Some(app));
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

// ── #201: the App file an App registration names ──────────────────────

#[test]
fn load_app_file_reads_the_saved_app() {
  let home = TempDir::new().expect("temp home");
  let app = sample_app();
  app_store::save_app(home.path(), &app).expect("save_app");
  let loaded = app_store::load_app_file(&app_store::app_path(home.path())).expect("load");
  assert_eq!(loaded, app);
}

#[test]
fn load_app_file_names_the_missing_file_and_create_app() {
  let home = TempDir::new().expect("temp home");
  let path = app_store::app_path(home.path());
  let err = app_store::load_app_file(&path).expect_err("missing");
  let msg = err.to_string();
  assert!(msg.contains(&path.display().to_string()), "{msg}");
  assert!(msg.contains("toolu-runner create-app"), "{msg}");
}

#[test]
fn secret_values_lists_every_secret_and_debug_redacts_them() {
  let app = sample_app();
  assert_eq!(
    app.secret_values(),
    [DUMMY_PEM, CLIENT_SECRET, WEBHOOK_SECRET],
    "the masker must learn the PEM, client secret and webhook secret"
  );
  let debug = format!("{app:?}");
  for secret in ["MIIphony", CLIENT_SECRET, WEBHOOK_SECRET] {
    assert!(!debug.contains(secret), "Debug leaked {secret}: {debug}");
  }
  assert!(
    debug.contains("my-runner-app"),
    "identity stays visible: {debug}"
  );
}

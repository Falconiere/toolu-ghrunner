//! Integration tests for Finding 2: `login` (`AuthStore::File::save`) and
//! `create-app` (`app_store::save_app`) harden the runner home to 0700 when
//! they are the first command to create it — neither runs tracing init (the
//! only other place the home is chmod'd), so an umask-default 0755 home would
//! otherwise stay world-listable.
//!
//! Real data only: real secrets are written into a real, not-yet-existing home
//! under a `tempfile` parent, then the home's on-disk mode is stat'd back.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;

use config::app_store::{self, StoredApp};
use config::auth_store::{AuthStore, StoredToken};

/// A phony PEM body — right shape, no live key material.
const DUMMY_PEM: &str =
  "-----BEGIN RSA PRIVATE KEY-----\nMIIphony\n-----END RSA PRIVATE KEY-----\n";

#[test]
fn auth_store_save_hardens_new_home_to_0700() {
  let parent = tempfile::tempdir().expect("temp parent");
  let home = parent.path().join("fresh-home");
  assert!(!home.exists(), "precondition: home does not exist yet");

  let store = AuthStore::File(home.clone());
  store
    .save(&StoredToken {
      access_token: "gho_phony_test_token".to_owned(),
      scope: "repo".to_owned(),
      host: "github.com".to_owned(),
      issued_at: "2026-07-15T00:00:00Z".to_owned(),
    })
    .expect("save token");

  let mode = fs::metadata(&home).expect("stat home").permissions().mode() & 0o777;
  assert_eq!(mode, 0o700, "login must chmod the new home 0700");
}

#[test]
fn app_store_save_hardens_new_home_to_0700() {
  let parent = tempfile::tempdir().expect("temp parent");
  let home = parent.path().join("fresh-home");
  assert!(!home.exists(), "precondition: home does not exist yet");

  app_store::save_app(
    &home,
    &StoredApp {
      host: "github.com".to_owned(),
      app_id: 123_456,
      slug: "my-runner-app".to_owned(),
      owner: "falconiere".to_owned(),
      client_id: "Iv1.abcdef1234567890".to_owned(),
      client_secret: "cs_phony".to_owned(),
      webhook_secret: None,
      pem: DUMMY_PEM.to_owned(),
      html_url: "https://github.com/apps/my-runner-app".to_owned(),
      created_at: "2026-07-15T00:00:00Z".to_owned(),
    },
  )
  .expect("save app");

  let mode = fs::metadata(&home).expect("stat home").permissions().mode() & 0o777;
  assert_eq!(mode, 0o700, "create-app must chmod the new home 0700");
}

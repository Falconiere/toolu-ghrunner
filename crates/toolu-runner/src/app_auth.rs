//! GitHub App authentication for `register` and the always-online re-mint
//! (#201): load the App `create-app` saved, register its secrets with the
//! `SecretMasker`, find its installation on the registration's repo or org,
//! and mint installation tokens — cached in memory by
//! [`InstallationTokens`] while `wire::net::github_app::reuse_deadline`
//! allows, never persisted.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use config::app_store::{self, StoredApp};
use shared::{RunnerError, SecretMasker};
use wire::net::RunnerScope;
use wire::net::github_app::{
  create_installation_token, find_installation, list_installations, reuse_deadline,
};

/// Per-request timeout of every App API call.
const APP_API_TIMEOUT: Duration = Duration::from_secs(30);

/// Register `value` with the masker (a poisoned lock still masks).
fn mask(masker: &Mutex<SecretMasker>, value: &str) {
  let mut guard = masker
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner);
  guard.add_secret(value);
}

/// Load the App file at `path` and register its secrets with `masker`
/// before anything can log them.
///
/// # Errors
///
/// `RunnerError::Config` naming the file (missing or unparseable),
/// `RunnerError::Io` on other read failures.
pub(crate) fn load_app(
  path: &Path,
  masker: &Mutex<SecretMasker>,
) -> Result<StoredApp, RunnerError> {
  let app = app_store::load_app_file(path)?;
  masker
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .add_secrets(app.secret_values());
  Ok(app)
}

/// Sign a JWT for `app` and mask it (it is a credential for ten minutes).
fn app_jwt(app: &StoredApp, masker: &Mutex<SecretMasker>) -> Result<String, RunnerError> {
  let jwt = protocol::app_jwt::build_app_jwt(&app.pem, app.app_id)?;
  mask(masker, &jwt);
  Ok(jwt)
}

/// What `register` needs from the App: the installation it registers under
/// and a token to mint with.
pub(crate) struct AppRegistration {
  /// The installation covering the registration's repo or org.
  pub(crate) installation_id: i64,
  /// A fresh installation token (masked).
  pub(crate) token: String,
}

/// Find `app`'s installation on `url`'s repo or org and mint a token.
///
/// # Errors
///
/// `RunnerError::Auth` when the App is not installed there (naming where it
/// is installed and the install URL) or GitHub rejects the App;
/// `RunnerError::Network` for transient failures; `RunnerError::Config` for
/// an unaddressable URL.
pub(crate) async fn register_installation(
  client: &reqwest::Client,
  app: &StoredApp,
  url: &str,
  masker: &Mutex<SecretMasker>,
) -> Result<AppRegistration, RunnerError> {
  let (api_base, scope) = wire::net::resolve_target(url)?;
  let jwt = app_jwt(app, masker)?;
  let Some(installation) =
    find_installation(client, &api_base, &jwt, &scope, APP_API_TIMEOUT).await?
  else {
    return Err(not_installed(client, app, &api_base, &jwt, &scope).await);
  };
  let token =
    create_installation_token(client, &api_base, &jwt, installation.id, APP_API_TIMEOUT).await?;
  mask(masker, &token.token);
  Ok(AppRegistration {
    installation_id: installation.id,
    token: token.token,
  })
}

/// The "not installed on <target>" error, naming the accounts the App IS
/// installed on when GitHub lists them (a failed listing is left out).
async fn not_installed(
  client: &reqwest::Client,
  app: &StoredApp,
  api_base: &str,
  jwt: &str,
  scope: &RunnerScope,
) -> RunnerError {
  let installed_on = match list_installations(client, api_base, jwt, APP_API_TIMEOUT).await {
    Ok(list) => {
      let accounts: Vec<String> = list
        .into_iter()
        .filter_map(|installation| installation.account.map(|account| account.login))
        .collect();
      if accounts.is_empty() {
        " (it has no installation yet)".to_owned()
      } else {
        format!(" (it is installed on: {})", accounts.join(", "))
      }
    },
    Err(e) => {
      tracing::debug!(error = %e, "could not list the App's installations");
      String::new()
    },
  };
  RunnerError::Auth(format!(
    "GitHub App '{}' (App {}) is not installed on {}{installed_on} — install it at {}, or \
     register with a personal token (--token or TOOLU_RUNNER_TOKEN)",
    app.slug,
    app.app_id,
    scope.display_name(),
    app.install_url()
  ))
}

/// The fix to print when an App re-mint is rejected.
pub(crate) fn app_remint_help(app: &StoredApp, app_file: &str, installation_id: i64) -> String {
  format!(
    "the GitHub App '{}' (App {}, installation {installation_id}) could not mint a runner: \
     check that it is still installed ({}) with Administration (repository) or Self-hosted \
     runners (organization) write access, and that {app_file} holds its current private key; \
     then run `toolu-runner register --app --replace` (or `--token … --replace` to use a \
     personal token)",
    app.slug,
    app.app_id,
    app.install_url()
  )
}

/// A cached installation token.
struct CachedToken {
  installation_id: i64,
  token: String,
  /// Unix second after which the token is no longer reused.
  reuse_until: i64,
}

/// The re-mint loop's installation-token cache: memory only, one entry.
#[derive(Default)]
pub(crate) struct InstallationTokens {
  cached: Mutex<Option<CachedToken>>,
}

impl InstallationTokens {
  /// A token for `installation_id`: the cached one while it may be reused,
  /// else a fresh mint (then cached when its `expires_at` parses).
  ///
  /// # Errors
  ///
  /// As [`create_installation_token`] (`Auth` fatal, `Network` transient),
  /// or `RunnerError::Config` for an unaddressable `url`.
  pub(crate) async fn bearer(
    &self,
    client: &reqwest::Client,
    app: &StoredApp,
    url: &str,
    installation_id: i64,
    masker: &Mutex<SecretMasker>,
  ) -> Result<String, RunnerError> {
    let now = chrono::Utc::now().timestamp();
    if let Some(token) = self.reusable(installation_id, now) {
      return Ok(token);
    }
    let (api_base, _scope) = wire::net::resolve_target(url)?;
    let jwt = app_jwt(app, masker)?;
    let minted =
      create_installation_token(client, &api_base, &jwt, installation_id, APP_API_TIMEOUT).await?;
    mask(masker, &minted.token);
    let mut cached = self
      .cached
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    *cached = reuse_deadline(&minted.expires_at, now).map(|reuse_until| CachedToken {
      installation_id,
      token: minted.token.clone(),
      reuse_until,
    });
    Ok(minted.token)
  }

  /// The cached token for `installation_id` if it may still be used at `now`.
  fn reusable(&self, installation_id: i64, now: i64) -> Option<String> {
    let cached = self
      .cached
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    cached
      .as_ref()
      .filter(|entry| entry.installation_id == installation_id && now < entry.reuse_until)
      .map(|entry| entry.token.clone())
  }
}

//! How `register` authenticates (#201): the GitHub App saved by
//! `create-app` (an installation token, [`crate::app_auth`]) when `--app` is
//! given or the App exists for the host and no personal token was given;
//! else `--token` > `TOOLU_RUNNER_TOKEN` > the stored `login` token, with
//! the device flow inline on an interactive terminal. Also resolves an org
//! `--runner-group` name to its id with that bearer.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use config::app_store::{self, StoredApp};
use config::auth_store::{self, AuthStore, BearerDecision};
use config::registration_auth::AuthSection;
use shared::{RunnerError, SecretMasker};
use wire::net::RunnerScope;

use crate::cli::RegisterAuthArgs;
use crate::login_cmd;

/// The bearer `register` mints with and the `[auth]` it persists.
pub(crate) struct RegisterAuth {
  /// The REST bearer: a personal token or an App installation token.
  pub(crate) bearer: String,
  /// The persisted `[auth]` section.
  pub(crate) section: AuthSection,
  /// `GitHub App '<slug>' (installation <id>)` for an App registration.
  pub(crate) app_label: Option<String>,
}

impl RegisterAuth {
  /// A mint failure, naming the App permissions when an App was used.
  pub(crate) fn explain(&self, e: &RunnerError) -> String {
    match (&self.app_label, e) {
      (Some(label), RunnerError::Auth(_)) => format!(
        "{e}\n(registering as {label}: the App needs Administration write access on a \
         repository, Self-hosted runners write access on an organization)"
      ),
      _ => e.to_string(),
    }
  }
}

/// Resolve how `register` authenticates: the GitHub App ([`pick_app`]) when
/// one applies, else the personal-token chain ([`resolve_register_bearer`]).
pub(crate) async fn resolve(
  args: &RegisterAuthArgs,
  host: &str,
  home: &Path,
  url: &str,
  client: &reqwest::Client,
  masker: &Mutex<SecretMasker>,
) -> Result<RegisterAuth, Box<dyn std::error::Error>> {
  let token_given =
    args.token.is_some() || config::config::runner_token().is_some_and(|token| !token.is_empty());
  let Some((app_file, app)) = pick_app(args.app, token_given, home, host, masker)? else {
    let bearer = resolve_register_bearer(host, args.token.clone(), home).await?;
    return Ok(RegisterAuth {
      bearer,
      section: AuthSection::Token,
      app_label: None,
    });
  };
  let registration = crate::app_auth::register_installation(client, &app, url, masker).await?;
  let app_file = std::path::absolute(&app_file).unwrap_or(app_file);
  Ok(RegisterAuth {
    bearer: registration.token,
    section: AuthSection::App {
      app_file: app_file.to_string_lossy().into_owned(),
      installation_id: registration.installation_id,
    },
    app_label: Some(format!(
      "GitHub App '{}' (installation {})",
      app.slug, registration.installation_id
    )),
  })
}

/// The App `register` uses, if any: `--app` requires `<home>/github-app.json`
/// for `host`; without it, the file is used when it exists for `host` and no
/// personal token was given. Its secrets are masked on load.
fn pick_app(
  app_flag: bool,
  token_given: bool,
  home: &Path,
  host: &str,
  masker: &Mutex<SecretMasker>,
) -> Result<Option<(PathBuf, StoredApp)>, RunnerError> {
  let path = app_store::app_path(home);
  if !app_flag && (token_given || !path.exists()) {
    return Ok(None);
  }
  let app = crate::app_auth::load_app(&path, masker)?;
  if app.host.eq_ignore_ascii_case(host) {
    return Ok(Some((path, app)));
  }
  if !app_flag {
    return Ok(None);
  }
  Err(RunnerError::Config(format!(
    "the GitHub App in {} belongs to {}, not {host} — create one for {host} or pass --token",
    path.display(),
    app.host
  )))
}

/// The runner group to register under. For an organization URL a group
/// *name* (anything but a number or `Default`) is resolved to its id through
/// the org's runner-group list and the id is returned, so re-mint needs no
/// lookup; otherwise `group` is returned as given.
pub(crate) async fn resolve_runner_group(
  group: &str,
  url: &str,
  bearer: &str,
  client: &reqwest::Client,
) -> Result<String, RunnerError> {
  let trimmed = group.trim();
  if trimmed.parse::<i64>().is_ok() || trimmed.eq_ignore_ascii_case("default") {
    return Ok(group.to_owned());
  }
  let (api_base, scope) = wire::net::resolve_target(url)?;
  let RunnerScope::Org { org } = scope else {
    return Ok(group.to_owned());
  };
  let groups = wire::net::runner_groups::list_runner_groups(
    client,
    &api_base,
    &org,
    bearer,
    crate::register_cmd::MINT_TIMEOUT,
  )
  .await
  .map_err(|e| {
    RunnerError::Config(format!(
      "could not list the runner groups of {org} ({e}) — pass the group's numeric ID with \
           --runner-group"
    ))
  })?;
  let Some(id) = wire::net::runner_groups::pick_group(&groups, trimmed) else {
    let names: Vec<&str> = groups.iter().map(|g| g.name.as_str()).collect();
    return Err(RunnerError::Config(format!(
      "runner group '{trimmed}' not found in {org} (groups: {})",
      names.join(", ")
    )));
  };
  Ok(id.to_string())
}

/// Resolve the `generate-jitconfig` bearer for `host`: flag >
/// `TOOLU_RUNNER_TOKEN` env > the stored `login` token (store pinned to
/// the runner home, shared by all repos). With no token, an interactive
/// stderr starts the inline device flow (persisting the minted token for
/// next time); non-interactive fails listing the manual options.
async fn resolve_register_bearer(
  host: &str,
  flag: Option<String>,
  home: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
  let store = AuthStore::new(home);
  let resolved = auth_store::resolve_bearer(&store, host, flag)?;
  match auth_store::decide_bearer(resolved, std::io::stderr().is_terminal()) {
    BearerDecision::Use(token) => Ok(token),
    BearerDecision::StartDeviceFlow => {
      let stored = login_cmd::run_device_flow(host, None, &store, |dc| {
        eprintln!("Enter code {} at {}", dc.user_code, dc.verification_uri);
      })
      .await?;
      Ok(stored.access_token)
    },
    BearerDecision::Fail(msg) => Err(RunnerError::Auth(msg).into()),
  }
}

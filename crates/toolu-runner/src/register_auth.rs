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
  /// [`implicit_note`] when the App was picked automatically (no `--app`).
  pub(crate) implicit_note: Option<String>,
}

impl RegisterAuth {
  /// A failure after authentication (runner groups, the mint), naming the
  /// App permissions when an App was refused and, for an automatically picked
  /// App, how to register without it.
  pub(crate) fn explain(&self, e: &RunnerError) -> String {
    let permissions = if let (Some(label), RunnerError::Auth(_)) = (&self.app_label, e) {
      format!(
        "\n(registering as {label}: the App needs Administration write access on a \
         repository, Self-hosted runners write access on an organization)"
      )
    } else {
      String::new()
    };
    let note = self.implicit_note.as_deref().unwrap_or_default();
    format!("{e}{permissions}{note}")
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
      implicit_note: None,
    });
  };
  let registration = crate::app_auth::register_installation(client, &app, url, masker)
    .await
    .map_err(|e| with_implicit_note(e, args.app, &app_file, host))?;
  let app_file = absolute_app_file(&app_file)?;
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
    implicit_note: (!args.app).then(|| implicit_note(&app_file, host)),
  })
}

/// `app_file` as the absolute path `[auth] app_file` persists: `run` reads it
/// from whatever directory the supervisor starts it in.
///
/// # Errors
///
/// `RunnerError::Config` naming the file when it cannot be made absolute
/// (an empty path, or a relative one when the current directory is
/// unreadable), so a relative path is never persisted.
fn absolute_app_file(app_file: &Path) -> Result<PathBuf, RunnerError> {
  std::path::absolute(app_file).map_err(|e| {
    RunnerError::Config(format!(
      "cannot record the GitHub App file '{}' as an absolute path: {e}",
      app_file.display()
    ))
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
  let app = crate::app_auth::load_app(&path, masker)
    .map_err(|e| with_implicit_note(e, app_flag, &path, host))?;
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

/// Why an automatically picked App (no `--app`) was used, and how to
/// register without it.
fn implicit_note(path: &Path, host: &str) -> String {
  format!(
    "\n(the GitHub App was used automatically because {} exists for {host}: pass --token or \
     set TOOLU_RUNNER_TOKEN to register with a personal token, or move the file aside to use \
     the stored `login` token)",
    path.display()
  )
}

/// Append [`implicit_note`] to an App failure when the App was picked
/// automatically, keeping the error kind.
fn with_implicit_note(error: RunnerError, app_flag: bool, path: &Path, host: &str) -> RunnerError {
  if app_flag {
    return error;
  }
  let note = implicit_note(path, host);
  // Keep the kind the run loop and `main` act on; anything else is a setup
  // error of the App file.
  if let RunnerError::Auth(msg) = error {
    return RunnerError::Auth(format!("{msg}{note}"));
  }
  if let RunnerError::Network(msg) = error {
    return RunnerError::Network(format!("{msg}{note}"));
  }
  if let RunnerError::Config(msg) = error {
    return RunnerError::Config(format!("{msg}{note}"));
  }
  RunnerError::Config(format!("{error}{note}"))
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
    if let RunnerError::Network(msg) = e {
      return RunnerError::Network(format!(
        "could not list the runner groups of {org} ({msg}) — retry, or pass the group's \
         numeric ID with --runner-group"
      ));
    }
    RunnerError::Config(format!(
      "could not list the runner groups of {org} ({e}) — the bearer may not read the org's \
       runner groups; pass the group's numeric ID with --runner-group"
    ))
  })?;
  let Some(id) = wire::net::runner_groups::pick_group(&groups, trimmed) else {
    let names: Vec<&str> = groups.iter().map(|g| g.name.as_str()).collect();
    let partial = if groups.len() >= wire::net::runner_groups::PAGE_SIZE {
      "; only the first page was searched — pass the group's numeric ID"
    } else {
      ""
    };
    return Err(RunnerError::Config(format!(
      "runner group '{trimmed}' not found in {org} (groups: {}){partial}",
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

#[cfg(test)]
#[path = "tests/register_auth.rs"]
mod tests;

//! `register` subcommand: mint a JIT config via `generate-jitconfig` and
//! persist the registration.
//!
//! Split out of `main.rs` to keep the CLI entrypoint under the crate's
//! per-file complexity limit. `--url` is optional: when absent, the repo
//! is inferred from the cwd git remote `origin` (github.com only — GHES
//! keeps the explicit `--url` path). The REST bearer resolves flag >
//! `TOOLU_RUNNER_TOKEN` env > stored `login` token, and an interactive
//! terminal with no token runs the device flow inline
//! ([`login_cmd::run_device_flow`]). State lands in the per-repo
//! `<home>/runners/<owner>/<repo>/` dir unless `--config` overrides it.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::Duration;

use config::auth_store::{self, AuthStore, BearerDecision};
use config::config::{
  CacheSection, CredentialsFile, RunnerRegistrationConfig, RuntimeConfig, ServicesSection,
  ShadowSection, WorkspaceSection, save_config as save_reg_config, save_credentials,
};
use config::{registry, repo_infer};
use shared::RunnerError;

use crate::cli::{
  RegisterArgs, credentials_path_for, default_labels, runner_name_or_hostname,
  work_folder_or_default,
};
use crate::login_cmd;

/// `register`: resolve the target repo and bearer, POST
/// `generate-jitconfig`, and persist config + credentials (all-or-nothing).
pub(crate) async fn cmd_register(args: RegisterArgs) -> Result<(), Box<dyn std::error::Error>> {
  crate::init_runner_tracing().map_err(|e| format!("startup init: {e}"))?;

  let (url, host) = resolve_url_and_host(args.url).map_err(|e| format!("{e}"))?;

  let home = registry::runner_home();
  let config_path = match args.config.clone() {
    Some(path) => path,
    None => register_config_path(&url, &home).map_err(|e| format!("{e}"))?,
  };
  let creds_path = credentials_path_for(&config_path);
  let runner_name = runner_name_or_hostname(args.name);
  let labels = if args.labels.is_empty() {
    default_labels()
  } else {
    args.labels
  };

  ensure_not_registered(&config_path, args.replace)?;

  let token = resolve_register_bearer(&host, args.token.clone(), &home).await?;

  let runner_id = register_and_persist(RegisterPersist {
    url: &url,
    token: &token,
    runner_name: &runner_name,
    labels: &labels,
    runner_group: &args.runner_group,
    work_folder: &work_folder_or_default(args.work.as_ref()),
    host: &host,
    config_path: &config_path,
    creds_path: &creds_path,
    replace: args.replace,
  })
  .await
  .map_err(|e| format!("{e}"))?;

  report_registered(
    &runner_name,
    runner_id,
    &host,
    &config_path,
    &creds_path,
    &labels,
  );
  Ok(())
}

/// Resolve the registration `(url, host)`: an explicit `--url` is
/// validated and used verbatim; when absent, the repo is inferred from
/// the cwd git remote `origin` — github.com only, any other inferred
/// host (GHES) requires an explicit `--url`.
fn resolve_url_and_host(url: Option<String>) -> Result<(String, String), RunnerError> {
  if let Some(url) = url {
    let host = parse_and_validate_url(&url)?;
    return Ok((url, host));
  }
  let cwd = std::env::current_dir()
    .map_err(|e| RunnerError::Config(format!("could not read the current directory: {e}")))?;
  let inferred = repo_infer::detect_repo(&cwd)?;
  if !inferred.host.eq_ignore_ascii_case("github.com") {
    return Err(RunnerError::Config(format!(
      "the `origin` remote points at '{}', not github.com — repo inference is github.com only; \
       GHES registrations need an explicit --url",
      inferred.host
    )));
  }
  Ok((
    format!("https://github.com/{}/{}", inferred.owner, inferred.repo),
    "github.com".to_owned(),
  ))
}

/// Validate a `--url` and return its host (must contain a dot — bare
/// hostnames like `localhost` are rejected).
fn parse_and_validate_url(url: &str) -> Result<String, RunnerError> {
  let parsed =
    url::Url::parse(url).map_err(|e| RunnerError::Config(format!("invalid --url: {e}")))?;
  let host = parsed
    .host_str()
    .ok_or_else(|| RunnerError::Config("URL missing host".to_owned()))?
    .to_owned();
  if !host.contains('.') {
    return Err(RunnerError::Config(format!(
      "invalid host '{host}' — runner accepts github.com and GHES hosts only"
    )));
  }
  Ok(host)
}

/// Default config path for `register` when `--config` is absent.
///
/// Repo URLs (two path segments, `<owner>/<repo>[.git]`) — inferred or
/// explicit, github.com or GHES — get the per-repo
/// `<home>/runners/<owner>/<repo>/config.toml`. Org-level URLs (a single
/// path segment) keep the legacy single-slot `<home>/config.toml`
/// default, unchanged behavior.
fn register_config_path(url: &str, home: &Path) -> Result<PathBuf, RunnerError> {
  match owner_repo_from_url(url) {
    Some((owner, repo)) => Ok(registry::runner_dir(home, &owner, &repo)?.join("config.toml")),
    None => Ok(home.join("config.toml")),
  }
}

/// Lift `(owner, repo)` from a validated registration URL's first two
/// path segments (`.git` stripped from the repo). `None` for org-level
/// URLs (fewer than two segments) — mint-time validation still applies.
fn owner_repo_from_url(url: &str) -> Option<(String, String)> {
  let parsed = url::Url::parse(url).ok()?;
  let mut segments = parsed.path_segments()?.filter(|s| !s.is_empty());
  let owner = segments.next()?;
  let repo_raw = segments.next()?;
  let repo = repo_raw.strip_suffix(".git").unwrap_or(repo_raw);
  if repo.is_empty() {
    return None;
  }
  Some((owner.to_owned(), repo.to_owned()))
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
      let stored = login_cmd::run_device_flow(host, None, &store).await?;
      Ok(stored.access_token)
    },
    BearerDecision::Fail(msg) => Err(RunnerError::Auth(msg).into()),
  }
}

/// Refuse to overwrite an existing registration unless `--replace` was given.
fn ensure_not_registered(
  config_path: &Path,
  replace: bool,
) -> Result<(), Box<dyn std::error::Error>> {
  if config_path.exists() && !replace {
    return Err(
      format!(
        "registration already exists at {} — pass --replace to overwrite",
        config_path.display()
      )
      .into(),
    );
  }
  Ok(())
}

/// Log + print the registration result.
fn report_registered(
  runner_name: &str,
  runner_id: i64,
  host: &str,
  config_path: &Path,
  creds_path: &Path,
  labels: &[String],
) {
  tracing::info!(
    path = %config_path.display(),
    credentials = %creds_path.display(),
    runner = %runner_name,
    runner_id,
    host = %host,
    labels = ?labels,
    "registered runner via generate-jitconfig"
  );
  println!(
    "registered runner '{runner_name}' (id {runner_id}) at {host} (config: {}, creds: {})",
    config_path.display(),
    creds_path.display()
  );
}

/// Inputs for [`register_and_persist`] — the live register + write step.
struct RegisterPersist<'a> {
  url: &'a str,
  token: &'a str,
  runner_name: &'a str,
  labels: &'a [String],
  runner_group: &'a str,
  work_folder: &'a str,
  host: &'a str,
  config_path: &'a Path,
  creds_path: &'a Path,
  replace: bool,
}

/// POST `generate-jitconfig` for `p` and return the minted registration.
async fn mint_jit(p: &RegisterPersist<'_>) -> Result<wire::net::JitRegistration, RunnerError> {
  let client = reqwest::Client::builder()
    .timeout(Duration::from_secs(30))
    .build()
    .map_err(|e| RunnerError::Network(format!("HTTP client: {e}")))?;
  wire::net::register_jit(
    &client,
    &wire::net::RegisterParams {
      url: p.url,
      runner_token: p.token,
      name: p.runner_name,
      labels: p.labels,
      runner_group_id: runner_group_id(p.runner_group),
      work_folder: p.work_folder,
      replace: p.replace,
    },
  )
  .await
}

/// Live JIT registration (all-or-nothing): POST generate-jitconfig, parse
/// the minted config, then persist real config + credentials. Returns the
/// assigned runner ID. Any failure returns before touching either file.
///
/// The RSA→JWT→OAuth2 chain runs at `run` time from the stored jit_config,
/// not here. `auth_token` stores the runner's non-secret `client_id`.
async fn register_and_persist(p: RegisterPersist<'_>) -> Result<i64, RunnerError> {
  let registration = mint_jit(&p).await?;

  // Decode the minted config to confirm it parses and to lift the
  // client_id (a stable, non-secret identity) for the auth_token field.
  let parsed = protocol::JitConfig::parse(&registration.encoded_jit_config)
    .map_err(|e| RunnerError::Protocol(format!("minted jit_config did not parse: {e}")))?;
  let client_id = parsed.credentials.data.client_id;
  let runner_id = registration.runner_id;

  let config = build_registration_config(&p, &client_id, registration);

  // Snapshot any pre-existing config BEFORE overwriting so a rollback can
  // restore it — re-registration must not destroy the previous registration
  // when the credentials write fails.
  let previous_config = std::fs::read(p.config_path).ok();

  // Persist only after the live call + parse both succeed.
  save_reg_config(p.config_path, &config)?;
  let creds = CredentialsFile {
    access_token: client_id,
    issued_at: chrono::Utc::now().to_rfc3339(),
    expires_at: None,
  };
  // Registration is all-or-nothing: a credentials write failure must not
  // leave a config without creds (a half-registered state). Roll the config
  // file back (best-effort) before surfacing the error.
  if let Err(e) = save_credentials(p.creds_path, &creds) {
    roll_back_config(p.config_path, previous_config.as_deref());
    return Err(e);
  }
  ensure_diag_dir(p.config_path);
  Ok(runner_id)
}

/// Best-effort host for a validated registration URL, mirroring
/// `status_cmd`. `runner_url` already passed `register`-time validation, so
/// a parse miss falls back to github.com rather than failing the re-mint.
pub(crate) fn host_from_runner_url(url: &str) -> String {
  url::Url::parse(url)
    .ok()
    .and_then(|u| u.host_str().map(str::to_owned))
    .unwrap_or_else(|| "github.com".to_owned())
}

/// Re-mint a JIT config for an existing registration and persist it,
/// preserving all user-edited config sections. Same all-or-nothing
/// rollback contract as [`register_and_persist`].
pub(crate) async fn remint_and_persist(
  prior: &RunnerRegistrationConfig,
  bearer: &str,
  config_path: &Path,
  creds_path: &Path,
) -> Result<(), RunnerError> {
  let host = host_from_runner_url(&prior.runner_url);
  let p = RegisterPersist {
    url: &prior.runner_url,
    token: bearer,
    runner_name: &prior.runner_name,
    labels: &prior.labels,
    runner_group: &prior.runner_group,
    work_folder: &prior.runtime.work_dir,
    host: &host,
    config_path,
    creds_path,
    replace: true,
  };
  let registration = mint_jit(&p).await?;

  // Lift the client_id exactly as register_and_persist does; the re-minted
  // config keeps prior's [services]/[cache]/[workspace]/[shadow] verbatim.
  let parsed = protocol::JitConfig::parse(&registration.encoded_jit_config)
    .map_err(|e| RunnerError::Protocol(format!("re-minted jit_config did not parse: {e}")))?;
  let client_id = parsed.credentials.data.client_id;
  let merged = config::remint::merge_reminted_config(
    prior,
    registration.encoded_jit_config,
    registration.runner_id,
    client_id.clone(),
  );

  // Snapshot BEFORE overwriting so a credentials-write failure rolls the
  // config back — same all-or-nothing contract as register_and_persist.
  let previous_config = std::fs::read(config_path).ok();
  save_reg_config(config_path, &merged)?;
  let creds = CredentialsFile {
    access_token: client_id,
    issued_at: chrono::Utc::now().to_rfc3339(),
    expires_at: None,
  };
  if let Err(e) = save_credentials(creds_path, &creds) {
    roll_back_config(config_path, previous_config.as_deref());
    return Err(e);
  }
  Ok(())
}

/// Pre-create the registration's `_diag/` dir so the persisted layout is
/// self-evident right after `register`. `run` creates every run-critical
/// dir on its own anyway (`config::lockfile::acquire` creates the lock's
/// parent dir, the journal writer creates `<data_dir>/_diag/jobs/`, and
/// `shared::startup` creates the default home's `_diag/` for tracing), so
/// a failure here is WARNed, never fatal — the registration itself is
/// already complete.
fn ensure_diag_dir(config_path: &Path) {
  let Some(dir) = config_path.parent() else {
    return;
  };
  let diag = dir.join("_diag");
  if let Err(e) = std::fs::create_dir_all(&diag) {
    tracing::warn!(path = %diag.display(), error = %e, "could not pre-create the _diag dir");
  }
}

/// Best-effort rollback of the config file after a failed registration:
/// restore the pre-existing bytes when there were any (the overwrite keeps
/// the file's 0600 mode), otherwise remove the newly created file.
fn roll_back_config(path: &Path, previous: Option<&[u8]>) {
  let result = match previous {
    Some(bytes) => std::fs::write(path, bytes),
    None => std::fs::remove_file(path),
  };
  if let Err(e) = result {
    tracing::warn!(error = %e, "failed to roll back config after credentials write error");
  }
}

/// Assemble the persisted [`RunnerRegistrationConfig`] from the minted
/// registration. `auth_token` carries the non-secret `client_id`;
/// `data_dir` is the config file's own directory, so the lock, `_diag/`,
/// and the journal land per-registration.
fn build_registration_config(
  p: &RegisterPersist<'_>,
  client_id: &str,
  registration: wire::net::JitRegistration,
) -> RunnerRegistrationConfig {
  let runtime = RuntimeConfig {
    jit_config: registration.encoded_jit_config,
    work_dir: p.work_folder.to_owned(),
    data_dir: p.config_path.parent().map_or_else(
      || "~/.toolu-runner".to_owned(),
      |dir| dir.to_string_lossy().into_owned(),
    ),
    protocol_version: if p.host.eq_ignore_ascii_case("github.com") {
      "v2".to_owned()
    } else {
      "v1".to_owned()
    },
  };
  RunnerRegistrationConfig {
    runner_url: p.url.to_owned(),
    runner_name: p.runner_name.to_owned(),
    runner_id: registration.runner_id,
    auth_token: client_id.to_owned(),
    labels: p.labels.to_vec(),
    runner_group: p.runner_group.to_owned(),
    runtime,
    services: ServicesSection::default(),
    cache: CacheSection::default(),
    workspace: WorkspaceSection::default(),
    shadow: ShadowSection::default(),
  }
}

/// Map a `--runner-group` string to a `generate-jitconfig` group ID.
///
/// A numeric value is used directly; non-numeric yields `None`, which
/// [`wire::net::register_jit`] defaults to `1` (Default). "Default" is the
/// CLI's own default value and the canonical name of group 1, so it maps
/// to `None` silently; any other group *name* is not supported by the JIT
/// API and is WARNed about so the fallback to Default is not silent.
fn runner_group_id(group: &str) -> Option<i64> {
  let trimmed = group.trim();
  if let Ok(id) = trimmed.parse::<i64>() {
    return Some(id);
  }
  if !trimmed.is_empty() && !trimmed.eq_ignore_ascii_case("default") {
    tracing::warn!(
      runner_group = trimmed,
      "runner group names are not supported (a numeric group ID is required); \
       defaulting to the Default group"
    );
  }
  None
}

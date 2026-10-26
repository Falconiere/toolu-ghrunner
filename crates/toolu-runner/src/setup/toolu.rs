//! `setup`'s toolu.sh branch (#207, screen 16): pair this box with a
//! toolu.sh org by a code typed in the console, or by `--enroll-token` with
//! no questions; store the `tlu_h_` host token (0600, masked) in the #206
//! registration `<home>/toolu/<org>/`; run it under the machine service; and
//! confirm the service polls and toolu.sh enabled the machine. A box that is
//! already paired is refreshed with `PATCH /runner/host` instead of pairing a
//! second machine. Contract: CodaSignal/toolu.sh
//! `docs/guides/self-hosted-machines.md`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use config::config::{
  CacheSection, CredentialsFile, RunnerRegistrationConfig, RuntimeConfig, ServicesSection,
  ShadowSection, WorkspaceSection, load_config, save_config, save_credentials,
};
use config::provider::ProviderSection;
use config::registry::{self, RegistrationEntry, RegistrationScope};
use protocol::toolu_pair::{HostGrant, MachineReport, SELF_HOSTED_LABEL};
use wire::net::toolu_jobs::{TooluClient, TooluError};
use wire::net::toolu_pair::{enroll, pair_start};

use super::Flow;
use super::args::SetupArgs;
use super::plan::SetupError;
use super::prompts::Prompts;
use super::toolu_wait::{self, Waited};
use crate::doctor_registrations::{RunState, run_state};
use crate::toolu_host;

/// How often the first-poll wait re-reads the registration's lock.
const LOCK_POLL: Duration = Duration::from_millis(250);

/// Where jobs come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
  /// GitHub (#204).
  Github,
  /// toolu.sh (#207).
  Toolu,
}

/// The source the flags settle: `--source`, else `--enroll-token` (toolu.sh),
/// else `--org` / `--repo` (GitHub); `None` = ask (or GitHub unattended).
///
/// # Errors
///
/// Exit 2 for an unknown `--source`, or `--enroll-token` with GitHub.
pub(crate) fn flag_source(args: &SetupArgs) -> Result<Option<Source>, SetupError> {
  let source = match args.source.as_deref() {
    Some("github") => Some(Source::Github),
    Some("toolu") => Some(Source::Toolu),
    Some(other) => {
      return Err(SetupError::usage(format!(
        "--source '{other}' is not one of github|toolu"
      )));
    },
    None if args.enroll_token.is_some() => Some(Source::Toolu),
    None if args.org.is_some() || !args.repo.is_empty() => Some(Source::Github),
    None => None,
  };
  if source == Some(Source::Github) && args.enroll_token.is_some() {
    return Err(SetupError::usage(
      "--enroll-token pairs with toolu.sh: drop it, or use --source toolu",
    ));
  }
  Ok(source)
}

/// The enrollment token once the source is toolu.sh: `--enroll-token`, else
/// `TOOLU_RUNNER_ENROLL_TOKEN`. The variable never picks the source, so one
/// left exported cannot turn a GitHub setup into a pairing.
fn enroll_token(args: &SetupArgs) -> Option<String> {
  args
    .enroll_token
    .clone()
    .or_else(crate::config::enroll_token)
}

/// Exit 2 naming the first GitHub-only flag given with the toolu.sh source.
fn reject_github_flags(args: &SetupArgs) -> Result<(), SetupError> {
  let given = [
    (args.org.is_some(), "--org"),
    (!args.repo.is_empty(), "--repo"),
    (args.github_url.is_some(), "--github-url"),
    (args.auth.is_some(), "--auth"),
    (args.import.is_some(), "--import"),
    (args.app_id.is_some(), "--app-id"),
    (args.app_key.is_some(), "--app-key"),
    (args.token.is_some(), "--token"),
    (args.token_stdin, "--token-stdin"),
    (args.runner_group.is_some(), "--runner-group"),
    (args.name.is_some(), "--name"),
    (args.no_default_labels, "--no-default-labels"),
    (args.mode.ephemeral, "--ephemeral"),
    (args.mode.replace, "--replace"),
  ];
  match given.iter().find(|(set, _)| *set) {
    Some((_, flag)) => Err(SetupError::usage(format!(
      "{flag} is GitHub-only; a toolu.sh machine takes --labels, --work-dir, --no-service, \
       --enable-linger, --enroll-token, --yes and --dry-run"
    ))),
    None => Ok(()),
  }
}

/// The toolu.sh registrations already on this machine.
fn paired<'a>(flow: &'a Flow<'a>) -> Vec<&'a RegistrationEntry> {
  flow
    .facts
    .existing
    .iter()
    .filter(|entry| matches!(entry.scope, RegistrationScope::Toolu(_)))
    .collect()
}

/// The toolu.sh branch: refresh an existing pairing, or pair / enroll.
///
/// # Errors
///
/// [`SetupError`] with the epic's codes: 2 usage, auth, a denied pairing or
/// a refused enrollment token; 1 runtime or an expired code unattended; 130
/// cancelled.
pub(super) async fn run(flow: &Flow<'_>, prompts: &mut Option<Prompts>) -> Result<(), SetupError> {
  let (args, masker) = (flow.args, flow.masker);
  reject_github_flags(args)?;
  let token = enroll_token(args);
  if let Some(token) = &token
    && let Ok(mut masker) = masker.lock()
  {
    masker.add_secret(token);
  }
  let api = crate::config::toolu_api_url();
  let existing = paired(flow);
  if args.dry_run() {
    review(flow, &api, existing.first().copied(), token.is_some());
    return Ok(());
  }
  if let Some(entry) = existing.first() {
    return refresh(flow, entry, prompts).await;
  }
  let enable_linger =
    super::scope::resolve_linger(args, prompts, flow.facts, !args.mode.no_service)?;
  let report = toolu_wait::machine_report(&args.labels, flow.facts.cpus, flow.facts.memory_mb);
  let client = super::http_client()?;
  let grant = match &token {
    Some(token) => enroll(&client, &api, token, &report)
      .await
      .map_err(|e| refused("enroll", e))?,
    None => pair(flow, prompts, &client, &api, &report).await?,
  };
  if let Ok(mut masker) = masker.lock() {
    masker.add_secret(&grant.host_token);
  }
  let config_path = persist(flow, &api, &grant)?;
  if let Err(e) = crate::supervisor_signal::notify(flow.home) {
    tracing::warn!(error = %e, "could not notify the toolu-runner service");
  }
  let cfg = load(&config_path)?;
  let host_client = client_for(&cfg, &config_path, flow)?;
  announce(flow, &host_client, &grant, &config_path).await?;
  finish(flow, &cfg, &config_path, &host_client, enable_linger).await
}

/// `paired: host <id> · workspace <name>` (from `GET /runner/host`, the
/// machine's first authenticated request) and the saved-token line.
async fn announce(
  flow: &Flow<'_>,
  client: &TooluClient,
  grant: &HostGrant,
  config_path: &Path,
) -> Result<(), SetupError> {
  let out = flow.out;
  match client.host().await {
    Ok(host) => out.done(
      "paired",
      &format!("host {} · workspace {}", host.host_id, host.org.name),
    ),
    Err(TooluError::Revoked(why)) => return Err(revoked(config_path, &why)),
    Err(e) => out.warn(
      "paired",
      &format!(
        "host {} · workspace {} (toolu.sh did not answer: {e})",
        grant.host_id, grant.org
      ),
    ),
  }
  out.done("host token", "saved (0600 · never logged)");
  Ok(())
}

/// Device-code pairing: show the code, open the page on a desktop, wait;
/// on a terminal an expired code can be replaced (enter) or given up
/// (ctrl+c).
async fn pair(
  flow: &Flow<'_>,
  prompts: &mut Option<Prompts>,
  client: &reqwest::Client,
  api: &str,
  report: &MachineReport,
) -> Result<HostGrant, SetupError> {
  let out = flow.out;
  loop {
    let start = pair_start(client, api, report)
      .await
      .map_err(|e| refused("start pairing", e))?;
    if let Ok(mut masker) = flow.masker.lock() {
      masker.add_secret(&start.device_code);
    }
    out.rail("Pair this machine with toolu.sh");
    out.line(&format!(
      "go to   {}",
      toolu_wait::display_uri(&start.verification_uri)
    ));
    out.line(&format!("enter   {}", start.user_code));
    if !flow.facts.headless {
      crate::login_cmd::open_browser_best_effort(&start.verification_uri_complete);
    }
    match toolu_wait::wait_for_approval(client, api, &start, out).await? {
      Waited::Granted(grant) => return Ok(grant),
      Waited::Denied => {
        out.error(
          "pairing",
          "Pairing was denied in the console. Nothing was saved.",
        );
        return Err(SetupError::usage(
          "the pairing was denied in the toolu.sh console",
        ));
      },
      Waited::Expired => {
        out.error("pairing", "Pairing code expired. Nothing was saved.");
        let Some(prompts) = prompts.as_mut() else {
          return Err(SetupError::runtime(
            "the pairing code expired before it was approved; run setup again for a new code",
          ));
        };
        prompts
          .press_enter("Press enter for a new code, or ctrl+c to stop.")
          .map_err(SetupError::from)?;
      },
    }
  }
}

/// A refused or failed pairing / enrollment request as a [`SetupError`].
fn refused(what: &str, e: TooluError) -> SetupError {
  match e {
    TooluError::Revoked(_) if what == "enroll" => SetupError::usage(
      "toolu.sh refused the enrollment token (invalid, expired, revoked or already used). \
       Nothing was saved.",
    ),
    TooluError::Revoked(why) => {
      SetupError::usage(format!("toolu.sh refused to {what} (401: {why})"))
    },
    TooluError::NotFound => SetupError::runtime(format!(
      "toolu.sh does not offer self-hosted machines at {} ({what}: 404)",
      crate::config::toolu_api_url()
    )),
    TooluError::Refused(why) => SetupError::usage(format!("toolu.sh refused to {what}: {why}")),
    TooluError::Protocol(why) | TooluError::Conflict(why) | TooluError::Transient(why) => {
      SetupError::runtime(format!("could not {what} with toolu.sh: {why}"))
    },
  }
}

/// The error for a machine toolu.sh no longer accepts.
fn revoked(config_path: &Path, why: &str) -> SetupError {
  SetupError::usage(format!(
    "toolu.sh rejected this machine's host token ({why}): it was revoked. Pair it again: {}",
    toolu_host::revoked_fix(config_path)
  ))
}

/// A failed `/runner/host` call (`what`) as a [`SetupError`].
fn host_failure(config_path: &Path, what: &str, e: TooluError) -> SetupError {
  match e {
    TooluError::Revoked(why) => revoked(config_path, &why),
    TooluError::Refused(why) => SetupError::usage(format!("toolu.sh refused to {what}: {why}")),
    e @ (TooluError::Conflict(_)
    | TooluError::NotFound
    | TooluError::Transient(_)
    | TooluError::Protocol(_)) => SetupError::runtime(format!("could not {what} on toolu.sh: {e}")),
  }
}

/// Write `<home>/toolu/<org>/`: the host token first (0600), the config
/// last (a registration is listed by its `config.toml`).
fn persist(flow: &Flow<'_>, api: &str, grant: &HostGrant) -> Result<PathBuf, SetupError> {
  let dir = registry::toolu_dir(flow.home, &grant.org)
    .map_err(|e| SetupError::runtime(format!("toolu.sh sent an unusable org id: {e}")))?;
  registry::ensure_runner_home(flow.home).map_err(|e| SetupError::runtime(e.to_string()))?;
  let config_path = dir.join("config.toml");
  save_credentials(
    &crate::cli::credentials_path_for(&config_path),
    &CredentialsFile {
      access_token: grant.host_token.clone(),
      issued_at: chrono::Utc::now().to_rfc3339(),
      expires_at: None,
    },
  )
  .map_err(|e| SetupError::runtime(format!("could not save the host token: {e}")))?;
  let cfg = RunnerRegistrationConfig {
    runner_url: String::new(),
    runner_name: String::new(),
    runner_id: 0,
    auth_token: String::new(),
    labels: grant.labels.clone(),
    runner_group: String::new(),
    ephemeral: false,
    provider: ProviderSection::Toolu {
      api_url: api.to_owned(),
      host_id: grant.host_id.clone(),
      org: grant.org.clone(),
    },
    runtime: RuntimeConfig {
      jit_config: String::new(),
      work_dir: crate::cli::work_folder_or_default(flow.args.work_dir.as_ref()),
      data_dir: dir.to_string_lossy().into_owned(),
      protocol_version: String::new(),
    },
    services: ServicesSection::default(),
    cache: CacheSection::default(),
    workspace: WorkspaceSection::default(),
    shadow: ShadowSection::default(),
    network: config::network::NetworkSection::default(),
    auth: config::registration_auth::AuthSection::Token,
  };
  save_config(&config_path, &cfg)
    .map_err(|e| SetupError::runtime(format!("could not save the registration: {e}")))?;
  Ok(config_path)
}

/// Load a toolu.sh registration.
fn load(config_path: &Path) -> Result<RunnerRegistrationConfig, SetupError> {
  load_config(config_path)
    .map_err(|e| SetupError::runtime(format!("could not read {}: {e}", config_path.display())))
}

/// The machine's `/runner/*` client (token masked).
fn client_for(
  cfg: &RunnerRegistrationConfig,
  config_path: &Path,
  flow: &Flow<'_>,
) -> Result<TooluClient, SetupError> {
  toolu_host::client_for(
    cfg,
    &crate::cli::credentials_path_for(config_path),
    |token| {
      if let Ok(mut masker) = flow.masker.lock() {
        masker.add_secret(token);
      }
    },
  )
  .map_err(SetupError::runtime)
}

/// Re-run on a paired box: `PATCH /runner/host` with this box's report
/// (`--labels`, else the stored labels), then the same finish.
pub(super) async fn refresh(
  flow: &Flow<'_>,
  entry: &RegistrationEntry,
  prompts: &mut Option<Prompts>,
) -> Result<(), SetupError> {
  let (args, out) = (flow.args, flow.out);
  reject_github_flags(args)?;
  let mut cfg = load(&entry.config_path)?;
  let client = client_for(&cfg, &entry.config_path, flow)?;
  if enroll_token(args).is_some() {
    out.warn(
      "enroll",
      "this machine is already paired; the enrollment token was not used",
    );
  }
  let labels = if args.labels.is_empty() {
    cfg.labels.clone()
  } else {
    args.labels.clone()
  };
  let report = toolu_wait::machine_report(&labels, flow.facts.cpus, flow.facts.memory_mb);
  let host = client
    .update_host(&report.patch())
    .await
    .map_err(|e| host_failure(&entry.config_path, "update the machine", e))?;
  cfg.labels.clone_from(&host.labels);
  save_config(&entry.config_path, &cfg)
    .map_err(|e| SetupError::runtime(format!("could not save the registration: {e}")))?;
  out.done(
    "updated",
    &format!("host {} · workspace {}", host.host_id, host.org.name),
  );
  let enable_linger =
    super::scope::resolve_linger(args, prompts, flow.facts, !args.mode.no_service)?;
  finish(flow, &cfg, &entry.config_path, &client, enable_linger).await
}

/// Keep as is: what toolu.sh says about the machine; writes nothing.
/// `Ok(false)` when toolu.sh no longer accepts it (shown as an error row,
/// so the other registrations are still checked).
pub(super) async fn keep(flow: &Flow<'_>, entry: &RegistrationEntry) -> Result<bool, SetupError> {
  let cfg = load(&entry.config_path)?;
  let client = client_for(&cfg, &entry.config_path, flow)?;
  match client.host().await {
    Ok(host) => flow.out.done("online", &toolu_host::host_line(&host)),
    Err(TooluError::Revoked(why)) => {
      flow
        .out
        .error("online", &revoked(&entry.config_path, &why).message);
      return Ok(false);
    },
    Err(e) => flow.out.warn(
      "online",
      &format!("could not ask toolu.sh about the machine: {e}"),
    ),
  }
  Ok(true)
}

/// Service, first poll, and the `runs-on` line.
async fn finish(
  flow: &Flow<'_>,
  cfg: &RunnerRegistrationConfig,
  config_path: &Path,
  client: &TooluClient,
  enable_linger: bool,
) -> Result<(), SetupError> {
  let out = flow.out;
  if flow.args.mode.no_service {
    out.done(
      "service",
      "skipped (--no-service): start it with `toolu-runner run` or `toolu-runner install-service`",
    );
  } else {
    super::apply::install_machine_service(enable_linger).await?;
    out.done("service", "installed the toolu-runner machine service");
    out.waiting("first poll", "waiting for the service to poll toolu.sh");
    let data_dir = shared::paths::expand_tilde(Path::new(&cfg.runtime.data_dir));
    wait_for_service(&data_dir.join(".lock"), crate::config::online_timeout()).await?;
    let host = client
      .host()
      .await
      .map_err(|e| host_failure(config_path, "confirm the first poll", e))?;
    if !host.enabled {
      return Err(SetupError::runtime(
        "toolu.sh has not enabled the machine yet; check it with `toolu-runner doctor`",
      ));
    }
    out.done("first poll", "enabled");
  }
  println!("runs-on: [{SELF_HOSTED_LABEL}]");
  out.done(
    "next",
    "check it with `toolu-runner status`, `toolu-runner doctor`; remove it with `toolu-runner remove`",
  );
  Ok(())
}

/// Wait until a live process (the machine service's `run --all`) holds the
/// registration's lock, at most `timeout`.
async fn wait_for_service(lock: &Path, timeout: Duration) -> Result<(), SetupError> {
  let deadline = Instant::now() + timeout;
  loop {
    if matches!(run_state(lock).await, RunState::Active { .. }) {
      return Ok(());
    }
    if Instant::now() >= deadline {
      return Err(SetupError::runtime(format!(
        "the toolu-runner service did not start polling within {}s; check it with \
         `toolu-runner doctor`",
        timeout.as_secs()
      )));
    }
    tokio::time::sleep(LOCK_POLL).await;
  }
}

/// `--dry-run`: the toolu.sh Review, no request.
fn review(flow: &Flow<'_>, api: &str, existing: Option<&RegistrationEntry>, enrolling: bool) {
  let (args, out) = (flow.args, flow.out);
  out.rail("Review");
  out.line(&format!("source: toolu.sh ({api})"));
  let method = match (existing, enrolling) {
    (Some(entry), _) => format!(
      "refresh the paired machine ({}) with PATCH /runner/host",
      entry.config_path.display()
    ),
    (None, true) => "enrollment token (no questions)".to_owned(),
    (None, false) => "pairing code typed in the toolu.sh console".to_owned(),
  };
  out.line(&format!("method: {method}"));
  out.line(&format!("labels: {}", args.labels.join(", ")));
  out.line(&format!(
    "work dir: {}",
    crate::cli::work_folder_or_default(args.work_dir.as_ref())
  ));
  out.line(if args.mode.no_service {
    "service: not installed (--no-service)"
  } else {
    "service: the toolu-runner machine service"
  });
  out.end("dry run: nothing was written");
}

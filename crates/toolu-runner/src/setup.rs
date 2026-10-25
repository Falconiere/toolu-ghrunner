//! `toolu-runner setup` (#204): interactive and flag-driven onboarding
//! for the GitHub path. Machine check → target → auth → scope → runner
//! details → service → Review → apply → online confirmation, plus the
//! re-run menu on a configured machine. Nothing is written before
//! Review (a GitHub App created on the way is the disclosed exception:
//! GitHub returns its key exactly once).
//!
//! With `--yes` or no TTY there are no prompts: a missing answer exits
//! 2 naming its flag and writes nothing. Exit codes: 0 ok, 1 runtime,
//! 2 usage/auth, 130 cancelled.

pub(crate) mod apply;
pub(crate) mod args;
pub(crate) mod github;
pub(crate) mod machine;
pub(crate) mod plan;
pub(crate) mod prompts;
pub(crate) mod render;
pub(crate) mod scope;
pub(crate) mod stage;

use std::io::IsTerminal as _;
use std::path::Path;
use std::sync::{Arc, Mutex};

use config::registry;
use shared::SecretMasker;

use crate::run_cmd::CodedExit;

use args::SetupArgs;
use plan::SetupError;
use prompts::Prompts;
use render::Out;
use stage::{StageOutcome, stage_plan};

/// `setup` entry point (see the module docs for the flow and codes).
pub(crate) async fn cmd_setup(args: SetupArgs) -> Result<(), Box<dyn std::error::Error>> {
  match run_setup(&args).await {
    Ok(()) => Ok(()),
    Err(e) => Err(Box::new(CodedExit::new(e.code, e.message))),
  }
}

/// The whole flow, in [`SetupError`] terms.
async fn run_setup(args: &SetupArgs) -> Result<(), SetupError> {
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  crate::init_tracing_for(&masker)
    .map_err(|e| SetupError::runtime(format!("startup init: {e}")))?;
  if args.source.as_deref() == Some("toolu") {
    return Err(SetupError::usage(
      "the toolu.sh source is not available yet (#207); re-run with --source github",
    ));
  }
  if let Some(source) = &args.source
    && source != "github"
  {
    return Err(SetupError::usage(format!(
      "--source '{source}' is not one of github|toolu"
    )));
  }
  let github_base = plan::validate_github_url(args.github_url.as_deref())?;
  let host = plan::host_of(&github_base);
  let home = registry::runner_home();
  let facts = machine::gather(&home).map_err(SetupError::runtime)?;
  let interactive = !args.yes && !args.dry_run && tty_available();
  let out = Out {
    plain: render::plain_mode(std::io::stdout().is_terminal(), crate::config::no_color()),
  };
  out.done("machine", &facts.summary());
  let mut prompts = if interactive {
    match Prompts::open() {
      Ok(prompts) => Some(prompts),
      Err(_) => None,
    }
  } else {
    None
  };
  let interactive = prompts.is_some();
  if interactive {
    spawn_signal_restore();
  }

  // Re-run menu (interactive, configured machine, no explicit target).
  if !facts.existing.is_empty() && args.org.is_none() && args.repo.is_empty() {
    if let Some(choice) = rerun_choice(&facts, &mut prompts, &out)? {
      return match choice {
        Rerun::Keep => keep_as_is(&facts, &home, args, &masker, &out).await,
        Rerun::Remove(entry) => remove_one(&entry, &home, &masker).await,
        Rerun::Reconfigure(entry) => {
          reconfigure(
            entry,
            args,
            &github_base,
            &host,
            &home,
            &mut prompts,
            &out,
            &masker,
          )
          .await
        },
        Rerun::Add => {
          build_and_apply(
            args,
            &github_base,
            &host,
            &home,
            None,
            &facts,
            &mut prompts,
            &out,
            &masker,
          )
          .await
        },
      };
    }
  }
  build_and_apply(
    args,
    &github_base,
    &host,
    &home,
    None,
    &facts,
    &mut prompts,
    &out,
    &masker,
  )
  .await
}

/// What the re-run menu picked.
enum Rerun {
  /// Verify only; change nothing.
  Keep,
  /// A fresh target under the same service.
  Add,
  /// Replace this registration's settings.
  Reconfigure(config::registry::RegistrationEntry),
  /// Unregister and delete this registration.
  Remove(config::registry::RegistrationEntry),
}

/// The re-run menu, when it applies. `Ok(None)` = fall through to a
/// fresh plan (unattended runs, or Add handled by the caller).
fn rerun_choice(
  facts: &machine::MachineFacts,
  prompts: &mut Option<Prompts>,
  out: &Out,
) -> Result<Option<Rerun>, SetupError> {
  let Some(prompts) = prompts.as_mut() else {
    return Ok(None);
  };
  out.rail("This machine already has runners");
  for entry in &facts.existing {
    out.line(&format!(
      "{} ({})",
      entry.scope.label(),
      entry.config_path.display()
    ));
  }
  let options = vec![
    "Keep as is".to_owned(),
    "Add another runner".to_owned(),
    "Reconfigure".to_owned(),
    "Remove".to_owned(),
  ];
  let picked = prompts
    .select("What would you like to do?", &options, 0)
    .map_err(SetupError::from)?;
  match picked {
    0 => Ok(Some(Rerun::Keep)),
    1 => Ok(Some(Rerun::Add)),
    2 | 3 => {
      let names: Vec<String> = facts
        .existing
        .iter()
        .map(|entry| entry.scope.label())
        .collect();
      let index = prompts
        .select("Which runner?", &names, 0)
        .map_err(SetupError::from)?;
      let entry = facts.existing[index].clone();
      Ok(Some(if picked == 2 {
        Rerun::Reconfigure(entry)
      } else {
        Rerun::Remove(entry)
      }))
    },
    _ => Ok(None),
  }
}

/// Whether a `/dev/tty` prompt could be opened at all.
fn tty_available() -> bool {
  std::fs::OpenOptions::new()
    .read(true)
    .write(true)
    .open("/dev/tty")
    .is_ok()
}

/// Restore the terminal and exit with the signal's code on
/// SIGTERM/SIGHUP while prompts may hold raw mode.
fn spawn_signal_restore() {
  tokio::spawn(async {
    let mut term = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
      Ok(signal) => signal,
      Err(_) => return,
    };
    let mut hup = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()) {
      Ok(signal) => signal,
      Err(_) => return,
    };
    let code = tokio::select! {
      _ = term.recv() => 143,
      _ = hup.recv() => 129,
    };
    prompts::restore_terminal();
    std::process::exit(code);
  });
}

/// Keep as is: verify every registration locally and, when a bearer can
/// be resolved, live on GitHub. Writes nothing.
async fn keep_as_is(
  facts: &machine::MachineFacts,
  home: &Path,
  args: &SetupArgs,
  masker: &Arc<Mutex<SecretMasker>>,
  out: &Out,
) -> Result<(), SetupError> {
  let client = http_client()?;
  for entry in &facts.existing {
    let cfg = config::config::load_config(&entry.config_path).map_err(|e| {
      SetupError::runtime(format!(
        "could not read {}: {e}",
        entry.config_path.display()
      ))
    })?;
    out.done(
      "registration",
      &format!("{} at {}", cfg.runner_name, cfg.runner_url),
    );
    match verify_bearer(&cfg, home, args, &client, masker).await {
      Some(bearer) => {
        match github::find_runner(&client, &cfg.runner_url, &bearer, &cfg.runner_name).await {
          Ok(Some(runner)) => out.done(
            "online",
            &format!("{} is {} on GitHub", cfg.runner_name, runner.status),
          ),
          Ok(None) => out.warn(
            "online",
            &format!("{} is not listed on GitHub", cfg.runner_name),
          ),
          Err(e) => out.warn(
            "online",
            &format!("could not check {}: {e}", cfg.runner_name),
          ),
        }
      },
      None => out.warn(
        "online",
        &format!(
          "{} live state not verified (no credential available)",
          cfg.runner_name
        ),
      ),
    }
  }
  out.done("keep", "Nothing to change.");
  Ok(())
}

/// Resolve a verify bearer for an existing registration from its
/// `[auth]` section (App → installation token; token → flag/env/store).
async fn verify_bearer(
  cfg: &config::config::RunnerRegistrationConfig,
  home: &Path,
  args: &SetupArgs,
  client: &reqwest::Client,
  masker: &Arc<Mutex<SecretMasker>>,
) -> Option<String> {
  use config::registration_auth::AuthSection;
  match &cfg.auth {
    AuthSection::App {
      app_file,
      installation_id,
    } => {
      let app = crate::app_auth::load_app(Path::new(app_file), masker).ok()?;
      let tokens = crate::app_auth::InstallationTokens::default();
      tokens
        .bearer(client, &app, &cfg.runner_url, *installation_id, masker)
        .await
        .ok()
    },
    AuthSection::Token => {
      let host = plan::host_of(&cfg.runner_url);
      let store = config::auth_store::AuthStore::new(home);
      config::auth_store::resolve_bearer(&store, &host, args.token.clone()).ok()?
    },
  }
}

/// Remove one registration through the existing remove core, with a
/// bearer setup resolved for it (the core does not mint App tokens).
async fn remove_one(
  entry: &config::registry::RegistrationEntry,
  home: &Path,
  masker: &Arc<Mutex<SecretMasker>>,
) -> Result<(), SetupError> {
  let cfg = config::config::load_config(&entry.config_path)
    .map_err(|e| SetupError::runtime(e.to_string()))?;
  let client = http_client()?;
  let token = verify_bearer(&cfg, home, &empty_args(), &client, masker).await;
  crate::cmd_remove(crate::cli::RemoveArgs {
    config: Some(entry.config_path.clone()),
    token,
    force: false,
    skip_unregister: false,
  })
  .await
  .map_err(|e| SetupError::runtime(e.to_string()))
}

/// An empty flag set for bearer resolution helpers.
fn empty_args() -> SetupArgs {
  SetupArgs {
    source: None,
    org: None,
    repo: Vec::new(),
    github_url: None,
    auth: None,
    import: None,
    app_id: None,
    app_key: None,
    token: None,
    token_stdin: false,
    runner_group: None,
    name: None,
    labels: Vec::new(),
    no_default_labels: false,
    work_dir: None,
    ephemeral: false,
    replace: false,
    no_service: false,
    enable_linger: false,
    yes: false,
    dry_run: false,
  }
}

/// Reconfigure: a fresh plan locked to the picked registration, with
/// `--replace` semantics.
async fn reconfigure(
  entry: config::registry::RegistrationEntry,
  args: &SetupArgs,
  github_base: &str,
  host: &str,
  home: &Path,
  prompts: &mut Option<Prompts>,
  out: &Out,
  masker: &Arc<Mutex<SecretMasker>>,
) -> Result<(), SetupError> {
  let cfg = config::config::load_config(&entry.config_path)
    .map_err(|e| SetupError::runtime(e.to_string()))?;
  let mut owned = empty_args();
  owned.replace = true;
  owned.name = Some(cfg.runner_name.clone());
  match &cfg.auth {
    config::registration_auth::AuthSection::App { .. } => owned.auth = Some("app".to_owned()),
    config::registration_auth::AuthSection::Token => owned.auth = Some("token".to_owned()),
  }
  match &entry.scope {
    config::registry::RegistrationScope::Repo { owner, repo } => {
      owned.repo = vec![format!("{owner}/{repo}")];
    },
    config::registry::RegistrationScope::Org(org) => owned.org = Some(org.clone()),
    _ => {},
  }
  build_and_apply(
    &owned_with(args, owned),
    github_base,
    host,
    home,
    None,
    &machine::gather(home).map_err(SetupError::runtime)?,
    prompts,
    out,
    masker,
  )
  .await
}

/// Flags for a reconfigure: the picked registration's name/labels as
/// defaults, the caller's flags winning where set.
fn owned_with(args: &SetupArgs, mut owned: SetupArgs) -> SetupArgs {
  if args.name.is_some() {
    owned.name = args.name.clone();
  }
  if !args.labels.is_empty() {
    owned.labels = args.labels.clone();
  }
  if args.org.is_some() {
    owned.org = args.org.clone();
  }
  if !args.repo.is_empty() {
    owned.repo = args.repo.clone();
  }
  if args.auth.is_some() {
    owned.auth = args.auth.clone();
  }
  if args.token.is_some() {
    owned.token = args.token.clone();
  }
  owned
}

/// The env-mode HTTP client setup's live reads share.
fn http_client() -> Result<reqwest::Client, SetupError> {
  network::client_builder(&shared::NetworkConfig::default())
    .map_err(|e| SetupError::runtime(format!("could not build the HTTP client: {e}")))?
    .build()
    .map_err(|e| SetupError::runtime(format!("could not build the HTTP client: {e}")))
}

/// Build a plan (flags and/or prompts), review it, and apply it.
#[allow(clippy::too_many_arguments)]
async fn build_and_apply(
  args: &SetupArgs,
  github_base: &str,
  host: &str,
  home: &Path,
  _locked: Option<()>,
  facts: &machine::MachineFacts,
  prompts: &mut Option<Prompts>,
  out: &Out,
  masker: &Arc<Mutex<SecretMasker>>,
) -> Result<(), SetupError> {
  let client = http_client()?;
  loop {
    let staged = match stage_plan(
      args,
      github_base,
      host,
      home,
      facts,
      prompts,
      out,
      &client,
      masker,
    )
    .await?
    {
      StageOutcome::Plan(staged) => staged,
      StageOutcome::Kept => return Ok(()),
    };
    out.rail("Review");
    for line in stage::review_lines(&staged.plan, staged.auth_label.as_deref()) {
      out.line(&line);
    }
    if args.dry_run {
      out.end("dry run: nothing was written");
      return Ok(());
    }
    if let Some(prompts) = prompts.as_mut()
      && !args.yes
    {
      let options = vec![
        "Apply".to_owned(),
        "Go back and change the answers".to_owned(),
        "Cancel".to_owned(),
      ];
      match prompts
        .select("Apply this setup?", &options, 0)
        .map_err(SetupError::from)?
      {
        0 => {},
        1 => continue,
        _ => return Err(SetupError::cancelled()),
      }
    }
    out.end("applying");
    let auth = apply::scope_bearer(
      &staged.plan,
      home,
      &client,
      masker,
      staged.token_value.as_deref(),
    )
    .await?;
    apply::persist_import(&staged.plan, home).await?;
    if staged.plan.service {
      out.waiting("online", "waiting for GitHub to report the runner online");
    }
    let applied = match apply::apply_plan(
      &staged.plan,
      home,
      &auth,
      staged.token_value.as_deref(),
      masker,
    )
    .await
    {
      Ok(applied) => applied,
      Err(e) => {
        out.error("apply", &e.message);
        return Err(e);
      },
    };
    for runner in &applied {
      if staged.plan.service {
        out.done("online", &format!("{} is online on GitHub", runner.name));
      } else {
        out.done(
          "service",
          "skipped (--no-service): start it with `toolu-runner run` or `toolu-runner install-service`",
        );
      }
      println!("runs-on: self-hosted, {}", runner.labels.join(", "));
      out.done(
        "registered",
        &format!(
          "{} (id {}) at {}{}",
          runner.name,
          runner.runner_id,
          runner.url,
          if runner.fresh {
            ""
          } else {
            " (already registered)"
          }
        ),
      );
    }
    out.done(
      "next",
      "check it with `toolu-runner status`, `toolu-runner doctor`; remove it with `toolu-runner remove`",
    );
    return Ok(());
  }
}

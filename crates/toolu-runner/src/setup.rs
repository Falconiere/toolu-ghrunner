//! `toolu-runner setup` (#204): interactive and flag-driven onboarding.
//! Machine check → job source → (GitHub) target → auth → scope → runner
//! details → service → Review → apply → online confirmation, or (toolu.sh,
//! #207, [`toolu`]) pairing or enrollment → service → first poll; plus the
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
pub(crate) mod reconfigure;
pub(crate) mod render;
pub(crate) mod scope;
pub(crate) mod stage;
pub(crate) mod toolu;
pub(crate) mod toolu_wait;

use std::io::IsTerminal as _;
use std::path::Path;
use std::sync::{Arc, Mutex};

use config::registry::{self, RegistrationScope};
use shared::SecretMasker;

use crate::run_cmd::CodedExit;

use args::SetupArgs;
use plan::SetupError;
use prompts::Prompts;
use render::Out;
use stage::StageOutcome;
use toolu::Source;

/// The shared flow inputs.
struct Flow<'a> {
  /// The caller's flags.
  args: &'a SetupArgs,
  /// GitHub web base.
  github_base: &'a str,
  /// Registration host.
  host: &'a str,
  /// The runner home.
  home: &'a Path,
  /// Machine-check facts.
  facts: &'a machine::MachineFacts,
  /// The step renderer (Copy).
  out: Out,
  /// The secret masker.
  masker: &'a Arc<Mutex<SecretMasker>>,
}

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
  let source = toolu::flag_source(args)?;
  let github_base = plan::validate_github_url(args.github_url.as_deref())?;
  let host = plan::host_of(&github_base);
  let home = registry::runner_home();
  let facts = machine::gather(&home).map_err(SetupError::runtime)?;
  let interactive = !args.yes() && !args.dry_run() && tty_available();
  let out = Out {
    plain: render::plain_mode(std::io::stdout().is_terminal(), crate::config::no_color()),
  };
  out.done("machine", &facts.summary());
  let mut prompts = if interactive {
    Prompts::open().ok()
  } else {
    None
  };
  let interactive = prompts.is_some();
  // Signals outside a raw-mode prompt (during apply or the online
  // poll) cancel this token; `run_setup` races the flow against it
  // and exits 130 through the normal error path, so destructors run
  // and the terminal is restored by dropping the prompts.
  let cancel = tokio_util::sync::CancellationToken::new();
  if interactive {
    spawn_signal_restore(cancel.clone());
  }

  let flow = Flow {
    args,
    github_base: &github_base,
    host: &host,
    home: &home,
    facts: &facts,
    out,
    masker: &masker,
  };
  // Re-run menu (interactive, configured machine, no explicit target;
  // `--source github` alone still shows it).
  let explicit = args.org.is_some() || !args.repo.is_empty() || source == Some(Source::Toolu);
  let work = async {
    if !facts.existing.is_empty()
      && !explicit
      && let Some(choice) = rerun_choice(&facts, &mut prompts, out)?
    {
      return match choice {
        Rerun::Keep => keep_as_is(&flow).await,
        Rerun::Remove(entry) => remove_one(&entry, &home, &masker).await,
        Rerun::Reconfigure(entry) if matches!(entry.scope, RegistrationScope::Toolu(_)) => {
          toolu::refresh(&flow, &entry, &mut prompts).await
        },
        Rerun::Reconfigure(entry) => reconfigure::reconfigure(entry, &flow, &mut prompts).await,
        Rerun::Add => fresh(&flow, &mut prompts, None).await,
      };
    }
    fresh(&flow, &mut prompts, source).await
  };
  tokio::select! {
    result = work => result,
    () = cancel.cancelled() => Err(SetupError::cancelled()),
  }
}

/// A fresh setup from `source`, asking for it on a terminal when the
/// flags left it open (unattended: GitHub, #204's default).
async fn fresh(
  flow: &Flow<'_>,
  prompts: &mut Option<Prompts>,
  source: Option<Source>,
) -> Result<(), SetupError> {
  let source = match (source, prompts.as_mut()) {
    (Some(source), _) => source,
    (None, Some(prompts)) => {
      let options = vec![
        "GitHub      register with a repo or an organization".to_owned(),
        "toolu.sh    pair with your toolu.sh workspace".to_owned(),
      ];
      match prompts
        .select("Where should this runner get jobs from?", &options, 0)
        .map_err(SetupError::from)?
      {
        1 => Source::Toolu,
        _ => Source::Github,
      }
    },
    (None, None) => Source::Github,
  };
  match source {
    Source::Toolu => toolu::run(flow, prompts).await,
    Source::Github => build_and_apply(flow, prompts).await,
  }
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
  out: Out,
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
      let Some(entry) = facts.existing.get(index).cloned() else {
        return Ok(None);
      };
      Ok(Some(if picked == 2 {
        Rerun::Reconfigure(entry)
      } else {
        Rerun::Remove(entry)
      }))
    },
    _ => Ok(None),
  }
}

/// Whether a `/dev/tty` prompt could be opened at all. A one-time
/// `open(2)` at startup (the controlling terminal is already ours, so
/// it cannot block waiting for a peer); the result only decides
/// interactive vs unattended before any prompt work begins.
fn tty_available() -> bool {
  std::fs::OpenOptions::new()
    .read(true)
    .write(true)
    .open("/dev/tty")
    .is_ok()
}

/// On SIGTERM/SIGHUP/SIGINT: restore the terminal and cancel the
/// flow's token, so setup exits 130 (or the error path's code)
/// gracefully instead of dying mid-write. Inside a raw-mode prompt
/// Ctrl+C already arrives as a byte and cancels the prompt itself;
/// this covers the canonical-mode stretches (apply, online poll).
fn spawn_signal_restore(cancel: tokio_util::sync::CancellationToken) {
  tokio::spawn(async move {
    let Ok(mut term) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    else {
      return;
    };
    let Ok(mut hup) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()) else {
      return;
    };
    let Ok(mut int) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
    else {
      return;
    };
    tokio::select! {
      _ = term.recv() => {},
      _ = hup.recv() => {},
      _ = int.recv() => {},
    }
    prompts::restore_terminal();
    cancel.cancel();
  });
}

/// Keep as is: verify every registration locally and, when a bearer can
/// be resolved, live on GitHub. Writes nothing.
async fn keep_as_is(flow: &Flow<'_>) -> Result<(), SetupError> {
  let (facts, home, args, masker, out) = (flow.facts, flow.home, flow.args, flow.masker, flow.out);
  let client = http_client()?;
  for entry in &facts.existing {
    if matches!(entry.scope, RegistrationScope::Toolu(_)) {
      out.done("registration", &entry.scope.label());
      toolu::keep(flow, entry).await?;
      continue;
    }
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
  // A toolu.sh machine revokes itself with its own host token.
  let token = if cfg.provider.is_github() {
    verify_bearer(&cfg, home, &reconfigure::empty_args(), &client, masker).await
  } else {
    None
  };
  crate::cmd_remove(crate::cli::RemoveArgs {
    config: Some(entry.config_path.clone()),
    token,
    force: false,
    skip_unregister: false,
  })
  .await
  .map_err(|e| SetupError::runtime(e.to_string()))
}

/// Build a plan (flags and/or prompts), review it, and apply it.
async fn build_and_apply(flow: &Flow<'_>, prompts: &mut Option<Prompts>) -> Result<(), SetupError> {
  let (args, github_base, host, home, facts, out, masker) = (
    flow.args,
    flow.github_base,
    flow.host,
    flow.home,
    flow.facts,
    flow.out,
    flow.masker,
  );
  let _ = host;
  let client = http_client()?;
  loop {
    let stage_flow = stage::StageFlow {
      args,
      github_base,
      host,
      home,
      facts,
      out,
      client: &client,
      masker,
    };
    let staged = match stage::stage_plan(&stage_flow, prompts).await? {
      StageOutcome::Plan(staged) => *staged,
      StageOutcome::Kept => return Ok(()),
    };
    out.rail("Review");
    for line in stage::review_lines(&staged.plan, staged.auth_label.as_deref()) {
      out.line(&line);
    }
    if args.dry_run() {
      out.end("dry run: nothing was written");
      return Ok(());
    }
    if let Some(prompts) = prompts.as_mut()
      && !args.yes()
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
    if staged.plan.service.install {
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
      if staged.plan.service.install {
        out.done("online", &format!("{} is online on GitHub", runner.name));
      } else {
        out.done(
          "service",
          "skipped (--no-service): start it with `toolu-runner run` or `toolu-runner install-service`",
        );
      }
      println!("runs-on: {}", runner.labels.join(", "));
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

/// The env-mode HTTP client setup's live reads share.
pub(crate) fn http_client() -> Result<reqwest::Client, SetupError> {
  network::client_builder(&shared::NetworkConfig::default())
    .map_err(|e| SetupError::runtime(format!("could not build the HTTP client: {e}")))?
    .build()
    .map_err(|e| SetupError::runtime(format!("could not build the HTTP client: {e}")))
}

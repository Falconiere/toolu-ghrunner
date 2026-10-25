//! Reconfigure (#204): a fresh plan locked to a picked existing
//! registration, derived from that registration's own config (its
//! GitHub base, labels, work dir, ephemeral flag, runner group and
//! auth kind), with the caller's flags winning where set.

use super::args::SetupArgs;
use super::plan::SetupError;
use super::prompts::Prompts;
use super::{Flow, build_and_apply, machine, plan};

/// An empty flag set for Bearer [REDACTED] helpers.
pub(super) fn empty_args() -> SetupArgs {
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
    mode: crate::setup::args::SetupMode {
      ephemeral: false,
      replace: false,
      no_service: false,
    },
    flow: crate::setup::args::SetupFlow {
      enable_linger: false,
      yes: false,
      dry_run: false,
    },
  }
}

/// Reconfigure: a fresh plan locked to the picked registration, with
/// `--replace` semantics.
pub(super) async fn reconfigure(
  entry: config::registry::RegistrationEntry,
  flow: &Flow<'_>,
  prompts: &mut Option<Prompts>,
) -> Result<(), SetupError> {
  let (args, home, out, masker) = (flow.args, flow.home, flow.out, flow.masker);
  let cfg = config::config::load_config(&entry.config_path)
    .map_err(|e| SetupError::runtime(e.to_string()))?;
  let mut owned = empty_args();
  owned.mode.replace = true;
  owned.name = Some(cfg.runner_name.clone());
  // The picked registration's own settings are the defaults: its
  // labels verbatim (no defaults re-added), work dir and ephemeral.
  owned.labels.clone_from(&cfg.labels);
  owned.no_default_labels = true;
  owned.work_dir = Some(std::path::PathBuf::from(&cfg.runtime.work_dir));
  owned.mode.ephemeral = cfg.ephemeral;
  // Its runner group too: an org runner in a non-Default group must
  // not be silently moved back to Default.
  if !cfg.runner_group.is_empty() {
    owned.runner_group = Some(cfg.runner_group.clone());
  }
  // Its GitHub base comes from its own registration URL, never from
  // this invocation's --github-url (a GHES registration stays on GHES).
  let base: Option<String> = match &entry.scope {
    config::registry::RegistrationScope::Repo { owner, repo } => {
      let suffix = format!("/{owner}/{repo}");
      cfg.runner_url.strip_suffix(&suffix).map(str::to_owned)
    },
    config::registry::RegistrationScope::Org(org) => {
      let suffix = format!("/{org}");
      cfg.runner_url.strip_suffix(&suffix).map(str::to_owned)
    },
    config::registry::RegistrationScope::Toolu(_) | config::registry::RegistrationScope::Legacy => {
      None
    },
  };
  let Some(base) = base else {
    return Err(SetupError::usage(
      "setup cannot reconfigure a toolu.sh or legacy registration; use `toolu-runner remove` and register again",
    ));
  };
  owned.github_url = Some(base);
  match &cfg.auth {
    config::registration_auth::AuthSection::App { .. } => owned.auth = Some("app".to_owned()),
    config::registration_auth::AuthSection::Token => owned.auth = Some("token".to_owned()),
  }
  match &entry.scope {
    config::registry::RegistrationScope::Repo { owner, repo } => {
      owned.repo = vec![format!("{owner}/{repo}")];
    },
    config::registry::RegistrationScope::Org(org) => owned.org = Some(org.clone()),
    config::registry::RegistrationScope::Toolu(_) | config::registry::RegistrationScope::Legacy => {
    },
  }
  let owned = owned_with(args, owned);
  let derived_base = owned
    .github_url
    .clone()
    .unwrap_or_else(|| flow.github_base.to_owned());
  let derived_host = plan::host_of(&derived_base);
  let facts = machine::gather(home).map_err(SetupError::runtime)?;
  let inner = Flow {
    args: &owned,
    github_base: &derived_base,
    host: &derived_host,
    home,
    facts: &facts,
    out,
    masker,
  };
  build_and_apply(&inner, prompts).await
}

/// Flags for a reconfigure: the picked registration's name/labels as
/// defaults, the caller's flags winning where set.
fn owned_with(args: &SetupArgs, mut owned: SetupArgs) -> SetupArgs {
  if args.name.is_some() {
    owned.name.clone_from(&args.name);
  }
  if !args.labels.is_empty() {
    owned.labels.clone_from(&args.labels);
    // Caller-supplied labels follow normal `--labels` semantics
    // (additive unless the caller also said --no-default-labels).
    owned.no_default_labels = args.no_default_labels;
  }
  // Every other caller flag survives the menu too: dropping
  // --no-service / --enable-linger / --work-dir / import flags here
  // would make Reconfigure do something the caller never asked for.
  if args.runner_group.is_some() {
    owned.runner_group.clone_from(&args.runner_group);
  }
  if args.work_dir.is_some() {
    owned.work_dir.clone_from(&args.work_dir);
  }
  if args.import.is_some() {
    owned.import.clone_from(&args.import);
  }
  if args.app_id.is_some() {
    owned.app_id = args.app_id;
  }
  if args.app_key.is_some() {
    owned.app_key.clone_from(&args.app_key);
  }
  owned.token_stdin = owned.token_stdin || args.token_stdin;
  owned.mode.no_service = owned.mode.no_service || args.mode.no_service;
  owned.mode.ephemeral = owned.mode.ephemeral || args.mode.ephemeral;
  owned.flow.enable_linger = owned.flow.enable_linger || args.flow.enable_linger;
  owned.flow.yes = owned.flow.yes || args.flow.yes;
  owned.flow.dry_run = owned.flow.dry_run || args.flow.dry_run;
  if args.org.is_some() {
    owned.org.clone_from(&args.org);
  }
  if !args.repo.is_empty() {
    owned.repo.clone_from(&args.repo);
  }
  if args.auth.is_some() {
    owned.auth.clone_from(&args.auth);
  }
  if args.token.is_some() {
    owned.token.clone_from(&args.token);
  }
  owned
}

//! Plan staging for `setup`: target → auth → scope → runner details →
//! service, producing the one [`ResolvedPlan`] the Review shows and
//! apply executes. Reads only; the one write is a GitHub App created
//! interactively (disclosed on the Review).

use std::io::Read as _;
use std::path::Path;
use std::sync::{Arc, Mutex};

use shared::SecretMasker;

use super::apply::{self, AuthContext};
use super::args::SetupArgs;
use super::github;
use super::machine::MachineFacts;
use super::plan::{self, AuthPlan, ResolvedPlan, SetupError, Target};
use super::prompts::Prompts;
use super::render::Out;
use super::scope;

/// What staging produced: a plan to review, or a Keep-as-is verdict.
pub(crate) enum StageOutcome {
  /// Review and apply this plan.
  Plan(Box<Staged>),
  /// An identical registration already covers the target.
  Kept,
}

/// A plan plus the in-memory secrets and labels the stages resolved.
pub(crate) struct Staged {
  pub(crate) plan: ResolvedPlan,
  pub(crate) token_value: Option<String>,
  pub(crate) auth_label: Option<String>,
}

/// The shared staging inputs.
pub(crate) struct StageFlow<'a> {
  /// The caller's flags.
  pub(crate) args: &'a SetupArgs,
  /// GitHub web base.
  pub(crate) github_base: &'a str,
  /// Registration host.
  pub(crate) host: &'a str,
  /// The runner home.
  pub(crate) home: &'a Path,
  /// Machine-check facts.
  pub(crate) facts: &'a MachineFacts,
  /// The step renderer.
  pub(crate) out: Out,
  /// The HTTP client for live reads.
  pub(crate) client: &'a reqwest::Client,
  /// The secret masker.
  pub(crate) masker: &'a Arc<Mutex<SecretMasker>>,
}

/// The staged plan building: target → auth → scope → details → service.
/// Reads only; the one write is a GitHub App created interactively
/// (disclosed on the Review).
pub(crate) async fn stage_plan(
  flow: &StageFlow<'_>,
  prompts: &mut Option<Prompts>,
) -> Result<StageOutcome, SetupError> {
  let (args, github_base, host, home, facts, out, client) = (
    flow.args,
    flow.github_base,
    flow.host,
    flow.home,
    flow.facts,
    flow.out,
    flow.client,
  );
  let _ = flow.host;
  let interactive = prompts.is_some();
  let _ = interactive;
  // Auth grammar and the GHES rule are flag validation, decided
  // before any network call; the org's live check still precedes
  // asking for any credential.
  let saved_early = saved_app_hint(home);
  let auth_early = plan::auth_plan(args, saved_early)?;
  if host != "github.com"
    && !plan::is_loopback(host)
    && matches!(
      auth_early,
      AuthPlan::CreateApp | AuthPlan::Import { .. } | AuthPlan::SavedApp
    )
  {
    return Err(SetupError::usage(
      "GitHub Apps are github.com-only in setup: re-run with --auth token for a GHES host",
    ));
  }
  // Target (org checked live before any credential is asked for).
  let mut target = resolve_target(args, github_base, prompts, out, client).await?;
  // Existing-target rule: identical plan = keep; different = --replace.
  if let Some(done) = existing_match(&target, github_base, home, args)? {
    out.done("registration", &done);
    out.done("keep", "Nothing to change.");
    return Ok(StageOutcome::Kept);
  }
  if prompts.is_none()
    && matches!(
      plan::auth_plan(args, saved_app_hint(home))?,
      AuthPlan::CreateApp
    )
  {
    return Err(SetupError::usage(
      "creating a GitHub App needs a browser: re-run interactively, or use --auth import / --auth token",
    ));
  }
  let saved_app = config::app_store::app_path(home).exists();
  let mut auth = plan::auth_plan(args, saved_app)?;
  // Headless + create: steer to import/token.
  if facts.headless && matches!(auth, AuthPlan::CreateApp) {
    if interactive {
      out.warn(
        "auth",
        "this machine looks headless, and creating a GitHub App here needs a browser (#205 covers SSH); use an existing App or a token",
      );
      let options = vec![
        "Use an existing App (--import)".to_owned(),
        "Paste a personal access token".to_owned(),
        "Cancel".to_owned(),
      ];
      match prompts
        .as_mut()
        .map(|p| p.select("How should setup authenticate?", &options, 1))
        .transpose()
        .map_err(SetupError::from)?
      {
        Some(0) => {
          return Err(SetupError::usage(
            "re-run with --auth import --import <file> (the App to reuse)",
          ));
        },
        Some(1) => auth = AuthPlan::Token,
        _ => return Err(SetupError::cancelled()),
      }
    } else {
      return Err(SetupError::usage(
        "creating a GitHub App needs a browser this machine does not have: use --auth import or --auth token",
      ));
    }
  }
  // Authenticate (in memory), creating the App interactively if asked.
  let (auth_ctx, token_value) = authenticate(flow, &mut auth, &target, prompts).await?;
  // Scope: runner group (org) or repositories (personal).
  let scope_flow = scope::ScopeFlow {
    github_base,
    home,
    args,
    out,
    client,
  };
  let (runner_group, repos) =
    scope::resolve_scope(&scope_flow, &mut target, &auth_ctx, prompts).await?;
  let _ = repos;
  // The picker may have chosen the repos only now: probe them before
  // any registration is attempted (the exit-2 admin gate).
  if matches!(auth, AuthPlan::Token)
    && let Target::Repos { repos: chosen } = &target
    && !chosen.is_empty()
  {
    let first_url = target.first_url(github_base)?;
    let base = github::api_base(&first_url)?;
    github::probe_token(client, &base, &auth_ctx.bearer, None, chosen).await?;
  }
  // Details.
  let name = scope::resolve_name(args, prompts)?;
  let confirmed_replace =
    scope::check_collision(&scope_flow, &target, &name, &auth_ctx, prompts).await?;
  let labels_preview = config::labels::resolve_labels(
    &args.labels,
    args.no_default_labels,
    &config::labels::default_labels(),
  )
  .map_err(|e| SetupError::usage(e.to_string()))?;
  let _ = labels_preview;
  let service = !args.mode.no_service;
  let enable_linger = scope::resolve_linger(args, prompts, facts, service)?;
  Ok(StageOutcome::Plan(Box::new(Staged {
    plan: ResolvedPlan {
      github_base: github_base.to_owned(),
      host: host.to_owned(),
      target,
      auth,
      runner_group,
      name,
      labels: args.labels.clone(),
      work_folder: crate::cli::work_folder_or_default(args.work_dir.as_ref()),
      mode: super::plan::PlanMode {
        ephemeral: args.mode.ephemeral,
        replace: args.mode.replace || confirmed_replace,
        no_default_labels: args.no_default_labels,
      },
      service: super::plan::PlanService {
        install: service,
        enable_linger,
      },
    },
    token_value,
    auth_label: auth_ctx.label.clone(),
  })))
}

/// Whether a saved App exists under `home`.
fn saved_app_hint(home: &Path) -> bool {
  config::app_store::app_path(home).exists()
}

/// The target from flags or prompts, with the org checked live first.
async fn resolve_target(
  args: &SetupArgs,
  github_base: &str,
  prompts: &mut Option<Prompts>,
  out: Out,
  client: &reqwest::Client,
) -> Result<Target, SetupError> {
  if let Some(target) = plan::flag_target(args)? {
    if let Target::Org { org } = &target {
      check_org_live(org, github_base, client).await?;
    }
    return Ok(target);
  }
  let Some(prompts) = prompts.as_mut() else {
    return Err(SetupError::usage(
      "no target given: pass --org <name> or --repo <owner/name>",
    ));
  };
  let options = vec!["An organization".to_owned(), "My repositories".to_owned()];
  let kind = prompts
    .select("Where should the runners register?", &options, 0)
    .map_err(SetupError::from)?;
  if kind == 0 {
    loop {
      let org = prompts
        .input("Organization name", "")
        .map_err(SetupError::from)?;
      if org.trim().is_empty() {
        continue;
      }
      match check_org_live(org.trim(), github_base, client).await {
        Ok(()) => {
          return Ok(Target::Org {
            org: org.trim().to_owned(),
          });
        },
        Err(e) if e.code == 2 => out.warn("target", &e.message),
        Err(e) => return Err(e),
      }
    }
  }
  Ok(Target::Repos { repos: Vec::new() })
}

/// Live org check: 404 warns (interactive) or exits 2 (unattended).
async fn check_org_live(
  org: &str,
  github_base: &str,
  client: &reqwest::Client,
) -> Result<(), SetupError> {
  let url = format!("{github_base}/{org}");
  let base = github::api_base(&url)?;
  match github::check_org(client, &base, org).await? {
    github::OrgCheck::Found(_) => Ok(()),
    github::OrgCheck::Missing => Err(SetupError::usage(format!(
      "no organization \"{org}\" on GitHub — check the spelling (pass --org <name>)"
    ))),
  }
}

/// Whether an identical registration already covers this target; the
/// caller then reports Keep and exits 0.
fn existing_match(
  target: &Target,
  github_base: &str,
  home: &Path,
  args: &SetupArgs,
) -> Result<Option<String>, SetupError> {
  if args.mode.replace {
    return Ok(None);
  }
  let wanted_name = crate::cli::runner_name_or_hostname(args.name.clone());
  let wanted_labels = config::labels::resolve_labels(
    &args.labels,
    args.no_default_labels,
    &config::labels::default_labels(),
  )
  .map_err(|e| SetupError::usage(e.to_string()))?;
  let wanted_work = crate::cli::work_folder_or_default(args.work_dir.as_ref());
  let mut matched: Vec<String> = Vec::new();
  let mut any_existing = false;
  for url in target.urls(github_base) {
    let config_path = crate::register_cmd::register_config_path(&url, home)
      .map_err(|e| SetupError::usage(e.to_string()))?;
    if config_path.exists() {
      any_existing = true;
      let cfg = config::config::load_config(&config_path)
        .map_err(|e| SetupError::runtime(e.to_string()))?;
      if cfg.runner_name != wanted_name
        || cfg.labels != wanted_labels
        || cfg.ephemeral != args.mode.ephemeral
        || cfg.runtime.work_dir != wanted_work
      {
        return Err(SetupError::usage(format!(
          "runner '{}' is already registered for {url} with different settings — pass --replace to reconfigure it",
          cfg.runner_name
        )));
      }
      matched.push(format!(
        "{} already registered at {}",
        cfg.runner_name, cfg.runner_url
      ));
    }
  }
  if any_existing && matched.len() == target.urls(github_base).len() {
    // A service that never landed (apply failed after registering) is
    // not "keep as is": fall through so apply installs it and polls.
    if !args.mode.no_service && !machine_unit_present() {
      return Ok(None);
    }
    return Ok(Some(matched.join("; ")));
  }
  Ok(None)
}

/// Whether the machine unit file exists at its expected destination.
fn machine_unit_present() -> bool {
  let Ok(supervisor) = crate::service_cmd::current_supervisor() else {
    return true;
  };
  let Ok(user_home) = crate::service_cmd::home_dir() else {
    return true;
  };
  let id = crate::machine_service::machine_id();
  crate::service_cmd::dest_path(supervisor, &id, &user_home).exists()
    || crate::service_cmd::dest_path_scoped(
      supervisor,
      crate::machine_service::UnitScope::System,
      &id,
      &user_home,
    )
    .exists()
}

/// Authenticate for the scope reads: resolves the bearer in memory and
/// (token auth) probes the token's real permissions.
async fn authenticate(
  flow: &StageFlow<'_>,
  auth: &mut AuthPlan,
  target: &Target,
  prompts: &mut Option<Prompts>,
) -> Result<(AuthContext, Option<String>), SetupError> {
  let (args, github_base, home, out, client, masker) = (
    flow.args,
    flow.github_base,
    flow.home,
    flow.out,
    flow.client,
    flow.masker,
  );
  if matches!(auth, AuthPlan::CreateApp) {
    // Interactive only (headless/unattended were steered away above):
    // create through the existing create-app flow. The App is saved at
    // creation — the disclosed Review exception.
    let org = match target {
      Target::Org { org } => Some(org.clone()),
      Target::Repos { .. } => None,
    };
    let create_args = crate::cli::CreateAppArgs {
      name: None,
      org,
      account: None,
      import: None,
      app_id: None,
      app_key: None,
      host: "github.com".to_owned(),
      no_browser: false,
      force: false,
    };
    match crate::create_app_cmd::cmd_create_app(&create_args, home).await {
      Ok(()) => {
        *auth = AuthPlan::SavedApp;
        out.done("auth", "GitHub App created and saved (0600)");
      },
      Err(e) => {
        if prompts.is_some() {
          out.warn("auth", &format!("GitHub did not create the App: {e}"));
          let options = vec![
            "Paste a personal access token instead".to_owned(),
            "Cancel".to_owned(),
          ];
          let picked = prompts
            .as_mut()
            .map(|p| p.select("Use a token instead?", &options, 0))
            .transpose()
            .map_err(SetupError::from)?;
          if picked == Some(0) {
            *auth = AuthPlan::Token;
          } else {
            return Err(SetupError::runtime(e.to_string()));
          }
        } else {
          return Err(SetupError::usage(format!(
            "{e} — if GitHub refused the App (only org owners can create one), re-run with --auth token"
          )));
        }
      },
    }
  }
  let token_value = if matches!(auth, AuthPlan::Token) {
    Some(token_from(args, prompts, out)?)
  } else {
    None
  };
  let staged_plan = ResolvedPlan {
    github_base: github_base.to_owned(),
    host: plan::host_of(github_base),
    target: target.clone(),
    auth: auth.clone(),
    runner_group: None,
    name: String::new(),
    labels: Vec::new(),
    work_folder: String::new(),
    mode: super::plan::PlanMode {
      ephemeral: false,
      replace: false,
      no_default_labels: false,
    },
    service: super::plan::PlanService {
      install: false,
      enable_linger: false,
    },
  };
  let ctx = apply::scope_bearer(&staged_plan, home, client, masker, token_value.as_deref()).await?;
  if matches!(auth, AuthPlan::Token) {
    let first_url = target.first_url(github_base)?;
    let base = github::api_base(&first_url)?;
    let (org, repos) = match target {
      Target::Org { org } => (Some(org.as_str()), Vec::new()),
      Target::Repos { repos } => (None, repos.clone()),
    };
    github::probe_token(client, &base, &ctx.bearer, org, &repos).await?;
    out.done("auth", "token accepted (permissions checked live)");
  } else if let Some(label) = &ctx.label {
    out.done("auth", label);
  }
  Ok((ctx, token_value))
}

/// The token value: flag (warned), stdin, env, or a no-echo prompt.
fn token_from(
  args: &SetupArgs,
  prompts: &mut Option<Prompts>,
  out: Out,
) -> Result<String, SetupError> {
  if let Some(token) = &args.token {
    out.warn(
      "auth",
      "--token values can land in shell history; prefer TOOLU_RUNNER_TOKEN or --token-stdin",
    );
    return Ok(token.clone());
  }
  if args.token_stdin {
    let mut text = String::new();
    std::io::stdin()
      .read_to_string(&mut text)
      .map_err(|e| SetupError::usage(format!("could not read the token from stdin: {e}")))?;
    let token = text.trim().to_owned();
    if token.is_empty() {
      return Err(SetupError::usage("--token-stdin read an empty token"));
    }
    return Ok(token);
  }
  if let Some(token) = config::config::runner_token().filter(|t| !t.is_empty()) {
    return Ok(token);
  }
  let Some(prompts) = prompts.as_mut() else {
    return Err(SetupError::usage(
      "no token available: pass --token, --token-stdin, or set TOOLU_RUNNER_TOKEN",
    ));
  };
  prompts
    .secret("Paste a personal access token (input hidden)")
    .map_err(SetupError::from)
}

/// The Review lines (screen 12): one summary, nothing hidden.
pub(crate) fn review_lines(plan: &ResolvedPlan, auth_label: Option<&str>) -> Vec<String> {
  let auth = match (&plan.auth, auth_label) {
    (_, Some(label)) => label.to_owned(),
    (AuthPlan::Token, _) => "personal access token".to_owned(),
    (AuthPlan::CreateApp, _) => {
      "create a GitHub App (saved 0600 as soon as GitHub returns it)".to_owned()
    },
    (AuthPlan::Import { .. }, _) => "existing GitHub App (import)".to_owned(),
    (AuthPlan::SavedApp, _) => "saved GitHub App".to_owned(),
  };
  let labels = config::labels::resolve_labels(
    &plan.labels,
    plan.mode.no_default_labels,
    &config::labels::default_labels(),
  )
  .unwrap_or_else(|_| plan.labels.clone());
  vec![
    format!("source: GitHub ({})", plan.github_base),
    format!("target: {}", plan.target.label()),
    format!("auth: {auth}"),
    format!(
      "runner: {} · labels {} · work dir {}",
      plan.name,
      labels.join(", "),
      plan.work_folder
    ),
    format!(
      "service: {} · mode: {}",
      if plan.service.install {
        "machine service (run --all)"
      } else {
        "none (--no-service)"
      },
      if plan.mode.ephemeral {
        "ephemeral (one job)"
      } else {
        "persistent"
      }
    ),
    format!(
      "runner group: {} · linger: {}",
      plan.runner_group.as_deref().unwrap_or("Default"),
      if plan.service.enable_linger {
        "on"
      } else {
        "off"
      }
    ),
  ]
}

//! Apply: the single write path. Auth persistence, registrations via
//! `register_cmd::register_and_persist`, the machine unit via
//! `machine_service`, and the online poll. Every step first checks
//! current state, so re-running setup after a failure resumes instead
//! of duplicating.

use std::path::Path;
use std::sync::{Arc, Mutex};

use config::auth_store::{AuthStore, StoredToken};
use config::registration_auth::AuthSection;
use shared::SecretMasker;

use super::github;
use super::plan::{AuthPlan, ResolvedPlan, SetupError};

/// The bearer apply and verify use, plus the `[auth]` to persist.
pub(crate) struct AuthContext {
  /// REST bearer (personal token or App installation token).
  pub(crate) bearer: String,
  /// The persisted `[auth]` section.
  pub(crate) section: AuthSection,
  /// `GitHub App '<slug>' (installation <id>)` for App auth.
  pub(crate) label: Option<String>,
  /// Whether the bearer is an App installation token (repo lists use
  /// `/installation/repositories`).
  pub(crate) app_installation: bool,
}

/// One registered (or already-registered) runner.
pub(crate) struct Applied {
  /// Registration URL.
  pub(crate) url: String,
  /// Runner name.
  pub(crate) name: String,
  /// Labels as registered.
  pub(crate) labels: Vec<String>,
  /// GitHub runner id.
  pub(crate) runner_id: i64,
  /// Whether this run minted it (vs found it already registered).
  pub(crate) fresh: bool,
}

/// Resolve the scope/apply bearer for `plan` without writing anything:
/// a token value, the saved App, or an imported App loaded in memory.
///
/// # Errors
///
/// [`SetupError`] code 2 naming the missing credential source.
pub(crate) async fn scope_bearer(
  plan: &ResolvedPlan,
  home: &Path,
  client: &reqwest::Client,
  masker: &Mutex<SecretMasker>,
  token_value: Option<&str>,
) -> Result<AuthContext, SetupError> {
  let first_url = plan.target.first_url(&plan.github_base)?;
  match &plan.auth {
    AuthPlan::Token => {
      let token = token_value.ok_or_else(|| {
        SetupError::usage(
          "no token available: pass --token, --token-stdin, or set TOOLU_RUNNER_TOKEN",
        )
      })?;
      Ok(AuthContext {
        bearer: token.to_owned(),
        section: AuthSection::Token,
        label: None,
        app_installation: false,
      })
    },
    AuthPlan::SavedApp | AuthPlan::CreateApp => {
      let path = config::app_store::app_path(home);
      let app =
        crate::app_auth::load_app(&path, masker).map_err(|e| SetupError::usage(e.to_string()))?;
      app_context(client, &app, &path, &first_url, masker).await
    },
    AuthPlan::Import { file, app_id, key } => {
      let app = imported_app(file.as_deref(), *app_id, key.as_deref()).await?;
      let dest = config::app_store::app_path(home);
      if dest.exists()
        && let Ok(saved) = config::app_store::load_app(home)
        && let Some(saved) = saved
        && saved.app_id != app.app_id
      {
        return Err(SetupError::usage(format!(
          "a different GitHub App ({}) is already saved at {}; the import names App {} — remove the saved App or re-run `create-app --force` first",
          saved.slug,
          dest.display(),
          app.app_id
        )));
      }
      app_context(client, &app, &dest, &first_url, masker).await
    },
  }
}

/// The in-memory App from `--import` / `--app-id` + `--app-key`
/// (mirrors `app_import::source_app`; nothing is saved here).
async fn imported_app(
  file: Option<&Path>,
  app_id: Option<i64>,
  key: Option<&Path>,
) -> Result<config::app_store::StoredApp, SetupError> {
  if let Some(file) = file {
    return config::app_store::load_app_file(file).map_err(|e| SetupError::usage(e.to_string()));
  }
  let (Some(app_id), Some(key)) = (app_id, key) else {
    return Err(SetupError::usage(
      "--auth import needs --import <file>, or --app-id <id> with --app-key <pem>",
    ));
  };
  let pem = tokio::fs::read_to_string(key)
    .await
    .map_err(|e| SetupError::usage(format!("could not read the App key {}: {e}", key.display())))?;
  Ok(config::app_store::StoredApp {
    host: "github.com".to_owned(),
    app_id,
    slug: String::new(),
    owner: String::new(),
    client_id: String::new(),
    client_secret: String::new(),
    webhook_secret: None,
    pem,
    html_url: String::new(),
    created_at: chrono::Utc::now().to_rfc3339(),
    owner_id: None,
    installation_id: None,
  })
}

/// Mint the installation bearer for `app` against `url`.
async fn app_context(
  client: &reqwest::Client,
  app: &config::app_store::StoredApp,
  app_file: &Path,
  url: &str,
  masker: &Mutex<SecretMasker>,
) -> Result<AuthContext, SetupError> {
  let registration = crate::app_auth::register_installation(client, app, url, masker)
    .await
    .map_err(|e| SetupError::usage(e.to_string()))?;
  let absolute = std::path::absolute(app_file).map_err(|e| {
    SetupError::usage(format!(
      "cannot record the GitHub App file '{}' as an absolute path: {e}",
      app_file.display()
    ))
  })?;
  Ok(AuthContext {
    bearer: registration.token,
    section: AuthSection::App {
      app_file: absolute.to_string_lossy().into_owned(),
      installation_id: registration.installation_id,
    },
    label: Some(format!(
      "GitHub App '{}' (installation {})",
      app.slug, registration.installation_id
    )),
    app_installation: true,
  })
}

/// Persist an imported App at Apply time through the existing
/// `create-app` entry (re-proves the key, saves 0600, records the
/// installation). No-op for other auth plans.
///
/// # Errors
///
/// The `create-app` failure, with its exit code preserved.
pub(crate) async fn persist_import(plan: &ResolvedPlan, home: &Path) -> Result<(), SetupError> {
  let AuthPlan::Import { file, app_id, key } = &plan.auth else {
    return Ok(());
  };
  // Already saved as this same App (a resumed run): persisting again
  // would fail in create-app's existing-file guard for no reason.
  let wanted_id = match (file, app_id) {
    (Some(file), _) => config::app_store::load_app_file(file)
      .map(|app| app.app_id)
      .unwrap_or_default(),
    (None, Some(app_id)) => *app_id,
    (None, None) => 0,
  };
  if wanted_id != 0
    && let Ok(Some(saved)) = config::app_store::load_app(home)
    && saved.app_id == wanted_id
  {
    return Ok(());
  }
  let org = match &plan.target {
    super::plan::Target::Org { org } => Some(org.clone()),
    super::plan::Target::Repos { .. } => None,
  };
  let args = crate::cli::CreateAppArgs {
    name: None,
    org,
    account: None,
    import: file.clone(),
    app_id: *app_id,
    app_key: key.clone(),
    host: "github.com".to_owned(),
    no_browser: true,
    force: false,
    session: crate::cli::HeadlessFlags::default(),
    expected_owner: None,
  };
  crate::create_app_cmd::cmd_create_app(&args, home)
    .await
    .map_err(|e| {
      let code = e
        .downcast_ref::<crate::run_cmd::CodedExit>()
        .map_or(2, |coded| coded.code);
      SetupError {
        code,
        message: e.to_string(),
      }
    })
}

/// The online-poll client, built from a registration's own
/// `[network]` (a proxied/CA-pinned registration polls the same way).
fn poll_client(plan: &ResolvedPlan, home: &Path) -> Result<reqwest::Client, SetupError> {
  let Some(url) = plan.target.urls(&plan.github_base).into_iter().next() else {
    return crate::register_cmd::build_register_client(&shared::NetworkConfig::default())
      .map_err(|e| SetupError::runtime(e.to_string()));
  };
  let config_path = crate::register_cmd::register_config_path(&url, home)
    .map_err(|e| SetupError::usage(e.to_string()))?;
  let network = match config::config::load_config(&config_path) {
    Ok(cfg) => cfg.network.resolve(),
    Err(e) => {
      tracing::warn!(%e, path = %config_path.display(), "could not read registration config for the online poll; using env network mode");
      shared::NetworkConfig::default()
    },
  };
  crate::register_cmd::build_register_client(&network)
    .map_err(|e| SetupError::runtime(e.to_string()))
}

/// Store a personal token in the existing 0600 token store.
fn store_token(home: &Path, host: &str, token: &str) -> Result<(), SetupError> {
  AuthStore::new(home)
    .save(&StoredToken {
      access_token: token.to_owned(),
      scope: String::new(),
      host: host.to_owned(),
      issued_at: chrono::Utc::now().to_rfc3339(),
    })
    .map_err(|e| SetupError::runtime(format!("could not store the token: {e}")))
}

/// Apply `plan`: store the token (token auth), register every target,
/// install the machine unit, and poll until GitHub reports each runner
/// online (skipped without the service).
///
/// # Errors
///
/// The first failing step's [`SetupError`]; completed registrations
/// stay, and re-running setup resumes from the failed step.
pub(crate) async fn apply_plan(
  plan: &ResolvedPlan,
  home: &Path,
  auth: &AuthContext,
  token_value: Option<&str>,
  masker: &Arc<Mutex<SecretMasker>>,
) -> Result<Vec<Applied>, SetupError> {
  if let AuthPlan::Token = plan.auth
    && let Some(token) = token_value
  {
    store_token(home, &plan.host, token)?;
  }
  let labels = config::labels::resolve_labels(
    &plan.labels,
    plan.mode.no_default_labels,
    &config::labels::default_labels(),
  )
  .map_err(|e| SetupError::usage(e.to_string()))?;
  let mut applied = Vec::new();
  for url in plan.target.urls(&plan.github_base) {
    applied.push(register_one(plan, home, auth, &labels, &url, masker).await?);
  }
  if plan.service.install {
    install_machine_service(plan.service.enable_linger).await?;
    let client = poll_client(plan, home)?;
    for runner in &applied {
      let summary = github::poll_online(
        &client,
        &runner.url,
        &auth.bearer,
        &runner.name,
        crate::config::online_timeout(),
      )
      .await?;
      tracing::info!(runner = %runner.name, status = %summary.status, "runner online");
    }
  }
  Ok(applied)
}

/// Register one target, or report the identical existing registration
/// as already done (the resume path).
async fn register_one(
  plan: &ResolvedPlan,
  home: &Path,
  auth: &AuthContext,
  labels: &[String],
  url: &str,
  masker: &Arc<Mutex<SecretMasker>>,
) -> Result<Applied, SetupError> {
  let config_path = crate::register_cmd::register_config_path(url, home)
    .map_err(|e| SetupError::usage(e.to_string()))?;
  let creds_path = crate::cli::credentials_path_for(&config_path);
  if config_path.exists() && !plan.mode.replace {
    let existing = config::config::load_config(&config_path)
      .map_err(|e| SetupError::runtime(format!("could not read {}: {e}", config_path.display())))?;
    if existing.runner_name == plan.name {
      return Ok(Applied {
        url: url.to_owned(),
        name: plan.name.clone(),
        labels: existing.labels.clone(),
        runner_id: existing.runner_id,
        fresh: false,
      });
    }
    return Err(SetupError::usage(format!(
      "a registration already exists at {} for runner '{}' — pass --replace to replace it",
      config_path.display(),
      existing.runner_name
    )));
  }
  let replacing = config_path.exists();
  let network = crate::register_cmd::replaced_network(&config_path, true);
  let client = crate::register_cmd::build_register_client(&network.resolve())
    .map_err(|e| SetupError::runtime(e.to_string()))?;
  let runner_id = crate::register_cmd::register_and_persist(
    crate::register_cmd::RegisterPersist {
      url,
      token: &auth.bearer,
      runner_name: &plan.name,
      labels,
      runner_group: plan.runner_group.as_deref().unwrap_or("Default"),
      work_folder: &plan.work_folder,
      host: &plan.host,
      config_path: &config_path,
      creds_path: &creds_path,
      replace: plan.mode.replace || replacing,
      ephemeral: plan.mode.ephemeral,
      network,
      auth: auth.section.clone(),
      hint_org_404: false,
    },
    &client,
  )
  .await
  .map_err(|e| SetupError::runtime(crate::app_auth::masked(masker, &e.to_string())))?;
  if let Some(label) = &auth.label {
    tracing::info!(%label, "registered with a GitHub App");
  }
  Ok(Applied {
    url: url.to_owned(),
    name: plan.name.clone(),
    labels: labels.to_vec(),
    runner_id,
    fresh: true,
  })
}

/// Install/update the one machine unit (`run --all`) off the async
/// runtime; linger was already decided by setup.
pub(crate) async fn install_machine_service(enable_linger: bool) -> Result<(), SetupError> {
  let args = crate::cli::InstallServiceArgs {
    config: None,
    print: false,
    no_activate: false,
    remove: false,
    linger: crate::cli::LingerArgs {
      enable_linger,
      yes: false,
      decided: Some(enable_linger),
    },
  };
  tokio::task::spawn_blocking(move || {
    crate::machine_service::cmd_install_machine(&args).map_err(|e| e.to_string())
  })
  .await
  .map_err(|e| SetupError::runtime(format!("the service install did not finish: {e}")))?
  .map_err(SetupError::runtime)
}

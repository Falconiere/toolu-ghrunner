//! `create-app` subcommand: mint the runner's GitHub App via the
//! App-manifest flow (personal, or owned by an `--org`), persist it, and
//! wait for its installation (#202).
//!
//! Split out of `main.rs` to keep the CLI entrypoint under the crate's
//! per-file complexity limit. The pure pieces (manifest JSON per
//! [`ManifestTarget`], CSRF state, callback / response parsing) live in
//! [`protocol::app_manifest`]; the loopback [`CallbackServer`], the
//! code→credentials exchange and the App calls live in [`wire::net`]; the
//! `0600` persistence lives in [`config::app_store`]; the installation wait
//! in [`crate::app_install`]; `--import` / `--app-id` in
//! [`crate::app_import`]. This handler wires them together, guarding on
//! host + an existing App file BEFORE checking the org, binding a socket,
//! opening a browser, or making any other network call.

use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use config::app_store::{self, StoredApp};
use protocol::app_manifest::{self, AppManifest, ConversionResponse, ManifestTarget};
use shared::{RunnerError, SecretMasker};
use tokio::io::AsyncBufReadExt as _;
use wire::net::app_manifest::{CallbackServer, convert_manifest_code};
use wire::net::github_app::find_org;

use crate::app_auth::masked;
use crate::app_install::{
  WaitIo, WaitOutcome, WaitTarget, WaitTiming, installed_line, wait_for_installation,
};
use crate::cli::{CreateAppArgs, runner_name_or_hostname};
use crate::run_cmd::CodedExit;

/// The only GitHub host `create-app` supports this release.
const SUPPORTED_HOST: &str = "github.com";

/// How long to wait for the browser round trip before giving up.
const BROWSER_TIMEOUT: Duration = Duration::from_secs(300);

/// Timeout of the org check.
const ORG_CHECK_TIMEOUT: Duration = Duration::from_secs(30);

/// Exit code of an unattended run whose installation never came (epic #199:
/// runtime failure).
const INSTALL_TIMEOUT_EXIT: i32 = 1;

/// Exit code of a cancelled wait (epic #199).
const CANCELLED_EXIT: i32 = 130;

/// What every `create-app` mode shares: the HTTP client, the API base, where
/// the App is saved, and the masker its secrets are registered with.
pub(crate) struct AppContext<'a> {
  /// The runner home (`TOOLU_RUNNER_HOME`).
  pub(crate) home: &'a Path,
  /// `<home>/github-app.json`.
  pub(crate) dest: PathBuf,
  /// HTTP client (env-mode networking).
  pub(crate) client: reqwest::Client,
  /// REST API base (`TOOLU_RUNNER_GITHUB_API_URL`, default api.github.com).
  pub(crate) api_base: String,
  /// Masks the App's secrets, JWTs and tokens in every printed error.
  pub(crate) masker: Mutex<SecretMasker>,
  /// `--no-browser`.
  pub(crate) no_browser: bool,
}

impl AppContext<'_> {
  /// `e` as a boxed error with every known secret masked (`main` prints it
  /// outside the tracing redactor).
  pub(crate) fn fail(&self, e: &dyn std::fmt::Display) -> Box<dyn std::error::Error> {
    masked(&self.masker, &e.to_string()).into()
  }
}

/// `create-app`: create (or, with `--import` / `--app-id`, reuse) the
/// runner's GitHub App, save it, and wait for its installation.
///
/// # Errors
///
/// Exit 2 for a usage, auth or config problem (unsupported host, existing
/// App without `--force`, unknown org, a key that does not match, …); exit
/// 1 when an unattended wait times out; exit 130 when the wait is cancelled.
pub(crate) async fn cmd_create_app(
  args: &CreateAppArgs,
  home: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
  // Guards first — before any bind / browser / network.
  if args.host != SUPPORTED_HOST {
    return Err(unsupported_host_error(&args.host).into());
  }
  let ctx = AppContext {
    home,
    dest: app_store::app_path(home),
    client: build_client()?,
    api_base: crate::config::github_api_url(),
    masker: Mutex::new(SecretMasker::new()),
    no_browser: args.no_browser,
  };
  if args.import.is_some() || args.app_id.is_some() {
    return crate::app_import::import_app(args, &ctx).await;
  }
  if ctx.dest.exists() && !args.force {
    return Err(existing_app_error(&ctx.dest).into());
  }
  create_app(args, &ctx).await
}

/// Create mode: org check, the manifest round trip, save at once, wait.
async fn create_app(
  args: &CreateAppArgs,
  ctx: &AppContext<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
  let target = match &args.org {
    Some(org) => ManifestTarget::Org(check_org(ctx, org).await?),
    None => ManifestTarget::Personal,
  };
  let name = default_name(args);

  let state = app_manifest::new_state();
  let server = CallbackServer::bind(state).await?;
  let manifest = AppManifest::for_target(&name, &server.callback_url(), &target);
  let manifest_json = manifest.to_json()?;
  let form_action = target.form_action();

  prompt_browser(&server.local_url(), &form_action, args.no_browser);
  let code = server
    .wait_for_code(manifest_json, &form_action, BROWSER_TIMEOUT)
    .await
    .map_err(|e| refused_form_hint(&e, &target))?;

  let resp = convert_manifest_code(&ctx.client, &ctx.api_base, &code)
    .await
    .map_err(|e| ctx.fail(&e))?;
  let app = build_stored_app(resp, args.host.clone());
  // Saved at once: the App exists on GitHub and its key can't be fetched
  // again (the epic's one exception to "nothing written before Review").
  app_store::save_app(ctx.home, &app)?;
  register_secrets(ctx, &app);
  println!("App created \u{b7} {} \u{b7} App {}", app.slug, app.app_id);
  println!("key saved to {} (0600)", ctx.dest.display());
  let account = app.owner.clone();
  wait_and_record(ctx, app, &account).await
}

/// `GET /orgs/{org}` before anything else: a typo (or a user login) fails
/// here. Returns the org login as GitHub spells it.
async fn check_org(ctx: &AppContext<'_>, org: &str) -> Result<String, Box<dyn std::error::Error>> {
  match find_org(&ctx.client, &ctx.api_base, org, ORG_CHECK_TIMEOUT).await {
    Ok(Some(found)) => Ok(found.login),
    Ok(None) => Err(format!("No organization \"{org}\" on github.com. Check the spelling.").into()),
    Err(e) => Err(format!("could not check the organization \"{org}\" on GitHub: {e}").into()),
  }
}

/// The App name: `--name`, else `toolu-runner-<org>`, else
/// `toolu-runner-<account>`, else `toolu-runner-<hostname>` (no credential
/// exists yet to learn the personal login).
fn default_name(args: &CreateAppArgs) -> String {
  if let Some(name) = &args.name {
    return name.clone();
  }
  match args.org.as_ref().or(args.account.as_ref()) {
    Some(account) => format!("toolu-runner-{account}"),
    None => format!("toolu-runner-{}", runner_name_or_hostname(None)),
  }
}

/// Add the org-refusal hint to a failed browser round trip for an org (a
/// non-owner gets GitHub's refusal page, which never redirects back).
fn refused_form_hint(e: &RunnerError, target: &ManifestTarget) -> String {
  match target {
    ManifestTarget::Org(org) => format!(
      "{e}. If GitHub refused the form: only owners of {org} (and its GitHub App managers) can \
       create an App for it — ask an owner, or register with a personal token \
       (`toolu-runner register --token …`)"
    ),
    ManifestTarget::Personal => e.to_string(),
  }
}

/// Register `app`'s secrets with the masker before anything can print them.
pub(crate) fn register_secrets(ctx: &AppContext<'_>, app: &StoredApp) {
  ctx
    .masker
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .add_secrets(app.secret_values());
}

/// Print the install link, open it, wait for the installation on `account`,
/// then record its id in `github-app.json`.
///
/// # Errors
///
/// [`CodedExit`] 1 when an unattended wait times out, 130 when it is
/// cancelled (Ctrl-C or Cancel); a masked error when GitHub rejects the App.
pub(crate) async fn wait_and_record(
  ctx: &AppContext<'_>,
  mut app: StoredApp,
  account: &str,
) -> Result<(), Box<dyn std::error::Error>> {
  let url = app.install_url();
  println!("Install the App on {account}:\n  {url}");
  let target = WaitTarget {
    client: &ctx.client,
    api_base: &ctx.api_base,
    app: &app,
    account,
    masker: &ctx.masker,
  };
  let timing = WaitTiming {
    timeout: crate::config::install_wait(),
    poll: crate::config::install_poll(),
  };
  let installation = match run_wait(ctx, &target, timing, &url).await? {
    WaitOutcome::Installed(installation) => installation,
    WaitOutcome::Cancelled => return Err(not_installed(ctx, CANCELLED_EXIT, "cancelled", &url)),
    WaitOutcome::TimedOut => {
      let why = format!(
        "the App isn't installed on {account} after {} s",
        timing.timeout.as_secs()
      );
      return Err(not_installed(ctx, INSTALL_TIMEOUT_EXIT, &why, &url));
    },
  };
  let line = installed_line(&target, &installation).await;
  app.installation_id = Some(installation.id);
  app_store::save_app(ctx.home, &app)?;
  println!("{line}");
  Ok(())
}

/// Open the link and run the wait on this process's terminal: line commands
/// only when stdin is one; Ctrl-C cancels.
async fn run_wait(
  ctx: &AppContext<'_>,
  target: &WaitTarget<'_>,
  timing: WaitTiming,
  url: &str,
) -> Result<WaitOutcome, Box<dyn std::error::Error>> {
  let no_browser = ctx.no_browser;
  let open = move |link: &str| {
    if !no_browser {
      crate::login_cmd::open_browser_best_effort(link);
    }
  };
  open(url);
  let input = std::io::stdin()
    .is_terminal()
    .then(|| tokio::io::BufReader::new(tokio::io::stdin()).lines());
  let mut out = std::io::stdout();
  let io = WaitIo {
    url,
    open: &open,
    input,
    out: &mut out,
  };
  Ok(tokio::select! {
    outcome = wait_for_installation(target, timing, io) => outcome.map_err(|e| ctx.fail(&e))?,
    _ = tokio::signal::ctrl_c() => WaitOutcome::Cancelled,
  })
}

/// The exit for a wait that ended without an installation, naming how to
/// resume (the App is already saved).
fn not_installed(
  ctx: &AppContext<'_>,
  code: i32,
  why: &str,
  url: &str,
) -> Box<dyn std::error::Error> {
  Box::new(CodedExit::new(
    code,
    format!(
      "{why} — the App is saved at {path}; install it at {url}, then run `toolu-runner \
       create-app --import {path}`",
      path = ctx.dest.display()
    ),
  ))
}

/// Error message for an unsupported `--host` (github.com only this release).
fn unsupported_host_error(host: &str) -> String {
  format!(
    "--host '{host}' is not supported yet — `create-app` targets github.com only this release \
     (GHES comes later); re-run without --host, or with --host github.com"
  )
}

/// Error message for an existing App file when `--force` was not given.
pub(crate) fn existing_app_error(path: &Path) -> String {
  format!(
    "a GitHub App is already saved at {} — pass --force to overwrite it",
    path.display()
  )
}

/// Print the local URL (always), then open a browser unless `no_browser`,
/// and print a "waiting" line. Browser launch is best-effort — the printed
/// URL always works when typed manually.
fn prompt_browser(local_url: &str, form_action: &str, no_browser: bool) {
  println!("Create the App in your browser: {local_url}");
  println!("  (it sends you on to {form_action})");
  if no_browser {
    println!("(--no-browser set: not launching a browser; open the URL above yourself)");
  } else {
    crate::login_cmd::open_browser_best_effort(local_url);
  }
  println!("Waiting for the browser to finish creating the GitHub App\u{2026}");
}

/// Build the HTTP client for the App calls (30s timeout), matching the
/// `register` / `login` client builders.
fn build_client() -> Result<reqwest::Client, RunnerError> {
  // No registration exists yet: env-mode networking (#94).
  network::client_builder(&shared::NetworkConfig::default())?
    .timeout(Duration::from_secs(30))
    .build()
    .map_err(|e| RunnerError::Network(format!("failed to build the create-app HTTP client: {e}")))
}

/// Assemble the persisted [`StoredApp`] from the conversion response,
/// stamping `created_at` as RFC3339 now — the same mechanism
/// `config::auth_store::StoredToken.issued_at` uses.
fn build_stored_app(resp: ConversionResponse, host: String) -> StoredApp {
  StoredApp {
    host,
    app_id: resp.id,
    slug: resp.slug,
    owner_id: resp.owner.id,
    owner: resp.owner.login,
    client_id: resp.client_id,
    client_secret: resp.client_secret,
    webhook_secret: resp.webhook_secret,
    pem: resp.pem,
    html_url: resp.html_url,
    created_at: chrono::Utc::now().to_rfc3339(),
    installation_id: None,
  }
}

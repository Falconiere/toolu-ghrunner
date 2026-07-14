//! `create-app` subcommand: mint the runner's GitHub App via the
//! App-manifest flow and persist it.
//!
//! Split out of `main.rs` to keep the CLI entrypoint under the crate's
//! per-file complexity limit. The pure pieces (manifest JSON, CSRF state,
//! callback / response parsing) live in [`protocol::app_manifest`]; the
//! loopback [`CallbackServer`] and the code→credentials exchange
//! ([`convert_manifest_code`]) live in [`wire::net::app_manifest`]; the
//! `0600` persistence lives in [`config::app_store`]. This handler wires
//! them together, guarding on host + an existing App file BEFORE binding a
//! socket, opening a browser, or making any network call.

use std::path::Path;
use std::time::Duration;

use config::app_store::{self, StoredApp};
use protocol::app_manifest::{self, AppManifest, ConversionResponse};
use shared::RunnerError;
use wire::net::app_manifest::{CallbackServer, convert_manifest_code};

use crate::cli::{CreateAppArgs, runner_name_or_hostname};

/// The only GitHub host `create-app` supports this release.
const SUPPORTED_HOST: &str = "github.com";

/// How long to wait for the browser round trip before giving up.
const BROWSER_TIMEOUT: Duration = Duration::from_secs(300);

/// `create-app`: run the GitHub App manifest flow and persist the minted App.
///
/// Order matters — the host and existing-file guards fire first, before any
/// socket bind, browser launch, or network call. Then it binds the loopback
/// callback server, opens the browser (unless `--no-browser`), waits for the
/// one-time code, exchanges it for the App credentials, and saves them
/// `0600` to `<home>/github-app.json`, printing a secret-free summary.
pub(crate) async fn cmd_create_app(
  args: &CreateAppArgs,
  home: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
  // Guards first — before any bind / browser / network.
  if args.host != SUPPORTED_HOST {
    return Err(unsupported_host_error(&args.host).into());
  }
  let path = app_store::app_path(home);
  if path.exists() && !args.force {
    return Err(existing_app_error(&path).into());
  }

  let name = match &args.name {
    Some(name) => name.clone(),
    None => format!("toolu-runner-{}", runner_name_or_hostname(None)),
  };

  let state = app_manifest::new_state();
  let server = CallbackServer::bind(state).await?;
  let manifest = AppManifest::for_runner(&name, &server.callback_url());
  let manifest_json = manifest.to_json()?;

  prompt_browser(&server.local_url(), args.no_browser);
  let code = server.wait_for_code(manifest_json, BROWSER_TIMEOUT).await?;

  let client = build_client()?;
  let resp = convert_manifest_code(&client, &args.host, &code).await?;

  let app = build_stored_app(resp, args.host.clone());
  app_store::save_app(home, &app)?;
  println!("{}", app.safe_summary(&path));
  Ok(())
}

/// Error message for an unsupported `--host` (github.com only this release).
fn unsupported_host_error(host: &str) -> String {
  format!(
    "--host '{host}' is not supported yet — `create-app` targets github.com only this release \
     (GHES and organization-owned Apps come later); re-run without --host, or with \
     --host github.com"
  )
}

/// Error message for an existing App file when `--force` was not given.
fn existing_app_error(path: &Path) -> String {
  format!(
    "a GitHub App is already saved at {} — pass --force to overwrite it",
    path.display()
  )
}

/// Print the local URL (always), then open a browser unless `no_browser`,
/// and print a "waiting" line. Browser launch is best-effort — the printed
/// URL always works when typed manually.
fn prompt_browser(local_url: &str, no_browser: bool) {
  println!("Open this URL to create the GitHub App: {local_url}");
  if no_browser {
    println!("(--no-browser set: not launching a browser; open the URL above yourself)");
  } else {
    crate::login_cmd::open_browser_best_effort(local_url);
  }
  println!("Waiting for the browser to finish creating the GitHub App\u{2026}");
}

/// Build the HTTP client for the manifest conversion (30s timeout), matching
/// the `register` / `login` client builders.
fn build_client() -> Result<reqwest::Client, RunnerError> {
  reqwest::Client::builder()
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
    owner: resp.owner.login,
    client_id: resp.client_id,
    client_secret: resp.client_secret,
    webhook_secret: resp.webhook_secret,
    pem: resp.pem,
    html_url: resp.html_url,
    created_at: chrono::Utc::now().to_rfc3339(),
  }
}

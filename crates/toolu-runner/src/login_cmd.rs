//! `login` / `logout` subcommands: GitHub OAuth device-flow login.
//!
//! Split out of `main.rs` to keep the CLI entrypoint under the crate's
//! per-file complexity limit. [`run_device_flow`] is the shared flow body
//! ([`wire::net::device_auth`] + [`config::auth_store::AuthStore`]):
//! [`cmd_login`] wraps it, and `register` reuses it inline when no token
//! is available on an interactive terminal. Tokens are stored per host at
//! the runner home (`registry::runner_home()`), shared by every per-repo
//! registration; [`cmd_logout`] deletes them.

use std::time::Duration;

use clap::{Args, ValueHint};
use config::auth_store::{AuthStore, StoredToken};
use config::registry;
use shared::RunnerError;
use wire::net;

/// github.com OAuth App `client_id` for the device-flow `login`.
/// Placeholder until the real App is registered.
const DEVICE_CLIENT_ID: &str = "REPLACE_ME";

/// Extended help footer for `login --help`.
const LOGIN_AFTER_HELP: &str = "\
Examples:
  toolu-runner login
  toolu-runner login ghes.example.com --client-id Iv1.0123456789abcdef";

/// Arguments for the `login` subcommand.
#[derive(Debug, Args)]
#[command(after_help = LOGIN_AFTER_HELP)]
pub(crate) struct LoginArgs {
  /// GitHub host to log in to (github.com or a GHES hostname).
  #[arg(default_value = "github.com", value_name = "HOST", value_hint = ValueHint::Hostname)]
  hostname: String,
  /// OAuth App client_id for the device flow.
  ///
  /// Resolution order: this flag > TOOLU_RUNNER_CLIENT_ID env > the
  /// built-in github.com App. Required for GHES (register an OAuth App
  /// on the GHES host); the built-in github.com App is not wired yet, so
  /// github.com currently needs it too.
  #[arg(long, value_name = "CLIENT_ID")]
  client_id: Option<String>,
}

/// Arguments for the `logout` subcommand.
#[derive(Debug, Args)]
pub(crate) struct LogoutArgs {
  /// GitHub host to log out of.
  #[arg(default_value = "github.com", value_name = "HOST", value_hint = ValueHint::Hostname)]
  hostname: String,
}

/// `login`: run the GitHub OAuth device flow and persist the token in the
/// runner-home token store (0600 file; OS keyring when
/// `TOOLU_RUNNER_KEYRING` opts in). The store is shared by all per-repo
/// registrations — no config file is involved.
pub(crate) async fn cmd_login(args: LoginArgs) -> Result<(), Box<dyn std::error::Error>> {
  let store = AuthStore::new(&registry::runner_home());
  let stored = run_device_flow(&args.hostname, args.client_id, &store, |dc| {
    eprintln!("Enter code {} at {}", dc.user_code, dc.verification_uri)
  })
  .await?;
  println!("logged in to {} (scopes: {})", args.hostname, stored.scope);
  Ok(())
}

/// Run the GitHub OAuth device flow against `host` and persist the minted
/// token in `store`, returning it. Routes the user code + verification URL
/// through the `present` callback (so the caller owns how it is shown),
/// best-effort opens the browser, and polls until the grant completes. The
/// effective client_id is `client_id_override` > `TOOLU_RUNNER_CLIENT_ID`
/// env > the baked-in `DEVICE_CLIENT_ID` constant. That constant is still
/// the compile-time placeholder (no OAuth App is registered yet), so
/// whenever it would be used the flow errors BEFORE any network call — in
/// practice github.com needs `--client-id` or `TOOLU_RUNNER_CLIENT_ID` too,
/// until the real App exists. Shared by `login` and `register`'s inline flow.
pub(crate) async fn run_device_flow(
  host: &str,
  client_id_override: Option<String>,
  store: &AuthStore,
  present: impl Fn(&net::device_auth::DeviceCodeResponse),
) -> Result<StoredToken, Box<dyn std::error::Error>> {
  let client_id: String = client_id_override
    .or_else(|| std::env::var("TOOLU_RUNNER_CLIENT_ID").ok())
    .unwrap_or_else(|| DEVICE_CLIENT_ID.to_owned());

  // The built-in client_id is a compile-time placeholder: no real OAuth App
  // is configured. Fail before any network call — for GHES the caller must
  // register their own App, for github.com the built-in App is not wired yet.
  if client_id == DEVICE_CLIENT_ID {
    let msg = if host == "github.com" {
      "no GitHub OAuth App configured — pass --client-id or set TOOLU_RUNNER_CLIENT_ID".to_owned()
    } else {
      format!("GHES login needs --client-id for an OAuth App registered on {host}")
    };
    return Err(msg.into());
  }

  let client = reqwest::Client::builder()
    .timeout(Duration::from_secs(30))
    .build()
    .map_err(|e| RunnerError::Network(format!("HTTP client: {e}")))?;

  let dc =
    net::device_auth::request_device_code(&client, host, &client_id, "repo admin:org").await?;
  present(&dc);
  open_browser_best_effort(&dc.verification_uri);

  let tok = net::device_auth::poll_for_token(&client, host, &client_id, &dc).await?;

  let stored = StoredToken {
    access_token: tok.access_token,
    scope: tok.scope,
    host: host.to_owned(),
    issued_at: chrono::Utc::now().to_rfc3339(),
  };
  store.save(&stored)?;
  Ok(stored)
}

/// `logout`: delete the stored login token for the host from the
/// runner-home store. Idempotent — a missing token is a no-op.
pub(crate) fn cmd_logout(args: &LogoutArgs) -> Result<(), Box<dyn std::error::Error>> {
  AuthStore::new(&registry::runner_home()).delete(&args.hostname)?;
  println!("Logged out of {}", args.hostname);
  Ok(())
}

/// Best-effort browser launch. Every error is ignored: login still works
/// by typing the code at the printed URL manually. `pub(crate)` so
/// `create-app`'s manifest flow can reuse the same launcher. The child's
/// stdout+stderr are null-redirected — it is fire-and-forget and no output
/// is wanted, so an opener's stray line can never corrupt the CLI output or
/// the `setup` wizard's alternate screen.
pub(crate) fn open_browser_best_effort(url: &str) {
  let mut command = if cfg!(target_os = "macos") {
    let mut c = std::process::Command::new("open");
    c.arg(url);
    c
  } else if cfg!(target_os = "windows") {
    let mut c = std::process::Command::new("cmd");
    c.args(["/c", "start", "", url]);
    c
  } else {
    let mut c = std::process::Command::new("xdg-open");
    c.arg(url);
    c
  };
  let _ = command
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .spawn();
}

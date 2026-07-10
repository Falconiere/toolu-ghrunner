//! `login` / `logout` subcommands: GitHub OAuth device-flow login.
//!
//! Split out of `main.rs` to keep the CLI entrypoint under the crate's
//! per-file complexity limit. [`cmd_login`] runs the device flow
//! ([`crate`]'s [`toolu_runner::net::device_auth`]) and persists the token
//! via [`toolu_runner::auth_store::AuthStore`]; [`cmd_logout`] deletes it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::Args;
use shared::RunnerError;
use toolu_runner::auth_store::{AuthStore, StoredToken};
use toolu_runner::net;

use crate::{DEVICE_CLIENT_ID, default_config_path};

/// Arguments for the `login` subcommand.
#[derive(Debug, Args)]
pub(crate) struct LoginArgs {
  /// GitHub host to log in to (github.com or a GHES hostname).
  #[arg(default_value = "github.com")]
  hostname: String,
  /// OAuth App `client_id`. Required for GHES; defaults to the built-in
  /// github.com App when logging in to github.com.
  #[arg(long)]
  client_id: Option<String>,
  /// Path to the runner config file (its parent dir holds the token store).
  #[arg(long)]
  config: Option<PathBuf>,
}

/// Arguments for the `logout` subcommand.
#[derive(Debug, Args)]
pub(crate) struct LogoutArgs {
  /// GitHub host to log out of.
  #[arg(default_value = "github.com")]
  hostname: String,
  /// Path to the runner config file (its parent dir holds the token store).
  #[arg(long)]
  config: Option<PathBuf>,
}

/// `login`: run the GitHub OAuth device flow and persist the token.
///
/// Prints the user code, best-effort opens the browser, polls for the
/// token, then stores it in the [`AuthStore`]. No config file is required —
/// the token store lives in the config path's parent dir.
pub(crate) async fn cmd_login(args: LoginArgs) -> Result<(), Box<dyn std::error::Error>> {
  let host = &args.hostname;

  // Effective client_id: --client-id flag > TOOLU_RUNNER_CLIENT_ID env >
  // the built-in DEVICE_CLIENT_ID placeholder.
  let client_id: String = args
    .client_id
    .clone()
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

  let config_path = args.config.clone().unwrap_or_else(default_config_path);
  let data_dir = data_dir_for_config(&config_path);

  let client = reqwest::Client::builder()
    .timeout(Duration::from_secs(30))
    .build()
    .map_err(|e| RunnerError::Network(format!("HTTP client: {e}")))?;

  let dc =
    net::device_auth::request_device_code(&client, host, &client_id, "repo admin:org").await?;
  eprintln!("Enter code {} at {}", dc.user_code, dc.verification_uri);
  open_browser_best_effort(&dc.verification_uri);

  let tok = net::device_auth::poll_for_token(&client, host, &client_id, &dc).await?;

  let stored = StoredToken {
    access_token: tok.access_token,
    scope: tok.scope,
    host: host.clone(),
    issued_at: chrono::Utc::now().to_rfc3339(),
  };
  AuthStore::new(&data_dir).save(&stored)?;

  println!("logged in to {host} (scopes: {})", stored.scope);
  Ok(())
}

/// `logout`: delete the stored login token for the host. Idempotent —
/// a missing token is a no-op.
pub(crate) fn cmd_logout(args: &LogoutArgs) -> Result<(), Box<dyn std::error::Error>> {
  let config_path = args.config.clone().unwrap_or_else(default_config_path);
  let data_dir = data_dir_for_config(&config_path);
  AuthStore::new(&data_dir).delete(&args.hostname)?;
  println!("Logged out of {}", args.hostname);
  Ok(())
}

/// Data dir for the login-token store when no `RunnerConfig` is loaded.
/// The store sits next to `config.toml`, i.e. the config path's parent
/// (default `~/.toolu-runner`).
pub(crate) fn data_dir_for_config(config_path: &Path) -> PathBuf {
  config_path.parent().map_or_else(
    || shared::paths::expand_tilde(Path::new("~/.toolu-runner")),
    Path::to_path_buf,
  )
}

/// Best-effort browser launch. Every error is ignored: login still works
/// by typing the code at the printed URL manually.
fn open_browser_best_effort(url: &str) {
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
  let _ = command.spawn();
}

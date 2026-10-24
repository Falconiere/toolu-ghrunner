//! `create-app --import <file>` / `--app-id <id> --app-key <pem>` (#202,
//! design screen 04): reuse an App another machine created instead of
//! creating one more. The key is proven with `GET /app` (an App JWT GitHub
//! must verify) before anything is written; the saved identity is refreshed
//! from GitHub's reply; then the installation on the target account is
//! looked up, or waited for.

use std::path::Path;

use config::app_store::{self, StoredApp};
use protocol::app_jwt::AppInfo;
use shared::RunnerError;
use wire::net::github_app::get_app;

use crate::app_auth::app_jwt;
use crate::app_install::installation_on;
use crate::cli::CreateAppArgs;
use crate::create_app_cmd::{AppContext, existing_app_error, register_secrets, wait_and_record};

/// Timeout of the `GET /app` proof.
const PROOF_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Import mode of `create-app`.
///
/// # Errors
///
/// Exit 2 when the source is unreadable, names another host, would
/// overwrite a different saved App without `--force`, or its key does not
/// match the App (nothing is written then); otherwise as
/// [`wait_and_record`].
pub(crate) async fn import_app(
  args: &CreateAppArgs,
  ctx: &AppContext<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
  let (mut app, source) = source_app(args)?;
  if app.host != args.host {
    return Err(
      format!(
        "the App in {source} is on {}, but `create-app` supports github.com only",
        app.host
      )
      .into(),
    );
  }
  if ctx.dest.exists() && !args.force && !same_file(&source, &ctx.dest) {
    return Err(existing_app_error(&ctx.dest).into());
  }
  register_secrets(ctx, &app);
  let info = prove_key(ctx, &app).await?;
  refresh_identity(&mut app, info);
  let account = args.org.clone().unwrap_or_else(|| app.owner.clone());
  let found = installation_on(&ctx.client, &ctx.api_base, &app, &account, &ctx.masker)
    .await
    .map_err(|e| ctx.fail(&e))?;
  app.installation_id = found.map(|installation| installation.id);
  app_store::save_app(ctx.home, &app)?;
  if app.installation_id.is_some() {
    println!(
      "Key matches App {} \u{b7} installed on {account}",
      app.app_id
    );
    return Ok(());
  }
  println!("Key matches App {}", app.app_id);
  wait_and_record(ctx, app, &account, args.org.as_deref()).await
}

/// The App to import and where it came from (for messages).
fn source_app(args: &CreateAppArgs) -> Result<(StoredApp, String), Box<dyn std::error::Error>> {
  if let Some(file) = &args.import {
    return Ok((app_store::load_app_file(file)?, file.display().to_string()));
  }
  let (Some(app_id), Some(key)) = (args.app_id, &args.app_key) else {
    return Err("pass --import <file>, or --app-id <id> with --app-key <pem file>".into());
  };
  let pem = std::fs::read_to_string(key)
    .map_err(|e| format!("could not read the App key {}: {e}", key.display()))?;
  let app = StoredApp {
    host: args.host.clone(),
    app_id,
    slug: String::new(),
    owner: String::new(),
    client_id: String::new(),
    // GitHub never returns these for an existing App; the runner needs
    // neither (it authenticates with the key).
    client_secret: String::new(),
    webhook_secret: None,
    pem,
    html_url: String::new(),
    created_at: chrono::Utc::now().to_rfc3339(),
    owner_id: None,
    installation_id: None,
  };
  Ok((app, key.display().to_string()))
}

/// `GET /app` with the App's JWT: GitHub answers only when the key belongs
/// to the App, and the reply must name the same App.
async fn prove_key(
  ctx: &AppContext<'_>,
  app: &StoredApp,
) -> Result<AppInfo, Box<dyn std::error::Error>> {
  let jwt = app_jwt(app, &ctx.masker).map_err(|e| ctx.fail(&e))?;
  let info = match get_app(&ctx.client, &ctx.api_base, &jwt, PROOF_TIMEOUT).await {
    Ok(info) => info,
    Err(RunnerError::Auth(msg)) => {
      return Err(ctx.fail(&format!(
        "the private key does not match App {} — nothing was saved. GitHub: {msg}",
        app.app_id
      )));
    },
    Err(e) => return Err(ctx.fail(&e)),
  };
  if info.id != app.app_id {
    return Err(
      format!(
        "GitHub answered for App {} instead of App {} — nothing was saved",
        info.id, app.app_id
      )
      .into(),
    );
  }
  Ok(info)
}

/// Take the App's current identity from GitHub's `GET /app` reply.
fn refresh_identity(app: &mut StoredApp, info: AppInfo) {
  app.slug = info.slug;
  app.html_url = info.html_url;
  if !info.client_id.is_empty() {
    app.client_id = info.client_id;
  }
  if let Some(owner) = info.owner {
    app.owner = owner.login;
    app.owner_id = Some(owner.id);
  }
}

/// Whether `a` and `b` name the same file (re-importing the saved App onto
/// itself never needs `--force`).
fn same_file(a: &str, b: &Path) -> bool {
  match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
    (Ok(a), Ok(b)) => a == b,
    _ => false,
  }
}

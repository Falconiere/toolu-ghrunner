//! `status` subcommand: print local config + credential + login state.
//!
//! Split out of `main.rs` to keep the CLI entrypoint under the crate's
//! per-file complexity limit. Reads only local files — no network.

use std::path::Path;

use config::auth_store::AuthStore;
use config::config::{RunnerRegistrationConfig, load_config as load_reg_config};
use config::lockfile::LockBody;
use config::provider::ProviderSection;
use config::registration_auth::AuthSection;
use config::{registry, stop_marker};
use shared::paths::expand_tilde;

use crate::cli::{StatusArgs, credentials_path_for};

/// `status`: print the persisted registration, credential presence, and any
/// stored device-flow login token for the registered host. No network.
/// The registration resolves exactly like `run` / `remove`: `--config`
/// flag > cwd-inferred `runners/<owner>/<repo>/` registration > the sole
/// existing one (ambiguity errors listing every candidate).
pub(crate) fn cmd_status(args: StatusArgs) -> Result<(), Box<dyn std::error::Error>> {
  let config_path = crate::resolve_config(args.config)?;
  let creds_path = credentials_path_for(&config_path);
  if !config_path.exists() {
    return Err(format!("config not found at {}", config_path.display()).into());
  }
  // A TOML error quotes its source line, which can hold a secret the
  // masker never learned: print only what `doctor` would share.
  let cfg = load_reg_config(&config_path).map_err(|e| {
    crate::doctor_registrations::without_quoted_strings(
      &crate::doctor_registrations::without_source_excerpt(&e.to_string()),
    )
  })?;
  let creds_summary = if creds_path.exists() {
    "credentials present"
  } else {
    "credentials MISSING"
  };
  if let ProviderSection::Toolu {
    api_url,
    host_id,
    org,
  } = &cfg.provider
  {
    print_toolu(&cfg, api_url, host_id, org, creds_summary);
    return Ok(());
  }
  println!("runner:    {}", cfg.runner_name);
  println!("url:       {}", cfg.runner_url);
  println!("runner_id: {}", cfg.runner_id);
  println!("labels:    {:?}", cfg.labels);
  println!("group:     {}", cfg.runner_group);
  let mode = if cfg.ephemeral {
    "ephemeral (one job, then unregister)"
  } else {
    "persistent (re-mints after each job)"
  };
  println!("mode:      {mode}");
  println!("auth:      {}", auth_summary(&cfg.auth));
  println!("protocol:  {}", cfg.runtime.protocol_version);
  println!("data_dir:  {}", cfg.runtime.data_dir);
  println!("work_dir:  {}", cfg.runtime.work_dir);
  println!("jit_cfg:   {} bytes", cfg.runtime.jit_config.len());
  println!("creds:     {creds_summary}");
  println!(
    "run:       {}",
    run_summary(&expand_tilde(Path::new(&cfg.runtime.data_dir)))
  );

  // Login state (no network): report any stored device-flow login token
  // for the host the runner registered against. The token store is pinned
  // to the runner home (`registry::runner_home()`, where login/register
  // write it, shared by every per-repo registration), NOT to the resolved
  // config's directory — a custom --config must not make status wrongly
  // report "not logged in".
  let host = url::Url::parse(&cfg.runner_url)
    .ok()
    .and_then(|u| u.host_str().map(str::to_owned))
    .unwrap_or_else(|| "github.com".to_owned());
  let store = AuthStore::new(&registry::runner_home());
  let backend = match &store {
    AuthStore::File(_) => "0600 file (default; set TOOLU_RUNNER_KEYRING=1 for the OS keyring)",
    AuthStore::Keyring => "OS keyring (TOOLU_RUNNER_KEYRING opt-in)",
  };
  println!("tokens:    {backend}");
  match store.load(&host)? {
    Some(tok) => println!("login:     logged in to {host} (scopes: {})", tok.scope),
    None => println!(
      "login:     not logged in to {host} — run `toolu-runner login`; a login stored in \
       the OS keyring by an older version is only read with TOOLU_RUNNER_KEYRING=1"
    ),
  }
  Ok(())
}

/// A toolu.sh machine (#207): no GitHub runner, JIT or login to show; the
/// host token is the registration's own `credentials.json`.
fn print_toolu(
  cfg: &RunnerRegistrationConfig,
  api_url: &str,
  host_id: &str,
  org: &str,
  creds_summary: &str,
) {
  println!("source:    toolu.sh ({api_url})");
  println!("org:       {org}");
  println!("host_id:   {host_id}");
  println!("labels:    {:?}", cfg.labels);
  println!("data_dir:  {}", cfg.runtime.data_dir);
  println!("work_dir:  {}", cfg.runtime.work_dir);
  println!("creds:     {creds_summary} (host token)");
  println!(
    "run:       {}",
    run_summary(&expand_tilde(Path::new(&cfg.runtime.data_dir)))
  );
}

/// How the registration re-mints: a personal token, or the GitHub App named
/// by its (local, never fetched) App file.
fn auth_summary(auth: &AuthSection) -> String {
  match auth {
    AuthSection::Token => "token (TOOLU_RUNNER_TOKEN or the stored login)".to_owned(),
    AuthSection::App {
      app_file,
      installation_id,
    } => match config::app_store::load_app_file(std::path::Path::new(app_file)) {
      Ok(app) => format!(
        "GitHub App '{}' (App {}, installation {installation_id}, {app_file})",
        app.slug, app.app_id
      ),
      Err(e) => format!("GitHub App (installation {installation_id}) — {e}"),
    },
  }
}

/// Whether a `run` serves the registration in `data_dir` (from its lock
/// body, never by locking), else why `run --all` stopped it (`.stopped`),
/// else `not running`.
fn run_summary(data_dir: &Path) -> String {
  let lock = data_dir.join(".lock");
  // No lock file, or an empty body (a released lock), is "nobody holds it";
  // anything else unreadable is said, not hidden.
  let raw = match std::fs::read_to_string(&lock) {
    Ok(raw) => raw,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
    Err(e) => return format!("unknown (cannot read {}: {e})", lock.display()),
  };
  if !raw.trim().is_empty() {
    match serde_json::from_str::<LockBody>(&raw) {
      Ok(body) if crate::doctor_registrations::holder_alive(&body) => {
        return format!("active (pid {}, since {})", body.pid, body.started_at);
      },
      Ok(_) => {},
      Err(e) => return format!("unknown (lock body unreadable: {e})"),
    }
  }
  match stop_marker::read(data_dir) {
    Ok(Some(marker)) => format!(
      "stopped by the service at {}: {}",
      marker.stopped_at, marker.error
    ),
    Ok(None) => "not running".to_owned(),
    Err(e) => format!("not running ({e})"),
  }
}

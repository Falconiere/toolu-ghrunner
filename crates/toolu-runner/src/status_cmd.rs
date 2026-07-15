//! `status` subcommand: print local config + credential + login state.
//!
//! Split out of `main.rs` to keep the CLI entrypoint under the crate's
//! per-file complexity limit. Reads only local files — no network.

use config::auth_store::AuthStore;
use config::config::load_config as load_reg_config;
use config::registry;

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
  let cfg = load_reg_config(&config_path).map_err(|e| format!("{e}"))?;
  let creds_summary = if creds_path.exists() {
    "credentials present"
  } else {
    "credentials MISSING"
  };
  println!("runner:    {}", cfg.runner_name);
  println!("url:       {}", cfg.runner_url);
  println!("runner_id: {}", cfg.runner_id);
  println!("labels:    {:?}", cfg.labels);
  println!("group:     {}", cfg.runner_group);
  println!("protocol:  {}", cfg.runtime.protocol_version);
  println!("data_dir:  {}", cfg.runtime.data_dir);
  println!("work_dir:  {}", cfg.runtime.work_dir);
  println!("jit_cfg:   {} bytes", cfg.runtime.jit_config.len());
  println!("creds:     {creds_summary}");

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

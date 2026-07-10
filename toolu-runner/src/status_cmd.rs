//! `status` subcommand: print local config + credential + login state.
//!
//! Split out of `main.rs` to keep the CLI entrypoint under the crate's
//! per-file complexity limit. Reads only local files — no network.

use toolu_runner::auth_store::AuthStore;
use toolu_runner::config::{load_config as load_reg_config, resolve_data_dir};

use crate::{StatusArgs, credentials_path_for, default_config_path};

/// `status`: print the persisted registration, credential presence, and any
/// stored device-flow login token for the registered host. No network.
pub(crate) fn cmd_status(args: StatusArgs) -> Result<(), Box<dyn std::error::Error>> {
  let config_path = args.config.unwrap_or_else(default_config_path);
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
  // for the host the runner registered against.
  let data_dir = resolve_data_dir(&cfg.runtime.data_dir).map_err(|e| format!("{e}"))?;
  let host = url::Url::parse(&cfg.runner_url)
    .ok()
    .and_then(|u| u.host_str().map(str::to_owned))
    .unwrap_or_else(|| "github.com".to_owned());
  match AuthStore::new(&data_dir).load(&host)? {
    Some(tok) => println!("login:     logged in to {host} (scopes: {})", tok.scope),
    None => println!("login:     not logged in to {host}"),
  }
  Ok(())
}

//! A toolu.sh machine registration's own `/runner/host` side (#207), shared
//! by `setup`, `status`, `doctor` and `remove`: build its client from
//! `[provider] kind = "toolu"` + the `credentials.json` host token, describe
//! the machine, and revoke it.

use std::path::Path;

use config::config::{RunnerRegistrationConfig, load_credentials};
use config::provider::ProviderSection;
use protocol::toolu_pair::HostInfo;
use wire::net::toolu_jobs::{TooluClient, TooluError};

/// The `/runner/*` client of the toolu.sh registration `cfg` whose
/// credentials sit at `creds_path`, through its `[network]`. `mask` receives
/// the host token first, so the caller's output masker learns it before
/// anything can print it.
///
/// # Errors
///
/// A message when `cfg` is not a toolu.sh registration, its credentials do
/// not load or hold no token, or the client cannot be built.
pub(crate) fn client_for(
  cfg: &RunnerRegistrationConfig,
  creds_path: &Path,
  mask: impl FnOnce(&str),
) -> Result<TooluClient, String> {
  let ProviderSection::Toolu { api_url, .. } = &cfg.provider else {
    return Err("not a toolu.sh registration".to_owned());
  };
  let creds = load_credentials(creds_path)
    .map_err(|e| format!("cannot read the host token ({}): {e}", creds_path.display()))?;
  mask(&creds.access_token);
  let client = network::client_builder(&cfg.network_config())
    .and_then(|builder| {
      builder
        .build()
        .map_err(|e| shared::RunnerError::Network(e.to_string()))
    })
    .map_err(|e| format!("cannot build the toolu.sh client: {e}"))?;
  TooluClient::new(client, api_url, &creds.access_token).map_err(|e| e.to_string())
}

/// `host <id> · workspace <name> · enabled` (or `not enabled yet`).
pub(crate) fn host_line(host: &HostInfo) -> String {
  let state = if host.enabled {
    "enabled"
  } else {
    "not enabled yet"
  };
  format!(
    "host {} · workspace {} · {state}",
    host.host_id, host.org.name
  )
}

/// The fix for a machine toolu.sh no longer accepts.
pub(crate) fn revoked_fix(config_path: &Path) -> String {
  format!(
    "toolu-runner remove --config {} && toolu-runner setup --source toolu",
    config_path.display()
  )
}

/// `remove`'s first step for a toolu.sh registration: `DELETE /runner/host`.
/// `Ok(true)` once toolu.sh no longer has the machine (revoked now, or a 401:
/// already revoked).
///
/// # Errors
///
/// No usable host token, or any other failure: nothing local may be deleted
/// then, or the machine would keep a token nobody can revoke.
pub(crate) async fn revoke(
  cfg: &RunnerRegistrationConfig,
  creds_path: &Path,
) -> Result<bool, Box<dyn std::error::Error>> {
  // Nothing `remove` prints carries the token (`TooluError` never echoes the
  // bearer), so there is no output masker to teach.
  let client = client_for(cfg, creds_path, |_token| {}).map_err(|e| {
    format!(
      "cannot revoke the machine on toolu.sh: {e}\nNothing local was deleted — restore \
       credentials.json, or pass --skip-unregister to remove local state and revoke the \
       machine in the toolu.sh console."
    )
  })?;
  println!("revoking the machine on toolu.sh …");
  match client.revoke_host().await {
    Ok(()) => Ok(true),
    Err(TooluError::Revoked(why)) => {
      println!("toolu.sh no longer knows this machine ({why}): it was already revoked.");
      Ok(true)
    },
    Err(e) => Err(
      format!(
        "revoking the machine on toolu.sh failed: {e}\nNothing local was deleted — retry, or \
         pass --skip-unregister to remove local state anyway."
      )
      .into(),
    ),
  }
}

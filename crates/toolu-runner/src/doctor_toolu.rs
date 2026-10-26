//! `doctor` rows for a toolu.sh machine (#207): the registration's `toolu`
//! row (in place of a GitHub JIT) and one `toolu.sh (<api host>)` section
//! whose `host` row asks toolu.sh about the machine with `GET /runner/host`
//! — the only request `doctor` sends with a host token, and none with
//! `--offline`. Like any authenticated `/runner/*` request it refreshes the
//! machine's last-seen time on toolu.sh; it changes nothing else.

use std::path::Path;
use std::time::Duration;

use config::config::RunnerRegistrationConfig;
use config::provider::ProviderSection;
use shared::SecretMasker;
use wire::net::toolu_jobs::TooluError;

use crate::doctor_report::{Level, Row, Section};
use crate::toolu_host;

/// The registration block's `toolu` row: org, host id and API origin.
pub(crate) fn provider_row(config: &RunnerRegistrationConfig) -> Option<Row> {
  let ProviderSection::Toolu {
    api_url,
    host_id,
    org,
  } = &config.provider
  else {
    return None;
  };
  Some(Row::new(
    Level::Ok,
    "toolu",
    format!("toolu.sh org {org}, host {host_id} ({api_url})"),
  ))
}

/// The `toolu.sh (<api host>)` section of the toolu.sh registration at
/// `config_path`; the host token joins `masker` before anything renders.
pub(crate) async fn section(
  config_path: &Path,
  config: &RunnerRegistrationConfig,
  offline: bool,
  timeout: Duration,
  masker: &mut SecretMasker,
) -> Option<Section> {
  let ProviderSection::Toolu { api_url, .. } = &config.provider else {
    return None;
  };
  let host = url::Url::parse(api_url)
    .ok()
    .and_then(|url| url.host_str().map(str::to_owned))
    .unwrap_or_else(|| api_url.clone());
  let title = format!("toolu.sh ({host})");
  if offline {
    return Some(Section::new(
      &title,
      vec![Row::new(Level::Skip, "host", "offline")],
    ));
  }
  Some(Section::new(
    &title,
    vec![host_row(config_path, config, timeout, masker).await],
  ))
}

/// `GET /runner/host` as a row, bounded by `timeout`.
async fn host_row(
  config_path: &Path,
  config: &RunnerRegistrationConfig,
  timeout: Duration,
  masker: &mut SecretMasker,
) -> Row {
  let creds_path = crate::cli::credentials_path_for(config_path);
  let client = toolu_host::client_for(config, &creds_path, |token| masker.add_secret(token));
  let client = match client {
    Ok(client) => client,
    Err(error) => {
      return Row::new(Level::Fail, "host", error).with_fix(toolu_host::revoked_fix(config_path));
    },
  };
  match tokio::time::timeout(timeout, client.host()).await {
    Ok(Ok(host)) => Row::new(Level::Ok, "host", toolu_host::host_line(&host)),
    Ok(Err(TooluError::Revoked(why))) => Row::new(
      Level::Fail,
      "host",
      format!("toolu.sh rejected the host token ({why}): the machine was revoked"),
    )
    .with_fix(toolu_host::revoked_fix(config_path)),
    Ok(Err(error)) => Row::new(Level::Warn, "host", error.to_string()),
    Err(_) => Row::new(
      Level::Warn,
      "host",
      format!("toolu.sh did not answer within {}s", timeout.as_secs()),
    ),
  }
}

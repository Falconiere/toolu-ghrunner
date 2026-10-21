//! `doctor` GitHub auth rows: per GitHub host, which bearer `run` would
//! re-mint with (`TOOLU_RUNNER_TOKEN` > the stored `login` token, exactly
//! `resolve_bearer`'s order) and whether GitHub still accepts it.
//!
//! The verification is one authenticated `GET <api>/user` per host — the
//! only credentialed request `doctor` makes, and none with `--offline`. The
//! bearer is added to the report's masker before anything is rendered.

use std::time::Duration;

use config::auth_store::AuthStore;
use config::config::RunnerRegistrationConfig;
use config::registry;
use shared::{NetworkConfig, SecretMasker};

use crate::doctor_report::{Level, Row, Section};

/// One GitHub host the registrations use.
pub(crate) struct AuthHost {
  /// The host name the token store is keyed by.
  host: String,
  /// Its REST API base (`https://api.github.com` or `<origin>/api/v3`).
  api_base: String,
  /// A persistent registration on this host needs a bearer to re-mint.
  needs_bearer: bool,
  /// The first registration's `[network]` on this host.
  network: NetworkConfig,
}

/// `(host, api base)` for a registration URL, by `register`'s own rule
/// ([`wire::net::register::api_base`]).
pub(crate) fn api_base(runner_url: &str) -> Option<(String, String)> {
  let url = url::Url::parse(runner_url).ok()?;
  let host = url.host_str()?.to_owned();
  let base = wire::net::register::api_base(&url, &host);
  Some((host, base))
}

/// The hosts to check: each distinct registration host in order, or
/// github.com when there is no loadable registration.
pub(crate) fn auth_hosts(configs: &[&RunnerRegistrationConfig]) -> Vec<AuthHost> {
  let mut hosts: Vec<AuthHost> = Vec::new();
  for config in configs {
    let Some((host, api_base)) = api_base(&config.runner_url) else {
      continue;
    };
    match hosts.iter_mut().find(|known| known.api_base == api_base) {
      Some(known) => known.needs_bearer |= !config.ephemeral,
      None => hosts.push(AuthHost {
        host,
        api_base,
        needs_bearer: !config.ephemeral,
        network: config.network_config(),
      }),
    }
  }
  if hosts.is_empty() {
    hosts.push(AuthHost {
      host: "github.com".to_owned(),
      api_base: "https://api.github.com".to_owned(),
      needs_bearer: false,
      network: NetworkConfig::default(),
    });
  }
  hosts
}

/// The bearer source name for `TOOLU_RUNNER_TOKEN`.
const ENV_SOURCE: &str = "TOOLU_RUNNER_TOKEN";

/// `toolu-runner login`, naming the host when it is not github.com.
fn login_fix(host: &str) -> String {
  if host == "github.com" {
    "toolu-runner login".to_owned()
  } else {
    format!("toolu-runner login {host}")
  }
}

/// The fix for a rejected bearer from `source`: `TOOLU_RUNNER_TOKEN`
/// outranks the stored login, so `login` alone cannot replace it.
fn rejected_fix(source: &str, host: &str) -> String {
  if source == ENV_SOURCE {
    format!(
      "export a valid {ENV_SOURCE}, or unset it and run `{}`",
      login_fix(host)
    )
  } else {
    login_fix(host)
  }
}

/// `FAIL` when a persistent registration needs the bearer, else `WARN`.
fn needed(needs_bearer: bool) -> Level {
  if needs_bearer {
    Level::Fail
  } else {
    Level::Warn
  }
}

/// The `token` row for a `GET /user` answer with `status` and `body`; a
/// rejection fails only when `needs_bearer` (a persistent registration
/// re-mints with it).
pub(crate) fn verdict_row(
  source: &str,
  status: u16,
  body: &str,
  scopes: Option<&str>,
  host: &str,
  needs_bearer: bool,
) -> Row {
  let json: Option<serde_json::Value> = serde_json::from_str(body).ok();
  let field = |key: &str| {
    json
      .as_ref()
      .and_then(|value| value.get(key))
      .and_then(serde_json::Value::as_str)
      .map(str::to_owned)
  };
  match status {
    200 => {
      let login = field("login").unwrap_or_else(|| "unknown".to_owned());
      let detail = match scopes.filter(|scopes| !scopes.is_empty()) {
        Some(scopes) => format!("{source}: valid (login {login}; scopes {scopes})"),
        None => format!("{source}: valid (login {login})"),
      };
      Row::new(Level::Ok, "token", detail)
    },
    401 => {
      let reason = field("message").map_or_else(String::new, |message| format!(": {message}"));
      Row::new(
        needed(needs_bearer),
        "token",
        format!("{source}: rejected by GitHub (HTTP 401{reason})"),
      )
      .with_fix(rejected_fix(source, host))
    },
    other => Row::new(
      Level::Warn,
      "token",
      format!("{source}: could not verify (HTTP {other})"),
    ),
  }
}

/// The bearer `run` would use for `host` and where it came from.
fn bearer(host: &str) -> Result<Option<(String, String)>, String> {
  if let Some(token) = config::config::runner_token() {
    return Ok(Some((ENV_SOURCE.to_owned(), token)));
  }
  let store = AuthStore::new(&registry::runner_home());
  let source = match &store {
    AuthStore::File(_) => "stored login (0600 file)",
    AuthStore::Keyring => "stored login (OS keyring)",
  };
  store
    .load(host)
    .map(|stored| stored.map(|token| (source.to_owned(), token.access_token)))
    .map_err(|error| error.to_string())
}

/// `GET <api>/user` with the bearer, as a `token` row.
async fn verify(target: &AuthHost, source: &str, token: &str, timeout: Duration) -> Row {
  let client = match crate::check_probe::build_client(timeout, &target.network) {
    Ok(client) => client,
    Err(error) => {
      return Row::new(
        Level::Warn,
        "token",
        format!("{source}: could not verify: {error}"),
      );
    },
  };
  let sent = client
    .get(format!("{}/user", target.api_base))
    .bearer_auth(token)
    .header("Accept", "application/vnd.github+json")
    .send()
    .await;
  match sent {
    Ok(response) => {
      let status = response.status().as_u16();
      let scopes = response
        .headers()
        .get("x-oauth-scopes")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
      let body = match response.text().await {
        Ok(body) => body,
        // An unread body must not pass for an empty one (`login unknown`).
        Err(error) => {
          return Row::new(
            Level::Warn,
            "token",
            format!(
              "{source}: could not verify: unreadable response (HTTP {status}): {}",
              error.without_url()
            ),
          );
        },
      };
      verdict_row(
        source,
        status,
        &body,
        scopes.as_deref(),
        &target.host,
        target.needs_bearer,
      )
    },
    Err(error) => Row::new(
      Level::Warn,
      "token",
      format!(
        "{source}: could not verify: {}",
        crate::check_probe::describe_error(&error)
      ),
    ),
  }
}

/// The `github auth (<host>)` section; registers any bearer with `masker`.
pub(crate) async fn auth_section(
  target: &AuthHost,
  offline: bool,
  timeout: Duration,
  masker: &mut SecretMasker,
) -> Section {
  let title = format!("github auth ({})", target.host);
  let row = match bearer(&target.host) {
    Err(error) => Row::new(
      needed(target.needs_bearer),
      "token",
      format!("token store unreadable: {error}"),
    )
    .with_fix(login_fix(&target.host)),
    Ok(Some((source, token))) => {
      if !token.is_empty() {
        masker.add_secret(&token);
      }
      if offline {
        Row::new(
          Level::Skip,
          "token",
          format!("{source}: present, not verified (offline)"),
        )
      } else {
        verify(target, &source, &token, timeout).await
      }
    },
    Ok(None) if target.needs_bearer => Row::new(
      Level::Fail,
      "token",
      "none: a persistent registration cannot re-mint after its next job",
    )
    .with_fix(login_fix(&target.host)),
    Ok(None) => Row::new(
      Level::Info,
      "token",
      "none (only needed for persistent registrations)",
    )
    .with_fix(login_fix(&target.host)),
  };
  Section::new(&title, vec![row])
}

#[cfg(test)]
#[path = "tests/doctor_auth.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/doctor_auth_verify.rs"]
mod verify_tests;

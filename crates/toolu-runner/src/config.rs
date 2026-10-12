//! Environment-derived configuration for the CLI bin.

use std::path::{Path, PathBuf};
use std::time::Duration;

use listener::session_conflict::SessionConflictPolicy;

/// Overrides the wait between session-conflict retries (milliseconds).
const SESSION_CONFLICT_INTERVAL_KEY: &str = "TOOLU_RUNNER_SESSION_CONFLICT_INTERVAL_MS";

/// Read `TOOLU_JITCONFIG`; invalid Unicode is diagnosed and treated as unset.
pub(crate) fn jit_config_raw() -> Option<String> {
  read("TOOLU_JITCONFIG")
}

/// Read `TOOLU_DEADLINE`; invalid Unicode is diagnosed and treated as unset.
pub(crate) fn deadline_raw() -> Option<String> {
  read("TOOLU_DEADLINE")
}

/// Read `TOOLU_RUNNER_CLIENT_ID`; invalid Unicode is diagnosed and treated as unset.
pub(crate) fn runner_client_id() -> Option<String> {
  read("TOOLU_RUNNER_CLIENT_ID")
}

/// Read the installing shell's `PATH`; invalid Unicode is diagnosed and treated
/// as unset. `install-service` bakes it into the launchd unit, so a supervised
/// runner's steps see the same tools an operator has in a terminal.
pub(crate) fn path() -> Option<String> {
  read("PATH")
}

/// The first *set* variable of a proxy pair (a set-but-empty upper-case name
/// still wins), mirroring hyper-util's `Matcher::from_system`, which every
/// reqwest client in the runner uses for its system proxy.
fn first_set(upper: &'static str, lower: &'static str) -> Option<(&'static str, String)> {
  read(upper)
    .map(|value| (upper, value))
    .or_else(|| read(lower).map(|value| (lower, value)))
}

/// `HTTPS_PROXY` / `https_proxy`: the proxy for `https://` destinations.
pub(crate) fn https_proxy() -> Option<(&'static str, String)> {
  first_set("HTTPS_PROXY", "https_proxy")
}

/// `HTTP_PROXY` / `http_proxy`: the proxy for `http://` destinations.
pub(crate) fn http_proxy() -> Option<(&'static str, String)> {
  first_set("HTTP_PROXY", "http_proxy")
}

/// `ALL_PROXY` / `all_proxy`: the fallback proxy for both schemes.
pub(crate) fn all_proxy() -> Option<(&'static str, String)> {
  first_set("ALL_PROXY", "all_proxy")
}

/// `NO_PROXY` / `no_proxy`: the proxy bypass list.
pub(crate) fn no_proxy() -> Option<(&'static str, String)> {
  first_set("NO_PROXY", "no_proxy")
}

/// Whether `REQUEST_METHOD` is set: hyper-util then treats the process as a
/// CGI script and ignores every proxy variable (the httpoxy guard).
pub(crate) fn cgi_request() -> bool {
  std::env::var_os("REQUEST_METHOD").is_some()
}

/// `SSL_CERT_FILE`: a PEM bundle rustls-native-certs loads instead of the
/// platform trust store.
pub(crate) fn ssl_cert_file() -> Option<String> {
  read("SSL_CERT_FILE")
}

/// `SSL_CERT_DIR`: certificate directories rustls-native-certs loads instead
/// of the platform trust store.
pub(crate) fn ssl_cert_dir() -> Option<String> {
  read("SSL_CERT_DIR")
}

/// `NODE_EXTRA_CA_CERTS`: extra CAs Node steps (which inherit the runner's
/// environment) add to their trust store.
pub(crate) fn node_extra_ca_certs() -> Option<String> {
  read("NODE_EXTRA_CA_CERTS")
}

/// `TOOLU_RUNNER_NODE_DIR`: offline Node.js runtimes in upstream's
/// `externals/` layout (`node24/bin/node`, `node24_alpine/bin/node`). Empty
/// counts as unset; `~` is expanded and a relative path made absolute, since
/// job containers bind-mount it.
pub(crate) fn node_dir() -> Option<PathBuf> {
  let raw = read("TOOLU_RUNNER_NODE_DIR").filter(|raw| !raw.is_empty())?;
  let path = shared::paths::expand_tilde(Path::new(&raw));
  match std::path::absolute(&path) {
    Ok(absolute) => Some(absolute),
    Err(error) => {
      report(
        "TOOLU_RUNNER_NODE_DIR",
        &format!("could not be made absolute ({error}); using it as given"),
      );
      Some(path)
    },
  }
}

/// `DOCKER_HOST`: the Docker daemon endpoint the runner and the docker CLI use.
pub(crate) fn docker_host() -> Option<String> {
  read("DOCKER_HOST")
}

/// The session-conflict retry schedule: upstream's 30 s × 8 attempts, with
/// the interval overridable by `TOOLU_RUNNER_SESSION_CONFLICT_INTERVAL_MS`
/// (the attempt count is fixed). An empty value counts as unset; zero or a
/// non-integer is diagnosed and the default kept.
pub(crate) fn session_conflict_policy() -> SessionConflictPolicy {
  let Some(raw) = read(SESSION_CONFLICT_INTERVAL_KEY).filter(|raw| !raw.is_empty()) else {
    return SessionConflictPolicy::default();
  };
  if let Some(interval) = parse_interval_ms(&raw) {
    return SessionConflictPolicy {
      interval,
      ..SessionConflictPolicy::default()
    };
  }
  report(
    SESSION_CONFLICT_INTERVAL_KEY,
    "is not a positive integer of milliseconds; using the default 30 s",
  );
  SessionConflictPolicy::default()
}

/// Parse a positive whole number of milliseconds.
pub(crate) fn parse_interval_ms(raw: &str) -> Option<Duration> {
  raw
    .parse::<u64>()
    .ok()
    .filter(|ms| *ms > 0)
    .map(Duration::from_millis)
}

fn read(key: &str) -> Option<String> {
  match std::env::var(key) {
    Ok(value) => Some(value),
    Err(std::env::VarError::NotPresent) => None,
    Err(std::env::VarError::NotUnicode(_)) => {
      report_invalid_unicode(key);
      None
    },
  }
}

fn report_invalid_unicode(key: &str) {
  report(key, "is not valid Unicode; treating it as unset");
}

/// Diagnose a bad variable by name only (never its value): WARN once tracing
/// is up, stderr before.
fn report(key: &str, problem: &str) {
  if tracing::dispatcher::has_been_set() {
    tracing::warn!(variable = key, "environment variable {problem}");
  } else {
    eprintln!("toolu-runner: {key} {problem}");
  }
}

#[cfg(test)]
#[path = "tests/config.rs"]
mod tests;

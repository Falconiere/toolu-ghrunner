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

/// How long `remove` waits for `run --all` to stop a registration it asked
/// to drain (#208): 30 s, overridable in milliseconds by
/// `TOOLU_RUNNER_REMOVE_WAIT_MS` (tests use a short wait for the busy case).
/// An empty value counts as unset; zero or a non-integer is diagnosed and
/// the default kept.
pub(crate) fn remove_wait() -> Duration {
  const KEY: &str = "TOOLU_RUNNER_REMOVE_WAIT_MS";
  const DEFAULT: Duration = Duration::from_secs(30);
  let Some(raw) = read(KEY).filter(|raw| !raw.is_empty()) else {
    return DEFAULT;
  };
  parse_interval_ms(&raw).unwrap_or_else(|| {
    report(
      KEY,
      "is not a positive integer of milliseconds; using the default 30 s",
    );
    DEFAULT
  })
}

/// How long a toolu.sh machine's poll loop waits after an empty queue (#206):
/// 5 s, overridable in milliseconds by `TOOLU_RUNNER_POLL_INTERVAL_MS` (tests
/// poll faster). An empty value counts as unset; zero or a non-integer is
/// diagnosed and the default kept.
pub(crate) fn poll_interval() -> Duration {
  duration_ms("TOOLU_RUNNER_POLL_INTERVAL_MS", Duration::from_secs(5))
}

/// How long `setup` polls GitHub for the runner to report `online`:
/// 60 s, overridable in milliseconds by `TOOLU_RUNNER_ONLINE_TIMEOUT_MS`
/// (tests shorten it). An empty value counts as unset; zero or a
/// non-integer is diagnosed and the default kept.
pub(crate) fn online_timeout() -> Duration {
  duration_ms("TOOLU_RUNNER_ONLINE_TIMEOUT_MS", Duration::from_secs(60))
}

/// `NO_COLOR`: any non-empty value asks `setup` for plain output.
pub(crate) fn no_color() -> bool {
  read("NO_COLOR").is_some_and(|value| !value.is_empty())
}

/// Whether this session arrived over SSH (`SSH_CONNECTION` or `SSH_TTY`
/// set non-empty): a headless signal for `setup`'s machine check.
pub(crate) fn ssh_session() -> bool {
  ["SSH_CONNECTION", "SSH_TTY"]
    .iter()
    .any(|key| read(key).is_some_and(|value| !value.is_empty()))
}

/// Whether a graphical display is reachable (`DISPLAY` or
/// `WAYLAND_DISPLAY` set non-empty): the opposite headless signal.
pub(crate) fn display_present() -> bool {
  ["DISPLAY", "WAYLAND_DISPLAY"]
    .iter()
    .any(|key| read(key).is_some_and(|value| !value.is_empty()))
}

/// Where `doctor` reads the latest release when `TOOLU_RUNNER_RELEASES_URL`
/// is unset: this repository's latest GitHub release.
const DEFAULT_RELEASES_URL: &str =
  "https://api.github.com/repos/Falconiere/toolu-ghrunner/releases/latest";

/// `TOOLU_RUNNER_RELEASES_URL`: the GitHub-API-shaped release document
/// `doctor` compares the build against (a mirror, a fork, or a test peer).
/// An empty value counts as unset.
pub(crate) fn releases_url() -> String {
  read("TOOLU_RUNNER_RELEASES_URL")
    .filter(|raw| !raw.is_empty())
    .unwrap_or_else(|| DEFAULT_RELEASES_URL.to_owned())
}

/// The REST API base `create-app` talks to when `TOOLU_RUNNER_GITHUB_API_URL`
/// is unset (#202: github.com only).
const DEFAULT_GITHUB_API_URL: &str = "https://api.github.com";

/// How long `create-app` waits for the App's installation before asking
/// (interactive) or giving up (unattended): 10 minutes.
const DEFAULT_INSTALL_WAIT: Duration = Duration::from_secs(600);

/// How often `create-app` polls `GET /app/installations`.
const DEFAULT_INSTALL_POLL: Duration = Duration::from_secs(5);

/// `TOOLU_RUNNER_GITHUB_API_URL`: the GitHub REST API base `create-app` uses
/// for the org check, the manifest conversion and the App calls (a test peer
/// or a mirror). Empty counts as unset; a trailing `/` is dropped.
pub(crate) fn github_api_url() -> String {
  api_base_or_default(read("TOOLU_RUNNER_GITHUB_API_URL"))
}

/// `raw` without a trailing `/`, or [`DEFAULT_GITHUB_API_URL`] when it is
/// unset or empty.
pub(crate) fn api_base_or_default(raw: Option<String>) -> String {
  raw
    .map(|raw| raw.trim_end_matches('/').to_owned())
    .filter(|raw| !raw.is_empty())
    .unwrap_or_else(|| DEFAULT_GITHUB_API_URL.to_owned())
}

/// The get.toolu.sh relay `create-app` uses on a headless box when
/// `TOOLU_RUNNER_RELAY_URL` is unset (#205, CodaSignal/toolu.sh#88).
const DEFAULT_RELAY_URL: &str = "https://get.toolu.sh";

/// `TOOLU_RUNNER_RELAY_URL`: the relay origin a headless `create-app`
/// prints its form link under and claims the manifest code from (a test
/// peer). Empty counts as unset; a trailing `/` is dropped.
pub(crate) fn relay_url() -> String {
  read("TOOLU_RUNNER_RELAY_URL")
    .map(|raw| raw.trim_end_matches('/').to_owned())
    .filter(|raw| !raw.is_empty())
    .unwrap_or_else(|| DEFAULT_RELAY_URL.to_owned())
}

/// The toolu.sh API origin `setup` pairs with when
/// `TOOLU_RUNNER_TOOLU_API_URL` is unset (#207).
const DEFAULT_TOOLU_API_URL: &str = "https://console.toolu.sh";

/// `TOOLU_RUNNER_TOOLU_API_URL`: the toolu.sh origin `setup` pairs with and
/// records as the machine's `api_url` (the dev stack, or a test peer). Empty
/// counts as unset; a trailing `/` is dropped.
pub(crate) fn toolu_api_url() -> String {
  read("TOOLU_RUNNER_TOOLU_API_URL")
    .map(|raw| raw.trim_end_matches('/').to_owned())
    .filter(|raw| !raw.is_empty())
    .unwrap_or_else(|| DEFAULT_TOOLU_API_URL.to_owned())
}

/// `TOOLU_RUNNER_ENROLL_TOKEN`: `setup`'s enrollment token once the source is
/// toolu.sh (`--source toolu` or the prompt) and `--enroll-token` is not given (keeps the single-use `tlu_e_` token out
/// of `ps` and shell history). Empty counts as unset.
pub(crate) fn enroll_token() -> Option<String> {
  read("TOOLU_RUNNER_ENROLL_TOKEN").filter(|raw| !raw.is_empty())
}

/// `TOOLU_RUNNER_INSTALL_WAIT_MS`: how long `create-app` waits for the
/// installation before asking or giving up (default 10 minutes).
pub(crate) fn install_wait() -> Duration {
  duration_ms("TOOLU_RUNNER_INSTALL_WAIT_MS", DEFAULT_INSTALL_WAIT)
}

/// `TOOLU_RUNNER_INSTALL_POLL_MS`: how often `create-app` polls for the
/// installation (default 5 s).
pub(crate) fn install_poll() -> Duration {
  duration_ms("TOOLU_RUNNER_INSTALL_POLL_MS", DEFAULT_INSTALL_POLL)
}

/// A positive millisecond duration from `key`, else `default`. Empty counts
/// as unset; zero or a non-integer is diagnosed and the default kept.
fn duration_ms(key: &str, default: Duration) -> Duration {
  ms_or_default(read(key).as_deref(), default).unwrap_or_else(|| {
    report(
      key,
      "is not a positive integer of milliseconds; using the default",
    );
    default
  })
}

/// `raw` as a positive millisecond duration; `default` when unset or empty;
/// `None` when it is set but not a positive integer.
pub(crate) fn ms_or_default(raw: Option<&str>, default: Duration) -> Option<Duration> {
  match raw.filter(|raw| !raw.is_empty()) {
    None => Some(default),
    Some(raw) => parse_interval_ms(raw),
  }
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

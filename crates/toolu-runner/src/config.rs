//! Environment-derived configuration for the CLI bin.

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

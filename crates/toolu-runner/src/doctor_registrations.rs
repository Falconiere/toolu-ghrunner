//! `doctor` per-registration rows: whether the config, credentials and JIT
//! config load, whether a `run` is serving it (from the lock body — never by
//! taking the lock), its supervisor unit, a pending removal, and the last job
//! from its journal.
//!
//! Everything here is read-only: paths are expanded with
//! `shared::paths::expand_tilde`, never `config::resolve_data_dir` (which
//! creates the directory).

use std::path::{Path, PathBuf};
use std::time::Duration;

use config::config::{RunnerRegistrationConfig, load_config, load_credentials};
use config::lockfile::{LockBody, pid_start_time};
use shared::RunnerError;
use shared::paths::expand_tilde;

use crate::doctor_report::{Level, Row};

/// The fix for anything only a fresh registration repairs.
const REREGISTER: &str = "re-register: toolu-runner register --replace --url <repo url>";
/// How long to wait before re-reading an empty lock body: `run` truncates
/// the file and then writes the body, so a read can land in between.
const BODY_SETTLE: Duration = Duration::from_millis(100);
/// Clock slack when comparing a holder's start time with the body's
/// `started_at` (whole-second start times, clock adjustments).
const START_SLACK_SECS: u64 = 60;

/// One registration's report block, plus what later sections need from it.
pub(crate) struct Checked {
  /// The `owner/repo (mode, runner name):` sub-heading.
  pub(crate) heading: String,
  /// Its rows, in report order.
  pub(crate) rows: Vec<Row>,
  /// Its work dir (`~` expanded, not created), when the config parsed.
  pub(crate) work_dir: Option<PathBuf>,
  /// A supervisor unit file exists for it.
  pub(crate) unit_installed: bool,
  /// The parsed config, when it loaded.
  pub(crate) config: Option<RunnerRegistrationConfig>,
}

/// Whether a `run` holds the registration, from the `.lock` body alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunState {
  /// The body's PID is alive and started before the body was written.
  Active {
    /// The holder's PID.
    pid: u32,
    /// When it took the lock.
    since: String,
  },
  /// No body, or its PID is gone or now names a later process (reused).
  Stopped,
  /// A body that does not parse.
  Unreadable(String),
}

/// `owner/repo` for a `runners/<owner>/<repo>/config.toml`, else the path.
fn label(config_path: &Path) -> String {
  crate::service_cmd::owner_repo(config_path).map_or_else(
    || config_path.display().to_string(),
    |(owner, repo)| format!("{owner}/{repo}"),
  )
}

/// The body's PID is alive and its process started no later than the body
/// was written (a reboot can hand the PID to an unrelated, later process).
/// An unparseable `started_at` falls back to liveness alone. Known limit: a
/// wall clock stepped forward by more than [`START_SLACK_SECS`] after `run`
/// wrote the body (a host without an RTC syncing late) reads as reused.
pub(crate) fn holder_alive(body: &LockBody) -> bool {
  let Some(started) = pid_start_time(body.pid) else {
    return false;
  };
  match chrono::DateTime::parse_from_rfc3339(&body.started_at) {
    Ok(at) => u64::try_from(at.timestamp())
      .is_ok_and(|written| started <= written.saturating_add(START_SLACK_SECS)),
    Err(_) => true,
  }
}

/// Read the lock body at `lock` without opening it for locking; an empty
/// body is re-read once after [`BODY_SETTLE`].
pub(crate) async fn run_state(lock: &Path) -> RunState {
  let read = || match std::fs::read_to_string(lock) {
    Ok(raw) => Ok(raw),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(RunState::Stopped),
    Err(error) => Err(RunState::Unreadable(error.to_string())),
  };
  let mut raw = match read() {
    Ok(raw) => raw,
    Err(state) => return state,
  };
  if raw.trim().is_empty() {
    tokio::time::sleep(BODY_SETTLE).await;
    raw = match read() {
      Ok(raw) => raw,
      Err(state) => return state,
    };
  }
  match serde_json::from_str::<LockBody>(&raw) {
    Ok(body) if holder_alive(&body) => RunState::Active {
      pid: body.pid,
      since: body.started_at,
    },
    Ok(_) => RunState::Stopped,
    Err(error) => RunState::Unreadable(error.to_string()),
  }
}

/// `message` with every serde-quoted string value (`string "…"`) cut out: a
/// mistyped field can hold a secret the masker never learned, because the
/// file that would name it did not load.
pub(crate) fn without_quoted_strings(message: &str) -> String {
  const MARK: &str = "string \"";
  let mut out = String::with_capacity(message.len());
  let mut rest = message;
  while let Some((head, tail)) = rest
    .find(MARK)
    .and_then(|at| rest.split_at_checked(at + MARK.len()))
  {
    out.push_str(head);
    out.push_str("…\"");
    // Skip the Debug-escaped value through its closing quote; without one,
    // drop the remainder rather than risk echoing it.
    let mut escaped = false;
    let mut end = tail.len();
    for (index, c) in tail.char_indices() {
      match c {
        _ if escaped => escaped = false,
        '\\' => escaped = true,
        '"' => {
          end = index + 1;
          break;
        },
        _ => {},
      }
    }
    rest = tail.get(end..).unwrap_or_default();
  }
  out.push_str(rest);
  out
}

/// A parse error fit for a shared report: no source excerpt and no quoted
/// string values.
fn shareable(error: &str) -> String {
  without_quoted_strings(&without_source_excerpt(error))
}

/// `error` without the source excerpt a TOML parse error quotes (the
/// `N | <line>` gutter lines): a hand-edited `config.toml` line can hold a
/// secret, such as an unquoted proxy password, and this report is meant to
/// be shared. The location and the message stay.
pub(crate) fn without_source_excerpt(error: &str) -> String {
  error
    .lines()
    .filter(|line| {
      !line
        .trim_start()
        .trim_start_matches(|c: char| c.is_ascii_digit())
        .trim_start()
        .starts_with('|')
    })
    .map(str::trim)
    .filter(|line| !line.is_empty())
    .collect::<Vec<_>>()
    .join("; ")
}

/// The `run` row: a stopped persistent registration fails (its jobs queue
/// forever), a stopped ephemeral one only warns.
pub(crate) fn run_row(state: &RunState, ephemeral: bool, config_path: &Path) -> Row {
  let detail = match state {
    RunState::Active { pid, since } => {
      return Row::new(
        Level::Ok,
        "run",
        format!("active (pid {pid}, since {since})"),
      );
    },
    RunState::Stopped => "not running".to_owned(),
    RunState::Unreadable(error) => {
      format!("lock body unreadable ({error}); treated as not running")
    },
  };
  let config = config_path.display();
  if ephemeral {
    Row::new(Level::Warn, "run", detail).with_fix(format!("toolu-runner run --config {config}"))
  } else {
    Row::new(Level::Fail, "run", detail)
      .with_fix(format!("toolu-runner install-service --config {config}"))
  }
}

/// The `creds` row for a present credentials file with unix `mode`.
pub(crate) fn creds_mode_row(path: &Path, mode: u32) -> Row {
  let perms = mode & 0o777;
  // No group or other bit set: the low six bits are all zero.
  if perms.trailing_zeros() >= 6 {
    return Row::new(Level::Ok, "creds", format!("present ({perms:04o})"));
  }
  Row::new(
    Level::Warn,
    "creds",
    format!("mode {perms:04o}: readable beyond the owner"),
  )
  .with_fix(format!("chmod 600 {}", path.display()))
}

/// The `creds` row: missing or unparseable fails, a loose mode warns.
fn creds_row(path: &Path) -> Row {
  if !path.exists() {
    return Row::new(Level::Fail, "creds", format!("missing {}", path.display()))
      .with_fix(REREGISTER);
  }
  if let Err(error) = load_credentials(path) {
    return Row::new(
      Level::Fail,
      "creds",
      format!("unreadable: {}", shareable(&error.to_string())),
    )
    .with_fix(REREGISTER);
  }
  match std::fs::metadata(path) {
    Ok(meta) => creds_mode_row(path, mode_of(&meta)),
    Err(error) => Row::new(Level::Warn, "creds", format!("cannot stat: {error}")),
  }
}

#[cfg(unix)]
fn mode_of(meta: &std::fs::Metadata) -> u32 {
  use std::os::unix::fs::PermissionsExt as _;
  meta.permissions().mode()
}

#[cfg(not(unix))]
fn mode_of(_meta: &std::fs::Metadata) -> u32 {
  0o600
}

/// The `last job` row from the newest journal in `jobs_dir`.
pub(crate) fn last_job_row(jobs_dir: &Path) -> Row {
  match observability::journal::reader::scan_jobs(jobs_dir) {
    Ok(jobs) => match jobs.first() {
      Some(job) => {
        let conclusion = job
          .conclusion
          .as_deref()
          .unwrap_or("running or interrupted");
        let name = job.job_name.as_deref().unwrap_or(&job.job_id);
        Row::new(
          Level::Info,
          "last job",
          format!("{conclusion} — {name} at {}", job.started),
        )
      },
      None => Row::new(Level::Info, "last job", "no jobs yet"),
    },
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
      Row::new(Level::Info, "last job", "no jobs yet")
    },
    Err(error) => Row::new(
      Level::Warn,
      "last job",
      format!("cannot read {}: {error}", jobs_dir.display()),
    ),
  }
}

/// The block for a registration whose `config.toml` does not load.
fn unloadable(config_path: &Path, error: &RunnerError) -> Checked {
  Checked {
    heading: format!("  {}:", label(config_path)),
    rows: vec![
      Row::new(
        Level::Fail,
        "config",
        format!(
          "cannot load {}: {}",
          config_path.display(),
          shareable(&error.to_string())
        ),
      )
      .with_fix(REREGISTER),
    ],
    work_dir: None,
    unit_installed: false,
    config: None,
  }
}

/// The `jit` row: the stored JIT envelope must decode to its service URLs.
fn jit_row(config: &RunnerRegistrationConfig) -> Row {
  match crate::check_endpoints::jit_endpoints(&config.runtime.jit_config) {
    Ok(_) => Row::new(
      Level::Ok,
      "jit",
      format!("parses ({} bytes)", config.runtime.jit_config.len()),
    ),
    Err(error) => Row::new(Level::Fail, "jit", format!("unreadable: {error}")).with_fix(REREGISTER),
  }
}

/// The `pending` row when a `.pending_remove` marker sits in `data_dir`.
fn pending_row(data_dir: &Path, config_path: &Path) -> Option<Row> {
  data_dir.join(".pending_remove").exists().then(|| {
    Row::new(
      Level::Warn,
      "pending",
      "removal pending: `run` exits at its next job boundary",
    )
    .with_fix(format!(
      "toolu-runner remove --config {}",
      config_path.display()
    ))
  })
}

/// Check one registration's local state, rows in report order (config,
/// creds, jit, run, service, pending, last job); supervisor queries are
/// bounded by `timeout`.
pub(crate) async fn check_registration(config_path: &Path, timeout: Duration) -> Checked {
  let config = match load_config(config_path) {
    Ok(config) => config,
    Err(error) => return unloadable(config_path, &error),
  };
  let mode = if config.ephemeral {
    "ephemeral"
  } else {
    "persistent"
  };
  let data_dir = expand_tilde(Path::new(&config.runtime.data_dir));
  let mut rows = vec![
    Row::new(Level::Ok, "config", config_path.display().to_string()),
    creds_row(&crate::cli::credentials_path_for(config_path)),
    jit_row(&config),
    run_row(
      &run_state(&data_dir.join(".lock")).await,
      config.ephemeral,
      config_path,
    ),
  ];
  let mut unit_installed = false;
  if let Some((row, installed)) =
    crate::doctor_service::service_row(config_path, config.ephemeral, timeout).await
  {
    rows.push(row);
    unit_installed = installed;
  }
  rows.extend(pending_row(&data_dir, config_path));
  rows.push(last_job_row(&data_dir.join("_diag").join("jobs")));
  Checked {
    heading: format!(
      "  {} ({mode}, runner {}):",
      label(config_path),
      config.runner_name
    ),
    rows,
    work_dir: Some(expand_tilde(Path::new(&config.runtime.work_dir))),
    unit_installed,
    config: Some(config),
  }
}

#[cfg(test)]
#[path = "tests/doctor_registrations.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/doctor_registrations_state.rs"]
mod state_tests;

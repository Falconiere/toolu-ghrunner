//! Tracing initialization for toolu-runner.
//!
//! `tracing-subscriber` + `EnvFilter` only. No OTel, no dotenvy walk.
//!
//! Two public entry points:
//! - [`init`] — plain tracing, no secret redaction.
//! - [`init_with_redactor`] — wraps every log line through a
//!   [`SecretRedactor`] before it reaches the file sink. This guarantees
//!   that registered secrets never land in `_diag/<service>.log`
//!   unredacted, satisfying the runner's secret-handling spec.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// Set 0o700 permissions on `path`. Failure is non-fatal (logged as warn).
/// Used after creating data directories to prevent other local users
/// from reading runner logs and state.
#[cfg(unix)]
fn set_dir_perms(path: &std::path::Path) {
  if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)) {
    tracing::warn!(error = %e, path = %path.display(), "failed to set 0o700 permissions");
  }
}

#[cfg(not(unix))]
fn set_dir_perms(_path: &std::path::Path) {}

use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Pluggable secret redactor used by the tracing layer.
///
/// The runner provides an implementation that masks registered secrets
/// (typically `toolu_runner::execution::SecretMasker`). Each completed log
/// line is passed to [`SecretRedactor::redact`] before being written to the
/// underlying sink.
///
/// The trait is `Send + Sync` so a single instance can be shared between
/// the stderr and file subscribers without locking.
pub trait SecretRedactor: Send + Sync {
  /// Return a redacted copy of `line`. Implementations should be pure
  /// (no side effects) so they can run on the tracing thread.
  fn redact(&self, line: &str) -> String;
}

/// Default data directory under the user's home.
fn default_data_dir() -> PathBuf {
  crate::paths::expand_tilde(Path::new("~/.toolu-runner"))
}

/// Diagnostics directory: `data_dir/_diag/`.
fn diag_dir(data_dir: &Path) -> PathBuf {
  data_dir.join("_diag")
}

/// Delete rolled log files older than 14 days. Failure is non-fatal
/// (logged as warn). Called on startup after the diag dir is created.
fn cleanup_old_logs(diag: &Path, service: &str) {
  let prefix = format!("{service}.log.");
  let Ok(entries) = std::fs::read_dir(diag) else {
    return;
  };
  for entry in entries.flatten() {
    let path = entry.path();
    let fname = match path.file_name().and_then(|n| n.to_str()) {
      Some(n) if n.starts_with(&prefix) => n,
      _ => continue,
    };
    let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) else {
      continue;
    };
    let Ok(age) = mtime.elapsed() else {
      continue;
    };
    if age > chrono::TimeDelta::days(14).to_std().unwrap_or_default()
      && let Err(e) = std::fs::remove_file(&path)
    {
      tracing::warn!(error = %e, path = %fname, "failed to remove old log");
    }
  }
}

/// Initialize tracing for the runner.
///
/// Loads `.env` from `manifest_dir` (no-op if the file is absent), resolves
/// the data dir, and attaches:
/// - a `tracing_subscriber::fmt` layer writing pretty lines to stderr,
/// - a `tracing_subscriber::fmt` JSON layer writing to
///   `data_dir/_diag/<service>.log` (created if absent).
///
/// `EnvFilter` is built from `RUST_LOG` first, then `TOOLU_RUNNER_LOG`,
/// falling back to `info`.
///
/// Uses `try_init` so duplicate calls (e.g. in tests) are silently ignored.
///
/// # Errors
///
/// Returns `RunnerError::Io` if the diagnostics directory cannot be created.
pub fn init(manifest_dir: &str, service: &str) -> Result<(), crate::RunnerError> {
  load_dotenv(Path::new(manifest_dir));

  let data_dir = default_data_dir();
  let diag = diag_dir(&data_dir);
  std::fs::create_dir_all(&diag).map_err(|source| crate::RunnerError::WorkspaceInit {
    path: diag.clone(),
    source,
  })?;
  set_dir_perms(&data_dir);
  set_dir_perms(&diag);
  cleanup_old_logs(&diag, service);

  let file_appender = tracing_appender::rolling::daily(&diag, format!("{service}.log"));

  let env_filter = build_env_filter();

  let stderr_layer = tracing_subscriber::fmt::layer()
    .with_writer(std::io::stderr)
    .with_target(true)
    .with_thread_names(false)
    .compact();

  let file_layer = tracing_subscriber::fmt::layer()
    .with_writer(file_appender)
    .with_target(true)
    .with_thread_names(true)
    .json();

  tracing_subscriber::registry()
    .with(env_filter)
    .with(stderr_layer)
    .with(file_layer)
    .try_init()
    .ok();

  Ok(())
}

/// Initialize tracing with a [`SecretRedactor`] wrapping both sinks.
///
/// Every line written by `tracing-subscriber`'s fmt layer is passed
/// through `redactor.redact(line)` before reaching stderr or the
/// diagnostics file. This is the only safe way to satisfy the runner's
/// "no secret ever reaches `_diag/runner.log` unredacted" spec: the file
/// writer is wrapped at the byte level, so secrets are gone from disk
/// regardless of how the upstream log line was produced.
///
/// The redactor is shared by both layers via an `Arc`, so a single
/// instance can hold the registered secrets.
///
/// # Errors
///
/// Returns `RunnerError::WorkspaceInit` if the diagnostics directory
/// cannot be created.
pub fn init_with_redactor(
  manifest_dir: &str,
  service: &str,
  redactor: Arc<dyn SecretRedactor>,
) -> Result<(), crate::RunnerError> {
  load_dotenv(Path::new(manifest_dir));

  let data_dir = default_data_dir();
  let diag = diag_dir(&data_dir);
  std::fs::create_dir_all(&diag).map_err(|source| crate::RunnerError::WorkspaceInit {
    path: diag.clone(),
    source,
  })?;
  set_dir_perms(&data_dir);
  set_dir_perms(&diag);
  cleanup_old_logs(&diag, service);

  let file_appender = tracing_appender::rolling::daily(&diag, format!("{service}.log"));

  let env_filter = build_env_filter();

  let shared_redactor = redactor;

  let stderr_layer = tracing_subscriber::fmt::layer()
    .with_writer(RedactingMakeWriter::new(
      std::io::stderr,
      Arc::clone(&shared_redactor),
    ))
    .with_target(true)
    .with_thread_names(false)
    .compact();

  let file_layer = tracing_subscriber::fmt::layer()
    .with_writer(RedactingMakeWriter::new(
      file_appender,
      Arc::clone(&shared_redactor),
    ))
    .with_target(true)
    .with_thread_names(true)
    .json();

  tracing_subscriber::registry()
    .with(env_filter)
    .with(stderr_layer)
    .with(file_layer)
    .try_init()
    .ok();

  Ok(())
}

/// Build the `EnvFilter` from `TOOLU_RUNNER_LOG` → `RUST_LOG` → `info`.
///
/// By default, the filter is capped at `info` to prevent runaway debug/trace
/// log output from leaking secrets to the file sink. Set
/// `TOOLU_RUNNER_ALLOW_VERBOSE=1` to honor the full env-var level.
fn build_env_filter() -> EnvFilter {
  if std::env::var("TOOLU_RUNNER_ALLOW_VERBOSE")
    .map(|v| v == "1")
    .unwrap_or(false)
  {
    EnvFilter::try_from_env("TOOLU_RUNNER_LOG")
      .or_else(|_| EnvFilter::try_from_env("RUST_LOG"))
      .unwrap_or_else(|_| EnvFilter::new("info"))
  } else {
    EnvFilter::new("info")
  }
}

/// Load a `.env` file from `manifest_dir` (no-op if absent).
fn load_dotenv(manifest_dir: &Path) {
  let path = manifest_dir.join(".env");
  if !path.exists() {
    return;
  }
  match dotenvy::from_path(&path) {
    Ok(()) => tracing::info!(".env loaded from {}", path.display()),
    Err(e) => eprintln!("warning: could not load {}: {e}", path.display()),
  }
}

/// A [`MakeWriter`] that wraps an inner writer and routes every completed
/// line through a [`SecretRedactor`] before flushing to disk/stdout.
pub struct RedactingMakeWriter<W> {
  inner: W,
  redactor: Arc<dyn SecretRedactor>,
}

impl<W> RedactingMakeWriter<W> {
  /// Wrap an inner `MakeWriter` so all output passes through `redactor`.
  pub fn new(inner: W, redactor: Arc<dyn SecretRedactor>) -> Self {
    Self { inner, redactor }
  }
}

impl<'a, W> tracing_subscriber::fmt::MakeWriter<'a> for RedactingMakeWriter<W>
where
  W: tracing_subscriber::fmt::MakeWriter<'a>,
{
  type Writer = RedactingWriter<W::Writer>;

  fn make_writer(&'a self) -> Self::Writer {
    RedactingWriter::new(self.inner.make_writer(), Arc::clone(&self.redactor))
  }
}

/// A [`Write`] adapter that buffers bytes, splits on newlines, and runs
/// each completed line through a [`SecretRedactor`] before writing it to
/// the inner writer.
///
/// `tracing-subscriber`'s fmt layer writes one event per `write` call,
/// ending with `\n`. We drain complete lines eagerly so secrets never
/// sit in the buffer longer than the single write that introduced them.
pub struct RedactingWriter<W> {
  inner: W,
  buffer: Vec<u8>,
  redactor: Arc<dyn SecretRedactor>,
}

impl<W: Write> Write for RedactingWriter<W> {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    self.buffer.extend_from_slice(buf);
    self.drain_complete_lines()?;
    Ok(buf.len())
  }

  fn flush(&mut self) -> io::Result<()> {
    self.drain_complete_lines()?;
    if !self.buffer.is_empty() {
      let pending = String::from_utf8_lossy(&self.buffer).into_owned();
      self.buffer.clear();
      let redacted = self.redactor.redact(&pending);
      self.inner.write_all(redacted.as_bytes())?;
    }
    self.inner.flush()
  }
}

impl<W: Write> RedactingWriter<W> {
  /// Build a redacting writer around `inner`.
  pub fn new(inner: W, redactor: Arc<dyn SecretRedactor>) -> Self {
    Self {
      inner,
      buffer: Vec::new(),
      redactor,
    }
  }

  /// Drain any buffered partial line and return the inner writer.
  ///
  /// Test-only helper — production code never calls this, but exposing
  /// it keeps the redaction test pure (it inspects a `Vec<u8>` sink).
  ///
  /// # Errors
  ///
  /// Returns any `io::Error` from flushing the buffered redacted line
  /// or from the underlying writer's `flush`.
  pub fn into_inner(mut self) -> io::Result<W> {
    self.flush()?;
    Ok(self.inner)
  }

  fn drain_complete_lines(&mut self) -> io::Result<()> {
    while let Some(pos) = self.buffer.iter().position(|b| *b == b'\n') {
      let line_bytes: Vec<u8> = self.buffer.drain(..=pos).collect();
      let line = String::from_utf8_lossy(&line_bytes);
      let redacted = self.redactor.redact(&line);
      self.inner.write_all(redacted.as_bytes())?;
    }
    Ok(())
  }
}

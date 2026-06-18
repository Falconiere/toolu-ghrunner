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
  if let Some(home) = std::env::var_os("HOME") {
    return PathBuf::from(home).join(".toolu-runner");
  }
  if let Some(profile) = std::env::var_os("USERPROFILE") {
    return PathBuf::from(profile).join(".toolu-runner");
  }
  PathBuf::from("/var/lib/toolu-runner")
}

/// Diagnostics directory: `data_dir/_diag/`.
fn diag_dir(data_dir: &Path) -> PathBuf {
  data_dir.join("_diag")
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
/// Before the tracing layer is attached, scans the process environment for
/// any `YAMLESS_*` keys (the deprecated prefix from a previous runner) and
/// emits a `tracing::warn!` for each. Users re-running an old shell profile
/// see a clear "this var is no longer recognized" message instead of silent
/// failure. Does not fail; just warns.
///
/// Uses `try_init` so duplicate calls (e.g. in tests) are silently ignored.
///
/// # Errors
///
/// Returns `RunnerError::Io` if the diagnostics directory cannot be created.
pub fn init(manifest_dir: &str, service: &str) -> Result<(), crate::RunnerError> {
  load_dotenv(Path::new(manifest_dir));

  warn_about_legacy_env();

  let data_dir = default_data_dir();
  let diag = diag_dir(&data_dir);
  std::fs::create_dir_all(&diag).map_err(|source| crate::RunnerError::WorkspaceInit {
    path: diag.clone(),
    source,
  })?;

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

/// Scan the process environment for any deprecated `YAMLESS_*` keys and
/// emit a `tracing::warn!` for each. Users get a clear signal that their
/// old env vars are no longer recognized (re-register with the new CLI).
///
/// Called from [`init`] / [`init_with_redactor`] before the subscriber is
/// installed, so the warning still surfaces even if the first `tracing`
/// event would otherwise be lost. Each key is written to stderr directly
/// (via `eprintln!`) so the warning is visible whether or not the
/// subscriber attaches successfully.
///
/// Returns the list of keys that were warned about — tests can assert
/// the result directly without depending on capturing stderr.
///
/// Public so the warning logic is testable in isolation — the canonical
/// test is in `toolu-runner/tests/failure_modes_test.rs`.
pub fn warn_about_legacy_env() -> Vec<String> {
  let legacy_keys = scan_legacy_env(std::env::vars());
  for key in &legacy_keys {
    eprintln!(
      "warning: ignoring legacy env var {key} — toolu-runner has no compatibility layer for the old prefix; use TOOLU_RUNNER_* instead"
    );
  }
  legacy_keys
}

/// Pure scan: return the subset of `env` whose keys start with `YAMLESS_`,
/// sorted for deterministic output. Extracted so tests can verify the
/// filter logic without mutating the process environment.
pub fn scan_legacy_env<I, K, V>(env: I) -> Vec<String>
where
  I: IntoIterator<Item = (K, V)>,
  K: AsRef<str>,
{
  let mut keys: Vec<String> = env
    .into_iter()
    .filter_map(|(k, _)| {
      if k.as_ref().starts_with("YAMLESS_") {
        Some(k.as_ref().to_owned())
      } else {
        None
      }
    })
    .collect();
  keys.sort();
  keys
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
  redactor: Box<dyn SecretRedactor>,
) -> Result<(), crate::RunnerError> {
  load_dotenv(Path::new(manifest_dir));

  let data_dir = default_data_dir();
  let diag = diag_dir(&data_dir);
  std::fs::create_dir_all(&diag).map_err(|source| crate::RunnerError::WorkspaceInit {
    path: diag.clone(),
    source,
  })?;

  let file_appender = tracing_appender::rolling::daily(&diag, format!("{service}.log"));

  let env_filter = build_env_filter();

  let shared_redactor = Arc::from(redactor);

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
fn build_env_filter() -> EnvFilter {
  EnvFilter::try_from_env("TOOLU_RUNNER_LOG")
    .or_else(|_| EnvFilter::try_from_env("RUST_LOG"))
    .unwrap_or_else(|_| EnvFilter::new("info"))
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

#[cfg(test)]
mod tests {
  use super::*;

  /// Minimal redactor used by the unit tests below.
  struct LiteralRedactor(&'static str, &'static str);

  impl SecretRedactor for LiteralRedactor {
    fn redact(&self, line: &str) -> String {
      line.replace(self.0, self.1)
    }
  }

  #[test]
  fn redacting_writer_replaces_secret_in_complete_line() {
    let redactor = Arc::new(LiteralRedactor("hunter2", "***"));
    let mut writer = RedactingWriter::new(Vec::<u8>::new(), redactor);
    writeln!(writer, "user logged in password=hunter2").unwrap();
    writer.flush().unwrap();
    let out = String::from_utf8(writer.into_inner().unwrap()).unwrap();
    assert!(!out.contains("hunter2"), "secret leaked: {out}");
    assert!(out.contains("password=***"), "expected redaction in: {out}");
  }

  #[test]
  fn redacting_writer_replaces_secret_split_across_writes() {
    let redactor = Arc::new(LiteralRedactor("hunter2", "***"));
    let mut writer = RedactingWriter::new(Vec::<u8>::new(), redactor);
    write!(writer, "first half ").unwrap();
    writeln!(writer, "password=hunter2").unwrap();
    writer.flush().unwrap();
    let out = String::from_utf8(writer.into_inner().unwrap()).unwrap();
    assert!(!out.contains("hunter2"), "secret leaked: {out}");
    assert!(out.contains("password=***"), "expected redaction in: {out}");
  }
}

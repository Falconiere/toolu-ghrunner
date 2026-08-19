//! Tests for the subscriber filter — driven through a real
//! `tracing_subscriber` writing real formatted output into a buffer, never by
//! reading the filter's directives back as a string. The question is what
//! reaches the journal, and only an installed subscriber answers it.

use std::io;
use std::sync::{Arc, Mutex, PoisonError};

use tracing_subscriber::fmt::MakeWriter;

use super::log_filter;

/// Stands in for `TOOLU_JITCONFIG` — a single-use GitHub credential. bollard
/// puts the whole `docker create` body, `Env` array included, in one `TRACE`
/// line, so this is the exact text that would reach the journal.
const JIT_CONFIG: &str = "eyJydW5uZXIiOiJ0b29sdS0xIiwidG9rZW4iOiJzZWNyZXQtaml0In0=";

/// A line this daemon logs about itself at `info` — present in every capture
/// below, which is what proves an empty assertion elsewhere means "filtered
/// out" rather than "the subscriber never ran".
const SERVING_LINE: &str = "toolu-daemon is serving";

/// A line this daemon logs about itself at `trace`, to show `RUST_LOG` is
/// still honoured for everything that is not bollard.
const TICK_LINE: &str = "reconcile tick starting";

/// Everything a subscriber wrote, shared between the writer the subscriber
/// owns and the test that reads it back.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
  /// The captured output as text.
  fn text(&self) -> String {
    let bytes = self.0.lock().unwrap_or_else(PoisonError::into_inner);
    String::from_utf8_lossy(&bytes).into_owned()
  }
}

impl io::Write for Captured {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    self
      .0
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .extend_from_slice(buf);
    Ok(buf.len())
  }

  fn flush(&mut self) -> io::Result<()> {
    Ok(())
  }
}

impl MakeWriter<'_> for Captured {
  type Writer = Self;

  fn make_writer(&self) -> Self::Writer {
    self.clone()
  }
}

/// Install the filter `requested` produces on a real subscriber, emit the
/// four events that matter, and return everything that made it through.
///
/// The bollard events are the real shapes bollard emits — `trace!("request:
/// {bytes:?}")` for the body it is about to send, `debug!` for the endpoint —
/// under bollard's own target.
fn logs_under(requested: Option<&str>) -> String {
  let captured = Captured::default();
  let subscriber = tracing_subscriber::fmt()
    .with_env_filter(log_filter(requested))
    .with_writer(captured.clone())
    .with_ansi(false)
    .finish();

  tracing::subscriber::with_default(subscriber, || {
    emit_bollards_lines();
    emit_this_daemons_lines();
  });

  captured.text()
}

/// The two lines bollard emits per Docker call, under its own target: the
/// request body at `TRACE` — the one carrying the container's `Env`, and with
/// it the JIT credential — and the endpoint at `DEBUG`.
fn emit_bollards_lines() {
  tracing::trace!(target: "bollard", "request: b\"{{\\\"Env\\\":[\\\"TOOLU_JITCONFIG={JIT_CONFIG}\\\"]}}\"");
  tracing::debug!(target: "bollard", "POST /containers/create");
}

/// Two lines this crate logs about itself, one at each end of the range
/// `RUST_LOG` is meant to keep controlling.
fn emit_this_daemons_lines() {
  tracing::info!(target: "daemon", "{SERVING_LINE}");
  tracing::trace!(target: "daemon::docker", "{TICK_LINE}");
}

/// The whole reason this module exists: `RUST_LOG=trace` is what an operator
/// reaches for when a box will not start a container, and it must not be what
/// prints every job's single-use GitHub credential into the journal.
#[test]
fn a_trace_filter_never_lets_bollard_log_a_request_body() {
  let logged = logs_under(Some("trace"));

  assert!(
    !logged.contains(JIT_CONFIG),
    "RUST_LOG=trace leaked a JIT credential through bollard's request log:\n{logged}"
  );
  assert!(
    !logged.contains("POST /containers/create"),
    "bollard is pinned at info, so its debug lines stay out too:\n{logged}"
  );
  assert!(
    logged.contains(TICK_LINE),
    "RUST_LOG=trace must still turn this crate's own trace lines on:\n{logged}"
  );
  assert!(
    logged.contains(SERVING_LINE),
    "the subscriber has to be writing at all for the assertions above to mean anything:\n{logged}"
  );
}

/// The ceiling is appended last precisely so it beats an explicit directive
/// for the same target — the one spelling that would otherwise turn the leak
/// back on.
#[test]
fn an_explicit_bollard_trace_directive_is_overridden() {
  let logged = logs_under(Some("info,bollard=trace"));

  assert!(
    !logged.contains(JIT_CONFIG),
    "an explicit bollard=trace must not survive the pinned ceiling:\n{logged}"
  );
  assert!(logged.contains(SERVING_LINE), "…and the rest still logs");
}

/// The default when `RUST_LOG` is unset: this crate at `info`, nothing below
/// it, and bollard silent as ever.
#[test]
fn an_unset_rust_log_logs_this_crate_at_info() {
  let logged = logs_under(None);

  assert!(logged.contains(SERVING_LINE));
  assert!(
    !logged.contains(TICK_LINE),
    "trace is below the default:\n{logged}"
  );
  assert!(!logged.contains(JIT_CONFIG));
}

/// A mistyped `RUST_LOG` falls back to the default whole. Dropping only the
/// bad directive would leave a filter that parsed *something*, which gets no
/// default directive added — the daemon would then log nothing about itself,
/// which reads like a daemon that has stopped working.
#[test]
fn an_unusable_rust_log_still_logs_this_crate_at_info() {
  let logged = logs_under(Some("=(this is not a directive"));

  assert!(
    logged.contains(SERVING_LINE),
    "an unusable RUST_LOG must not silence the daemon:\n{logged}"
  );
  assert!(!logged.contains(JIT_CONFIG));
}

//! Tests for execution-engine environment reads and diagnostics.

use std::ffi::OsString;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const PROBE_MODE: &str = "TOOLU_TEST_EXECUTION_CONFIG_PROBE";
const PROBE_KEY: &str = "TOOLU_TEST_NON_UNICODE_VALUE";
const CHILD_TEST: &str = "config::tests::invalid_unicode_read_child_probe";

#[cfg(unix)]
fn invalid_unicode_value() -> OsString {
  use std::os::unix::ffi::OsStringExt;
  OsString::from_vec(vec![0xff])
}

#[cfg(windows)]
fn invalid_unicode_value() -> OsString {
  use std::os::windows::ffi::OsStringExt;
  OsString::from_wide(&[0xd800])
}

#[derive(Clone)]
struct EventCapture {
  events: Arc<Mutex<Vec<(tracing::Level, String, String)>>>,
  next_span_id: Arc<AtomicU64>,
}

impl Default for EventCapture {
  fn default() -> Self {
    Self {
      events: Arc::default(),
      next_span_id: Arc::new(AtomicU64::new(1)),
    }
  }
}

impl EventCapture {
  fn saw_warning(&self, variable: &str) -> bool {
    let Ok(events) = self.events.lock() else {
      return false;
    };
    let saw_message = events.iter().any(|(level, field, value)| {
      *level == tracing::Level::WARN
        && field == "message"
        && value.contains("environment variable is not valid Unicode; treating as unset")
    });
    let saw_variable = events.iter().any(|(level, field, value)| {
      *level == tracing::Level::WARN && field == "variable" && value.contains(variable)
    });
    saw_message && saw_variable
  }
}

impl tracing::Subscriber for EventCapture {
  fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
    true
  }

  fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
    tracing::span::Id::from_u64(self.next_span_id.fetch_add(1, Ordering::Relaxed))
  }

  fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

  fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

  fn event(&self, event: &tracing::Event<'_>) {
    struct Collect<'a> {
      level: tracing::Level,
      events: &'a Mutex<Vec<(tracing::Level, String, String)>>,
    }

    impl tracing::field::Visit for Collect<'_> {
      fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if let Ok(mut events) = self.events.lock() {
          events.push((self.level, field.name().to_owned(), format!("{value:?}")));
        }
      }
    }

    event.record(&mut Collect {
      level: *event.metadata().level(),
      events: &self.events,
    });
  }

  fn enter(&self, _span: &tracing::span::Id) {}

  fn exit(&self, _span: &tracing::span::Id) {}
}

#[test]
fn invalid_unicode_read_child_probe() {
  let Some(mode) = std::env::var_os(PROBE_MODE) else {
    return;
  };
  let capture = EventCapture::default();
  let value = {
    let _guard = tracing::subscriber::set_default(capture.clone());
    if mode == "path" {
      super::path()
    } else {
      super::var(PROBE_KEY)
    }
  };
  let variable = if mode == "path" { "PATH" } else { PROBE_KEY };
  assert!(value.is_none(), "invalid Unicode must be treated as unset");
  assert!(
    capture.saw_warning(variable),
    "invalid Unicode must emit a WARN naming {variable} without logging its value"
  );
}

#[test]
fn invalid_unicode_reads_warn_and_return_none() {
  let invalid = invalid_unicode_value();
  for (mode, key) in [("path", "PATH"), ("var", PROBE_KEY)] {
    let output =
      std::process::Command::new(std::env::current_exe().expect("resolve current test binary"))
        .args(["--exact", CHILD_TEST, "--nocapture"])
        .env(PROBE_MODE, mode)
        .env(key, invalid.as_os_str())
        .output()
        .expect("run execution config child probe");
    assert!(
      output.status.success(),
      "{mode} child probe failed: {}",
      String::from_utf8_lossy(&output.stderr)
    );
  }
}

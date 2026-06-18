//! End-to-end test for `shared::startup::init_with_redactor`.
//!
//! Drives the full redaction pipeline: a test redactor that replaces
//! `hunter2` with `***`, fed through `RedactingWriter`, and writes a
//! fake log line containing the secret. Asserts the captured output
//! does not contain the secret.

use std::io::Write;
use std::sync::Arc;

use shared::startup::{RedactingWriter, SecretRedactor};

/// Test redactor that swaps `hunter2` for `***`. Mirrors the structure
/// `toolu_runner::execution::SecretMasker` would use when wired into
/// the tracing layer.
struct LiteralRedactor {
  from: &'static str,
  to: &'static str,
}

impl SecretRedactor for LiteralRedactor {
  fn redact(&self, line: &str) -> String {
    line.replace(self.from, self.to)
  }
}

#[test]
fn fake_log_line_with_secret_is_redacted() {
  let redactor: Arc<dyn SecretRedactor> =
    Arc::new(LiteralRedactor { from: "hunter2", to: "***" });

  let mut writer = RedactingWriter::new(Vec::<u8>::new(), redactor);

  // Mimic the shape of a `tracing_subscriber::fmt` line: timestamp +
  // level + target + body + newline.
  write!(
    writer,
    "2026-06-18T12:00:00Z INFO runner: user logged in password=hunter2"
  )
  .unwrap();
  writer.write_all(b"\n").unwrap();
  writer.flush().unwrap();

  let captured = String::from_utf8(writer.into_inner().unwrap()).unwrap();

  assert!(
    !captured.contains("hunter2"),
    "secret leaked into output: {captured}"
  );
  assert!(
    captured.contains("password=***"),
    "expected redaction marker in: {captured}"
  );
  assert!(
    captured.contains("user logged in"),
    "non-secret payload lost during redaction: {captured}"
  );
}

#[test]
fn secret_split_across_writes_is_redacted() {
  let redactor: Arc<dyn SecretRedactor> =
    Arc::new(LiteralRedactor { from: "hunter2", to: "***" });

  let mut writer = RedactingWriter::new(Vec::<u8>::new(), redactor);

  // Simulate `tracing-subscriber` issuing two `write` calls for the
  // same event (some sinks do this on large messages).
  write!(writer, "INFO runner: password=hu").unwrap();
  writer.write_all(b"nter2\n").unwrap();
  writer.flush().unwrap();

  let captured = String::from_utf8(writer.into_inner().unwrap()).unwrap();

  assert!(
    !captured.contains("hunter2"),
    "secret leaked into output: {captured}"
  );
  assert!(captured.contains("password=***"));
}

#[test]
fn buffer_with_no_secret_passes_through_untouched() {
  let redactor: Arc<dyn SecretRedactor> =
    Arc::new(LiteralRedactor { from: "hunter2", to: "***" });

  let mut writer = RedactingWriter::new(Vec::<u8>::new(), redactor);

  write!(writer, "INFO runner: hello world").unwrap();
  writer.write_all(b"\n").unwrap();
  writer.flush().unwrap();

  let captured = String::from_utf8(writer.into_inner().unwrap()).unwrap();

  assert_eq!(captured, "INFO runner: hello world\n");
}

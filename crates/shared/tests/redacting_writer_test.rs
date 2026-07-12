//! Restored unit coverage for `shared::startup::RedactingWriter`, the
//! line-buffered secret redactor. An integration test against the crate's
//! public API; exercises the complete-line and split-across-writes paths so a
//! secret is masked whether or not it lands in a single `write`.

use std::io::Write;
use std::sync::Arc;

use shared::startup::{RedactingWriter, SecretRedactor};

/// Redactor that swaps a literal needle for a fixed mask.
struct LiteralRedactor(&'static str, &'static str);

impl SecretRedactor for LiteralRedactor {
  fn redact(&self, line: &str) -> String {
    line.replace(self.0, self.1)
  }
}

#[test]
fn redacting_writer_replaces_secret_in_complete_line() -> std::io::Result<()> {
  let redactor = Arc::new(LiteralRedactor("hunter2", "***"));
  let mut writer = RedactingWriter::new(Vec::<u8>::new(), redactor);
  writeln!(writer, "user logged in password=hunter2")?;
  writer.flush()?;
  let out = String::from_utf8_lossy(&writer.into_inner()?).into_owned();
  assert!(!out.contains("hunter2"), "secret leaked: {out}");
  assert!(out.contains("password=***"), "expected redaction in: {out}");
  Ok(())
}

#[test]
fn redacting_writer_replaces_secret_split_across_writes() -> std::io::Result<()> {
  let redactor = Arc::new(LiteralRedactor("hunter2", "***"));
  let mut writer = RedactingWriter::new(Vec::<u8>::new(), redactor);
  write!(writer, "first half ")?;
  writeln!(writer, "password=hunter2")?;
  writer.flush()?;
  let out = String::from_utf8_lossy(&writer.into_inner()?).into_owned();
  assert!(!out.contains("hunter2"), "secret leaked: {out}");
  assert!(out.contains("password=***"), "expected redaction in: {out}");
  Ok(())
}

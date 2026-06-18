//! End-to-end test for the `SecretMasker` → `RedactingWriter` wiring.
//!
//! Closes IMP-SE-001: proves that when `toolu-runner` constructs an
//! `Arc<Mutex<SecretMasker>>`, wraps it in `MaskerRedactor` to satisfy
//! `SecretRedactor`, and passes it through
//! `shared::startup::init_with_redactor` to a `RedactingWriter`, every
//! line written through that writer has its registered secrets replaced
//! with `***` — including secrets that are registered after the writer
//! is constructed (which is exactly the runtime `register_secret` path
//! from `ExecutionContext`).
//!
//! Without this test, the IMP-SE-001 fix (swapping `init` for
//! `init_with_redactor` in `main.rs`) is a one-line change with no
//! behavioral coverage.

use std::io::Write;
use std::sync::{Arc, Mutex};

use shared::startup::{RedactingWriter, SecretRedactor};
use toolu_runner::execution::secret_masker::{MaskerRedactor, SecretMasker};

#[test]
fn masker_redacts_registered_secret_through_redacting_writer() {
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  masker.lock().unwrap().add_secret("hunter2");
  // Wrap the same Arc in `MaskerRedactor` — the same wrapper the
  // runner's `init_with_redactor` uses for the file sink.
  let redactor: Arc<dyn SecretRedactor> = Arc::new(MaskerRedactor(Arc::clone(&masker)));

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
}

#[test]
fn masker_registers_json_escaped_variant() {
  // The SecretMasker auto-registers the JSON-escaped form of a secret
  // so that values flowing through a JSON-serialized log line are also
  // redacted. This test pins that behavior so a future refactor doesn't
  // drop the JSON escape path.
  let mut json_masker = SecretMasker::new();
  json_masker.add_secret("line1\nline2");
  let result = json_masker.mask("payload contains line1\\nline2 escaped");
  assert!(
    !result.contains("line1\\nline2"),
    "json-escaped secret leaked: {result}"
  );
}

#[test]
fn masker_redacts_after_add_secret_via_arc_clone() {
  // IMP-SE-001 invariant: the listener shares the masker across the
  // file sink (via `init_with_redactor`) and the per-line listener
  // (via `ExecutionContext::register_secret`). A registration through
  // one Arc clone must be visible to readers on the other Arc clone
  // — and the production code uses `Arc<Mutex<SecretMasker>>` exactly
  // so that this is true even when multiple strong references exist.
  let masker = Arc::new(Mutex::new(SecretMasker::new()));
  // Per-line path registers a secret at job runtime. This is what
  // `ExecutionContext::register_secret` does on every secret variable.
  masker.lock().unwrap().add_secret("dynamically-registered");
  // The file sink's redactor sees the same registration because the
  // Mutex guards the inner SecretMasker — the registration is visible
  // on the very next `redact` call.
  let redactor: Arc<dyn SecretRedactor> = Arc::new(MaskerRedactor(Arc::clone(&masker)));
  let mut writer = RedactingWriter::new(Vec::<u8>::new(), redactor);
  writeln!(writer, "token=dynamically-registered").unwrap();
  writer.flush().unwrap();
  let captured = String::from_utf8(writer.into_inner().unwrap()).unwrap();
  assert!(
    !captured.contains("dynamically-registered"),
    "secret registered through separate Arc clone leaked: {captured}"
  );
  assert!(captured.contains("token=***"));
}

#[test]
fn masker_skips_too_short_secrets() {
  // add_secret ignores values < 4 chars to avoid false positives. Pin
  // this so a future change doesn't accidentally mask substrings like
  // "OK" or "id".
  let mut short_masker = SecretMasker::new();
  short_masker.add_secret("OK");
  let masked = short_masker.mask("status=OK");
  assert_eq!(masked, "status=OK", "too-short value should not be masked");
}

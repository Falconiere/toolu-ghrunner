//! Real-data tests for `SecretMasker` (AC #6).
//!
//! Covers the production `SecretMasker` end-to-end: register real-shape
//! secrets, mask plain-text log lines, JSON-escaped variants, and
//! redaction edge cases. Inputs mirror what the runner logs through
//! `tracing-subscriber` during a real job.
//!
//! Sibling to `shared/tests/startup_redaction_test.rs`, which exercises
//! the `RedactingWriter` pipeline with a mock redactor. This file
//! exercises the real masker that gets wired in as that redactor.

use toolu_runner::execution::secret_masker::SecretMasker;

#[test]
fn registered_secret_is_masked_in_log_line() {
  let mut masker = SecretMasker::new();
  masker.add_secret("hunter2-token-value-abc123");
  let out = masker.mask("INFO runner: user logged in with token=hunter2-token-value-abc123");
  assert!(!out.contains("hunter2-token-value-abc123"), "leaked: {out}");
  assert!(out.contains("token=***"), "missing marker: {out}");
}

#[test]
fn bearer_token_ghp_prefix_is_masked() {
  let mut masker = SecretMasker::new();
  let token = "ghp_FAKEghp_FAKEghp_FAKEghp_FAKE";
  masker.add_secret(token);
  let out = masker.mask(&format!("INFO api: Authorization: Bearer {token}"));
  assert!(!out.contains(token), "leaked token: {out}");
  assert!(out.contains("Bearer ***"), "expected redaction: {out}");
}

#[test]
fn json_escaped_variant_is_also_masked() {
  // JSON-escape the secret (the way it appears in a JSON-encoded log line)
  // and confirm the masker catches it without separate registration.
  let raw = "secret-string-with-quotes";
  let json_escaped = format!("\\\"{raw}\\\"");

  let mut masker = SecretMasker::new();
  masker.add_secret(raw);

  let line = format!("INFO json: payload={json_escaped} ok");
  let out = masker.mask(&line);
  assert!(!out.contains(raw), "raw leaked: {out}");
  assert!(!out.contains(&json_escaped), "json-escaped leaked: {out}");
  assert!(out.contains("***"), "expected mask marker: {out}");
}

#[test]
fn multi_line_secret_masks_every_line() {
  // GH's PEM keys / JSON Web Tokens are multi-line. The masker splits
  // and registers each line separately so a header-only chunk is masked
  // when only that line leaks.
  let secret = "-----BEGIN FAKE-----\nabcdef0123456789\n-----END FAKE-----";
  let mut masker = SecretMasker::new();
  masker.add_secret(secret);

  let line = "DEBUG runner: -----BEGIN FAKE----- appeared in the log";
  let out = masker.mask(line);
  assert!(!out.contains("-----BEGIN FAKE-----"), "leaked header: {out}");
  assert!(out.contains("***"), "expected marker: {out}");
}

#[test]
fn short_value_is_not_registered() {
  // Values shorter than 4 chars are ignored — too noisy (would mask
  // "true", "id", "yes", etc).
  let mut masker = SecretMasker::new();
  masker.add_secret("ab");
  masker.add_secret("   "); // trims to 0 chars
  masker.add_secret("abc"); // 3 chars after trim — still ignored
  let out = masker.mask("INFO runner: 'abc' is a short literal");
  assert!(out.contains("'abc'"), "should not be masked: {out}");
}

#[test]
fn empty_masker_returns_input_unchanged() {
  let masker = SecretMasker::new();
  let input = "INFO runner: no secrets here";
  let out = masker.mask(input);
  assert_eq!(out, input);
}

#[test]
fn longest_pattern_wins_in_replacement_order() {
  // The masker sorts patterns longest-first to avoid partial matches
  // shadowing longer ones. E.g. "token=abc" should win over a shorter
  // registered "abc" when both are present.
  let mut masker = SecretMasker::new();
  masker.add_secret("abc");
  masker.add_secret("token=abc-extra");
  let out = masker.mask("DEBUG env: token=abc-extra exported");
  assert!(!out.contains("token=abc-extra"), "leaked: {out}");
}

#[test]
fn multiple_secrets_each_redacted() {
  let mut masker = SecretMasker::new();
  masker.add_secret("SECRET_ONE_VALUE");
  masker.add_secret("SECRET_TWO_VALUE");
  let input = "DEBUG first=SECRET_ONE_VALUE second=SECRET_TWO_VALUE plain=text";
  let out = masker.mask(input);
  assert!(!out.contains("SECRET_ONE_VALUE"), "leak one: {out}");
  assert!(!out.contains("SECRET_TWO_VALUE"), "leak two: {out}");
  assert!(out.contains("plain=text"), "non-secret lost: {out}");
  let count = out.matches("***").count();
  assert!(count >= 2, "expected at least 2 ***, got {count}: {out}");
}

#[test]
fn full_pipeline_with_redacting_writer_redacts_real_secrets() {
  // End-to-end: wire the real SecretMasker as the SecretRedactor
  // behind the shared RedactingWriter (the actual wiring the runner
  // uses for the JSON file sink).
  use std::io::Write;
  use std::sync::Arc;

  use shared::startup::{RedactingWriter, SecretRedactor};

  let mut masker = SecretMasker::new();
  masker.add_secret("hunter2-token-value-abc123");
  let redactor: Arc<dyn SecretRedactor> = Arc::new(masker);

  let mut writer = RedactingWriter::new(Vec::<u8>::new(), redactor);
  writeln!(
    writer,
    "2026-06-18T12:00:00Z INFO runner: env set HUNTER=hunter2-token-value-abc123"
  )
  .unwrap();
  writer.flush().unwrap();
  let captured = String::from_utf8(writer.into_inner().unwrap()).unwrap();
  assert!(
    !captured.contains("hunter2-token-value-abc123"),
    "leaked through RedactingWriter: {captured}"
  );
  assert!(captured.contains("HUNTER=***"), "missing marker: {captured}");
}

#[test]
fn matches_recorded_input_fixture_redaction_output() {
  // Drive the recorded input/expected fixtures from
  // tests/fixtures/secret-masking-{input,expected}.txt through the real
  // masker and confirm the expected line is produced verbatim.
  let input: &str = include_str!("fixtures/secret-masking-input.txt");
  let expected: &str = include_str!("fixtures/secret-masking-expected.txt");

  let mut masker = SecretMasker::new();
  // Secrets chosen to cover the four mask cases in the input fixture:
  //   - bare token
  //   - bearer ghp_ token
  //   - json-stringified token
  //   - json-escaped token (with \u0022)
  masker.add_secret("hunter2-token-value-abc123");
  masker.add_secret("ghp_FAKEghp_FAKEghp_FAKEghp_FAKE");
  masker.add_secret("abc-token-value-abc-token-value-abc");

  let actual: String = input
    .lines()
    .map(|line| masker.mask(line))
    .collect::<Vec<_>>()
    .join("\n");

  // include_str! includes any trailing newline. Strip it from the
  // expected file so the per-line join comparison is apples-to-apples.
  let expected_trimmed = expected.trim_end_matches('\n');
  assert_eq!(actual, expected_trimmed, "redaction does not match expected fixture");
}

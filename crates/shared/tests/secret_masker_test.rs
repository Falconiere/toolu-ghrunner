//! Integration tests for `shared::SecretMasker` encoded-variant masking.
//!
//! Proves a registered secret is redacted not only raw and JSON-escaped but
//! also when it surfaces base64-, hex-, or percent-encoded — the shapes a
//! secret takes in `_diag/runner.log` and the journal JSONL (`echo $SECRET |
//! base64`, an `Authorization: Basic` header, a hex dump, a %-encoded URL
//! token). Real encodings, no mocks.

use std::borrow::Cow;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use shared::SecretMasker;

#[test]
fn base64_padded_and_unpadded_secret_is_masked() {
  let secret = "hunter2-secret-token-value";
  let mut masker = SecretMasker::new();
  masker.add_secret(secret);

  let standard = STANDARD.encode(secret);
  let no_pad = STANDARD_NO_PAD.encode(secret);
  // 26 bytes (% 3 == 2) => the two forms genuinely differ by padding.
  assert_ne!(
    standard, no_pad,
    "test input must exercise both base64 forms"
  );

  // `echo $SECRET | base64` produces the padded (STANDARD) form.
  let line = format!("dumped token: {standard}");
  let redacted = masker.mask(&line);
  assert!(
    !redacted.contains(&standard),
    "padded base64 secret leaked: {redacted}"
  );
  assert!(
    redacted.contains("dumped token: ***"),
    "expected mask marker: {redacted}"
  );

  // Unpadded (STANDARD_NO_PAD) form.
  let line = format!("token={no_pad}");
  let redacted = masker.mask(&line);
  assert!(
    !redacted.contains(&no_pad),
    "unpadded base64 secret leaked: {redacted}"
  );
  assert!(
    redacted.contains("token=***"),
    "expected mask marker: {redacted}"
  );
}

#[test]
fn authorization_basic_base64_credential_is_masked() {
  // GitHub masks the full `user:password` credential pair; here the pair
  // itself is the registered secret, so its base64 must be redacted.
  let credential = "ci-bot:s3cr3t-deploy-key";
  let mut masker = SecretMasker::new();
  masker.add_secret(credential);

  let b64 = STANDARD.encode(credential);
  let line = format!("Authorization: Basic {b64}");
  let redacted = masker.mask(&line);
  assert!(
    !redacted.contains(&b64),
    "Basic-auth base64 credential leaked: {redacted}"
  );
  assert!(
    redacted.contains("Authorization: Basic ***"),
    "expected mask marker: {redacted}"
  );
}

#[test]
fn hex_encoded_secret_is_masked_both_cases() {
  let mut masker = SecretMasker::new();
  masker.add_secret("zoo!"); // bytes: 7a 6f 6f 21

  let redacted = masker.mask("hexdump lower=7a6f6f21 upper=7A6F6F21");
  assert!(
    !redacted.contains("7a6f6f21"),
    "lowercase hex secret leaked: {redacted}"
  );
  assert!(
    !redacted.contains("7A6F6F21"),
    "uppercase hex secret leaked: {redacted}"
  );
  assert_eq!(redacted, "hexdump lower=*** upper=***");
}

#[test]
fn percent_encoded_secret_is_masked_both_cases() {
  let mut masker = SecretMasker::new();
  masker.add_secret("a b/c?"); // space, '/', '?' fall outside the unreserved set

  let redacted = masker.mask("url https://h/p?q=a%20b%2fc%3f&r=a%20b%2Fc%3F");
  assert!(
    !redacted.contains("a%20b%2fc%3f"),
    "lowercase %-encoded secret leaked: {redacted}"
  );
  assert!(
    !redacted.contains("a%20b%2Fc%3F"),
    "uppercase %-encoded secret leaked: {redacted}"
  );
  assert_eq!(redacted, "url https://h/p?q=***&r=***");
}

#[test]
fn raw_and_json_escaped_secret_still_masked() {
  let mut masker = SecretMasker::new();
  masker.add_secret("pa\"ss"); // value contains a double quote

  // Raw form.
  let redacted = masker.mask("login pa\"ss done");
  assert!(
    !redacted.contains("pa\"ss"),
    "raw secret leaked: {redacted}"
  );
  assert!(
    redacted.contains("login *** done"),
    "expected mask marker: {redacted}"
  );

  // JSON-serialized log line: the quote is backslash-escaped.
  let json_line = "{\"password\":\"pa\\\"ss\"}";
  let redacted = masker.mask(json_line);
  assert!(
    !redacted.contains("pa\\\"ss"),
    "json-escaped secret leaked: {redacted}"
  );
}

#[test]
fn short_secret_and_its_encodings_are_not_registered() {
  let mut masker = SecretMasker::new();
  masker.add_secret("ab"); // below the 4-char minimum => nothing registered

  // base64("ab") == "YWI=", hex("ab") == "6162"; none must be masked.
  let line = "raw=ab b64=YWI= hex=6162";
  assert_eq!(
    masker.mask(line),
    line,
    "short secret must register no patterns"
  );
}

/// Asserts `out` contains no substring of `pat` longer than `max_len`
/// characters — the leak `MatchKind::LeftmostLongest` would produce.
fn assert_no_long_substring_leak(out: &str, pat: &str, max_len: usize) {
  let chars: Vec<char> = pat.chars().collect();
  for len in (max_len + 1)..=chars.len() {
    for window in chars.windows(len) {
      let sub: String = window.iter().collect();
      assert!(
        !out.contains(&sub),
        "leaked substring {sub:?} of {pat:?} in {out:?}"
      );
    }
  }
}

#[test]
fn overlapping_patterns_merge_into_one_mask_no_leak() {
  // The case `MatchKind::LeftmostLongest` gets wrong: "xabc" and
  // "abcdef12" overlap in "xabcdef12y", so the merged single-pass mask
  // must cover their union ("***y"), not leak the shorter match's tail
  // ("***def12y", 5 bytes of "abcdef12" exposed).
  let mut masker = SecretMasker::new();
  masker.add_secret("abcdef12");
  masker.add_secret("xabc");
  let out = masker.mask("xabcdef12y");
  assert_eq!(out, "***y", "overlap must merge into one mask: {out}");
  assert_no_long_substring_leak(&out, "abcdef12", 3);
  assert_no_long_substring_leak(&out, "xabc", 3);
}

#[test]
fn multi_line_secret_masks_a_line_seen_alone() {
  // PEM keys / JWTs are multi-line; each line is registered separately so
  // a header-only leak is still masked.
  let secret = "-----BEGIN FAKE-----\nabcdef0123456789\n-----END FAKE-----";
  let mut masker = SecretMasker::new();
  masker.add_secret(secret);
  let out = masker.mask("header: -----BEGIN FAKE----- present");
  assert!(
    !out.contains("-----BEGIN FAKE-----"),
    "leaked header: {out}"
  );
  assert!(out.contains("***"), "expected marker: {out}");
}

#[test]
fn empty_masker_returns_input_unchanged() {
  let masker = SecretMasker::new();
  assert_eq!(masker.mask("no secrets here"), "no secrets here");
}

#[test]
fn automaton_none_falls_back_to_replace_loop_and_still_masks() {
  let mut masker = SecretMasker::new();
  masker.add_secret("fallback-secret-value-9");
  masker.force_automaton_none_for_test(); // simulate a build failure
  let out = masker.mask("token=fallback-secret-value-9 tail");
  assert_eq!(
    out, "token=*** tail",
    "fallback loop must still mask: {out}"
  );
}

/// AC-5: `mask` borrows the input untouched when nothing matches — no
/// allocation for the (overwhelmingly common) clean-line case.
#[test]
fn mask_borrows_input_for_a_no_match_line() {
  // The same synthetic PEM fixture blessed in `secrets.scanExempt`
  // (`crates/config/tests/app_store_test.rs`); reused here so this crate
  // needs no fixture of its own.
  let secret = "-----BEGIN RSA PRIVATE KEY-----\nMIIphony\n-----END RSA PRIVATE KEY-----\n";
  let mut masker = SecretMasker::new();
  masker.add_secret(secret);

  let clean = "nothing secret on this line";
  let out = masker.mask(clean);
  assert!(
    matches!(out, Cow::Borrowed(_)),
    "expected Cow::Borrowed for a no-match line, got Owned: {out}"
  );
  assert_eq!(out, clean);
}

/// AC-5: `mask` allocates (`Cow::Owned`) once a span is actually replaced —
/// both for the raw secret and for its JSON-escaped variant (the PEM
/// fixture's embedded newlines become `\n` inside a JSON string).
#[test]
fn mask_owns_output_for_raw_and_json_escaped_credential_matches() {
  let secret = "-----BEGIN RSA PRIVATE KEY-----\nMIIphony\n-----END RSA PRIVATE KEY-----\n";
  let mut masker = SecretMasker::new();
  masker.add_secret(secret);

  let raw_line = "leaking key body: MIIphony inline";
  let out = masker.mask(raw_line);
  assert!(
    matches!(out, Cow::Owned(_)),
    "expected Cow::Owned for a raw match, got Borrowed: {out}"
  );
  assert!(!out.contains("MIIphony"), "raw secret leaked: {out}");
  assert!(out.contains("***"), "expected mask marker: {out}");

  let json_line =
    "{\"key\":\"-----BEGIN RSA PRIVATE KEY-----\\nMIIphony\\n-----END RSA PRIVATE KEY-----\\n\"}";
  let out = masker.mask(json_line);
  assert!(
    matches!(out, Cow::Owned(_)),
    "expected Cow::Owned for a json-escaped match, got Borrowed: {out}"
  );
  assert!(
    !out.contains("MIIphony"),
    "json-escaped secret leaked: {out}"
  );
  assert!(out.contains("***"), "expected mask marker: {out}");
}

/// AC-8: `add_secrets` over 50 values must mask identically to 50
/// sequential `add_secret` calls — same patterns registered, same output.
#[test]
fn add_secrets_batch_matches_fifty_sequential_add_secret_calls() {
  let values: Vec<String> = (0..50)
    .map(|i| format!("batch-fixture-secret-{i:03}"))
    .collect();

  let mut batched = SecretMasker::new();
  batched.add_secrets(values.iter().map(String::as_str));

  let mut sequential = SecretMasker::new();
  for value in &values {
    sequential.add_secret(value);
  }

  // Deterministic fixture values (not indexed off the vec — `indexing_slicing`
  // is denied even in tests) touching the first, middle, and last generated secret.
  let line = "job touched batch-fixture-secret-000, batch-fixture-secret-024, \
    and batch-fixture-secret-049 amid plain text";
  assert_eq!(
    batched.mask(line),
    sequential.mask(line),
    "batch and sequential registration must mask identically"
  );
}

//! Integration tests for `shared::SecretMasker` encoded-variant masking.
//!
//! Proves a registered secret is redacted not only raw and JSON-escaped but
//! also when it surfaces base64-, hex-, or percent-encoded — the shapes a
//! secret takes in `_diag/runner.log` and the journal JSONL (`echo $SECRET |
//! base64`, an `Authorization: Basic` header, a hex dump, a %-encoded URL
//! token). Real encodings, no mocks.

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
  assert_ne!(standard, no_pad, "test input must exercise both base64 forms");

  // `echo $SECRET | base64` produces the padded (STANDARD) form.
  let redacted = masker.mask(&format!("dumped token: {standard}"));
  assert!(
    !redacted.contains(&standard),
    "padded base64 secret leaked: {redacted}"
  );
  assert!(
    redacted.contains("dumped token: ***"),
    "expected mask marker: {redacted}"
  );

  // Unpadded (STANDARD_NO_PAD) form.
  let redacted = masker.mask(&format!("token={no_pad}"));
  assert!(
    !redacted.contains(&no_pad),
    "unpadded base64 secret leaked: {redacted}"
  );
  assert!(redacted.contains("token=***"), "expected mask marker: {redacted}");
}

#[test]
fn authorization_basic_base64_credential_is_masked() {
  // GitHub masks the full `user:password` credential pair; here the pair
  // itself is the registered secret, so its base64 must be redacted.
  let credential = "ci-bot:s3cr3t-deploy-key";
  let mut masker = SecretMasker::new();
  masker.add_secret(credential);

  let b64 = STANDARD.encode(credential);
  let redacted = masker.mask(&format!("Authorization: Basic {b64}"));
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
  assert!(!redacted.contains("pa\"ss"), "raw secret leaked: {redacted}");
  assert!(redacted.contains("login *** done"), "expected mask marker: {redacted}");

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
  assert_eq!(masker.mask(line), line, "short secret must register no patterns");
}

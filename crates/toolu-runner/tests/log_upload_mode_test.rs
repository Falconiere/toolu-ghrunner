//! Tests for `UploadMode::for_content` — the 4 MiB boundary between
//! BlockBlob and AppendBlob transport selection.
//!
//! All tests are real-data (size thresholds, not HTTP requests).

use wire::reporting::log_upload::UploadMode;

#[test]
fn zero_bytes_is_block_blob() {
  assert_eq!(UploadMode::for_content(0), UploadMode::BlockBlob);
}

#[test]
fn exactly_4_mib_is_block_blob() {
  assert_eq!(
    UploadMode::for_content(4 * 1024 * 1024),
    UploadMode::BlockBlob
  );
}

#[test]
fn over_4_mib_is_append_blob() {
  assert_eq!(
    UploadMode::for_content(4 * 1024 * 1024 + 1),
    UploadMode::AppendBlob
  );
}

#[test]
fn large_content_is_append_blob() {
  assert_eq!(
    UploadMode::for_content(100 * 1024 * 1024),
    UploadMode::AppendBlob
  );
}

#[test]
fn one_byte_is_block_blob() {
  assert_eq!(UploadMode::for_content(1), UploadMode::BlockBlob);
}

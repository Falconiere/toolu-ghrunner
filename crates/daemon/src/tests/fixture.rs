//! Daemon-side assertions for the canonical VPS daemon HTTP contract
//! fixture, shared byte-for-byte with toolu.sh's vendored copy at
//! `packages/api/src/__tests__/fixtures/vps-daemon-contract.json` (asserted
//! there by `vps-contract-fixture.test.ts`). This repo's own copy lives at
//! `crates/daemon/tests/fixtures/vps-daemon-contract.json` — a crate-root
//! `tests/` directory, not `src/tests/`, because the guardrails' folder-tree
//! check only allows a plain `tests/` subdirectory (never a further-nested
//! one) anywhere under `src/`. Its recorded SHA-256 lives at
//! `tests/fixtures/vps-daemon-contract.sha256`, next to it.
//!
//! Two things are proved here, not assumed: (1) the mechanical drift
//! guard — this file's bytes still hash to the recorded value, so an edit to
//! either copy that isn't mirrored to its sibling (and re-recorded in both
//! repos) fails this suite; (2) the fixture's request/response/error bodies
//! round-trip through the REAL `routes::wire`/`routes::error` types with no
//! field loss — never a hand-written duplicate struct.

use std::fmt::Write as _;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::routes::error::ErrorBody;
use crate::routes::wire::{CreateJobRequest, CreateJobResponse};

/// The fixture bytes, verbatim — read at compile time so the drift guard
/// hashes exactly what every other assertion here parses.
const FIXTURE_BYTES: &str = include_str!("../../tests/fixtures/vps-daemon-contract.json");
/// The recorded SHA-256 of [`FIXTURE_BYTES`], as committed. toolu.sh records
/// the same value next to its own copy.
const RECORDED_SHA256: &str = include_str!("../../tests/fixtures/vps-daemon-contract.sha256");

/// Hex-encoded SHA-256 of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
  Sha256::digest(bytes)
    .iter()
    .fold(String::new(), |mut acc, byte| {
      // `write!` into a `String` is infallible — there is no I/O to fail.
      let _ = write!(acc, "{byte:02x}");
      acc
    })
}

/// Deserialize `raw` into the real wire type `T`, then serialize it back and
/// assert the result equals `raw` field-for-field — proving `T` loses and
/// invents nothing relative to the fixture. `expect`'s own panic message
/// already appends the wrapped error's `Debug` output, so the literals below
/// need no interpolation.
fn assert_round_trips_losslessly<T: DeserializeOwned + Serialize>(raw: &Value, what: &str) {
  let typed: T =
    serde_json::from_value(raw.clone()).expect("deserializing the fixture into the real wire type");
  let back = serde_json::to_value(&typed).expect("serializing the real wire type back to JSON");
  assert_eq!(
    &back, raw,
    "{what}: round-tripping through the real type changed the JSON — the fixture and \
     routes::wire/routes::error have drifted"
  );
}

#[test]
fn fixture_bytes_still_match_the_recorded_sha256() {
  let recorded = RECORDED_SHA256.trim();
  let actual = sha256_hex(FIXTURE_BYTES.as_bytes());
  assert_eq!(
    actual, recorded,
    "tests/fixtures/vps-daemon-contract.json no longer matches \
     tests/fixtures/vps-daemon-contract.sha256 in this repo. If you edited the fixture on \
     purpose, re-sync toolu.sh's vendored copy at \
     packages/api/src/__tests__/fixtures/vps-daemon-contract.json byte-for-byte and update the \
     recorded hash in BOTH repos."
  );
}

#[test]
fn fixture_request_round_trips_through_the_real_create_job_request() {
  let fixture: Value = serde_json::from_str(FIXTURE_BYTES).expect("fixture is valid JSON");
  let request = fixture
    .get("request")
    .expect("fixture carries a \"request\" object");
  assert_round_trips_losslessly::<CreateJobRequest>(request, "request");
}

#[test]
fn fixture_create_response_round_trips_through_the_real_create_job_response() {
  let fixture: Value = serde_json::from_str(FIXTURE_BYTES).expect("fixture is valid JSON");
  let response = fixture
    .get("createResponse")
    .expect("fixture carries a \"createResponse\" object");
  assert_round_trips_losslessly::<CreateJobResponse>(response, "createResponse");
}

#[test]
fn fixture_error_response_round_trips_through_the_real_error_body() {
  let fixture: Value = serde_json::from_str(FIXTURE_BYTES).expect("fixture is valid JSON");
  let error = fixture
    .get("errorResponse")
    .expect("fixture carries an \"errorResponse\" object");
  assert_round_trips_losslessly::<ErrorBody>(error, "errorResponse");
}

#[test]
fn fixture_request_carries_the_field_names_this_crate_pins() {
  // Not just "it parses" — the exact field names `client.ts` sends, spelled
  // out, so a rename on either side that a lenient deserialize would
  // silently accept (extra/missing-but-optional fields) still fails loudly
  // here.
  let fixture: Value = serde_json::from_str(FIXTURE_BYTES).expect("fixture is valid JSON");
  let request = fixture
    .get("request")
    .expect("fixture carries a \"request\" object");
  let object = request.as_object().expect("request is a JSON object");
  let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
  keys.sort_unstable();
  assert_eq!(
    keys,
    vec![
      "deadline",
      "image",
      "jitConfig",
      "jobRef",
      "purpose",
      "size"
    ],
    "request field names drifted from the pinned contract"
  );
  let size = request
    .get("size")
    .expect("request carries size")
    .as_object()
    .expect("size is an object");
  let mut size_keys: Vec<&str> = size.keys().map(String::as_str).collect();
  size_keys.sort_unstable();
  assert_eq!(size_keys, vec!["memoryMb", "vcpu"]);
  let job_ref = request
    .get("jobRef")
    .expect("request carries jobRef")
    .as_object()
    .expect("jobRef is an object");
  let mut job_ref_keys: Vec<&str> = job_ref.keys().map(String::as_str).collect();
  job_ref_keys.sort_unstable();
  assert_eq!(job_ref_keys, vec!["jobId", "org", "repo"]);
}

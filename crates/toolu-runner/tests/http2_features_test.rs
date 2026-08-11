//! Integration test asserting HTTP/2 support is enabled across the workspace.
//!
//! Reads the five `Cargo.toml` manifests that consume `reqwest` and verifies
//! each one explicitly lists the `"http2"` feature. This is AC-11 of the
//! job-speed improvements spec: the manifest check ensures no missed update
//! when features are added or refactored (S1).
//!
//! Uses real manifest files, not mocks — locates them via `CARGO_MANIFEST_DIR`
//! and relative paths.

use std::path::PathBuf;

/// Read and parse a Cargo.toml manifest at the given path.
fn read_manifest(path: &PathBuf) -> Result<toml::Table, Box<dyn std::error::Error>> {
  let content = std::fs::read_to_string(path)?;
  content.parse::<toml::Table>().map_err(Into::into)
}

/// Extract the reqwest dependencies' feature list from a manifest's
/// `[dependencies]` table.
fn get_reqwest_features(manifest: &toml::Table) -> Result<Vec<String>, Box<dyn std::error::Error>> {
  manifest
    .get("dependencies")
    .and_then(|deps| deps.get("reqwest"))
    .and_then(|req| {
      if let toml::Value::Table(tbl) = req {
        tbl.get("features").and_then(|f| f.as_array()).map(|arr| {
          arr
            .iter()
            .filter_map(|v| v.as_str())
            .map(str::to_owned)
            .collect()
        })
      } else {
        None
      }
    })
    .ok_or_else(|| "Could not find reqwest features in manifest".into())
}

#[test]
fn wire_reqwest_has_http2_feature() {
  let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
  let manifest_path = manifest_dir.join("../../crates/wire/Cargo.toml");

  let manifest = read_manifest(&manifest_path).expect("read wire/Cargo.toml");
  let features = get_reqwest_features(&manifest).expect("extract reqwest features from wire");

  assert!(
    features.iter().any(|f| f == "http2"),
    "wire reqwest features missing 'http2': {features:?}"
  );
}

#[test]
fn execution_reqwest_has_http2_feature() {
  let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
  let manifest_path = manifest_dir.join("../../crates/execution/Cargo.toml");

  let manifest = read_manifest(&manifest_path).expect("read execution/Cargo.toml");
  let features = get_reqwest_features(&manifest).expect("extract reqwest features from execution");

  assert!(
    features.iter().any(|f| f == "http2"),
    "execution reqwest features missing 'http2': {features:?}"
  );
}

#[test]
fn cache_reqwest_has_http2_feature() {
  let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
  let manifest_path = manifest_dir.join("../../crates/cache/Cargo.toml");

  let manifest = read_manifest(&manifest_path).expect("read cache/Cargo.toml");
  let features = get_reqwest_features(&manifest).expect("extract reqwest features from cache");

  assert!(
    features.iter().any(|f| f == "http2"),
    "cache reqwest features missing 'http2': {features:?}"
  );
}

#[test]
fn listener_reqwest_has_http2_feature() {
  let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
  let manifest_path = manifest_dir.join("../../crates/listener/Cargo.toml");

  let manifest = read_manifest(&manifest_path).expect("read listener/Cargo.toml");
  let features = get_reqwest_features(&manifest).expect("extract reqwest features from listener");

  assert!(
    features.iter().any(|f| f == "http2"),
    "listener reqwest features missing 'http2': {features:?}"
  );
}

#[test]
fn toolu_runner_reqwest_has_http2_feature() {
  let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
  let manifest_path = manifest_dir.join("./Cargo.toml");

  let manifest = read_manifest(&manifest_path).expect("read toolu-runner/Cargo.toml");
  let features =
    get_reqwest_features(&manifest).expect("extract reqwest features from toolu-runner");

  assert!(
    features.iter().any(|f| f == "http2"),
    "toolu-runner reqwest features missing 'http2': {features:?}"
  );
}

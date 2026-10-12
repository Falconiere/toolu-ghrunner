//! Integration tests for `config::labels` (#92) and the persisted
//! `ephemeral` registration key.
//!
//! Real data: `fixtures/jit_labels_probe.json` is a live github.com
//! `generate-jitconfig` capture (2026-10-11) of the label sets GitHub stored
//! for given request label lists. The resolver must produce exactly the set
//! GitHub keeps, so the persisted `labels` match the server's view. The
//! `ephemeral` key is round-tripped through a real `config.toml` on disk.

use std::collections::BTreeSet;

use config::config::load_config;
use config::labels::{
  MAX_LABEL_LEN, NO_DEFAULT_LABELS_WITHOUT_LABELS, default_labels, resolve_labels,
};
use serde_json::Value;
use shared::RunnerError;

/// The committed live capture.
const PROBE: &str = include_str!("fixtures/jit_labels_probe.json");

fn strings(items: &[&str]) -> Vec<String> {
  items.iter().map(|s| (*s).to_owned()).collect()
}

/// Case-insensitive label set, the way GitHub compares labels.
fn folded(labels: &[String]) -> BTreeSet<String> {
  labels.iter().map(|l| l.to_lowercase()).collect()
}

/// Boxed error alias so non-`#[test]` helpers can use `?`.
type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

/// A JSON array of strings (or of `{name}` objects when `key` is given).
fn names(value: &Value, key: Option<&str>) -> TestResult<Vec<String>> {
  let items = value.as_array().ok_or("expected a JSON array")?;
  items
    .iter()
    .map(|item| {
      let field = key.map_or(Some(item), |k| item.get(k));
      field
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "expected a string label".into())
    })
    .collect()
}

/// The probe's successful cases as `(request_labels, stored_labels)`.
fn probe_cases() -> TestResult<Vec<(Vec<String>, Vec<String>)>> {
  let probe: Value = serde_json::from_str(PROBE)?;
  let cases = probe
    .get("cases")
    .and_then(Value::as_array)
    .ok_or("cases array")?;
  cases
    .iter()
    .filter(|c| c.get("status").and_then(Value::as_u64) == Some(201))
    .map(|c| {
      Ok((
        names(c.get("request_labels").ok_or("request_labels")?, None)?,
        names(c.get("stored_labels").ok_or("stored_labels")?, Some("name"))?,
      ))
    })
    .collect()
}

/// AC-3: with `--no-default-labels`, the resolved list for each probed
/// request is exactly the set GitHub stored (no hidden defaults, the same
/// case-insensitive de-duplication), and keeps the first spelling.
#[test]
fn resolved_labels_match_what_github_stored() {
  let cases = probe_cases().expect("probe cases parse");
  assert_eq!(cases.len(), 2, "both 201 probe cases are present");
  for (request, stored) in cases {
    let resolved = resolve_labels(&request, true, &default_labels()).expect("labels resolve");
    assert_eq!(folded(&resolved), folded(&stored), "request {request:?}");
    assert_eq!(resolved.len(), stored.len(), "no duplicate survives");
  }
  let dup = resolve_labels(
    &strings(&["toolu-92-dup", "TOOLU-92-DUP", "self-hosted"]),
    true,
    &default_labels(),
  )
  .expect("labels resolve");
  assert_eq!(dup, strings(&["toolu-92-dup", "self-hosted"]));
}

/// AC-3: GitHub rejects an empty label list (recorded 422), so
/// `--no-default-labels` without labels fails before any request, with
/// upstream's message.
#[test]
fn no_default_labels_without_labels_is_rejected_like_github_rejects_empty() {
  let probe: Value = serde_json::from_str(PROBE).expect("probe fixture parses");
  let empty = probe
    .get("cases")
    .and_then(Value::as_array)
    .expect("cases array")
    .iter()
    .find(|c| {
      c.get("request_labels")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty)
    })
    .expect("empty-labels case recorded");
  assert_eq!(empty.get("status").and_then(Value::as_u64), Some(422));

  for user in [Vec::new(), strings(&["", ""])] {
    let result = resolve_labels(&user, true, &default_labels());
    assert!(
      matches!(&result, Err(RunnerError::Config(msg)) if msg == NO_DEFAULT_LABELS_WITHOUT_LABELS),
      "{user:?} -> {result:?}"
    );
  }
}

/// Defaults are upstream's `self-hosted`, `RUNNER_OS`, `RUNNER_ARCH`
/// spelling, and `--labels` adds to them (empty entries dropped, duplicates
/// of a default folded into it).
#[test]
fn defaults_use_runner_os_and_arch_and_labels_are_additive() {
  let defaults = default_labels();
  assert_eq!(
    defaults,
    vec![
      "self-hosted".to_owned(),
      shared::platform::runner_os().to_owned(),
      shared::platform::runner_arch().to_owned(),
    ]
  );
  let mut expected = defaults.clone();
  expected.push("gpu".to_owned());
  let resolved = resolve_labels(
    &strings(&["gpu", "", "SELF-HOSTED", "GPU"]),
    false,
    &defaults,
  )
  .expect("labels resolve");
  assert_eq!(resolved, expected);
  assert_eq!(
    resolve_labels(&[], false, &defaults).expect("defaults alone"),
    defaults
  );
}

/// A label longer than 256 characters is rejected (upstream
/// `Validators.LabelsValidator`); exactly 256 is accepted.
#[test]
fn label_length_limit_is_256() {
  let ok = "a".repeat(MAX_LABEL_LEN);
  assert_eq!(
    resolve_labels(std::slice::from_ref(&ok), true, &default_labels()).expect("256 accepted"),
    vec![ok]
  );
  let long = "b".repeat(MAX_LABEL_LEN + 1);
  let result = resolve_labels(&[long], false, &default_labels());
  assert!(
    matches!(&result, Err(RunnerError::Config(msg)) if msg.contains("256")),
    "{result:?}"
  );
}

/// AC-4: a `config.toml` written before #92 (no `ephemeral` key) loads as a
/// persistent registration; `ephemeral = true` loads as ephemeral.
#[test]
fn ephemeral_key_defaults_to_false_and_round_trips() {
  let dir = tempfile::tempdir().expect("tempdir");
  let legacy = r#"runner_url = "https://github.com/octo/demo"
runner_name = "runner-1"
runner_id = 7
auth_token = "client-id"
labels = ["self-hosted"]
runner_group = "Default"

[runtime]
jit_config = "blob"
work_dir = "_work"
data_dir = "/tmp/x"
protocol_version = "v2"
"#;
  let path = dir.path().join("config.toml");
  std::fs::write(&path, legacy).expect("write legacy config");
  assert!(!load_config(&path).expect("legacy loads").ephemeral);

  let ephemeral = legacy.replace(
    "runner_group = \"Default\"\n",
    "runner_group = \"Default\"\nephemeral = true\n",
  );
  std::fs::write(&path, ephemeral).expect("write ephemeral config");
  let loaded = load_config(&path).expect("ephemeral loads");
  assert!(loaded.ephemeral);

  config::config::save_config(&path, &loaded).expect("save");
  let text = std::fs::read_to_string(&path).expect("read back");
  assert!(text.contains("ephemeral = true"), "{text}");
  assert!(load_config(&path).expect("reload").ephemeral);
}

//! `runs.using` parity with upstream `ActionManifestManager` (actions/runner
//! `cab9d1c`, `ActionManifestManager.cs:464-507`) over real `action.yml` text.

use super::{RunsUsing, parse_action_manifest};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn node_action(using: &str) -> String {
  format!("name: probe\ndescription: probe\nruns:\n  using: '{using}'\n  main: index.js\n")
}

#[test]
fn supported_node_versions_parse_case_insensitively() -> TestResult {
  for (using, major) in [
    ("node12", 20),
    ("NODE16", 20),
    ("Node20", 20),
    ("node20", 20),
    ("node24", 24),
    ("Node24", 24),
  ] {
    let action = parse_action_manifest(&node_action(using))?;
    assert!(
      matches!(action.runs.using, RunsUsing::Node { major: m } if m == major),
      "{using}"
    );
  }
  Ok(())
}

#[test]
fn other_node_versions_fail_with_upstream_text() {
  for using in ["node22", "node18", "Node26", "node"] {
    let error = parse_action_manifest(&node_action(using)).map(|_| ());
    assert_eq!(
      error.map_err(|e| e.to_string()),
      Err(format!(
        "action manifest error: 'using: {using}' is not supported, use 'docker', 'node12', \
         'node16', 'node20' or 'node24' instead."
      )),
      "{using}"
    );
  }
}

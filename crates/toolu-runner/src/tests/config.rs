//! Tests for CLI environment accessors and startup-safe diagnostics.

use std::ffi::OsString;
use std::process::Command;

const PROBE_KEY: &str = "TOOLU_TEST_RUNNER_CONFIG_PROBE";
const CHILD_TEST: &str = "config::tests::invalid_unicode_cli_config_child_probe";

#[cfg(unix)]
fn invalid_unicode_value() -> OsString {
  use std::os::unix::ffi::OsStringExt;
  OsString::from_vec(vec![0xff])
}

#[cfg(windows)]
fn invalid_unicode_value() -> OsString {
  use std::os::windows::ffi::OsStringExt;
  OsString::from_wide(&[0xd800])
}

#[test]
#[ignore = "child-process probe; exercised by invalid_unicode_cli_config_values_are_diagnosed"]
fn invalid_unicode_cli_config_child_probe() {
  let Ok(key) = std::env::var(PROBE_KEY) else {
    return;
  };
  assert!(
    matches!(
      key.as_str(),
      "TOOLU_JITCONFIG" | "TOOLU_DEADLINE" | "TOOLU_RUNNER_CLIENT_ID"
    ),
    "unexpected child probe key: {key}"
  );
  let value = match key.as_str() {
    "TOOLU_JITCONFIG" => super::jit_config_raw(),
    "TOOLU_DEADLINE" => super::deadline_raw(),
    _ => super::runner_client_id(),
  };
  assert!(value.is_none(), "invalid Unicode must be treated as unset");
}

#[test]
fn invalid_unicode_cli_config_values_are_diagnosed() {
  let invalid = invalid_unicode_value();
  for key in [
    "TOOLU_JITCONFIG",
    "TOOLU_DEADLINE",
    "TOOLU_RUNNER_CLIENT_ID",
  ] {
    let output = Command::new(std::env::current_exe().expect("resolve current test binary"))
      .args(["--ignored", "--exact", CHILD_TEST, "--nocapture"])
      .env(PROBE_KEY, key)
      .env(key, invalid.as_os_str())
      .output()
      .expect("run CLI config child probe");
    assert!(
      output.status.success(),
      "{key} child probe failed: {}",
      String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
      stderr.contains(&format!("{key} is not valid Unicode")),
      "invalid Unicode must name {key} without logging its value; stderr: {stderr}"
    );
  }
}

#[test]
fn session_conflict_interval_accepts_only_positive_milliseconds() {
  assert_eq!(
    super::parse_interval_ms("50"),
    Some(std::time::Duration::from_millis(50))
  );
  assert_eq!(
    super::parse_interval_ms("30000"),
    Some(std::time::Duration::from_secs(30))
  );
  for bad in ["0", "-5", "1.5", "30s", " 50", ""] {
    assert_eq!(
      super::parse_interval_ms(bad),
      None,
      "{bad:?} must be rejected"
    );
  }
}

const POLICY_CHILD_TEST: &str = "config::tests::session_conflict_policy_child_probe";
/// Milliseconds the child expects `session_conflict_policy` to resolve to.
const EXPECT_MS_KEY: &str = "TOOLU_TEST_EXPECT_CONFLICT_INTERVAL_MS";

#[test]
#[ignore = "child-process probe; exercised by session_conflict_policy_reads_and_diagnoses_the_override"]
fn session_conflict_policy_child_probe() {
  let Ok(expected) = std::env::var(EXPECT_MS_KEY) else {
    return;
  };
  let policy = super::session_conflict_policy();
  assert_eq!(
    policy.interval,
    std::time::Duration::from_millis(expected.parse().expect("expected ms"))
  );
  assert_eq!(
    policy.max_attempts.get(),
    8,
    "the attempt count is not overridable"
  );
}

#[test]
fn session_conflict_policy_reads_and_diagnoses_the_override() {
  // (env value, expected interval ms, whether a diagnostic is expected)
  for (value, expected_ms, diagnosed) in [
    (Some("75"), "75", false),
    (Some(""), "30000", false),
    (None, "30000", false),
    (Some("0"), "30000", true),
    (Some("abc"), "30000", true),
  ] {
    let mut cmd = Command::new(std::env::current_exe().expect("resolve current test binary"));
    cmd
      .args(["--ignored", "--exact", POLICY_CHILD_TEST, "--nocapture"])
      .env(EXPECT_MS_KEY, expected_ms);
    match value {
      Some(v) => cmd.env("TOOLU_RUNNER_SESSION_CONFLICT_INTERVAL_MS", v),
      None => cmd.env_remove("TOOLU_RUNNER_SESSION_CONFLICT_INTERVAL_MS"),
    };
    let output = cmd
      .output()
      .expect("run session-conflict policy child probe");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{value:?}: {stderr}");
    assert_eq!(
      stderr.contains(
        "TOOLU_RUNNER_SESSION_CONFLICT_INTERVAL_MS is not a positive integer of milliseconds"
      ),
      diagnosed,
      "{value:?}: {stderr}"
    );
  }
}

#[test]
fn node_dir_is_unset_when_empty_and_absolute_when_relative() {
  temp_env::with_var("TOOLU_RUNNER_NODE_DIR", Some(""), || {
    assert_eq!(super::node_dir(), None);
  });
  temp_env::with_var("TOOLU_RUNNER_NODE_DIR", None::<&str>, || {
    assert_eq!(super::node_dir(), None);
  });
  temp_env::with_var("TOOLU_RUNNER_NODE_DIR", Some("externals"), || {
    let dir = super::node_dir();
    assert!(
      dir
        .as_ref()
        .is_some_and(|dir| dir.is_absolute() && dir.ends_with("externals")),
      "{dir:?}"
    );
  });
  temp_env::with_var(
    "TOOLU_RUNNER_NODE_DIR",
    Some("/opt/actions-runner/externals"),
    || {
      assert_eq!(
        super::node_dir(),
        Some(std::path::PathBuf::from("/opt/actions-runner/externals"))
      );
    },
  );
}

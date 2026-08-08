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
  let key = std::env::var(PROBE_KEY).expect("child probe requires PROBE_KEY");
  assert!(
    matches!(
      key.as_str(),
      "TOOLU_JITCONFIG" | "TOOLU_DEADLINE" | "TOOLU_RUNNER_TOKEN" | "TOOLU_RUNNER_CLIENT_ID"
    ),
    "unexpected child probe key: {key}"
  );
  let value = match key.as_str() {
    "TOOLU_JITCONFIG" => super::jit_config_raw(),
    "TOOLU_DEADLINE" => super::deadline_raw(),
    "TOOLU_RUNNER_TOKEN" => super::runner_token(),
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
    "TOOLU_RUNNER_TOKEN",
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

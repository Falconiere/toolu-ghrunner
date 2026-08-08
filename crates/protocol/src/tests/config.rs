//! Tests for hostname environment fallback and diagnostics.

use std::ffi::OsString;
use std::process::Command;

const PROBE_MODE: &str = "TOOLU_TEST_PROTOCOL_HOSTNAME_PROBE";
const CHILD_TEST: &str = "config::tests::invalid_unicode_hostname_child_probe";

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
#[ignore = "child-process probe; exercised by invalid_unicode_hostname_values_warn_and_follow_fallback_order"]
fn invalid_unicode_hostname_child_probe() {
  let Some(mode) = std::env::var_os(PROBE_MODE) else {
    return;
  };
  let hostname = super::hostname();
  if mode == "hostname" {
    assert_eq!(hostname.as_deref(), Some("fallback-host"));
  } else {
    assert!(hostname.is_none());
  }
}

#[test]
fn invalid_unicode_hostname_values_warn_and_follow_fallback_order() {
  let invalid = invalid_unicode_value();
  for (mode, invalid_key) in [("hostname", "HOSTNAME"), ("computername", "COMPUTERNAME")] {
    let mut command = Command::new(std::env::current_exe().expect("resolve current test binary"));
    command
      .args(["--ignored", "--exact", CHILD_TEST, "--nocapture"])
      .env(PROBE_MODE, mode);
    if mode == "hostname" {
      command
        .env("HOSTNAME", invalid.as_os_str())
        .env("COMPUTERNAME", "fallback-host");
    } else {
      command
        .env_remove("HOSTNAME")
        .env("COMPUTERNAME", invalid.as_os_str());
    }
    let output = command.output().expect("run protocol hostname child probe");
    assert!(
      output.status.success(),
      "{mode} child probe failed: {}",
      String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
      stderr.contains(&format!("{invalid_key} is not valid Unicode")),
      "invalid Unicode must name {invalid_key} without logging its value; stderr: {stderr}"
    );
  }
}

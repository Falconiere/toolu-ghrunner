//! Tests for process-environment configuration reads.

use std::ffi::{OsStr, OsString};
use std::process::{Command, Output};

const ALLOW_VERBOSE_PROBE: &str = "TOOLU_TEST_ALLOW_VERBOSE_PROBE";
const CHILD_TEST: &str = "config::tests::allow_verbose_child_probe";

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

fn run_allow_verbose_probe(value: Option<&OsStr>, expected: bool) -> Output {
  let mut command = Command::new(std::env::current_exe().expect("resolve current test binary"));
  command
    .args(["--exact", CHILD_TEST, "--nocapture"])
    .env(ALLOW_VERBOSE_PROBE, if expected { "1" } else { "0" });
  match value {
    Some(value) => {
      command.env("TOOLU_RUNNER_ALLOW_VERBOSE", value);
    },
    None => {
      command.env_remove("TOOLU_RUNNER_ALLOW_VERBOSE");
    },
  }
  command.output().expect("run allow_verbose child probe")
}

#[test]
fn allow_verbose_child_probe() {
  let Some(expected) = std::env::var_os(ALLOW_VERBOSE_PROBE) else {
    return;
  };
  assert_eq!(
    super::allow_verbose(),
    expected == "1",
    "allow_verbose returned the wrong value for the child environment"
  );
}

#[test]
fn allow_verbose_covers_unset_enabled_other_and_invalid_unicode() {
  let invalid = invalid_unicode_value();
  let cases: [(Option<&OsStr>, bool); 3] = [
    (None, false),
    (Some(OsStr::new("1")), true),
    (Some(OsStr::new("true")), false),
  ];

  for (value, expected) in cases {
    let output = run_allow_verbose_probe(value, expected);
    assert!(
      output.status.success(),
      "allow_verbose child probe failed: {}",
      String::from_utf8_lossy(&output.stderr)
    );
  }

  let invalid_output = run_allow_verbose_probe(Some(invalid.as_os_str()), false);
  assert!(
    invalid_output.status.success(),
    "invalid-Unicode child probe failed: {}",
    String::from_utf8_lossy(&invalid_output.stderr)
  );
  let stderr = String::from_utf8_lossy(&invalid_output.stderr);
  assert!(
    stderr.contains("TOOLU_RUNNER_ALLOW_VERBOSE is not valid Unicode"),
    "invalid Unicode must leave a startup diagnostic; stderr: {stderr}"
  );
}

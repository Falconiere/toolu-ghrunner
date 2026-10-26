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

#[cfg(target_os = "linux")]
const NODE_DIR_CHILD_TEST: &str = "config::tests::node_dir_unresolvable_cwd_child_probe";
/// Directory the child enters and deletes before resolving `TOOLU_RUNNER_NODE_DIR`.
const DOOMED_CWD_KEY: &str = "TOOLU_TEST_DOOMED_CWD";

#[test]
#[ignore = "child-process probe; exercised by node_dir_keeps_a_relative_path_when_the_cwd_is_gone"]
fn node_dir_unresolvable_cwd_child_probe() {
  let Ok(doomed) = std::env::var(DOOMED_CWD_KEY) else {
    return;
  };
  std::env::set_current_dir(&doomed).expect("enter the doomed cwd");
  std::fs::remove_dir(&doomed).expect("delete the cwd under the process");
  assert_eq!(
    super::node_dir(),
    Some(std::path::PathBuf::from("externals")),
    "an unresolvable cwd keeps the path as given"
  );
}

#[test]
#[cfg(target_os = "linux")]
fn node_dir_keeps_a_relative_path_when_the_cwd_is_gone() {
  // Linux `getcwd` fails with ENOENT once the cwd is unlinked, so
  // `std::path::absolute` really errors in the child.
  let doomed = tempfile::tempdir().expect("temp dir");
  let cwd = doomed.path().join("cwd");
  std::fs::create_dir(&cwd).expect("create the doomed cwd");
  let output = Command::new(std::env::current_exe().expect("resolve current test binary"))
    .args(["--ignored", "--exact", NODE_DIR_CHILD_TEST, "--nocapture"])
    .env(DOOMED_CWD_KEY, &cwd)
    .env("TOOLU_RUNNER_NODE_DIR", "externals")
    .output()
    .expect("run node_dir child probe");
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(output.status.success(), "child probe failed: {stderr}");
  assert!(
    stderr.contains("toolu-runner: TOOLU_RUNNER_NODE_DIR could not be made absolute ("),
    "the fallback must be reported by name; stderr: {stderr}"
  );
}

#[test]
fn the_github_api_base_defaults_to_api_github_com_and_drops_a_trailing_slash() {
  assert_eq!(super::api_base_or_default(None), "https://api.github.com");
  assert_eq!(
    super::api_base_or_default(Some(String::new())),
    "https://api.github.com"
  );
  assert_eq!(
    super::api_base_or_default(Some("http://127.0.0.1:8080/api/".to_owned())),
    "http://127.0.0.1:8080/api"
  );
}

#[test]
fn install_wait_settings_take_positive_milliseconds_or_the_default() {
  let default = std::time::Duration::from_secs(600);
  assert_eq!(super::ms_or_default(None, default), Some(default));
  assert_eq!(super::ms_or_default(Some(""), default), Some(default));
  assert_eq!(
    super::ms_or_default(Some("250"), default),
    Some(std::time::Duration::from_millis(250))
  );
  assert_eq!(super::ms_or_default(Some("0"), default), None);
  assert_eq!(super::ms_or_default(Some("ten"), default), None);
}

const POLL_CHILD_TEST: &str = "config::tests::poll_interval_child_probe";

#[test]
#[ignore = "child-process probe; exercised by poll_interval_reads_milliseconds_and_diagnoses_bad_values"]
fn poll_interval_child_probe() {
  println!("poll_interval_ms={}", super::poll_interval().as_millis());
}

/// #206: `TOOLU_RUNNER_POLL_INTERVAL_MS` sets the toolu.sh idle wait; unset or
/// empty keeps 5 s, and a bad value is diagnosed by name and ignored.
#[test]
fn poll_interval_reads_milliseconds_and_diagnoses_bad_values() {
  for (value, expected_ms, diagnosed) in [
    (None, 5_000, false),
    (Some(""), 5_000, false),
    (Some("250"), 250, false),
    (Some("0"), 5_000, true),
    (Some("abc"), 5_000, true),
  ] {
    let mut child = Command::new(std::env::current_exe().expect("resolve current test binary"));
    child.args(["--ignored", "--exact", POLL_CHILD_TEST, "--nocapture"]);
    match value {
      Some(v) => child.env("TOOLU_RUNNER_POLL_INTERVAL_MS", v),
      None => child.env_remove("TOOLU_RUNNER_POLL_INTERVAL_MS"),
    };
    let output = child.output().expect("run poll-interval child probe");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{value:?}: {stderr}");
    assert!(
      stdout.contains(&format!("poll_interval_ms={expected_ms}\n")),
      "{value:?}: {stdout}"
    );
    assert_eq!(
      stderr.contains("TOOLU_RUNNER_POLL_INTERVAL_MS is not a positive integer"),
      diagnosed,
      "{value:?}: {stderr}"
    );
  }
}

/// #207: the toolu.sh origin drops a trailing `/` and falls back to the
/// console when unset or empty; an empty enrollment token counts as unset.
#[test]
fn toolu_api_url_and_enroll_token_treat_empty_as_unset() {
  temp_env::with_vars(
    [
      ("TOOLU_RUNNER_TOOLU_API_URL", None::<&str>),
      ("TOOLU_RUNNER_ENROLL_TOKEN", None),
    ],
    || {
      assert_eq!(super::toolu_api_url(), "https://console.toolu.sh");
      assert_eq!(super::enroll_token(), None);
    },
  );
  temp_env::with_vars(
    [
      ("TOOLU_RUNNER_TOOLU_API_URL", Some("")),
      ("TOOLU_RUNNER_ENROLL_TOKEN", Some("")),
    ],
    || {
      assert_eq!(super::toolu_api_url(), "https://console.toolu.sh");
      assert_eq!(super::enroll_token(), None);
    },
  );
  temp_env::with_vars(
    [
      (
        "TOOLU_RUNNER_TOOLU_API_URL",
        Some("https://dev-console.toolu.sh/"),
      ),
      ("TOOLU_RUNNER_ENROLL_TOKEN", Some("tlu_e_x")),
    ],
    || {
      assert_eq!(super::toolu_api_url(), "https://dev-console.toolu.sh");
      assert_eq!(super::enroll_token().as_deref(), Some("tlu_e_x"));
    },
  );
}

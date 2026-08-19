//! Tests for daemon configuration: token file reading (real files, real
//! rotation) and environment parsing (real inputs, no process env mutation).

use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use super::{Config, ConfigError, EnvSource, read_tokens};

/// An in-memory [`EnvSource`] so env-parsing tests never touch real process
/// environment variables (which are global, shared mutable state across
/// concurrently-running tests).
struct MapEnv(HashMap<&'static str, String>);

impl EnvSource for MapEnv {
  fn lookup(&self, var: &'static str) -> Result<Option<String>, ConfigError> {
    Ok(self.0.get(var).cloned())
  }
}

/// The full set of required variables, so a single field can be overridden
/// or omitted per test without repeating the rest.
fn required_vars() -> HashMap<&'static str, String> {
  HashMap::from([
    ("TOOLU_DAEMON_TOKEN_FILE", "/etc/toolu/token".to_owned()),
    ("TOOLU_DAEMON_VCPU", "4".to_owned()),
    ("TOOLU_DAEMON_MEMORY_MB", "8192".to_owned()),
    (
      "TOOLU_DAEMON_IMAGE",
      "ghcr.io/toolu/runner:latest".to_owned(),
    ),
  ])
}

#[test]
fn defaults_are_applied_when_optional_vars_are_unset() {
  let config = Config::from_reader(&MapEnv(required_vars())).expect("required vars present");

  assert_eq!(
    config.bind,
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080))
  );
  assert_eq!(config.queue_max, 32);
  assert_eq!(config.runtime, "sysbox-runc");
}

#[test]
fn explicit_optional_vars_override_defaults() {
  let mut vars = required_vars();
  vars.insert("TOOLU_DAEMON_BIND", "0.0.0.0:9090".to_owned());
  vars.insert("TOOLU_DAEMON_QUEUE_MAX", "64".to_owned());
  vars.insert("TOOLU_DAEMON_RUNTIME", "runc".to_owned());

  let config = Config::from_reader(&MapEnv(vars)).expect("valid overrides");

  assert_eq!(
    config.bind,
    "0.0.0.0:9090".parse::<SocketAddr>().expect("valid addr")
  );
  assert_eq!(config.queue_max, 64);
  assert_eq!(config.runtime, "runc");
}

#[test]
fn required_vars_are_read_through() {
  let config = Config::from_reader(&MapEnv(required_vars())).expect("required vars present");

  assert_eq!(config.token_file.as_os_str(), "/etc/toolu/token");
  assert_eq!(config.vcpu, 4);
  assert_eq!(config.memory_mb, 8192);
  assert_eq!(config.image, "ghcr.io/toolu/runner:latest");
}

#[test]
fn missing_required_var_is_rejected() {
  for missing in [
    "TOOLU_DAEMON_TOKEN_FILE",
    "TOOLU_DAEMON_VCPU",
    "TOOLU_DAEMON_MEMORY_MB",
    "TOOLU_DAEMON_IMAGE",
  ] {
    let mut vars = required_vars();
    vars.remove(missing);

    let err = Config::from_reader(&MapEnv(vars)).expect_err("missing var must be rejected");
    assert!(matches!(err, ConfigError::MissingEnv(var) if var == missing));
  }
}

#[test]
fn invalid_numeric_value_is_rejected() {
  let mut vars = required_vars();
  vars.insert("TOOLU_DAEMON_VCPU", "not-a-number".to_owned());

  let err = Config::from_reader(&MapEnv(vars)).expect_err("non-numeric vCPU must be rejected");
  assert!(matches!(
    err,
    ConfigError::InvalidEnvValue { var: "TOOLU_DAEMON_VCPU", value } if value == "not-a-number"
  ));
}

#[test]
fn current_token_only_is_read() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("token");
  fs::write(&path, "abc123\n").expect("write token file");

  let tokens = read_tokens(&path).expect("read tokens");

  assert_eq!(tokens.current, "abc123");
  assert_eq!(tokens.previous, None);
}

#[test]
fn current_and_previous_token_are_read() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("token");
  fs::write(&path, "current-tok\nprevious-tok\n").expect("write token file");

  let tokens = read_tokens(&path).expect("read tokens");

  assert_eq!(tokens.current, "current-tok");
  assert_eq!(tokens.previous.as_deref(), Some("previous-tok"));
}

#[test]
fn rotation_is_seen_without_reconstructing_anything() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("token");
  fs::write(&path, "old-current\n").expect("write initial token file");

  let before = read_tokens(&path).expect("read before rotation");
  assert_eq!(before.current, "old-current");
  assert_eq!(before.previous, None);

  // Rewrite in place — same path value, no new handle constructed — and
  // read again through the identical `read_tokens` call.
  fs::write(&path, "new-current\nold-current\n").expect("write rotated token file");

  let after = read_tokens(&path).expect("read after rotation");
  assert_eq!(after.current, "new-current");
  assert_eq!(after.previous.as_deref(), Some("old-current"));
}

#[test]
fn missing_file_is_rejected() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("does-not-exist");

  let err = read_tokens(&path).expect_err("missing file must be rejected");
  assert!(matches!(&err, ConfigError::TokenFileRead { path: err_path, .. } if *err_path == path));
}

#[test]
fn empty_file_is_rejected() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("token");
  fs::File::create(&path).expect("create empty file");

  let err = read_tokens(&path).expect_err("empty file must be rejected");
  assert!(matches!(&err, ConfigError::TokenFileEmpty(err_path) if *err_path == path));
}

#[test]
fn empty_first_line_is_rejected() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("token");
  let mut file = fs::File::create(&path).expect("create file");
  writeln!(file, "\nsecond-line").expect("write blank first line");

  let err = read_tokens(&path).expect_err("empty first line must be rejected");
  assert!(matches!(&err, ConfigError::TokenFileEmpty(err_path) if *err_path == path));
}

#[test]
fn trailing_whitespace_and_blank_previous_line_are_handled() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("token");
  fs::write(&path, "tok-with-space   \n   \n").expect("write token file");

  let tokens = read_tokens(&path).expect("read tokens");

  assert_eq!(tokens.current, "tok-with-space");
  assert_eq!(tokens.previous, None);
}

use shared::RunnerError;

#[test]
fn display_includes_variant_payload() {
  let e = RunnerError::Protocol("bad token".to_owned());
  assert_eq!(e.to_string(), "protocol error: bad token");
}

#[test]
fn display_for_cancelled() {
  let e = RunnerError::Cancelled;
  assert_eq!(e.to_string(), "job cancelled");
}

#[test]
fn from_io_error() {
  let io = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
  let e: RunnerError = io.into();
  assert!(matches!(e, RunnerError::Io(_)));
}

#[test]
fn from_serde_json_error() {
  let bad: serde_json::Result<i32> = serde_json::from_str("not a number");
  let err = bad.expect_err("expected parse error");
  let e: RunnerError = err.into();
  assert!(matches!(e, RunnerError::Json(_)));
}

#[test]
fn workspace_init_preserves_path_and_source() {
  let io = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
  let path = std::path::PathBuf::from("/var/lib/toolu-runner");
  let e: RunnerError = RunnerError::WorkspaceInit { path, source: io };
  let s = e.to_string();
  assert!(s.contains("/var/lib/toolu-runner"), "got: {s}");
  assert!(s.contains("denied"), "got: {s}");
}

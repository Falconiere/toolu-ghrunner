//! Tests for `RunnerError` display formatting.

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

#[test]
fn action_fetch_keeps_the_download_prefix_and_bare_message() {
  let error = shared::ActionFetchError {
    kind: shared::ActionFetchKind::ArchiveStatus(500),
    message: "archive status 500 Internal Server Error".to_owned(),
  };
  assert_eq!(
    error.to_string(),
    "archive status 500 Internal Server Error"
  );
  assert_eq!(
    RunnerError::ActionFetch(error).to_string(),
    "action download failed: archive status 500 Internal Server Error"
  );
}

#[test]
fn action_fetch_categories_follow_the_upstream_exception_split() {
  use shared::ActionFetchKind as K;
  let category = |kind| {
    shared::ActionFetchError {
      kind,
      message: String::new(),
    }
    .infrastructure_category()
  };
  // ActionManager.cs: FailedToResolveActionDownloadInfoException.
  assert_eq!(category(K::ResolveService), Some("resolve_action"));
  // FailedToDownloadActionException wraps every archive failure but 403.
  for status in [401, 404, 429, 500, 503] {
    assert_eq!(
      category(K::ArchiveStatus(status)),
      Some("error_download_action")
    );
  }
  assert_eq!(category(K::ArchiveTransport), Some("error_download_action"));
  // AccessDeniedException is the user's, not the runner's.
  assert_eq!(category(K::ArchiveStatus(403)), None);
  // InvalidActionArchiveException.
  assert_eq!(category(K::ArchiveContent), Some("invalid_action_download"));
}

//! GHES resource-path boundary checks for action download discovery.

use super::valid_relative_resource_path;

#[test]
fn resource_path_rejects_encoded_and_plain_traversal() {
  for path in [
    "_apis/actions",
    "https://other.example/_apis/actions",
    "/_apis/../secrets",
    "/_apis/%2e%2e/secrets",
    "/_apis/%2E%2E/secrets",
    "/_apis/%2fsecrets",
    "/_apis/%5Csecrets",
  ] {
    assert!(!valid_relative_resource_path(path), "accepted {path}");
  }
  assert!(valid_relative_resource_path(
    "/_apis/distributedtask/hubs/{hubName}/plans/{planId}/actions"
  ));
}

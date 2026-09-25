//! GHES resource-path boundary checks for action download discovery.

use super::{LegacyContext, valid_relative_resource_path};
use reqwest::Url;

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

#[test]
fn substituted_scope_cannot_traverse_the_service_path() {
  let base = Url::parse("https://ghe.example/root").expect("service URL");
  let mut context = LegacyContext {
    base_url: base.to_string(),
    scope: "../../other".to_owned(),
    hub: "build".to_owned(),
    plan: "plan".to_owned(),
    job: "job".to_owned(),
  };
  let entry = serde_json::json!({"relativePath":"/_apis/{scopeIdentifier}/actions"});
  assert!(context.resource_url(&entry, &base).is_err());

  context.scope = "%2e%2e%2fother".to_owned();
  assert!(context.resource_url(&entry, &base).is_err());

  context.scope = "scope".to_owned();
  let url = context.resource_url(&entry, &base).expect("valid scope");
  assert_eq!(url.path(), "/root/_apis/scope/actions");
}

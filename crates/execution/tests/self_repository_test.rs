//! `$/` self-repository references in action manifests (actions/runner #4669,
//! v2.338.0): resolve to the enclosing repository and ref, reject `@ref`.

use execution::execution::actions::resolver::{parse_action_ref, resolve_self_reference};

const PARENT: Option<(&str, &str)> = Some(("octo/tools", "abc123"));

#[test]
fn bare_root_and_subpath_resolve_to_parent_repo_and_ref() {
  assert_eq!(
    resolve_self_reference("$/", PARENT).expect("ok").as_deref(),
    Some("octo/tools@abc123")
  );
  assert_eq!(
    resolve_self_reference("$/.github/actions/inventory-client", PARENT)
      .expect("ok")
      .as_deref(),
    Some("octo/tools/.github/actions/inventory-client@abc123")
  );
  let resolved = resolve_self_reference("$//actions/nested/composite", PARENT)
    .expect("ok")
    .expect("some");
  let parsed = parse_action_ref(&resolved).expect("round-trips through the real parser");
  assert_eq!(parsed.subpath.as_deref(), Some("actions/nested/composite"));
  assert_eq!(parsed.git_ref, "abc123");
}

#[test]
fn other_references_are_left_alone() {
  for uses in ["actions/checkout@v4", "./local-action", "docker://alpine:3"] {
    assert_eq!(resolve_self_reference(uses, PARENT).expect("ok"), None);
  }
}

#[test]
fn ref_suffix_is_rejected_with_upstream_message() {
  let err = resolve_self_reference("$/foo@v1", PARENT).expect_err("@ref refused");
  assert!(
    err
      .to_string()
      .contains("Self-repository references do not support an '@ref' suffix. Actual '$/foo@v1'"),
    "got {err}"
  );
}

#[test]
fn missing_parent_identity_is_an_error() {
  for parent in [None, Some(("", "abc")), Some(("octo/tools", ""))] {
    let err = resolve_self_reference("$/", parent).expect_err("no parent");
    assert!(
      err.to_string().contains("Unable to resolve self-reference"),
      "got {err}"
    );
  }
}

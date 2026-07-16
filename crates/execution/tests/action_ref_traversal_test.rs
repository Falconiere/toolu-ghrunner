//! FIX 4: a remote `uses:` subpath must not traverse out of the action cache
//! dir. `parse_action_ref` rejects `..`/absolute components (mirroring
//! `ActionRef::local_dir`), so `read_manifest` cannot read an `action.yml`
//! from outside the cache. Real ref strings, no mocks.

use execution::execution::actions::resolver::parse_action_ref;

#[test]
fn rejects_parent_traversal_in_remote_subpath() {
  let err = parse_action_ref("actions/checkout/../../../../etc@v4")
    .expect_err("a `..`-traversing subpath must be rejected");
  assert!(
    format!("{err}").contains("invalid action ref"),
    "expected an invalid-action-ref error; got {err}"
  );
}

#[test]
fn rejects_absolute_root_subpath() {
  // `owner/repo//etc` yields the subpath `/etc` (a root component), which
  // `cache_dir.join("/etc")` would resolve OUTSIDE the cache dir.
  let err =
    parse_action_ref("owner/repo//etc@v1").expect_err("a root-absolute subpath must be rejected");
  assert!(format!("{err}").contains("invalid action ref"));
}

#[test]
fn accepts_plain_remote_ref() {
  let ar = parse_action_ref("actions/checkout@v4").expect("a plain ref must still parse");
  assert_eq!(ar.owner, "actions");
  assert_eq!(ar.repo, "checkout");
  assert_eq!(ar.git_ref, "v4");
  assert!(ar.subpath.is_none());
}

#[test]
fn accepts_valid_subpath() {
  let ar = parse_action_ref("owner/repo/path/to/action@v1").expect("a valid subpath must parse");
  assert_eq!(ar.subpath.as_deref(), Some("path/to/action"));
}

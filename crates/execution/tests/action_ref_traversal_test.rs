//! FIX 4: a remote `uses:` subpath must not traverse out of the action cache
//! dir. `parse_action_ref` rejects `..`/absolute components (mirroring
//! `ActionRef::local_dir`), so `read_manifest` cannot read an `action.yml`
//! from outside the cache. Real ref strings, no mocks.
//!
//! Also covers the wire-shape reconstruction bug: `resolve_action` rebuilds
//! its `uses:` string from a step's `ActionStepDefinitionReference` (split
//! `name` / `path` / `git_ref` fields), and used to drop `path` entirely —
//! silently truncating any `{owner}/{repo}/{path}@{ref}` reference to
//! `{owner}/{repo}@{ref}`. `build_uses_ref` is the extracted production
//! function `resolve_action` actually calls, so these tests exercise the
//! real reconstruction, not a copy of it.

use execution::execution::action_exec::build_uses_ref;
use execution::execution::actions::resolver::parse_action_ref;
use shared::ActionStepDefinitionReference;

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

/// Build a wire-shape reference with the fields GitHub actually sends: `name`
/// ("owner/repo"), an optional `path` subpath, and `git_ref`.
fn wire_reference(name: &str, path: Option<&str>, git_ref: &str) -> ActionStepDefinitionReference {
  ActionStepDefinitionReference {
    ref_type: Some("repository".to_owned()),
    image: None,
    name: Some(name.to_owned()),
    git_ref: Some(git_ref.to_owned()),
    repository_type: Some("GitHub".to_owned()),
    path: path.map(ToOwned::to_owned),
  }
}

/// The exact production shape from CodaSignal/toolu.sh PR 40 job
/// 93208325542: `name: "falconiere/toolu-ghactions"`, `path: "code-review"`,
/// `git_ref: "v7"`. Before the fix, `path` was dropped and this resolved to
/// `falconiere/toolu-ghactions@v7` — the wrong (parent) repo root, no
/// `action.yml` there, and the job died with no log output.
#[test]
fn reconstructs_subpath_reference_from_wire_fields() {
  let reference = wire_reference("falconiere/toolu-ghactions", Some("code-review"), "v7");

  let uses = build_uses_ref(&reference);
  assert_eq!(uses, "falconiere/toolu-ghactions/code-review@v7");

  let ar = parse_action_ref(&uses).expect("the reconstructed ref must parse");
  assert_eq!(ar.owner, "falconiere");
  assert_eq!(ar.repo, "toolu-ghactions");
  assert_eq!(ar.git_ref, "v7");
  assert_eq!(ar.subpath.as_deref(), Some("code-review"));
}

/// The no-subpath form (e.g. `actions/checkout@v5`) must keep producing the
/// exact old `owner/repo@ref` string — the fix must not touch references
/// with an absent or empty `path`.
#[test]
fn reconstructs_plain_reference_with_no_path() {
  let reference = wire_reference("actions/checkout", None, "v5");

  let uses = build_uses_ref(&reference);
  assert_eq!(uses, "actions/checkout@v5");

  let ar = parse_action_ref(&uses).expect("the reconstructed ref must parse");
  assert_eq!(ar.owner, "actions");
  assert_eq!(ar.repo, "checkout");
  assert_eq!(ar.git_ref, "v5");
  assert!(ar.subpath.is_none());
}

/// An empty `path` (as opposed to `None`) must be treated identically to a
/// missing one — the wire could plausibly send `"path": ""`.
#[test]
fn empty_path_is_treated_as_absent() {
  let reference = wire_reference("actions/checkout", Some(""), "v5");
  assert_eq!(build_uses_ref(&reference), "actions/checkout@v5");
}

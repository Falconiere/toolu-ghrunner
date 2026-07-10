//! Tests for `parse_action_ref` — the only path that turns `uses:`
//! strings into tarball URLs. Every Node/composite action step flows
//! through this parser.
//!
//! Uses real-shape ref strings (no mock data).

use toolu_runner::execution::actions::resolver::{ActionRef, ActionRefKind, parse_action_ref};

#[test]
fn parses_remote_action_with_tag() {
  let ar = parse_action_ref("actions/checkout@v4").expect("valid ref");
  assert_eq!(ar.kind, ActionRefKind::Remote);
  assert_eq!(ar.owner, "actions");
  assert_eq!(ar.repo, "checkout");
  assert_eq!(ar.git_ref, "v4");
  assert!(ar.subpath.is_none());
}

#[test]
fn parses_remote_action_with_subpath_and_branch() {
  let ar = parse_action_ref("owner/repo/path/to/action@main").expect("valid ref");
  assert_eq!(ar.kind, ActionRefKind::Remote);
  assert_eq!(ar.owner, "owner");
  assert_eq!(ar.repo, "repo");
  assert_eq!(ar.git_ref, "main");
  assert_eq!(ar.subpath.as_deref(), Some("path/to/action"));
}

#[test]
fn parses_remote_action_with_full_sha() {
  let sha = "a".repeat(40);
  let uses = format!("owner/repo@{sha}");
  let ar = parse_action_ref(&uses).expect("valid ref with sha");
  assert_eq!(ar.kind, ActionRefKind::Remote);
  assert_eq!(ar.owner, "owner");
  assert_eq!(ar.repo, "repo");
  assert_eq!(ar.git_ref, sha);
}

#[test]
fn parses_local_action() {
  let ar = parse_action_ref("./.github/actions/local").expect("valid local ref");
  assert_eq!(ar.kind, ActionRefKind::Local);
  assert_eq!(ar.local_path.as_deref(), Some("./.github/actions/local"));
}

#[test]
fn rejects_empty_string() {
  let err = parse_action_ref("").expect_err("empty string should error");
  let msg = format!("{err}");
  assert!(
    msg.contains("invalid action ref"),
    "expected error msg: {msg}"
  );
}

#[test]
fn rejects_missing_ref() {
  let err = parse_action_ref("actions/checkout").expect_err("missing @ref should error");
  let msg = format!("{err}");
  assert!(msg.contains("missing @ref"), "expected error msg: {msg}");
}

#[test]
fn local_dir_rejects_parent_traversal() {
  let ar = parse_action_ref("./../outside").expect("parses as a local ref");
  assert_eq!(ar.kind, ActionRefKind::Local);
  assert_eq!(
    ar.local_dir(std::path::Path::new("/workspace")),
    None,
    "a local ref with `..` segments must not resolve outside the workspace"
  );
}

#[test]
fn local_dir_rejects_embedded_parent_traversal() {
  let ar = parse_action_ref("./actions/../../outside").expect("parses as a local ref");
  assert_eq!(
    ar.local_dir(std::path::Path::new("/workspace")),
    None,
    "embedded `..` segments must also be rejected"
  );
}

#[test]
fn local_dir_resolves_plain_relative_path() {
  let ar = parse_action_ref("./.github/actions/local").expect("valid local ref");
  assert_eq!(
    ar.local_dir(std::path::Path::new("/workspace")),
    Some(std::path::PathBuf::from("/workspace/.github/actions/local")),
    "a traversal-free local ref must resolve under the workspace"
  );
}

#[test]
fn local_dir_rejects_missing_dot_slash_prefix() {
  // `parse_action_ref` never produces a Local ref without the `./` prefix,
  // but `local_dir` is `pub`: a hand-built ref must resolve to None, not
  // silently to the workspace root.
  let ar = ActionRef {
    kind: ActionRefKind::Local,
    owner: String::new(),
    repo: String::new(),
    git_ref: String::new(),
    subpath: None,
    local_path: Some("no-prefix".to_owned()),
  };
  assert_eq!(
    ar.local_dir(std::path::Path::new("/workspace")),
    None,
    "a local path without the ./ prefix must not resolve"
  );
}

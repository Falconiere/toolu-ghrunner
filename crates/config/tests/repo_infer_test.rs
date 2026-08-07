//! Integration tests for `config::repo_infer` (zero-arg-register AC-1/AC-2).
//!
//! Real data only: `parse_remote_url` runs over a table of real-world
//! remote URL forms; `detect_repo` runs against real temp git repositories
//! built with `git init` + `git remote add origin` subprocesses.
//!
//! There is no in-process test for "an ambient `GIT_DIR` must not steer
//! `detect_repo`": setting it requires `std::env::set_var`, which is `unsafe`
//! in edition 2024, and this workspace sets `unsafe_code = "forbid"`. The
//! guard is the pre-push gate itself — git exports `GIT_DIR` into every hook
//! subprocess, so `./tools/check.sh all` runs these four `detect_repo` cases
//! in exactly the polluted environment that regressed them. Reproduce by hand
//! with: `GIT_DIR=$(git rev-parse --absolute-git-dir) cargo test -p config
//! --test repo_infer_test`.

use std::path::Path;

use config::repo_infer::{self, InferredRepo};
use tempfile::TempDir;

/// Run `git -C <dir> <args…>`, asserting it succeeds.
///
/// The `GIT_*` pointers are cleared for the same reason `detect_repo` clears
/// them: they outrank `-C` and would aim these fixture commands at whatever
/// repository the surrounding process belongs to. Without this the suite
/// passes standalone and fails under a git hook, which is exactly where the
/// pre-push gate runs it.
fn git(dir: &Path, args: &[&str]) -> Result<(), std::io::Error> {
  let out = std::process::Command::new("git")
    .arg("-C")
    .arg(dir)
    .args(args)
    .env_remove("GIT_DIR")
    .env_remove("GIT_WORK_TREE")
    .env_remove("GIT_INDEX_FILE")
    .env_remove("GIT_PREFIX")
    .env_remove("GIT_COMMON_DIR")
    .output()?;
  assert!(
    out.status.success(),
    "git {args:?} failed: {}",
    String::from_utf8_lossy(&out.stderr)
  );
  Ok(())
}

// ── AC-1: parse_remote_url over the real remote URL forms ───────────

#[test]
fn parse_remote_url_handles_every_real_github_form() {
  let table = [
    ("git@github.com:octocat/hello-world.git", "github.com"),
    ("https://github.com/octocat/hello-world.git", "github.com"),
    ("https://github.com/octocat/hello-world", "github.com"),
    ("ssh://git@github.com/octocat/hello-world.git", "github.com"),
    (
      "ssh://git@github.com:22/octocat/hello-world.git",
      "github.com",
    ),
  ];
  for (url, host) in table {
    assert_eq!(
      repo_infer::parse_remote_url(url),
      Some(InferredRepo {
        host: host.to_owned(),
        owner: "octocat".to_owned(),
        repo: "hello-world".to_owned(),
      }),
      "failed on {url:?}"
    );
  }
}

#[test]
fn parse_remote_url_keeps_ssh_alias_hosts_verbatim() {
  // Caller rejects non-github.com hosts later; the parser stays host-agnostic.
  assert_eq!(
    repo_infer::parse_remote_url("git@my-alias:x/y.git"),
    Some(InferredRepo {
      host: "my-alias".to_owned(),
      owner: "x".to_owned(),
      repo: "y".to_owned(),
    })
  );
}

#[test]
fn parse_remote_url_rejects_non_remote_shapes() {
  let non_remotes = [
    "",
    "/srv/git/hello-world.git",      // bare local path
    "../relative/repo",              // relative local path
    "git@github.com",                // no path at all
    "https://github.com/",           // no owner/repo segments
    "https://github.com/only-owner", // one segment
    "https://github.com/o/r/extra",  // three segments
    "ssh://git@/octocat/repo.git",   // empty host
    "file:///srv/git/repo.git",      // unsupported scheme, no scp shape
  ];
  for url in non_remotes {
    assert_eq!(
      repo_infer::parse_remote_url(url),
      None,
      "{url:?} must not parse"
    );
  }
}

// ── AC-2: detect_repo against real temp git repositories ────────────

#[test]
fn detect_repo_reads_the_origin_remote() {
  let dir = TempDir::new().unwrap();
  git(dir.path(), &["init", "-q"]).unwrap();
  git(
    dir.path(),
    &[
      "remote",
      "add",
      "origin",
      "git@github.com:octocat/hello-world.git",
    ],
  )
  .unwrap();
  assert_eq!(
    repo_infer::detect_repo(dir.path()).unwrap(),
    InferredRepo {
      host: "github.com".to_owned(),
      owner: "octocat".to_owned(),
      repo: "hello-world".to_owned(),
    }
  );
}

#[test]
fn detect_repo_errors_outside_a_git_repo_naming_url_flag() {
  let dir = TempDir::new().unwrap();
  let err = repo_infer::detect_repo(dir.path()).unwrap_err();
  let msg = err.to_string();
  assert!(
    msg.contains("not a git repository"),
    "must say the dir is not a git repository; got: {msg}"
  );
  assert!(
    msg.contains("--url"),
    "must name the --url escape hatch; got: {msg}"
  );
}

#[test]
fn detect_repo_errors_without_an_origin_remote_naming_url_flag() {
  let dir = TempDir::new().unwrap();
  git(dir.path(), &["init", "-q"]).unwrap();
  let err = repo_infer::detect_repo(dir.path()).unwrap_err();
  let msg = err.to_string();
  assert!(
    msg.contains("origin"),
    "must say the `origin` remote is missing; got: {msg}"
  );
  assert!(
    msg.contains("--url"),
    "must name the --url escape hatch; got: {msg}"
  );
}

#[test]
fn detect_repo_errors_on_unparseable_remote_naming_url_flag() {
  let dir = TempDir::new().unwrap();
  git(dir.path(), &["init", "-q"]).unwrap();
  // A local-path remote is valid for git but carries no owner/repo.
  git(
    dir.path(),
    &["remote", "add", "origin", "/srv/git/mirror.git"],
  )
  .unwrap();
  let err = repo_infer::detect_repo(dir.path()).unwrap_err();
  let msg = err.to_string();
  assert!(
    msg.contains("could not parse"),
    "must say the remote URL did not parse; got: {msg}"
  );
  assert!(
    msg.contains("--url"),
    "must name the --url escape hatch; got: {msg}"
  );
}

//! Real-data tests for the `hashFiles()` expression function.
//!
//! Every case runs against real files on a real filesystem (no mocks) and is
//! driven through the public `evaluate()` entry point, so the expression
//! lexer, parser, dispatcher, and globber are all exercised together.
//!
//! Expected digests are computed by an independent implementation (Python's
//! `hashlib`), not by this crate, from the algorithm `@actions/glob`'s
//! `internal-hash-files.ts` specifies:
//!
//! ```text
//! SHA256( SHA256(file_1) || SHA256(file_2) || ... )
//! ```
//!
//! where each inner digest contributes its raw 32 bytes and only the outer
//! digest is hex-encoded, folded in traversal order.

use std::collections::HashMap;
use std::error::Error;
use std::path::Path;

use toolu_runner::execution::expressions::evaluator::{self, EvalContext, JobStatus};
use toolu_runner::execution::expressions::types::ExprValue;

type TestResult = Result<(), Box<dyn Error>>;

/// Build an `EvalContext` rooted at `workspace` with no named contexts.
fn context(workspace: &Path) -> EvalContext {
  EvalContext {
    contexts: HashMap::new(),
    job_status: JobStatus::Success,
    workspace: Some(workspace.to_path_buf()),
  }
}

/// Write `content` to `workspace/relative`, creating parent directories.
fn write_file(workspace: &Path, relative: &str, content: &str) -> std::io::Result<()> {
  let path = workspace.join(relative);
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent)?;
  }
  std::fs::write(&path, content)
}

/// Evaluate `expr` against `workspace` and return the resulting string.
fn hash_of(workspace: &Path, expr: &str) -> Result<String, Box<dyn Error>> {
  match evaluator::evaluate(expr, &context(workspace))? {
    ExprValue::String(text) => Ok(text),
    ExprValue::Null
    | ExprValue::Bool(_)
    | ExprValue::Number(_)
    | ExprValue::Array(_)
    | ExprValue::Object(_) => Err("hashFiles must return a string".into()),
  }
}

/// The message of `expr`'s evaluation error, or the empty string if it
/// unexpectedly succeeded.
fn error_message(workspace: Option<&Path>, expr: &str) -> String {
  let ctx = workspace.map_or_else(
    || EvalContext {
      contexts: HashMap::new(),
      job_status: JobStatus::Success,
      workspace: None,
    },
    context,
  );
  evaluator::evaluate(expr, &ctx)
    .err()
    .map(|err| err.to_string())
    .unwrap_or_default()
}

/// Directory children are visited in byte-wise name order, depth-first — so
/// `a/` precedes `a-b/` because `strcmp("a", "a-b") < 0`, even though the full
/// path `a-b/Cargo.lock` sorts before `a/Cargo.lock` (`-` is 0x2d, `/` is 0x2f).
///
/// This is the case a naive full-path sort gets wrong, and hyphenated sibling
/// crate directories hit it constantly.
#[test]
fn dfs_order_visits_a_before_a_hyphen_b() -> TestResult {
  let workspace = tempfile::tempdir()?;
  write_file(workspace.path(), "a/Cargo.lock", "alpha")?;
  write_file(workspace.path(), "a-b/Cargo.lock", "beta")?;

  let digest = hash_of(workspace.path(), "hashFiles('**/Cargo.lock')")?;

  assert_eq!(
    digest, "8450e9a90d144185def662fffc477da5e0325d80be5de388ec20d9c58d6c72d0",
    "must equal SHA256(SHA256('alpha') || SHA256('beta'))"
  );
  Ok(())
}

/// The digest folds per-file SHA-256 digests, never the raw file bytes.
/// Guards against a regression to `hasher.update(&content)` over each file.
#[test]
fn raw_content_concatenation_is_not_the_algorithm() -> TestResult {
  let workspace = tempfile::tempdir()?;
  write_file(workspace.path(), "a/Cargo.lock", "alpha")?;
  write_file(workspace.path(), "a-b/Cargo.lock", "beta")?;

  let digest = hash_of(workspace.path(), "hashFiles('**/Cargo.lock')")?;

  assert_ne!(
    digest, "a4c4aeb92c20500f364b12b3771ef3a11193e2cf04d0f28956a829749993b39f",
    "must not equal SHA256('alpha' || 'beta') — that is the pre-fix behavior"
  );
  Ok(())
}

/// Zero matches yields the empty string, not an error and not a digest.
#[test]
fn no_match_returns_empty_string() -> TestResult {
  let workspace = tempfile::tempdir()?;
  write_file(workspace.path(), "src/main.rs", "fn main() {}")?;

  assert_eq!(hash_of(workspace.path(), "hashFiles('**/Cargo.lock')")?, "");
  Ok(())
}

/// A leading `!` negates, clearing an earlier positive match.
#[test]
fn negation_excludes_matched_file() -> TestResult {
  let workspace = tempfile::tempdir()?;
  write_file(workspace.path(), "a/Cargo.lock", "alpha")?;
  write_file(workspace.path(), "a-b/Cargo.lock", "beta")?;

  let digest = hash_of(
    workspace.path(),
    "hashFiles('**/Cargo.lock', '!a-b/Cargo.lock')",
  )?;

  assert_eq!(
    digest, "aa86be763e41db7eaae266afc79ab46d02343c5d3b05da171d351afbd25c1525",
    "must equal SHA256(SHA256('alpha')) — beta excluded"
  );
  Ok(())
}

/// Search roots are walked in the order the patterns declared them, not in
/// filesystem order: `z.lock` hashes before `a.lock`.
#[test]
fn search_roots_hash_in_declared_pattern_order() -> TestResult {
  let workspace = tempfile::tempdir()?;
  write_file(workspace.path(), "z.lock", "zed")?;
  write_file(workspace.path(), "a.lock", "ay")?;

  let digest = hash_of(workspace.path(), "hashFiles('z.lock', 'a.lock')")?;

  assert_eq!(
    digest, "ab36fcb21211c2b82444e09466a36d1efa90a84b719ca47451b881de6b006877",
    "must equal SHA256(SHA256('zed') || SHA256('ay'))"
  );
  Ok(())
}

/// minimatch runs with `dot: true`, so `*` matches a leading dot.
#[test]
fn dotfiles_are_matched_by_star() -> TestResult {
  let workspace = tempfile::tempdir()?;
  write_file(workspace.path(), ".hidden.lock", "hidden")?;

  let digest = hash_of(workspace.path(), "hashFiles('*.lock')")?;

  assert_eq!(
    digest, "122fe24badc5941886d112b99c35e2daeff9dba7b32ca25dc5c080aec79bbab8",
    "must equal SHA256(SHA256('hidden'))"
  );
  Ok(())
}

/// An empty file is hashed (contributing the empty-string SHA-256), not skipped.
#[test]
fn empty_file_is_hashed_not_skipped() -> TestResult {
  let workspace = tempfile::tempdir()?;
  write_file(workspace.path(), "empty.lock", "")?;

  let digest = hash_of(workspace.path(), "hashFiles('*.lock')")?;

  assert_eq!(
    digest, "5df6e0e2761359d30a8275058e299fcc0381534545f55cf43e41983f5d4c9456",
    "must equal SHA256(SHA256(''))"
  );
  Ok(())
}

/// Every pattern gains an implicit `<pattern>/**` twin, so naming a directory
/// hashes its contents recursively.
#[test]
fn implicit_descendants_hash_directory_contents() -> TestResult {
  let workspace = tempfile::tempdir()?;
  write_file(workspace.path(), "dist/a.txt", "one")?;
  write_file(workspace.path(), "dist/b.txt", "two")?;

  let digest = hash_of(workspace.path(), "hashFiles('dist')")?;

  assert_eq!(
    digest, "11914c19a28a98c57d12f3cce6c32b7944784f4b4781a706c24eb1dc284e2856",
    "must equal SHA256(SHA256('one') || SHA256('two'))"
  );
  Ok(())
}

/// Matches outside `GITHUB_WORKSPACE` are silently skipped, not hashed.
#[test]
fn paths_outside_the_workspace_are_skipped() -> TestResult {
  let workspace = tempfile::tempdir()?;
  let outside = tempfile::tempdir()?;
  write_file(outside.path(), "secret.lock", "should never be hashed")?;

  let pattern = outside.path().join("secret.lock");
  let expr = format!("hashFiles('{}')", pattern.display());

  assert_eq!(hash_of(workspace.path(), &expr)?, "");
  Ok(())
}

/// `..` is rejected at pattern-parse time, as GitHub does.
#[test]
fn parent_directory_escape_is_rejected() -> TestResult {
  let workspace = tempfile::tempdir()?;

  let message = error_message(Some(workspace.path()), "hashFiles('../outside/*.lock')");

  assert!(
    message.contains("relative pathing"),
    "`..` must be rejected by name, got: {message}"
  );
  Ok(())
}

/// Traversal classifies with `lstat` (never descending a symlinked directory),
/// but the final directory check follows symlinks — so a symlink to a
/// directory reaches the check and is skipped, yielding no digest.
#[cfg(unix)]
#[test]
fn symlink_to_directory_is_skipped() -> TestResult {
  let workspace = tempfile::tempdir()?;
  write_file(workspace.path(), "real/inner.txt", "inner")?;
  std::os::unix::fs::symlink(
    workspace.path().join("real"),
    workspace.path().join("link.lock"),
  )?;

  assert_eq!(hash_of(workspace.path(), "hashFiles('*.lock')")?, "");
  Ok(())
}

/// A symlink to a file is hashed by its target's content.
#[cfg(unix)]
#[test]
fn symlink_to_file_hashes_target_content() -> TestResult {
  let workspace = tempfile::tempdir()?;
  write_file(workspace.path(), "target.txt", "hidden")?;
  std::os::unix::fs::symlink(
    workspace.path().join("target.txt"),
    workspace.path().join("link.lock"),
  )?;

  let digest = hash_of(workspace.path(), "hashFiles('*.lock')")?;

  assert_eq!(
    digest, "122fe24badc5941886d112b99c35e2daeff9dba7b32ca25dc5c080aec79bbab8",
    "must equal SHA256(SHA256('hidden')) — the target's content"
  );
  Ok(())
}

/// `hashFiles()` outside a job workspace is an error, not a silent empty hash.
#[test]
fn missing_workspace_is_an_error() {
  let message = error_message(None, "hashFiles('*.lock')");

  assert!(
    message.contains("workspace"),
    "error should name the cause, got: {message}"
  );
}

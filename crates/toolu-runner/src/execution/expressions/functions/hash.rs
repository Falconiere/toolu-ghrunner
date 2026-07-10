//! GitHub-compatible `hashFiles()` for the `${{ }}` expression engine.
//!
//! The digest is `SHA256( SHA256(file_1) || SHA256(file_2) || ... )`, where
//! each inner digest contributes its **raw 32 bytes** (not hex) and only the
//! outer digest is hex-encoded. Files are folded in `glob_walk` traversal
//! order. Both details are load-bearing: `actions/cache` keys computed here
//! must equal the keys a GitHub-hosted runner computes for the same tree, or
//! every cache lookup misses.
//!
//! Mirrors `@actions/glob`'s `internal-hash-files.ts` and the runner's
//! `HashFilesFunction.cs`.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use shared::RunnerError;

use super::glob_walk::{literal_prefix, search_roots, walk};

/// The only option `hashFiles()` accepts, as a leading positional argument.
const FOLLOW_SYMLINKS_FLAG: &str = "--follow-symbolic-links";

/// minimatch's effective options: dotfiles match `*`, `*` never crosses `/`,
/// and matching is case-sensitive off Windows.
const MATCH_OPTIONS: glob::MatchOptions = glob::MatchOptions {
  case_sensitive: true,
  require_literal_separator: true,
  require_literal_leading_dot: false,
};

/// One parsed pattern: an absolute glob, its negation flag, and the DFS root
/// its literal prefix implies.
struct HashPattern {
  glob: glob::Pattern,
  negate: bool,
  search_root: PathBuf,
}

impl HashPattern {
  /// Compile an absolute pattern path into a matcher plus its search root.
  fn new(pattern: &Path, negate: bool) -> Result<Self, RunnerError> {
    let text = pattern.to_string_lossy();
    let glob = glob::Pattern::new(&text)
      .map_err(|err| RunnerError::Expression(format!("hashFiles pattern '{text}': {err}")))?;
    Ok(Self {
      glob,
      negate,
      search_root: literal_prefix(pattern),
    })
  }
}

/// Hash every file under `workspace` matching `args`, GitHub's way.
///
/// Accepts a leading `--follow-symbolic-links`; remaining arguments may carry
/// newline-separated patterns, where `#` comments blank out and leading `!`
/// negates. Returns the empty string when nothing matched.
///
/// # Errors
///
/// Returns `RunnerError::Expression` on an invalid option or pattern, a
/// `.`/`..` segment, or an unreadable file or directory.
pub fn hash_files(workspace: &Path, args: &[String]) -> Result<String, RunnerError> {
  let (follow_symlinks, raw_patterns) = split_options(args)?;
  let patterns = parse_patterns(workspace, raw_patterns)?;
  if patterns.is_empty() {
    return Ok(String::new());
  }

  let roots = search_roots(
    patterns
      .iter()
      .filter(|pattern| !pattern.negate)
      .map(|pattern| pattern.search_root.clone())
      .collect(),
  );

  fold_digests(workspace, &patterns, &roots, follow_symlinks)
}

/// Walk each root in order, folding every matched file's SHA-256 into one
/// outer SHA-256. Returns the empty string when nothing matched.
fn fold_digests(
  workspace: &Path,
  patterns: &[HashPattern],
  roots: &[PathBuf],
  follow_symlinks: bool,
) -> Result<String, RunnerError> {
  let mut outer = Sha256::new();
  let mut matched_any = false;

  {
    let mut visit = |path: &Path| -> Result<(), RunnerError> {
      if !path.starts_with(workspace) || !matches_patterns(patterns, path) {
        return Ok(());
      }
      if let Some(digest) = digest_file(path)? {
        outer.update(digest);
        matched_any = true;
      }
      Ok(())
    };

    for root in roots {
      walk(root, follow_symlinks, &mut visit)?;
    }
  }

  if !matched_any {
    return Ok(String::new());
  }
  Ok(format!("{:x}", outer.finalize()))
}

/// SHA-256 of `path`'s contents as raw bytes, or `None` if it is a directory.
///
/// Traversal classifies entries with `lstat`, but this check follows symlinks
/// — so a symlink to a directory is skipped, while a symlink to a file is
/// hashed by its target's content. A dangling symlink is a hard error, as it
/// is on GitHub.
fn digest_file(path: &Path) -> Result<Option<sha2::digest::Output<Sha256>>, RunnerError> {
  let metadata = std::fs::metadata(path)
    .map_err(|err| RunnerError::Expression(format!("hashFiles stat {}: {err}", path.display())))?;
  if metadata.is_dir() {
    return Ok(None);
  }

  let content = std::fs::read(path)
    .map_err(|err| RunnerError::Expression(format!("hashFiles read {}: {err}", path.display())))?;
  let mut inner = Sha256::new();
  inner.update(&content);
  Ok(Some(inner.finalize()))
}

/// Split a leading `--follow-symbolic-links` off the argument list.
///
/// The flag matches case-insensitively and only in the first argument;
/// any other leading `--` argument is an error. All three behaviors mirror
/// the runner's `HashFilesFunction.cs`, which compares with
/// `StringComparison.OrdinalIgnoreCase` and treats a `--` string in any
/// later parameter as an ordinary pattern.
fn split_options(args: &[String]) -> Result<(bool, &[String]), RunnerError> {
  let Some((first, rest)) = args.split_first() else {
    return Ok((false, args));
  };
  if !first.starts_with("--") {
    return Ok((false, args));
  }
  if first.eq_ignore_ascii_case(FOLLOW_SYMLINKS_FLAG) {
    return Ok((true, rest));
  }
  Err(RunnerError::Expression(format!(
    "hashFiles: invalid option '{first}'"
  )))
}

/// Expand arguments into absolute patterns, each followed by its implicit
/// `<pattern>/**` descendant twin (on by default in `@actions/glob`).
fn parse_patterns(workspace: &Path, args: &[String]) -> Result<Vec<HashPattern>, RunnerError> {
  let mut patterns = Vec::new();

  for arg in args {
    let normalized = arg.replace("\r\n", "\n").replace('\r', "\n");
    for line in normalized.split('\n') {
      let (negate, body) = strip_negation(line.trim());
      if body.is_empty() || body.starts_with('#') {
        continue;
      }
      reject_relative_segments(body)?;

      let absolute = if Path::new(body).is_absolute() {
        PathBuf::from(body)
      } else {
        workspace.join(body)
      };
      let ends_with_globstar = absolute
        .file_name()
        .is_some_and(|name| name.to_string_lossy() == "**");

      patterns.push(HashPattern::new(&absolute, negate)?);
      if !ends_with_globstar {
        patterns.push(HashPattern::new(&absolute.join("**"), negate)?);
      }
    }
  }

  Ok(patterns)
}

/// Strip leading `!` markers, each toggling negation.
fn strip_negation(line: &str) -> (bool, &str) {
  let mut negate = false;
  let mut rest = line;
  while let Some(stripped) = rest.strip_prefix('!') {
    negate = !negate;
    rest = stripped.trim();
  }
  (negate, rest)
}

/// Reject `..` anywhere and `.` outside the first segment, as GitHub does.
fn reject_relative_segments(pattern: &str) -> Result<(), RunnerError> {
  for (index, segment) in pattern.split('/').enumerate() {
    if segment == ".." || (segment == "." && index != 0) {
      return Err(RunnerError::Expression(format!(
        "hashFiles: relative pathing '.' and '..' is not allowed in '{pattern}'"
      )));
    }
  }
  Ok(())
}

/// Apply patterns in order: a positive match sets the hit, a negated match
/// clears it. Mirrors `patternHelper.match`'s `|=` / `&= ~` fold.
fn matches_patterns(patterns: &[HashPattern], path: &Path) -> bool {
  let mut hit = false;
  for pattern in patterns {
    if !pattern.glob.matches_path_with(path, MATCH_OPTIONS) {
      continue;
    }
    hit = !pattern.negate;
  }
  hit
}

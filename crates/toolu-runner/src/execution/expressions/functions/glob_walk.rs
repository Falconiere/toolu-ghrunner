//! Directory traversal for `hashFiles()`.
//!
//! Reproduces `@actions/glob`'s `DefaultGlobber.globGenerator` ordering:
//! depth-first pre-order from each search root, visiting a directory's
//! children in byte-wise filename order (what `readdir` yields on Linux and
//! macOS, where libuv sorts with `strcmp`).
//!
//! Order is load-bearing: `hashFiles` folds per-file digests into one hash in
//! traversal order, so a different order silently yields a different cache key.

use std::path::{Path, PathBuf};

use shared::RunnerError;

/// Glob metacharacters that terminate a pattern's literal prefix.
const GLOB_META: [char; 3] = ['*', '?', '['];

/// The literal directory prefix of `pattern`, used as a DFS search root.
///
/// `<ws>/**/Cargo.lock` roots at `<ws>`; a fully literal `<ws>/Cargo.lock`
/// roots at itself and is visited as a single file.
pub fn literal_prefix(pattern: &Path) -> PathBuf {
  let mut root = PathBuf::new();
  for component in pattern.components() {
    let text = component.as_os_str().to_string_lossy();
    if text.contains(|ch| GLOB_META.contains(&ch)) {
      break;
    }
    root.push(component);
  }
  root
}

/// Deduplicate search roots: drop exact repeats and any root nested inside
/// another, preserving declared order.
///
/// GitHub visits roots in the order the patterns named them, so two disjoint
/// roots hash in pattern order rather than in filesystem order.
pub fn search_roots(roots: Vec<PathBuf>) -> Vec<PathBuf> {
  let mut unique: Vec<PathBuf> = Vec::new();
  for root in roots {
    if !unique.contains(&root) {
      unique.push(root);
    }
  }
  unique
    .iter()
    .filter(|root| {
      let root: &PathBuf = root;
      !unique
        .iter()
        .any(|other| other != root && root.starts_with(other))
    })
    .cloned()
    .collect()
}

/// Depth-first pre-order walk from `root`, calling `visit` on every
/// non-directory entry, children in byte-wise filename order.
///
/// With `follow_symlinks` false (GitHub's default for `hashFiles`) a symlinked
/// directory is never descended into — it reaches `visit` as a candidate file,
/// exactly as `@actions/glob` does by classifying entries with `lstat`. A
/// missing root, or a dangling link encountered while following, is skipped.
///
/// # Errors
///
/// Returns `RunnerError::Expression` if a directory cannot be listed, and
/// propagates any error `visit` returns.
pub fn walk(
  root: &Path,
  follow_symlinks: bool,
  visit: &mut dyn FnMut(&Path) -> Result<(), RunnerError>,
) -> Result<(), RunnerError> {
  let mut stack = vec![root.to_path_buf()];

  while let Some(item) = stack.pop() {
    let metadata = if follow_symlinks {
      std::fs::metadata(&item)
    } else {
      std::fs::symlink_metadata(&item)
    };
    let Ok(metadata) = metadata else { continue };

    if !metadata.is_dir() {
      visit(&item)?;
      continue;
    }

    let mut children = read_children(&item)?;
    children.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    // `pop()` takes the tail, so push in descending order to visit ascending.
    stack.extend(children.into_iter().rev());
  }

  Ok(())
}

/// List a directory's entries as full paths.
fn read_children(dir: &Path) -> Result<Vec<PathBuf>, RunnerError> {
  let entries = std::fs::read_dir(dir).map_err(|err| {
    RunnerError::Expression(format!("hashFiles read_dir {}: {err}", dir.display()))
  })?;

  let mut children = Vec::new();
  for entry in entries {
    let entry = entry.map_err(|err| {
      RunnerError::Expression(format!("hashFiles read_dir {}: {err}", dir.display()))
    })?;
    children.push(entry.path());
  }
  Ok(children)
}

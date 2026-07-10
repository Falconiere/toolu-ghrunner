//! Deterministic BLAKE3 fingerprint of a directory tree (structure + content).
//!
//! A small self-contained depth-first walk, independent of the expression
//! engine's `glob_walk`: each directory's entries are visited in byte-sorted
//! name order (determinism only — no GitHub-compat requirement) and each
//! file's workspace-relative path plus its content digest are folded into one
//! BLAKE3. Symlinks are not followed into directories.

use std::path::{Path, PathBuf};

use shared::RunnerError;

/// Deterministic BLAKE3 digest of a directory tree's structure + file contents.
///
/// Walks depth-first, visiting each directory's entries in byte-sorted name
/// order, folding each file's relative path and content hash into one BLAKE3.
/// Returns `[0u8; 32]` for a missing or empty directory. Symlinks are recorded
/// by their link target and never followed into directories.
///
/// # Errors
///
/// Returns `RunnerError::Io` if a directory cannot be read or a file cannot be
/// opened / hashed.
pub fn fingerprint_dir(root: &Path) -> Result<[u8; 32], RunnerError> {
  if !root.is_dir() {
    return Ok([0u8; 32]);
  }
  let mut hasher = blake3::Hasher::new();
  let mut any = false;
  fold_dir(root, root, &mut hasher, &mut any)?;
  if any {
    Ok(*hasher.finalize().as_bytes())
  } else {
    Ok([0u8; 32])
  }
}

/// Fold one directory's entries into `hasher` in byte-sorted name order,
/// recursing into real subdirectories and folding everything else as a file.
fn fold_dir(
  root: &Path,
  dir: &Path,
  hasher: &mut blake3::Hasher,
  any: &mut bool,
) -> Result<(), RunnerError> {
  for path in sorted_entries(dir)? {
    let meta = std::fs::symlink_metadata(&path)?;
    if meta.is_dir() {
      fold_dir(root, &path, hasher, any)?;
    } else {
      fold_entry(root, &path, &meta, hasher)?;
      *any = true;
    }
  }
  Ok(())
}

/// Fold one non-directory entry: its workspace-relative path plus a content
/// digest (streamed file bytes, or the link target for a symlink).
fn fold_entry(
  root: &Path,
  path: &Path,
  meta: &std::fs::Metadata,
  hasher: &mut blake3::Hasher,
) -> Result<(), RunnerError> {
  // Entries come from walking `root`, so they are normally workspace-relative.
  // If strip_prefix ever fails (an entry outside the tree), skip it rather than
  // fold an absolute path — the fingerprint is workspace-scoped and must stay
  // portable across mount points.
  let Ok(rel) = path.strip_prefix(root) else {
    return Ok(());
  };
  hasher.update(rel.to_string_lossy().as_bytes());
  hasher.update(&[0u8]);
  if meta.is_symlink() {
    let target = std::fs::read_link(path)?;
    // Normalize an absolute in-workspace target to workspace-relative so two
    // identical trees at different mount points fingerprint the same. A relative
    // target is already portable; an absolute target outside the workspace is
    // inherently non-portable and kept verbatim.
    let target = target.strip_prefix(root).unwrap_or(target.as_path());
    hasher.update(b"L");
    hasher.update(target.to_string_lossy().as_bytes());
  } else {
    hasher.update(b"F");
    hasher.update(hash_file(path)?.as_bytes());
  }
  hasher.update(&[0u8]);
  Ok(())
}

/// BLAKE3 of a file's bytes, streamed so a large file is never fully buffered.
///
/// # Errors
///
/// Returns `RunnerError::Io` if the file cannot be opened or read.
fn hash_file(path: &Path) -> Result<blake3::Hash, RunnerError> {
  let mut hasher = blake3::Hasher::new();
  let mut file = std::fs::File::open(path)?;
  std::io::copy(&mut file, &mut hasher)?;
  Ok(hasher.finalize())
}

/// The child paths of `dir`, sorted by file name (deterministic order).
///
/// # Errors
///
/// Returns `RunnerError::Io` if the directory cannot be read.
fn sorted_entries(dir: &Path) -> Result<Vec<PathBuf>, RunnerError> {
  let mut paths: Vec<PathBuf> = Vec::new();
  for entry in std::fs::read_dir(dir)? {
    paths.push(entry?.path());
  }
  paths.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
  Ok(paths)
}

//! Filesystem-backed tests for Docker build-context filtering.
//!
//! Pattern cases follow Moby patternmatcher revision
//! `5a6d8429a19bb6948a372ff19e86fe83599a04b7` and Go `filepath.Match`
//! at tag `go1.25.1`; see the repository URLs in the Docker design spec.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;

use super::{DockerIgnore, Pattern, archive_context};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn write(root: &Path, relative: &str, value: &str) -> Result<(), std::io::Error> {
  let path = root.join(relative);
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent)?;
  }
  std::fs::write(path, value)
}

fn entries(archive: &[u8]) -> Result<BTreeSet<String>, std::io::Error> {
  let mut names = BTreeSet::new();
  for entry in tar::Archive::new(archive).entries()? {
    let mut entry = entry?;
    names.insert(entry.path()?.to_string_lossy().into_owned());
    let mut body = Vec::new();
    entry.read_to_end(&mut body)?;
  }
  Ok(names)
}

#[test]
fn recursive_patterns_and_negation_filter_the_archive() -> TestResult {
  let root = tempfile::tempdir()?;
  write(root.path(), "Dockerfile", "FROM scratch\n")?;
  write(
    root.path(),
    ".dockerignore",
    "\u{feff}# comment\n**/*.log\nsecret/**\n!secret/keep.txt\n",
  )?;
  write(root.path(), "visible.txt", "visible")?;
  write(root.path(), "nested/debug.log", "excluded")?;
  write(root.path(), "secret/drop.txt", "excluded")?;
  write(root.path(), "secret/keep.txt", "included")?;

  let archive = archive_context(root.path(), "Dockerfile")?;
  let names = entries(&archive)?;

  assert!(names.contains("Dockerfile"));
  assert!(names.contains(".dockerignore"));
  assert!(names.contains("visible.txt"));
  assert!(names.contains("secret/keep.txt"));
  assert!(!names.contains("nested/debug.log"));
  assert!(!names.contains("secret/drop.txt"));
  Ok(())
}

#[test]
fn dockerfile_specific_ignore_takes_precedence_and_controls_are_retained() -> TestResult {
  let root = tempfile::tempdir()?;
  write(root.path(), "lint.Dockerfile", "FROM scratch\n")?;
  write(root.path(), ".dockerignore", "root-only\n")?;
  write(
    root.path(),
    "lint.Dockerfile.dockerignore",
    "specific-only\nlint.Dockerfile\nlint.Dockerfile.dockerignore\n",
  )?;
  write(root.path(), "root-only", "included")?;
  write(root.path(), "specific-only", "excluded")?;

  let archive = archive_context(root.path(), "lint.Dockerfile")?;
  let names = entries(&archive)?;

  assert!(names.contains("lint.Dockerfile"));
  assert!(names.contains("lint.Dockerfile.dockerignore"));
  assert!(names.contains("root-only"));
  assert!(!names.contains("specific-only"));
  Ok(())
}

#[test]
fn escaped_literals_and_ordered_parent_negations_match_moby_rules() -> TestResult {
  let root = tempfile::tempdir()?;
  write(root.path(), "Dockerfile", "FROM scratch\n")?;
  write(
    root.path(),
    ".dockerignore",
    "literal\\*.txt\n[\\a].secret\n[\\q].secret\nparent\n!parent/keep.txt\n*.tmp\n!keep.tmp\nkeep.tmp\n",
  )?;
  write(root.path(), "literal*.txt", "excluded")?;
  write(root.path(), "literal-other.txt", "included")?;
  write(root.path(), "a.secret", "excluded")?;
  write(root.path(), "q.secret", "excluded")?;
  write(root.path(), "parent/drop.txt", "excluded")?;
  write(root.path(), "parent/keep.txt", "included")?;
  write(root.path(), "keep.tmp", "excluded by final rule")?;

  let archive = archive_context(root.path(), "Dockerfile")?;
  let names = entries(&archive)?;

  assert!(!names.contains("literal*.txt"));
  assert!(names.contains("literal-other.txt"));
  assert!(!names.contains("a.secret"));
  assert!(!names.contains("q.secret"));
  assert!(!names.contains("parent/drop.txt"));
  assert!(names.contains("parent/keep.txt"));
  assert!(!names.contains("keep.tmp"));
  Ok(())
}

#[test]
fn upstream_moby_pattern_corpus_matches_the_same_paths() -> TestResult {
  let cases = [
    ("*", "file", true),
    ("*", "dir/file", false),
    ("file*", "file-name", true),
    ("*file", "prefix-file", true),
    ("a*/b", "alpha/b", true),
    ("**", "anything/nested", true),
    ("**/**", "one/two", true),
    ("dir/**", "dir/nested/file", true),
    ("**/dir", "nested/dir", true),
    ("**/dir", "dir", true),
    ("**/dir2/*", "nested/dir2/file", true),
    ("**/dir2/**", "nested/dir2/one/two", true),
    ("**file", "prefix-file", true),
    ("**/file*txt", "nested/file-name.txt", true),
    ("**/**/*.txt", "one/two/file.txt", true),
    ("a[b-d]e", "ace", true),
    ("abc.def", "abc.def", true),
    ("abc?def", "abcXdef", true),
    ("literal^caret", "literal^caret", true),
    (r"escaped\letter", "escapedletter", true),
    (r"[\a]", "a", true),
    (r"[\q]", "q", true),
    ("a?b", "a/b", false),
  ];
  for (source, path, expected) in cases {
    let pattern = Pattern::compile(source, false)?;
    assert_eq!(
      pattern.matches(path),
      expected,
      "pattern {source}, path {path}"
    );
  }
  Ok(())
}

#[test]
fn malformed_filepath_patterns_are_rejected() {
  for source in ["unterminated[", "[]", "[-]", "[x-]", "trailing\\"] {
    assert!(Pattern::compile(source, false).is_err(), "pattern {source}");
  }
}

#[test]
fn excluded_directories_are_pruned_unless_a_negation_can_reinclude_a_descendant() -> TestResult {
  let unrelated = DockerIgnore {
    patterns: vec![
      Pattern::compile("parent", false)?,
      Pattern::compile("other/keep", true)?,
    ],
  };
  assert!(!unrelated.may_reinclude_descendant("parent"));

  let related = DockerIgnore {
    patterns: vec![
      Pattern::compile("parent", false)?,
      Pattern::compile("parent/keep", true)?,
    ],
  };
  assert!(related.may_reinclude_descendant("parent"));

  let recursive = DockerIgnore {
    patterns: vec![
      Pattern::compile("parent", false)?,
      Pattern::compile("**/keep", true)?,
    ],
  };
  assert!(recursive.may_reinclude_descendant("parent"));
  Ok(())
}

#[cfg(unix)]
#[test]
fn context_symlinks_are_archived_without_following_external_targets() -> TestResult {
  use std::os::unix::fs::symlink;

  let root = tempfile::tempdir()?;
  let outside = tempfile::tempdir()?;
  write(root.path(), "Dockerfile", "FROM scratch\n")?;
  write(outside.path(), "payload", "outside bytes")?;
  symlink(
    outside.path().join("payload"),
    root.path().join("external-link"),
  )?;

  let archive = archive_context(root.path(), "Dockerfile")?;
  let mut found = false;
  for entry in tar::Archive::new(archive.as_slice()).entries()? {
    let entry = entry?;
    if entry.path()?.as_ref() == Path::new("external-link") {
      assert!(entry.header().entry_type().is_symlink());
      assert_eq!(entry.size(), 0);
      found = true;
    }
  }
  assert!(found);
  Ok(())
}

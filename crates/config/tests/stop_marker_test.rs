//! `config::stop_marker` (#208): the `<data_dir>/.stopped` record a
//! `run --all` loop leaves when it ends with a fatal error, read back by
//! `status` and `doctor`. Real files in real temp dirs.

use config::stop_marker::{self, StopMarker};
use tempfile::TempDir;

fn marker() -> StopMarker {
  StopMarker {
    stopped_at: "2026-10-23T22:00:00+00:00".to_owned(),
    error: "re-mint rejected: 401 Bad credentials — re-mint needs a GitHub token".to_owned(),
  }
}

#[test]
fn write_then_read_round_trips() {
  let dir = TempDir::new().unwrap();
  stop_marker::write(dir.path(), &marker()).unwrap();
  assert_eq!(stop_marker::read(dir.path()).unwrap(), Some(marker()));
  assert_eq!(stop_marker::path(dir.path()), dir.path().join(".stopped"));
}

#[test]
fn write_creates_the_data_dir() {
  let dir = TempDir::new().unwrap();
  let data_dir = dir.path().join("runners").join("o").join("r");
  stop_marker::write(&data_dir, &marker()).unwrap();
  assert!(stop_marker::path(&data_dir).is_file());
}

#[cfg(unix)]
#[test]
fn the_marker_is_owner_only() {
  use std::os::unix::fs::PermissionsExt;
  let dir = TempDir::new().unwrap();
  stop_marker::write(dir.path(), &marker()).unwrap();
  let mode = std::fs::metadata(stop_marker::path(dir.path()))
    .unwrap()
    .permissions()
    .mode();
  assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn a_missing_marker_reads_as_none() {
  let dir = TempDir::new().unwrap();
  assert_eq!(stop_marker::read(dir.path()).unwrap(), None);
}

#[test]
fn a_corrupt_marker_is_an_error_naming_the_file() {
  let dir = TempDir::new().unwrap();
  let file = stop_marker::path(dir.path());
  std::fs::write(&file, "{not json").unwrap();
  let err = stop_marker::read(dir.path()).unwrap_err().to_string();
  let expected = format!(
    "config error: cannot parse {}: key must be a string at line 1 column 2",
    file.display()
  );
  assert_eq!(err, expected);
}

#[test]
fn clear_removes_the_marker_and_tolerates_absence() {
  let dir = TempDir::new().unwrap();
  stop_marker::write(dir.path(), &marker()).unwrap();
  stop_marker::clear(dir.path()).unwrap();
  assert!(!stop_marker::path(dir.path()).exists());
  stop_marker::clear(dir.path()).unwrap();
}

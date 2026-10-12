//! `capture::write_acquired` against a real sanitized acquire body.

use std::path::Path;

use crate::capture::write_acquired;

const ACQUIRED: &str = include_str!("../../../execution/tests/job_outputs_message.json");

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn captures(data_dir: &Path) -> std::io::Result<Vec<std::path::PathBuf>> {
  std::fs::read_dir(data_dir.join("_diag").join("captures"))?
    .map(|entry| entry.map(|e| e.path()))
    .collect()
}

#[test]
fn writes_one_private_file_equal_to_the_acquire_body() -> TestResult {
  let body: serde_json::Value = serde_json::from_str(ACQUIRED)?;
  let dir = tempfile::tempdir()?;
  let path = write_acquired(dir.path(), "plan-103", &body).ok_or("capture not written")?;

  assert_eq!(captures(dir.path())?, vec![path.clone()]);
  let name = path.file_name().and_then(|n| n.to_str()).ok_or("name")?;
  assert!(name.ends_with("-plan-103.json"), "{name}");
  let written: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
  assert_eq!(written, body, "capture must be the acquire body, unchanged");

  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
      std::fs::metadata(&path)?.permissions().mode() & 0o777,
      0o600
    );
    let parent = path.parent().ok_or("parent")?;
    assert_eq!(
      std::fs::metadata(parent)?.permissions().mode() & 0o777,
      0o700
    );
  }
  Ok(())
}

#[test]
fn plan_id_cannot_escape_the_capture_directory() -> TestResult {
  let body: serde_json::Value = serde_json::from_str(ACQUIRED)?;
  let dir = tempfile::tempdir()?;
  let path = write_acquired(dir.path(), "../../escape", &body).ok_or("capture not written")?;
  let name = path.file_name().and_then(|n| n.to_str()).ok_or("name")?;
  assert!(
    !name.contains('/'),
    "sanitized name must not contain '/': {name}"
  );
  assert_eq!(
    path.parent(),
    Some(dir.path().join("_diag").join("captures").as_path())
  );
  assert_eq!(captures(dir.path())?.len(), 1);
  Ok(())
}

#[test]
fn unwritable_data_dir_returns_none_without_panicking() -> TestResult {
  let body: serde_json::Value = serde_json::from_str(ACQUIRED)?;
  let dir = tempfile::tempdir()?;
  // A regular file where the data directory should be: every create fails.
  let blocker = dir.path().join("data");
  std::fs::write(&blocker, b"not a directory")?;
  assert!(write_acquired(&blocker, "plan-103", &body).is_none());
  Ok(())
}

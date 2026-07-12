//! Real-data workspace GC tests: age-based pruning of per-job workspace
//! directories under a real tempdir. No mocks — directories are backdated on
//! disk with `filetime::set_file_mtime` and the sweep runs against real
//! `std::fs` entries.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use filetime::FileTime;
use execution::execution::workspace_gc::gc_workspaces;

/// Boxed error alias for test helpers that use `?`.
type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

const HOUR: u64 = 3600;

/// Create `root/<name>/marker.txt` — a real directory holding a real file.
fn make_job_dir(root: &Path, name: &str) -> TestResult<()> {
  let dir = root.join(name);
  fs::create_dir_all(&dir)?;
  fs::write(dir.join("marker.txt"), name.as_bytes())?;
  Ok(())
}

/// Backdate `path`'s mtime to `secs` seconds before now.
fn age_by(path: &Path, secs: u64) -> TestResult<()> {
  let past = SystemTime::now()
    .checked_sub(Duration::from_secs(secs))
    .ok_or("age underflowed the clock")?;
  filetime::set_file_mtime(path, FileTime::from_system_time(past))?;
  Ok(())
}

#[test]
fn prunes_old_dirs_keeps_fresh_and_running() -> TestResult<()> {
  let tmp = tempfile::tempdir()?;
  let root = tmp.path();

  // Two stale dirs to reap, one stale dir that is the running job (must live),
  // and two fresh dirs.
  for name in [
    "job-old-a",
    "job-old-b",
    "job-keep",
    "job-fresh-a",
    "job-fresh-b",
  ] {
    make_job_dir(root, name)?;
  }
  for old in ["job-old-a", "job-old-b", "job-keep"] {
    age_by(&root.join(old), 48 * HOUR)?;
  }

  let removed = gc_workspaces(root, Duration::from_secs(24 * HOUR), "job-keep")?;

  assert_eq!(
    removed, 2,
    "exactly the two non-kept stale dirs are removed"
  );
  assert!(!root.join("job-old-a").exists(), "stale dir a is reaped");
  assert!(!root.join("job-old-b").exists(), "stale dir b is reaped");
  assert!(
    root.join("job-keep").exists(),
    "the running job survives despite being old"
  );
  assert!(root.join("job-fresh-a").exists(), "fresh dir a survives");
  assert!(root.join("job-fresh-b").exists(), "fresh dir b survives");
  Ok(())
}

#[test]
fn missing_root_is_ok_zero() -> TestResult<()> {
  let tmp = tempfile::tempdir()?;
  let missing = tmp.path().join("does-not-exist");

  let removed = gc_workspaces(&missing, Duration::from_secs(24 * HOUR), "job-keep")?;

  assert_eq!(removed, 0, "a missing workspace root prunes nothing");
  Ok(())
}

#[test]
fn plain_file_in_root_is_left_untouched() -> TestResult<()> {
  let tmp = tempfile::tempdir()?;
  let root = tmp.path();

  let stray = root.join("stray.txt");
  fs::write(&stray, b"not a job dir")?;
  age_by(&stray, 48 * HOUR)?;
  make_job_dir(root, "job-old")?;
  age_by(&root.join("job-old"), 48 * HOUR)?;

  let removed = gc_workspaces(root, Duration::from_secs(24 * HOUR), "job-keep")?;

  assert_eq!(removed, 1, "only the stale directory is removed");
  assert!(
    stray.exists(),
    "an aged plain file in the root is never removed"
  );
  assert!(
    !root.join("job-old").exists(),
    "the stale directory is removed"
  );
  Ok(())
}

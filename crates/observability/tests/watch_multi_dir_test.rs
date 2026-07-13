//! Integration tests for `watch` multi-dir journal discovery + merge
//! (zero-arg-register AC-10): with no usable config, `watch` browses
//! every `runners/<owner>/<repo>/_diag/jobs` plus the legacy home.
//!
//! Real data only: the canonical journal fixture (captured from a real
//! engine run) copied into real `tempfile` runner homes, with real
//! `config.toml` marker files (the TOML shape `save_config` writes) so
//! `config::registry::list_registrations` discovers the dirs.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use observability::watch::{discover_jobs_dirs, scan_all_jobs};
use tempfile::TempDir;

/// The canonical journal captured from a real engine run.
const CANONICAL: &str = concat!(
  env!("CARGO_MANIFEST_DIR"),
  "/../toolu-runner/tests/fixtures/journal/canonical.jsonl"
);

/// Job id inside `canonical.jsonl` (`job_acquired` line).
const CANONICAL_JOB_ID: &str = "0e9d8c7b-3333-4444-8555-666677778888";

/// The TOML body `save_config` persists — what a real registration holds.
const CONFIG_TOML: &str = concat!(
  "jit_config = \"eyIucnVubmVyIjoiZTMwPSJ9\"\n",
  "work_dir = \"/Users/runner/.toolu-runner/_work\"\n",
  "data_dir = \"/Users/runner/.toolu-runner\"\n",
  "protocol_version = \"v2\"\n",
);

/// Create `<home>/runners/<owner>/<repo>/` with a real `config.toml`
/// marker and the canonical journal at `_diag/jobs/<journal_name>`;
/// return the jobs dir.
fn add_runner_with_journal(
  home: &Path,
  owner: &str,
  repo: &str,
  journal_name: &str,
) -> Result<PathBuf, std::io::Error> {
  let reg_dir = home.join("runners").join(owner).join(repo);
  let jobs_dir = reg_dir.join("_diag").join("jobs");
  std::fs::create_dir_all(&jobs_dir)?;
  std::fs::write(reg_dir.join("config.toml"), CONFIG_TOML)?;
  std::fs::copy(CANONICAL, jobs_dir.join(journal_name))?;
  Ok(jobs_dir)
}

/// Create the legacy `<home>/_diag/jobs/<journal_name>` journal (no
/// legacy `config.toml` unless the test adds one); return the jobs dir.
fn add_legacy_journal(home: &Path, journal_name: &str) -> Result<PathBuf, std::io::Error> {
  let jobs_dir = home.join("_diag").join("jobs");
  std::fs::create_dir_all(&jobs_dir)?;
  std::fs::copy(CANONICAL, jobs_dir.join(journal_name))?;
  Ok(jobs_dir)
}

// ── discover_jobs_dirs: per-repo entries + legacy, deduplicated ──────

#[test]
fn discover_finds_registered_dirs_plus_legacy() {
  let home = TempDir::new().unwrap();
  let o1 = add_runner_with_journal(home.path(), "o1", "r1", "a.jsonl").unwrap();
  let o2 = add_runner_with_journal(home.path(), "o2", "r2", "b.jsonl").unwrap();
  let legacy = add_legacy_journal(home.path(), "c.jsonl").unwrap();

  let dirs = discover_jobs_dirs(home.path());
  assert_eq!(
    dirs.len(),
    3,
    "two registrations + legacy home expected; got: {dirs:?}"
  );
  for expected in [&o1, &o2, &legacy] {
    assert!(dirs.contains(expected), "missing {expected:?} in {dirs:?}");
  }
}

#[test]
fn discover_dedupes_legacy_registration_against_legacy_home() {
  let home = TempDir::new().unwrap();
  // A legacy config.toml makes list_registrations emit a legacy entry
  // whose derived jobs dir equals the always-added `<home>/_diag/jobs`.
  std::fs::write(home.path().join("config.toml"), CONFIG_TOML).unwrap();
  let legacy = add_legacy_journal(home.path(), "a.jsonl").unwrap();

  let dirs = discover_jobs_dirs(home.path());
  assert_eq!(dirs, vec![legacy], "legacy dir must appear exactly once");
}

#[test]
fn discover_empty_home_yields_only_legacy_dir() {
  let home = TempDir::new().unwrap();
  let dirs = discover_jobs_dirs(home.path());
  assert_eq!(dirs, vec![home.path().join("_diag").join("jobs")]);
}

/// `list_registrations` errors on an unreadable `runners/` dir; `watch`
/// discovery downgrades that to skip-and-continue (same tolerance
/// `scan_all_jobs` applies per dir): no per-repo dirs, but the legacy
/// home still browses. Unix-only: dir modes are not enforceable this
/// way elsewhere.
#[cfg(unix)]
#[test]
fn discover_tolerates_unreadable_runners_dir_keeping_legacy() {
  use std::os::unix::fs::PermissionsExt;
  let home = TempDir::new().unwrap();
  add_runner_with_journal(home.path(), "o1", "r1", "a.jsonl").unwrap();
  let runners = home.path().join("runners");
  std::fs::set_permissions(&runners, std::fs::Permissions::from_mode(0o000)).unwrap();
  // A privileged user (root CI containers) ignores dir modes — skip there.
  if std::fs::read_dir(&runners).is_ok() {
    std::fs::set_permissions(&runners, std::fs::Permissions::from_mode(0o755)).unwrap();
    eprintln!("skipping: this user can read a 000 dir (running privileged)");
    return;
  }

  let dirs = discover_jobs_dirs(home.path());

  // Restore before asserting so the tempdir cleans up even on failure.
  std::fs::set_permissions(&runners, std::fs::Permissions::from_mode(0o755)).unwrap();
  assert_eq!(
    dirs,
    vec![home.path().join("_diag").join("jobs")],
    "an unreadable registry scan must degrade to legacy-only browsing"
  );
}

// ── scan_all_jobs: merge across dirs, ordering, identity, tolerance ──

#[test]
fn merge_surfaces_jobs_from_all_sources_newest_first() {
  let home = TempDir::new().unwrap();
  // Distinct `<UTC ts>-<job>` names; o2's is newest, o1's is oldest.
  let o1 =
    add_runner_with_journal(home.path(), "o1", "r1", "2026-07-12T01-00-00Z-job.jsonl").unwrap();
  let o2 =
    add_runner_with_journal(home.path(), "o2", "r2", "2026-07-12T03-00-00Z-job.jsonl").unwrap();
  let legacy = add_legacy_journal(home.path(), "2026-07-12T02-00-00Z-job.jsonl").unwrap();

  let jobs = scan_all_jobs(&discover_jobs_dirs(home.path()));
  assert_eq!(jobs.len(), 3, "one job per source expected; got: {jobs:?}");

  // Newest first by journal file name (timestamp prefix).
  let paths: Vec<&Path> = jobs.iter().map(|j| j.path.as_path()).collect();
  assert_eq!(
    paths,
    vec![
      o2.join("2026-07-12T03-00-00Z-job.jsonl").as_path(),
      legacy.join("2026-07-12T02-00-00Z-job.jsonl").as_path(),
      o1.join("2026-07-12T01-00-00Z-job.jsonl").as_path(),
    ],
    "merged list must be newest-first across dirs"
  );

  // Every summary is parsed from the real canonical journal content.
  for job in &jobs {
    assert_eq!(job.job_id, CANONICAL_JOB_ID);
    assert_eq!(job.job_name.as_deref(), Some("build"));
    assert_eq!(job.conclusion.as_deref(), Some("success"));
  }
}

#[test]
fn merge_keys_by_full_path_so_same_names_never_collide() {
  let home = TempDir::new().unwrap();
  // The SAME journal file name in two different runner dirs.
  let name = "2026-07-12T01-00-00Z-job.jsonl";
  add_runner_with_journal(home.path(), "o1", "r1", name).unwrap();
  add_runner_with_journal(home.path(), "o2", "r2", name).unwrap();

  let jobs = scan_all_jobs(&discover_jobs_dirs(home.path()));
  assert_eq!(jobs.len(), 2, "both same-named journals must survive");
  let unique: HashSet<&Path> = jobs.iter().map(|j| j.path.as_path()).collect();
  assert_eq!(unique.len(), 2, "full paths must stay distinct keys");
}

#[test]
fn merge_skips_missing_dirs_without_failing() {
  let home = TempDir::new().unwrap();
  let o1 =
    add_runner_with_journal(home.path(), "o1", "r1", "2026-07-12T01-00-00Z-job.jsonl").unwrap();

  let dirs = vec![home.path().join("never-created"), o1.clone()];
  let jobs = scan_all_jobs(&dirs);
  assert_eq!(jobs.len(), 1, "the readable dir's job must still surface");
  let only = jobs.first().unwrap();
  assert_eq!(only.path, o1.join("2026-07-12T01-00-00Z-job.jsonl"));
}

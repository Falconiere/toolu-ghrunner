//! AC-16 — shadow-mode step observation (approach C, observation only).
//!
//! Real data, no mocks: drives the real [`ShadowObserver`] against real temp
//! workspaces and reads the real JSONL back. Proves it RECORDS would-hit /
//! false-hit and NEVER serves a cached result (there is no read-back-to-serve
//! path — the records are inspected only by the test).
//!
//! Asserts:
//!   1. would_hit: a repeated identical observation of an unchanged workspace
//!      records `would_hit=true, false_hit=false` on the second record.
//!   2. false_hit: an identical pre with a divergent post records
//!      `would_hit=true, false_hit=true`.
//!   3. disabled: a disabled observer writes no file at all.
//!   4. masking: a registered secret in the command never reaches the JSONL.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use toolu_runner::execution::secret_masker::SecretMasker;
use toolu_runner::execution::shadow::ShadowObserver;
use toolu_runner::execution::shadow::record::{ShadowRecord, StepKey};

/// Build the `StepKey` for one observation.
fn key<'a>(
  step_id: &'a str,
  cmd: &'a str,
  env_kv: &'a [(String, String)],
  cwd: &'a str,
) -> StepKey<'a> {
  StepKey {
    step_id,
    cmd,
    env_kv,
    cwd,
  }
}

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// A fresh shared masker.
fn masker() -> Arc<Mutex<SecretMasker>> {
  Arc::new(Mutex::new(SecretMasker::new()))
}

/// The env passed to every observation (a stable single pair).
fn env() -> Vec<(String, String)> {
  vec![("CI".to_owned(), "true".to_owned())]
}

/// The path the observer writes for `job_id` under `data_dir`.
fn shadow_path(data_dir: &Path, job_id: &str) -> PathBuf {
  data_dir
    .join("_diag")
    .join("shadow")
    .join(format!("{job_id}.jsonl"))
}

/// Parse every JSON line of a shadow jsonl into `ShadowRecord`s.
fn read_records(path: &Path) -> TestResult<Vec<ShadowRecord>> {
  let content = std::fs::read_to_string(path)?;
  let mut out = Vec::new();
  for line in content.lines() {
    if !line.trim().is_empty() {
      out.push(serde_json::from_str::<ShadowRecord>(line)?);
    }
  }
  Ok(out)
}

/// Wipe `ws` and recreate it holding a single `seed.txt` with `seed`.
fn reset_ws(ws: &Path, seed: &[u8]) -> TestResult {
  if ws.exists() {
    std::fs::remove_dir_all(ws)?;
  }
  std::fs::create_dir_all(ws)?;
  std::fs::write(ws.join("seed.txt"), seed)?;
  Ok(())
}

#[test]
fn would_hit_on_identical_unchanged_observation() -> TestResult {
  let tmp = tempfile::tempdir()?;
  let data_dir = tmp.path().join("data");
  let ws = tmp.path().join("ws");
  std::fs::create_dir_all(&ws)?;
  std::fs::write(ws.join("a.txt"), b"hello")?;

  let obs = ShadowObserver::new(true, &data_dir, "job-1", masker());
  let cwd = ws.to_string_lossy().into_owned();

  // First observe: workspace unchanged across the step.
  let pre1 = obs.pre(&ws)?;
  obs.post(&key("s1", "make", &env(), &cwd), pre1, &ws)?;
  // Second observe: the SAME, unchanged workspace.
  let pre2 = obs.pre(&ws)?;
  obs.post(&key("s1", "make", &env(), &cwd), pre2, &ws)?;

  let records = read_records(&shadow_path(&data_dir, "job-1"))?;
  assert_eq!(records.len(), 2);
  let first = records.first().ok_or("missing first record")?;
  let second = records.get(1).ok_or("missing second record")?;
  assert!(!first.would_hit, "first sighting is not a hit");
  assert!(second.would_hit, "identical repeat should be a would-hit");
  assert!(!second.false_hit, "unchanged post is not a false-hit");
  Ok(())
}

#[test]
fn false_hit_when_post_diverges_under_identical_pre() -> TestResult {
  let tmp = tempfile::tempdir()?;
  let data_dir = tmp.path().join("data");
  let ws = tmp.path().join("ws");
  let cwd = "/work";
  let obs = ShadowObserver::new(true, &data_dir, "job-2", masker());

  // Run 1: pre = {seed}, step writes out.txt = "1".
  reset_ws(&ws, b"seed")?;
  let pre1 = obs.pre(&ws)?;
  std::fs::write(ws.join("out.txt"), b"1")?;
  obs.post(&key("s1", "make", &env(), cwd), pre1, &ws)?;

  // Run 2: reset to the SAME pre content (identical pre), step writes a
  // DIFFERENT out.txt = "2" (divergent post).
  reset_ws(&ws, b"seed")?;
  let pre2 = obs.pre(&ws)?;
  assert_eq!(pre1, pre2, "pre-fingerprint must be identical across runs");
  std::fs::write(ws.join("out.txt"), b"2")?;
  obs.post(&key("s1", "make", &env(), cwd), pre2, &ws)?;

  let records = read_records(&shadow_path(&data_dir, "job-2"))?;
  assert_eq!(records.len(), 2);
  let second = records.get(1).ok_or("missing second record")?;
  assert!(
    second.would_hit,
    "identical (cmd,env,cwd,pre) should be a would-hit"
  );
  assert!(
    second.false_hit,
    "divergent post under identical pre is a false-hit"
  );
  Ok(())
}

#[test]
fn disabled_observer_writes_no_file() -> TestResult {
  let tmp = tempfile::tempdir()?;
  let data_dir = tmp.path().join("data");
  let ws = tmp.path().join("ws");
  std::fs::create_dir_all(&ws)?;
  std::fs::write(ws.join("a.txt"), b"hi")?;

  let obs = ShadowObserver::new(false, &data_dir, "job-3", masker());
  let pre = obs.pre(&ws)?;
  assert_eq!(pre, [0u8; 32], "disabled pre is a cheap [0;32] no-op");
  obs.post(&key("s1", "make", &env(), "/work"), pre, &ws)?;

  assert!(
    !shadow_path(&data_dir, "job-3").exists(),
    "disabled observer must not create a shadow jsonl"
  );
  Ok(())
}

#[test]
fn secret_in_command_is_masked_in_jsonl() -> TestResult {
  let tmp = tempfile::tempdir()?;
  let data_dir = tmp.path().join("data");
  let ws = tmp.path().join("ws");
  std::fs::create_dir_all(&ws)?;
  std::fs::write(ws.join("a.txt"), b"hi")?;

  let secret = "supersecrettoken123";
  let shared = masker();
  {
    // Fail-closed like production (shadow/mod.rs): a poisoned masker lock is a
    // hard error, never silently recovered. The fresh masker never poisons.
    let mut guard = shared.lock().expect("fresh masker lock is never poisoned");
    guard.add_secret(secret);
  }

  let obs = ShadowObserver::new(true, &data_dir, "job-4", Arc::clone(&shared));
  let pre = obs.pre(&ws)?;
  let cmd = format!("deploy --token={secret}");
  obs.post(&key("s1", &cmd, &env(), "/work"), pre, &ws)?;

  let bytes = std::fs::read(shadow_path(&data_dir, "job-4"))?;
  let text = String::from_utf8_lossy(&bytes);
  assert!(
    !text.contains(secret),
    "raw secret must never appear in the shadow jsonl"
  );
  Ok(())
}

/// Two workspaces with identical content — including a symlink whose target is
/// an absolute path inside the workspace — but at different filesystem
/// locations must fingerprint equally. Otherwise shadow mode records a spurious
/// divergence for a mount-point difference. Regression for the absolute
/// symlink-target leak.
#[cfg(unix)]
#[test]
fn fingerprint_is_portable_across_mount_points_with_symlinks() -> TestResult {
  let tmp = tempfile::tempdir()?;
  let data_dir = tmp.path().join("data");

  let make_ws = |name: &str| -> TestResult<PathBuf> {
    let ws = tmp.path().join(name);
    std::fs::create_dir_all(&ws)?;
    std::fs::write(ws.join("real.txt"), b"payload")?;
    // Absolute target pointing inside this workspace — the non-portable case.
    std::os::unix::fs::symlink(ws.join("real.txt"), ws.join("link.txt"))?;
    Ok(ws)
  };

  let ws_a = make_ws("mount_a")?;
  let ws_b = make_ws("mount_b")?;

  let obs = ShadowObserver::new(true, &data_dir, "job-sym", masker());
  let pre_a = obs.pre(&ws_a)?;
  let pre_b = obs.pre(&ws_b)?;
  assert_eq!(
    pre_a, pre_b,
    "identical trees at different mount points must fingerprint equally"
  );
  Ok(())
}

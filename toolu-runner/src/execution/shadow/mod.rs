//! Shadow-mode step observation (approach C, observation only).
//!
//! Per `run:` step, when enabled, the observer fingerprints the workspace
//! before and after the step and appends a would-hit / false-hit record to
//! `data_dir/_diag/shadow/<job_id>.jsonl`, masked through the job's
//! `SecretMasker`. It RECORDS only: no code path here returns or reuses a
//! memoized step result — shadow mode never serves a cached result. Off by
//! default (`RunnerConfig.shadow_enabled`).

/// Deterministic BLAKE3 fingerprint of a workspace directory tree.
pub mod fingerprint;
/// The shadow observation record and its digest helpers.
pub mod record;

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use shared::RunnerError;

use self::fingerprint::fingerprint_dir;
use self::record::{ShadowRecord, StepKey, digest_hex, env_digest, fingerprint_hex};
use crate::execution::secret_masker::SecretMasker;
use crate::journal::types::sanitize_job_id;

/// Observes `run:` steps and records would-hit / false-hit signals. It never
/// returns or reuses a step result — observation only, it does not serve.
pub struct ShadowObserver {
  enabled: bool,
  /// `data_dir/_diag/shadow` — created on first append.
  dir: PathBuf,
  /// `dir/<job_id>.jsonl`.
  path: PathBuf,
  masker: Arc<Mutex<SecretMasker>>,
  /// `(cmd, env, cwd, pre)` key -> the FIRST recorded post fingerprint, so
  /// `false_hit` measures divergence from the first observation.
  seen: Mutex<HashMap<String, [u8; 32]>>,
}

impl ShadowObserver {
  /// Build an observer writing to `data_dir/_diag/shadow/<job_id>.jsonl`.
  ///
  /// When `enabled` is false the observer is inert: `pre`/`post` are cheap
  /// no-ops and no file is ever created.
  pub fn new(
    enabled: bool,
    data_dir: &Path,
    job_id: &str,
    masker: Arc<Mutex<SecretMasker>>,
  ) -> Self {
    let dir = data_dir.join("_diag").join("shadow");
    let path = dir.join(format!("{}.jsonl", sanitize_job_id(job_id)));
    Self {
      enabled,
      dir,
      path,
      masker,
      seen: Mutex::new(HashMap::new()),
    }
  }

  /// Fingerprint `workspace` BEFORE the step runs; the caller holds the digest
  /// across the step. Returns `[0; 32]` when disabled.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Io` if the workspace tree cannot be walked.
  pub fn pre(&self, workspace: &Path) -> Result<[u8; 32], RunnerError> {
    if !self.enabled {
      return Ok([0u8; 32]);
    }
    fingerprint_dir(workspace)
  }

  /// After the step: fingerprint `workspace` again, compute `would_hit` (this
  /// `(cmd, env, cwd, pre)` key was seen before) and `false_hit` (seen AND the
  /// recorded post differs), append the masked JSONL record, and remember the
  /// key -> post. NEVER returns or reuses a cached step result.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Io` if the workspace tree cannot be walked. A record
  /// append failure is logged (WARN) and swallowed so it never fails the job.
  pub fn post(
    &self,
    key: &StepKey<'_>,
    pre: [u8; 32],
    workspace: &Path,
  ) -> Result<(), RunnerError> {
    if !self.enabled {
      return Ok(());
    }
    let post = fingerprint_dir(workspace)?;
    let cmd_digest = digest_hex(key.cmd.as_bytes());
    let env_dg = env_digest(key.env_kv);
    let map_key = observe_key(&cmd_digest, &env_dg, key.cwd, pre);
    let (would_hit, false_hit) = self.classify(&map_key, post);
    self.append(&ShadowRecord {
      step_id: key.step_id.to_owned(),
      cmd_digest,
      env_digest: env_dg,
      cwd: key.cwd.to_owned(),
      pre_digest: fingerprint_hex(pre),
      post_digest: fingerprint_hex(post),
      would_hit,
      false_hit,
    });
    Ok(())
  }

  /// Classify `key`: `would_hit` when already seen; `false_hit` when the first
  /// recorded post differs from `post`. Records `key -> post` on first sight,
  /// never overwriting it.
  ///
  /// Returns the neutral `(false, false)` (after a WARN) when the `seen` lock
  /// is poisoned — a panic mid-insert may have left the map incomplete, so the
  /// diagnostic-only signal is classified as a plain miss (fail closed) rather
  /// than computed from a possibly inconsistent map.
  fn classify(&self, key: &str, post: [u8; 32]) -> (bool, bool) {
    let Ok(mut seen) = self.seen.lock() else {
      tracing::warn!("shadow: seen lock poisoned; step classified as miss (fail closed)");
      return (false, false);
    };
    // `.copied()` ends the borrow of `seen` before the `else` branch inserts.
    if let Some(prior) = seen.get(key).copied() {
      (true, prior != post)
    } else {
      seen.insert(key.to_owned(), post);
      (false, false)
    }
  }

  /// Serialize, mask, and append one record as a JSON line. Any failure is
  /// logged (WARN) and swallowed — observation must never fail the job.
  fn append(&self, record: &ShadowRecord) {
    let Some(masked) = self.masked_json(record) else {
      return;
    };
    if let Err(e) = self.write_line(&masked) {
      tracing::warn!(error = %e, "shadow: record append failed; observation disabled for this step");
    }
  }

  /// Serialize and mask one record. Returns `None` (after a WARN) when
  /// serialization fails, or when the masker lock is poisoned — a panic
  /// mid-`add_secret` may have left the pattern set incomplete, so the
  /// diagnostic-only record is dropped (fail closed) rather than written
  /// possibly unmasked.
  fn masked_json(&self, record: &ShadowRecord) -> Option<String> {
    let json = match serde_json::to_string(record) {
      Ok(j) => j,
      Err(e) => {
        tracing::warn!(error = %e, "shadow: record serialization failed; line skipped");
        return None;
      },
    };
    let Ok(guard) = self.masker.lock() else {
      tracing::warn!("shadow: masker lock poisoned; record dropped (fail closed)");
      return None;
    };
    Some(guard.mask(&json))
  }

  /// Append `line + "\n"` to the shadow jsonl, creating `self.dir` first.
  ///
  /// # Errors
  ///
  /// Returns `RunnerError::Io` on a create-dir or write failure.
  fn write_line(&self, line: &str) -> Result<(), RunnerError> {
    std::fs::create_dir_all(&self.dir)?;
    let mut file = std::fs::OpenOptions::new()
      .create(true)
      .append(true)
      .open(&self.path)?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(())
  }
}

/// The dedup key for one observation: `cmd \0 env \0 len(cwd) : cwd \0 pre-hex`.
///
/// `cwd` is the only field with an unconstrained byte range, so it is
/// length-prefixed: two distinct working directories cannot produce the same
/// key even if one embeds the `\0` separator.
fn observe_key(cmd_digest: &str, env_digest: &str, cwd: &str, pre: [u8; 32]) -> String {
  format!(
    "{cmd_digest}\0{env_digest}\0{}:{cwd}\0{}",
    cwd.len(),
    fingerprint_hex(pre)
  )
}

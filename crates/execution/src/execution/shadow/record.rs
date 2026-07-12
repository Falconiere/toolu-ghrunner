//! The shadow observation record: one masked JSON line per observed `run:`
//! step. Records only — a record is never read back to serve a step result.

use serde::{Deserialize, Serialize};

/// One shadow-mode observation of a `run:` step, serialized as a single JSON
/// line to `_diag/shadow/<job_id>.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShadowRecord {
  /// The step's id.
  pub step_id: String,
  /// Hex BLAKE3 of the interpolated command string.
  pub cmd_digest: String,
  /// Hex BLAKE3 of the env rendered as sorted `KEY=VALUE\n` lines.
  pub env_digest: String,
  /// The step's working directory.
  pub cwd: String,
  /// Hex of the pre-step workspace fingerprint.
  pub pre_digest: String,
  /// Hex of the post-step workspace fingerprint.
  pub post_digest: String,
  /// This `(cmd, env, cwd, pre)` key was seen before with a recorded post.
  pub would_hit: bool,
  /// `would_hit` and the first recorded post differs from this post.
  pub false_hit: bool,
}

/// The identity of one observed `run:` step: its id, command, env, and cwd.
///
/// Bundled so `ShadowObserver::post` stays within the argument budget (clippy
/// counts `self`, capping a method at five explicit parameters).
pub struct StepKey<'a> {
  /// The step's id.
  pub step_id: &'a str,
  /// The interpolated command string.
  pub cmd: &'a str,
  /// The step env as `(KEY, VALUE)` pairs.
  pub env_kv: &'a [(String, String)],
  /// The step's working directory.
  pub cwd: &'a str,
}

/// Hex BLAKE3 digest of `bytes`.
pub fn digest_hex(bytes: &[u8]) -> String {
  blake3::hash(bytes).to_hex().to_string()
}

/// Hex rendering of a 32-byte workspace fingerprint.
pub fn fingerprint_hex(digest: [u8; 32]) -> String {
  blake3::Hash::from_bytes(digest).to_hex().to_string()
}

/// Hex BLAKE3 of the env rendered as sorted `KEY=VALUE\n` lines.
pub fn env_digest(env: &[(String, String)]) -> String {
  let mut lines: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
  lines.sort();
  let joined: String = lines.concat();
  digest_hex(joined.as_bytes())
}

//! Re-mint merge for the always-online run loop.
//!
//! When `run` re-registers after a job it mints a fresh JIT config, but must
//! not clobber the operator's `[services]` / `[cache]` / `[workspace]` /
//! `[shadow]` edits. [`merge_reminted_config`] folds only the three
//! mint-derived fields into a clone of the prior config, leaving every
//! user-editable section byte-identical.

use crate::config::RunnerRegistrationConfig;

/// Merge a freshly minted JIT registration into a prior registration config,
/// preserving every user-editable section verbatim.
///
/// Clones `prior` and overwrites ONLY the three mint-derived fields:
/// `runner_id`, `auth_token` (the OAuth `client_id`), and
/// `runtime.jit_config`. `runner_url`, `runner_name`, `labels`,
/// `runner_group`, the rest of `[runtime]`, and the `[services]` /
/// `[cache]` / `[workspace]` / `[shadow]` sections stay exactly as loaded.
///
/// Caller contract: this is a dumb merge — `jit_config`, `runner_id`, and
/// `client_id` must all come from the SAME minted registration; nothing
/// here cross-validates them.
pub fn merge_reminted_config(
  prior: &RunnerRegistrationConfig,
  jit_config: String,
  runner_id: i64,
  client_id: String,
) -> RunnerRegistrationConfig {
  let mut merged = prior.clone();
  merged.runner_id = runner_id;
  merged.auth_token = client_id;
  merged.runtime.jit_config = jit_config;
  merged
}

//! Runner label resolution for `register` (upstream `config.sh` parity).
//!
//! Mirrors actions/runner: the defaults `self-hosted`, the OS and the
//! architecture (GitHub's `RUNNER_OS` / `RUNNER_ARCH` spelling) are always
//! added unless `--no-default-labels`; `--labels` values are *additional*.
//! Empty entries are dropped, a label over [`MAX_LABEL_LEN`] characters is
//! rejected, and the list is de-duplicated case-insensitively (first spelling
//! wins) — GitHub stores labels the same way, so the persisted list matches
//! the server's.

use shared::RunnerError;

/// Longest label upstream accepts (`Validators.LabelsValidator`).
pub const MAX_LABEL_LEN: usize = 256;

/// Upstream's error for `--no-default-labels` without any label.
pub const NO_DEFAULT_LABELS_WITHOUT_LABELS: &str =
  "Disabling default labels via --no-default-labels without specifying --labels is not supported";

/// The default labels: `self-hosted`, `Linux`/`macOS`, `X64`/`ARM64`.
pub fn default_labels() -> Vec<String> {
  vec![
    "self-hosted".to_owned(),
    shared::platform::runner_os().to_owned(),
    shared::platform::runner_arch().to_owned(),
  ]
}

/// Resolve the labels a registration advertises: `defaults` (unless
/// `no_default_labels`) followed by the non-empty `user` labels, without
/// case-insensitive duplicates.
///
/// # Errors
///
/// `RunnerError::Config` when a label exceeds [`MAX_LABEL_LEN`] characters,
/// or when `no_default_labels` leaves no label at all
/// ([`NO_DEFAULT_LABELS_WITHOUT_LABELS`]).
pub fn resolve_labels(
  user: &[String],
  no_default_labels: bool,
  defaults: &[String],
) -> Result<Vec<String>, RunnerError> {
  let user: Vec<&String> = user.iter().filter(|l| !l.is_empty()).collect();
  if let Some(long) = user.iter().find(|l| l.chars().count() > MAX_LABEL_LEN) {
    let head: String = long.chars().take(32).collect();
    return Err(RunnerError::Config(format!(
      "label '{head}…' is longer than {MAX_LABEL_LEN} characters"
    )));
  }
  if no_default_labels && user.is_empty() {
    return Err(RunnerError::Config(
      NO_DEFAULT_LABELS_WITHOUT_LABELS.to_owned(),
    ));
  }
  let base: &[String] = if no_default_labels { &[] } else { defaults };
  let mut resolved: Vec<String> = Vec::with_capacity(base.len() + user.len());
  for label in base.iter().chain(user) {
    if !resolved
      .iter()
      .any(|seen| seen.to_lowercase() == label.to_lowercase())
    {
      resolved.push(label.clone());
    }
  }
  Ok(resolved)
}

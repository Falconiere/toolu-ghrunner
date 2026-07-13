//! Per-repo runner registration registry.
//!
//! Each registration lives in its own `<home>/runners/<owner>/<repo>/`
//! dir (config, credentials, lock, `_diag/`); the legacy single-slot
//! `<home>/config.toml` is honored read-only as a fallback. This module
//! owns the home/dir layout, registration discovery, and the config-path
//! resolution shared by `run` / `status` / `remove`: `--config` flag >
//! cwd-inferred repo > sole registration > error listing candidates.

use std::path::{Path, PathBuf};

use shared::RunnerError;

/// The runner home: `$TOOLU_RUNNER_HOME` when set (non-empty), else `~/.toolu-runner`.
pub fn runner_home() -> PathBuf {
  if let Some(home) = std::env::var_os("TOOLU_RUNNER_HOME")
    && !home.is_empty()
  {
    return PathBuf::from(home);
  }
  shared::paths::expand_tilde(Path::new("~/.toolu-runner"))
}

/// Per-repo registration dir: `<home>/runners/<owner>/<repo>`.
///
/// `owner` / `repo` are used verbatim as path components, after rejecting
/// anything that is not a plain single component (defense in depth —
/// GitHub names are already `[A-Za-z0-9._-]`).
///
/// # Errors
///
/// Returns `RunnerError::Config` when `owner` or `repo` is empty, `.`,
/// `..`, or contains a `/` or `\` path separator.
pub fn runner_dir(home: &Path, owner: &str, repo: &str) -> Result<PathBuf, RunnerError> {
  validate_component("owner", owner)?;
  validate_component("repo", repo)?;
  Ok(home.join("runners").join(owner).join(repo))
}

/// One discovered registration: its config path plus which repo it serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrationEntry {
  /// Path to the registration's `config.toml`.
  pub config_path: PathBuf,
  /// `"owner/repo"` for per-repo registrations; `None` for the legacy
  /// single-slot `<home>/config.toml`.
  pub owner_repo: Option<String>,
}

/// Scan `home` for registrations: every `runners/<owner>/<repo>/config.toml`
/// plus the legacy `<home>/config.toml`. A missing home or `runners/` dir
/// yields an empty list. Deterministic order: per-repo entries sorted by
/// `owner/repo`, the legacy entry last.
pub fn list_registrations(home: &Path) -> Vec<RegistrationEntry> {
  let mut entries = Vec::new();
  if let Ok(owners) = std::fs::read_dir(home.join("runners")) {
    for owner in owners.flatten() {
      // A non-dir (or unreadable) entry under runners/ is not a registration.
      let Ok(repos) = std::fs::read_dir(owner.path()) else {
        continue;
      };
      let owner_name = owner.file_name().to_string_lossy().into_owned();
      for repo in repos.flatten() {
        let config_path = repo.path().join("config.toml");
        if config_path.is_file() {
          let repo_name = repo.file_name().to_string_lossy().into_owned();
          entries.push(RegistrationEntry {
            config_path,
            owner_repo: Some(format!("{owner_name}/{repo_name}")),
          });
        }
      }
    }
  }
  entries.sort_by(|a, b| a.owner_repo.cmp(&b.owner_repo));
  let legacy = home.join("config.toml");
  if legacy.is_file() {
    entries.push(RegistrationEntry {
      config_path: legacy,
      owner_repo: None,
    });
  }
  entries
}

/// Resolve which registration config a command should use: `flag` as-is >
/// `inferred` `(owner, repo)` whose per-repo `config.toml` exists > the
/// sole registration (legacy included).
///
/// # Errors
///
/// `RunnerError::Config` when no registration exists (naming `toolu-runner
/// register`) or several exist and none matched (listing each candidate).
pub fn resolve_config_path(
  flag: Option<PathBuf>,
  home: &Path,
  inferred: Option<(&str, &str)>,
) -> Result<PathBuf, RunnerError> {
  if let Some(path) = flag {
    return Ok(path);
  }
  // An inferred name that is not a valid path component can have no
  // registration dir, so inference simply does not win — fall through.
  if let Some((owner, repo)) = inferred
    && let Ok(dir) = runner_dir(home, owner, repo)
  {
    let candidate = dir.join("config.toml");
    if candidate.is_file() {
      return Ok(candidate);
    }
  }
  let registrations = list_registrations(home);
  match registrations.as_slice() {
    [] => Err(RunnerError::Config(format!(
      "no runner registration found under {}; run `toolu-runner register` first",
      home.display()
    ))),
    [only] => Ok(only.config_path.clone()),
    candidates => {
      let names: Vec<&str> = candidates
        .iter()
        .map(|entry| entry.owner_repo.as_deref().unwrap_or("legacy"))
        .collect();
      Err(RunnerError::Config(format!(
        "several runner registrations found ({}); pass --config to pick one",
        names.join(", ")
      )))
    },
  }
}

/// Reject values unusable as a single path component: empty, `.`, `..`,
/// or containing a `/` / `\` separator.
fn validate_component(what: &str, value: &str) -> Result<(), RunnerError> {
  if value.is_empty()
    || value == "."
    || value == ".."
    || value.contains('/')
    || value.contains('\\')
  {
    return Err(RunnerError::Config(format!(
      "invalid {what} {value:?}: must be a single path component (not empty, `.` or `..`, no `/` \
       or `\\`)"
    )));
  }
  Ok(())
}

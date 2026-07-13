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

/// The runner home: `$TOOLU_RUNNER_HOME` when set (non-empty, a leading
/// `~` expanded), else `~/.toolu-runner`.
pub fn runner_home() -> PathBuf {
  if let Some(home) = std::env::var_os("TOOLU_RUNNER_HOME")
    && !home.is_empty()
  {
    // A `~/x` value must expand exactly like the built-in default does.
    return shared::paths::expand_tilde(Path::new(&home));
  }
  shared::paths::expand_tilde(Path::new("~/.toolu-runner"))
}

/// Per-repo registration dir: `<home>/runners/<owner>/<repo>`.
///
/// `owner` / `repo` are used verbatim as path components, after rejecting
/// anything that is not a plain single component (defense in depth —
/// GitHub names are already `[A-Za-z0-9._-]`; the check is deliberately
/// broader than that charset — it rejects anything path-hazardous rather
/// than mirroring GitHub's naming rules).
///
/// # Errors
///
/// Returns `RunnerError::Config` when `owner` or `repo` is empty, `.`,
/// `..`, or contains a `/` or `\` path separator or a NUL byte.
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
/// yields `Ok(empty)`; an entry under `runners/` (or under an owner dir)
/// that is not a directory is silently skipped — stray files are not
/// registrations. Deterministic order: per-repo entries sorted by
/// `owner/repo`, the legacy entry last.
///
/// # Errors
///
/// `RunnerError::Config` when an EXISTING `runners/` dir (or owner subdir)
/// cannot be read — the message names the path and the io error.
pub fn list_registrations(home: &Path) -> Result<Vec<RegistrationEntry>, RunnerError> {
  let mut entries = Vec::new();
  for owner in read_dir_entries(&home.join("runners"))? {
    // Explicit is_dir() filter: a stray file under `runners/` (or an
    // owner dir) is not a registration and is skipped, not an error.
    if !owner.path().is_dir() {
      continue;
    }
    let owner_name = owner.file_name().to_string_lossy().into_owned();
    for repo in read_dir_entries(&owner.path())? {
      if !repo.path().is_dir() {
        continue;
      }
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
  entries.sort_by(|a, b| a.owner_repo.cmp(&b.owner_repo));
  let legacy = home.join("config.toml");
  if legacy.is_file() {
    entries.push(RegistrationEntry {
      config_path: legacy,
      owner_repo: None,
    });
  }
  Ok(entries)
}

/// `read_dir` for the registry scan: a missing dir reads as empty (no
/// registrations yet — including a dir deleted mid-scan), while any other
/// failure on an EXISTING dir (e.g. permission denied) is an error naming
/// the path and the io error. Per-entry read errors are skipped by
/// `flatten` — one vanishing entry must not hide the other registrations.
fn read_dir_entries(dir: &Path) -> Result<Vec<std::fs::DirEntry>, RunnerError> {
  match std::fs::read_dir(dir) {
    Ok(iter) => Ok(iter.flatten().collect()),
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
    Err(e) => Err(RunnerError::Config(format!(
      "cannot read registrations dir {}: {e}",
      dir.display()
    ))),
  }
}

/// Resolve which registration config a command should use: `flag` as-is >
/// `inferred` `(owner, repo)` whose per-repo `config.toml` exists > the
/// sole registration (legacy included).
///
/// # Errors
///
/// `RunnerError::Config` when no registration exists (naming `toolu-runner
/// register`) or several exist and none matched (listing each candidate).
/// Propagates the [`list_registrations`] error for an unreadable
/// `runners/` dir.
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
  let registrations = list_registrations(home)?;
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
/// or containing a `/` / `\` separator or a NUL byte (NUL terminates C
/// path strings, so it can smuggle a truncated path past the check).
fn validate_component(what: &str, value: &str) -> Result<(), RunnerError> {
  if value.is_empty()
    || value == "."
    || value == ".."
    || value.contains('/')
    || value.contains('\\')
    || value.contains('\0')
  {
    return Err(RunnerError::Config(format!(
      "invalid {what} {value:?}: must be a single path component (not empty, `.` or `..`, no `/`, \
       `\\`, or NUL)"
    )));
  }
  Ok(())
}

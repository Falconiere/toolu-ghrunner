//! Runner registration registry.
//!
//! Each registration lives in its own dir (config, credentials, lock,
//! `_diag/`): `<home>/runners/<owner>/<repo>/` for a repository runner,
//! `<home>/orgs/<org>/` for an organization runner (#208 — `orgs` is a
//! reserved GitHub route, so no owner can be named that, and the two trees
//! never nest). The legacy single-slot `<home>/config.toml` is honored
//! read-only as a fallback. This module
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
  default_runner_home()
}

/// The default runner home, `~/.toolu-runner`, whatever `TOOLU_RUNNER_HOME`
/// says — what a unit that does not pin the home serves.
#[must_use]
pub fn default_runner_home() -> PathBuf {
  shared::paths::expand_tilde(Path::new("~/.toolu-runner"))
}

/// Whether the runner home differs from [`default_runner_home`] (a
/// customised `TOOLU_RUNNER_HOME`). The machine unit pins the home in its
/// environment only then; an unpinned unit serves the default.
#[must_use]
pub fn runner_home_is_custom() -> bool {
  runner_home() != default_runner_home()
}

/// Create the runner home (owner-only `0700` on Unix, best effort) when it
/// does not exist yet.
///
/// # Errors
///
/// `RunnerError::Io` when the directory cannot be created.
pub fn ensure_runner_home(home: &Path) -> Result<(), RunnerError> {
  if home.is_dir() {
    return Ok(());
  }
  std::fs::create_dir_all(home)?;
  crate::config::harden_dir_perms(home);
  Ok(())
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

/// Org-level registration dir: `<home>/orgs/<org>`.
///
/// `org` is used verbatim as one path component after the same rejection
/// rules as [`runner_dir`].
///
/// # Errors
///
/// Returns `RunnerError::Config` when `org` is empty, `.`, `..`, or contains
/// a `/` or `\` path separator or a NUL byte.
pub fn org_dir(home: &Path, org: &str) -> Result<PathBuf, RunnerError> {
  validate_component("org", org)?;
  Ok(home.join("orgs").join(org))
}

/// Which runner a discovered registration serves. The derived order is the
/// listing order: repositories, then organizations, then the legacy slot.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum RegistrationScope {
  /// A repository runner under `runners/<owner>/<repo>/`.
  Repo {
    /// The repository owner (user or organization).
    owner: String,
    /// The repository name.
    repo: String,
  },
  /// An organization runner under `orgs/<org>/`.
  Org(String),
  /// The legacy single-slot `<home>/config.toml`.
  Legacy,
}

impl RegistrationScope {
  /// The registration's display name: `owner/repo`, `<org> (org)`, or
  /// `legacy`.
  #[must_use]
  pub fn label(&self) -> String {
    match self {
      Self::Repo { owner, repo } => format!("{owner}/{repo}"),
      Self::Org(org) => format!("{org} (org)"),
      Self::Legacy => "legacy".to_owned(),
    }
  }
}

/// One discovered registration: its config path plus which runner it serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrationEntry {
  /// Path to the registration's `config.toml`.
  pub config_path: PathBuf,
  /// The repository, organization or legacy slot it serves.
  pub scope: RegistrationScope,
}

/// Scan `home` for registrations: every `runners/<owner>/<repo>/config.toml`,
/// every `orgs/<org>/config.toml`, plus the legacy `<home>/config.toml`. A
/// missing home, `runners/` or `orgs/` dir yields no entries; an entry that
/// is not a directory is silently skipped — stray files are not
/// registrations. Deterministic order: repositories by owner then repo,
/// organizations by name, the legacy entry last.
///
/// # Errors
///
/// `RunnerError::Config` when an EXISTING `runners/` (or owner subdir) or
/// `orgs/` dir cannot be read — the message names the path and the io error.
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
        entries.push(RegistrationEntry {
          config_path,
          scope: RegistrationScope::Repo {
            owner: owner_name.clone(),
            repo: repo.file_name().to_string_lossy().into_owned(),
          },
        });
      }
    }
  }
  for org in read_dir_entries(&home.join("orgs"))? {
    let config_path = org.path().join("config.toml");
    if org.path().is_dir() && config_path.is_file() {
      entries.push(RegistrationEntry {
        config_path,
        scope: RegistrationScope::Org(org.file_name().to_string_lossy().into_owned()),
      });
    }
  }
  let legacy = home.join("config.toml");
  if legacy.is_file() {
    entries.push(RegistrationEntry {
      config_path: legacy,
      scope: RegistrationScope::Legacy,
    });
  }
  entries.sort_by(|a, b| a.scope.cmp(&b.scope));
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
      let names: Vec<String> = candidates.iter().map(|entry| entry.scope.label()).collect();
      Err(RunnerError::Config(format!(
        "several runner registrations found ({}); pass --config to pick one",
        names.join(", ")
      )))
    },
  }
}

/// Reject values unusable as a single path component: empty, `.`, `..`,
/// or containing a `/` / `\` separator or a NUL byte. The NUL check is
/// defense in depth: Rust strings carry NUL harmlessly and the OS layer
/// rejects it in paths — this guards any future C-string boundary.
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

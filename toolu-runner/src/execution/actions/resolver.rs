use std::collections::HashMap;

use shared::RunnerError;

/// Kind of action reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionRefKind {
  /// Remote action from GitHub: `{owner}/{repo}@{ref}`
  Remote,
  /// Local action: `./{path}`
  Local,
}

/// Parsed action reference from a `uses:` field.
#[derive(Debug, Clone)]
pub struct ActionRef {
  pub kind: ActionRefKind,
  pub owner: String,
  pub repo: String,
  pub git_ref: String,
  pub subpath: Option<String>,
  pub local_path: Option<String>,
}

/// Info needed to download and locate an action.
#[derive(Debug, Clone)]
pub struct ResolvedAction {
  pub action_ref: ActionRef,
  pub tarball_url: String,
}

impl ActionRef {
  /// Cache directory key: `{owner}/{repo}/{ref}`.
  pub fn cache_key(&self) -> String {
    format!("{}/{}/{}", self.owner, self.repo, self.git_ref)
  }

  /// GitHub API tarball URL.
  pub fn tarball_url(&self, api_base: &str) -> String {
    format!(
      "{}/repos/{}/{}/tarball/{}",
      api_base, self.owner, self.repo, self.git_ref,
    )
  }

  /// Resolve a local `./path` ref to a directory under `base` (the checked-out
  /// repo / `GITHUB_WORKSPACE`). Returns `None` for non-local refs.
  pub fn local_dir(&self, base: &std::path::Path) -> Option<std::path::PathBuf> {
    let rel = self.local_path.as_deref()?.strip_prefix("./").unwrap_or("");
    Some(base.join(rel))
  }
}

/// Parse a `uses:` string into an `ActionRef`.
///
/// Formats:
/// - `{owner}/{repo}@{ref}` — standard remote action
/// - `{owner}/{repo}/{path}@{ref}` — remote with subpath
/// - `./{path}` — local action
///
/// # Errors
///
/// Returns `RunnerError::ActionResolution` on invalid formats.
pub fn parse_action_ref(uses: &str) -> Result<ActionRef, RunnerError> {
  if uses.starts_with("./") {
    return Ok(local_action_ref(uses));
  }

  let Some((path_part, git_ref)) = uses.split_once('@') else {
    return Err(RunnerError::ActionResolution(format!(
      "invalid action ref '{uses}': missing @ref"
    )));
  };

  if git_ref.is_empty() {
    return Err(RunnerError::ActionResolution(format!(
      "invalid action ref '{uses}': empty ref"
    )));
  }

  let parts: Vec<&str> = path_part.split('/').collect();

  let owner = parts.first().copied().unwrap_or_default();
  let repo = parts.get(1).copied().unwrap_or_default();

  if owner.is_empty() || repo.is_empty() {
    return Err(RunnerError::ActionResolution(format!(
      "invalid action ref '{uses}': need owner/repo"
    )));
  }

  let subpath = if parts.len() > 2 {
    Some(parts.get(2..).unwrap_or_default().join("/"))
  } else {
    None
  };

  Ok(ActionRef {
    kind: ActionRefKind::Remote,
    owner: owner.to_owned(),
    repo: repo.to_owned(),
    git_ref: git_ref.to_owned(),
    subpath,
    local_path: None,
  })
}

/// Build an `ActionRef` for a local `./path` reference.
///
/// A local ref carries no `@ref`; a trailing `@` left by callers that
/// unconditionally append `@{git_ref}` with an empty ref is stripped.
fn local_action_ref(uses: &str) -> ActionRef {
  let path = uses.strip_suffix('@').unwrap_or(uses);
  ActionRef {
    kind: ActionRefKind::Local,
    owner: String::new(),
    repo: String::new(),
    git_ref: String::new(),
    subpath: None,
    local_path: Some(path.to_owned()),
  }
}

/// Resolve a batch of `uses:` references.
///
/// Deduplicates remote refs and skips local refs.
///
/// # Errors
///
/// Returns `RunnerError::ActionResolution` on invalid refs.
pub fn resolve_action_refs(
  uses_refs: &[String],
) -> Result<HashMap<String, ResolvedAction>, RunnerError> {
  let api_base = "https://api.github.com";
  let mut resolved = HashMap::new();

  for uses in uses_refs {
    let action_ref = parse_action_ref(uses)?;

    if action_ref.kind == ActionRefKind::Local {
      continue;
    }

    let key = action_ref.cache_key();
    if resolved.contains_key(&key) {
      continue;
    }

    let tarball_url = action_ref.tarball_url(api_base);
    resolved.insert(
      key,
      ResolvedAction {
        action_ref,
        tarball_url,
      },
    );
  }

  Ok(resolved)
}

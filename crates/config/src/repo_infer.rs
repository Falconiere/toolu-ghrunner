//! Repo inference from the cwd git remote.
//!
//! [`parse_remote_url`] is the pure, host-agnostic URL parser (the caller
//! rejects non-github.com hosts); [`detect_repo`] shells out to
//! `git remote get-url origin` and maps each failure to a distinct
//! user-facing error naming the `--url` escape hatch.

use std::path::Path;
use std::process::Command;

use shared::RunnerError;

/// A repo parsed from a git remote URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InferredRepo {
  /// Remote host, e.g. `github.com` (ssh aliases pass through verbatim).
  pub host: String,
  /// Repo owner (user or organization).
  pub owner: String,
  /// Repo name, `.git` suffix stripped.
  pub repo: String,
}

/// Parse a git remote URL into host / owner / repo. Pure, host-agnostic.
///
/// Handles the real-world remote forms: scp-like `git@host:owner/repo.git`,
/// `https://host/owner/repo[.git]`, `ssh://git@host[:port]/owner/repo.git`.
/// Anything else (bare paths, URLs without an `owner/repo` path) is `None`.
pub fn parse_remote_url(url: &str) -> Option<InferredRepo> {
  let url = url.trim();
  if let Some(rest) = url.strip_prefix("ssh://") {
    return parse_hierarchical(rest);
  }
  if let Some(rest) = url.strip_prefix("https://") {
    return parse_hierarchical(rest);
  }
  parse_scp_like(url)
}

/// Detect the repo for `cwd` from its `origin` remote
/// (`git -C <cwd> remote get-url origin` + [`parse_remote_url`]).
///
/// # Errors
///
/// `RunnerError::Config` — distinct messages, each naming the `--url`
/// escape hatch — when `cwd` is not a git repository, has no `origin`
/// remote, or its remote URL does not parse.
pub fn detect_repo(cwd: &Path) -> Result<InferredRepo, RunnerError> {
  let output = Command::new("git")
    .arg("-C")
    .arg(cwd)
    .args(["remote", "get-url", "origin"])
    // Pin the locale: `classify_git_failure` matches English stderr text.
    .env("LC_ALL", "C")
    .output()
    .map_err(|e| {
      RunnerError::Config(format!(
        "could not run git to infer the repo from {}: {e}; pass --url with the repository URL \
         instead",
        cwd.display()
      ))
    })?;
  if !output.status.success() {
    return Err(classify_git_failure(
      cwd,
      &String::from_utf8_lossy(&output.stderr),
    ));
  }
  let stdout = String::from_utf8(output.stdout).map_err(|e| {
    RunnerError::Config(format!(
      "git returned a non-UTF-8 `origin` remote URL ({e}); pass --url with the repository URL \
       instead"
    ))
  })?;
  let url = stdout.trim();
  parse_remote_url(url).ok_or_else(|| {
    RunnerError::Config(format!(
      "could not parse the `origin` remote URL {url:?}; pass --url with the repository URL instead"
    ))
  })
}

/// Parse the `[user@]host[:port]/owner/repo[.git]` tail of an `ssh://` or
/// `https://` URL.
fn parse_hierarchical(rest: &str) -> Option<InferredRepo> {
  let (authority, path) = rest.split_once('/')?;
  let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
  let host = host.split_once(':').map_or(host, |(h, _)| h);
  if host.is_empty() {
    return None;
  }
  build(host, path)
}

/// Parse an scp-like remote: `user@host:owner/repo[.git]`.
fn parse_scp_like(url: &str) -> Option<InferredRepo> {
  let (_, rest) = url.split_once('@')?;
  let (host, path) = rest.split_once(':')?;
  if host.is_empty() || host.contains('/') {
    return None;
  }
  build(host, path)
}

/// Assemble an `InferredRepo` from a host and an `owner/repo[.git]` path
/// (one trailing `/` tolerated); `None` unless exactly two segments.
fn build(host: &str, path: &str) -> Option<InferredRepo> {
  let path = path.strip_suffix('/').unwrap_or(path);
  let (owner, repo) = path.split_once('/')?;
  if owner.is_empty() || repo.is_empty() || repo.contains('/') {
    return None;
  }
  let repo = repo.strip_suffix(".git").unwrap_or(repo);
  if repo.is_empty() {
    return None;
  }
  Some(InferredRepo {
    host: host.to_owned(),
    owner: owner.to_owned(),
    repo: repo.to_owned(),
  })
}

/// Map a failed `git remote get-url origin` to a user-facing error:
/// not-a-repo vs no-`origin`, with a generic fallback.
fn classify_git_failure(cwd: &Path, stderr: &str) -> RunnerError {
  let lower = stderr.to_lowercase();
  if lower.contains("not a git repository") {
    return RunnerError::Config(format!(
      "{} is not a git repository, so the repo cannot be inferred; pass --url with the repository \
       URL instead",
      cwd.display()
    ));
  }
  if lower.contains("no such remote") {
    return RunnerError::Config(format!(
      "the git repository at {} has no `origin` remote; pass --url with the repository URL instead",
      cwd.display()
    ));
  }
  RunnerError::Config(format!(
    "`git remote get-url origin` failed in {}: {}; pass --url with the repository URL instead",
    cwd.display(),
    stderr.trim()
  ))
}

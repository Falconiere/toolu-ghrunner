//! The resolved setup plan: pure data plus the flag grammar that turns
//! [`SetupArgs`] (and, interactively, prompt answers) into one plan the
//! single apply path executes. A missing unattended answer is a
//! [`SetupError`] with code 2 naming the flag.

use std::path::PathBuf;

use super::args::SetupArgs;

/// A setup failure with its process exit code (`0/1/2/130` contract).
#[derive(Debug)]
pub(crate) struct SetupError {
  /// Process exit code.
  pub(crate) code: i32,
  /// User-facing message (already secret-masked by callers).
  pub(crate) message: String,
}

impl SetupError {
  /// A usage/auth failure (exit 2).
  pub(crate) fn usage(message: impl Into<String>) -> Self {
    Self {
      code: 2,
      message: message.into(),
    }
  }

  /// A runtime failure (exit 1).
  pub(crate) fn runtime(message: impl Into<String>) -> Self {
    Self {
      code: 1,
      message: message.into(),
    }
  }

  /// A cancelled run (exit 130).
  pub(crate) fn cancelled() -> Self {
    Self {
      code: 130,
      message: "cancelled".to_owned(),
    }
  }
}

impl std::fmt::Display for SetupError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(&self.message)
  }
}

impl std::error::Error for SetupError {}

/// Where the runners register.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
  /// One organization-level runner.
  Org {
    /// The organization login.
    org: String,
  },
  /// One registration per repository (all under one owner).
  Repos {
    /// `owner/name` repositories.
    repos: Vec<String>,
  },
}

impl Target {
  /// Registration URLs under the GitHub web base.
  pub(crate) fn urls(&self, github_base: &str) -> Vec<String> {
    match self {
      Self::Org { org } => vec![format!("{github_base}/{org}")],
      Self::Repos { repos } => repos
        .iter()
        .map(|repo| format!("{github_base}/{repo}"))
        .collect(),
    }
  }

  /// The target's display name for messages.
  pub(crate) fn label(&self) -> String {
    match self {
      Self::Org { org } => format!("organization {org}"),
      Self::Repos { repos } => repos.join(", "),
    }
  }
}

/// How setup authenticates (resolved; secrets are fetched later and
/// never stored in the plan's `Debug` output).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AuthPlan {
  /// Reuse the App saved at `<home>/github-app.json`.
  SavedApp,
  /// Create a new App through the `create-app` browser flow.
  CreateApp,
  /// Import an existing App (`--import`, or `--app-id` + `--app-key`).
  Import {
    /// The `github-app.json` to import, if any.
    file: Option<PathBuf>,
    /// The App id, with `key`.
    app_id: Option<i64>,
    /// The App key PEM path, with `app_id`.
    key: Option<PathBuf>,
  },
  /// A personal access token (value resolved at the auth step).
  Token,
}

/// Registration-mode choices of a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanMode {
  /// Ephemeral registration.
  pub(crate) ephemeral: bool,
  /// Replace a same-name runner/registration.
  pub(crate) replace: bool,
  /// Drop the default labels.
  pub(crate) no_default_labels: bool,
}

/// Service choices of a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanService {
  /// Install the machine service.
  pub(crate) install: bool,
  /// The linger answer (decided in setup, never by the installer).
  pub(crate) enable_linger: bool,
}

/// One fully-resolved setup plan: the only thing apply executes.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedPlan {
  /// GitHub web base (`https://github.com`, GHES, or a loopback peer).
  pub(crate) github_base: String,
  /// Registration host derived from `github_base`.
  pub(crate) host: String,
  /// Where the runners register.
  pub(crate) target: Target,
  /// How setup authenticates.
  pub(crate) auth: AuthPlan,
  /// Runner group name/id for org targets.
  pub(crate) runner_group: Option<String>,
  /// Runner name (hostname default applied by the caller).
  pub(crate) name: String,
  /// Extra labels from flags/prompts.
  pub(crate) labels: Vec<String>,
  /// Work dir string persisted in the registration.
  pub(crate) work_folder: String,
  /// Registration-mode choices.
  pub(crate) mode: PlanMode,
  /// Service choices.
  pub(crate) service: PlanService,
}

/// Validate `--github-url`: `https` (or `http` only for loopback), no
/// userinfo/query/fragment/path. Returns the base without a trailing
/// slash.
///
/// # Errors
///
/// [`SetupError`] code 2 naming `--github-url`.
pub(crate) fn validate_github_url(raw: Option<&str>) -> Result<String, SetupError> {
  let Some(raw) = raw else {
    return Ok("https://github.com".to_owned());
  };
  let trimmed = raw.trim_end_matches('/');
  let parsed = url::Url::parse(trimmed)
    .map_err(|e| SetupError::usage(format!("--github-url '{raw}' is not a URL: {e}")))?;
  if !parsed.username().is_empty() || parsed.password().is_some() {
    return Err(SetupError::usage("--github-url must not carry credentials"));
  }
  let host = parsed
    .host_str()
    .ok_or_else(|| SetupError::usage(format!("--github-url '{raw}' has no host")))?;
  let loopback = matches!(host, "localhost" | "127.0.0.1" | "::1");
  match parsed.scheme() {
    "https" => {},
    "http" if loopback => {},
    scheme => {
      return Err(SetupError::usage(format!(
        "--github-url '{raw}' uses {scheme}: only https (or http to a loopback peer) is accepted"
      )));
    },
  }
  if !parsed.username().is_empty() || parsed.password().is_some() {
    return Err(SetupError::usage("--github-url must not carry credentials"));
  }
  if parsed.query().is_some()
    || parsed.fragment().is_some()
    || parsed.path() != "/" && parsed.path() != ""
  {
    return Err(SetupError::usage(
      "--github-url is the GitHub web base only (no path, query or fragment)",
    ));
  }
  if !loopback && !host.contains('.') {
    return Err(SetupError::usage(format!(
      "--github-url host '{host}' is not a GitHub host (github.com or a GHES host)"
    )));
  }
  Ok(trimmed.to_owned())
}

/// The host of a validated GitHub web base.
pub(crate) fn host_of(github_base: &str) -> String {
  url::Url::parse(github_base)
    .ok()
    .and_then(|url| url.host_str().map(str::to_owned))
    .unwrap_or_default()
}

/// Whether a host is a loopback test peer.
pub(crate) fn is_loopback(host: &str) -> bool {
  matches!(host, "localhost" | "127.0.0.1" | "::1")
}

/// The auth kind the flags ask for, with grammar enforced.
///
/// # Errors
///
/// [`SetupError`] code 2 naming the required/conflicting flag.
pub(crate) fn auth_plan(args: &SetupArgs, saved_app_exists: bool) -> Result<AuthPlan, SetupError> {
  let import_given = args.import.is_some() || args.app_id.is_some() || args.app_key.is_some();
  if args.app_id.is_some() != args.app_key.is_some() {
    return Err(SetupError::usage(
      "--app-id and --app-key must be given together",
    ));
  }
  match args.auth.as_deref() {
    Some("app") => {
      if args.token.is_some() || args.token_stdin || import_given {
        return Err(SetupError::usage(
          "--auth app conflicts with --token/--token-stdin/--import/--app-id/--app-key",
        ));
      }
      Ok(if saved_app_exists {
        AuthPlan::SavedApp
      } else {
        AuthPlan::CreateApp
      })
    },
    Some("import") => {
      if args.token.is_some() || args.token_stdin {
        return Err(SetupError::usage(
          "--auth import conflicts with --token/--token-stdin",
        ));
      }
      if !import_given {
        return Err(SetupError::usage(
          "--auth import needs --import <file>, or --app-id <id> with --app-key <pem>",
        ));
      }
      Ok(AuthPlan::Import {
        file: args.import.clone(),
        app_id: args.app_id,
        key: args.app_key.clone(),
      })
    },
    Some("token") => {
      if import_given {
        return Err(SetupError::usage(
          "--auth token conflicts with --import/--app-id/--app-key",
        ));
      }
      Ok(AuthPlan::Token)
    },
    Some(other) => Err(SetupError::usage(format!(
      "--auth '{other}' is not one of app|import|token"
    ))),
    None => {
      if import_given {
        return Ok(AuthPlan::Import {
          file: args.import.clone(),
          app_id: args.app_id,
          key: args.app_key.clone(),
        });
      }
      if args.token.is_some() || args.token_stdin {
        return Ok(AuthPlan::Token);
      }
      Ok(if saved_app_exists {
        AuthPlan::SavedApp
      } else {
        AuthPlan::CreateApp
      })
    },
  }
}

/// The target the flags name, if any. `None` means a prompt must ask
/// (interactive) or setup exits 2 (unattended, decided by the caller).
///
/// # Errors
///
/// [`SetupError`] code 2 for a malformed `--repo` or several owners.
pub(crate) fn flag_target(args: &SetupArgs) -> Result<Option<Target>, SetupError> {
  if let Some(org) = &args.org {
    if org.trim().is_empty() {
      return Err(SetupError::usage("--org needs an organization name"));
    }
    return Ok(Some(Target::Org { org: org.clone() }));
  }
  if args.repo.is_empty() {
    return Ok(None);
  }
  let mut owner: Option<String> = None;
  for repo in &args.repo {
    let Some((repo_owner, name)) = repo.split_once('/') else {
      return Err(SetupError::usage(format!(
        "--repo '{repo}' is not owner/name"
      )));
    };
    if repo_owner.is_empty() || name.is_empty() || name.contains('/') {
      return Err(SetupError::usage(format!(
        "--repo '{repo}' is not owner/name"
      )));
    }
    match &owner {
      Some(existing) if existing != repo_owner => {
        return Err(SetupError::usage(format!(
          "--repo values span several owners ({existing}, {repo_owner}); one setup run covers one account — re-run for {repo_owner}"
        )));
      },
      _ => owner = Some(repo_owner.to_owned()),
    }
  }
  Ok(Some(Target::Repos {
    repos: args.repo.clone(),
  }))
}

#[cfg(test)]
#[path = "tests/plan.rs"]
mod tests;

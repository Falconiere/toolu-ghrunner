//! Live GitHub reads for setup (org check, runner groups, repository
//! lists, token permission probes, name collision, online polling).
//! Every base derives from `wire::net::resolve_target` on the composed
//! registration URL; App creation is the one exception (it reads
//! `config::github_api_url()` inside `create-app`).

use std::time::Duration;

use shared::RunnerError;
use wire::net::runner_groups::RunnerGroup;

use super::plan::SetupError;

/// Per-request timeout for setup's live reads.
pub(crate) const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// The API base for a registration URL.
///
/// # Errors
///
/// [`SetupError`] code 2 for an unaddressable URL.
pub(crate) fn api_base(url: &str) -> Result<String, SetupError> {
  wire::net::resolve_target(url)
    .map(|(base, _)| base)
    .map_err(|e| SetupError::usage(e.to_string()))
}

/// What the live org check found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum OrgCheck {
  /// The org exists; its login as GitHub spells it.
  Found(String),
  /// No such org (a typo, or a user login).
  Missing,
}

/// Check an org name live, before any credential is asked for.
///
/// # Errors
///
/// [`SetupError`] code 1 for a GitHub/transport problem (a 403 or 5xx
/// is never rendered as a typo).
pub(crate) async fn check_org(
  client: &reqwest::Client,
  api_base: &str,
  org: &str,
) -> Result<OrgCheck, SetupError> {
  match wire::net::github_app::find_org(client, api_base, org, READ_TIMEOUT).await {
    Ok(Some(found)) => Ok(OrgCheck::Found(found.login)),
    Ok(None) => Ok(OrgCheck::Missing),
    Err(e) => Err(SetupError::runtime(format!(
      "could not check the organization \"{org}\" on GitHub: {e}"
    ))),
  }
}

/// The org's runner groups, or `None` when they cannot be listed (the
/// caller warns, uses Default, and names `--runner-group`).
pub(crate) async fn list_groups(
  client: &reqwest::Client,
  api_base: &str,
  org: &str,
  bearer: &str,
) -> Option<Vec<RunnerGroup>> {
  match wire::net::runner_groups::list_runner_groups(client, api_base, org, bearer, READ_TIMEOUT)
    .await
  {
    Ok(groups) => Some(groups),
    Err(e) => {
      tracing::warn!(%e, "could not list runner groups for {org}");
      None
    },
  }
}

/// Whether `group` is (or names) the Default group.
pub(crate) fn is_default_group(groups: &[RunnerGroup], chosen_id: i64) -> bool {
  chosen_id == 1
    || groups
      .iter()
      .any(|group| group.id == chosen_id && group.default)
}

/// Repositories the credential can register runners for: the App
/// installation's list for an installation token, the user's own list
/// for a personal token.
///
/// # Errors
///
/// [`SetupError`] code 2 when GitHub refuses the credential.
pub(crate) async fn list_repositories(
  client: &reqwest::Client,
  api_base: &str,
  bearer: &str,
  app_installation: bool,
) -> Result<Vec<String>, SetupError> {
  let repos = if app_installation {
    wire::net::github_app::installation_repositories(client, api_base, bearer, READ_TIMEOUT).await
  } else {
    wire::net::github_app::user_repositories(client, api_base, bearer, READ_TIMEOUT).await
  }
  .map_err(|e| SetupError::usage(format!("could not list repositories: {e}")))?;
  Ok(repos.into_iter().map(|repo| repo.full_name).collect())
}

/// The token's GitHub login (`GET /user`, personal tokens only).
async fn token_login(
  client: &reqwest::Client,
  api_base: &str,
  token: &str,
) -> Result<String, SetupError> {
  let response = client
    .get(format!("{api_base}/user"))
    .timeout(READ_TIMEOUT)
    .bearer_auth(token)
    .header("Accept", "application/vnd.github+json")
    .send()
    .await
    .map_err(|e| SetupError::runtime(format!("could not reach GitHub: {e}")))?;
  let status = response.status();
  if !status.is_success() {
    return Err(SetupError::usage(format!(
      "GitHub refused the token (GET /user answered {status}); check the token and re-run"
    )));
  }
  let body: serde_json::Value = response
    .json()
    .await
    .map_err(|e| SetupError::runtime(format!("GitHub's /user reply did not parse: {e}")))?;
  body
    .get("login")
    .and_then(serde_json::Value::as_str)
    .map(str::to_owned)
    .ok_or_else(|| SetupError::runtime("GitHub's /user reply named no login".to_owned()))
}

/// Probe that a personal token may administer the plan's targets:
/// org → active admin membership; repo → `permissions.admin`. The
/// `x-oauth-scopes` header is never read: the membership/permissions
/// replies gate classic and fine-grained tokens alike.
///
/// # Errors
///
/// [`SetupError`] code 2 naming the permission the token lacks.
pub(crate) async fn probe_token(
  client: &reqwest::Client,
  api_base: &str,
  token: &str,
  org: Option<&str>,
  repos: &[String],
) -> Result<String, SetupError> {
  let login = token_login(client, api_base, token).await?;
  if let Some(org) = org {
    let response = client
      .get(format!("{api_base}/orgs/{org}/memberships/{login}"))
      .timeout(READ_TIMEOUT)
      .bearer_auth(token)
      .header("Accept", "application/vnd.github+json")
      .send()
      .await
      .map_err(|e| SetupError::runtime(format!("could not reach GitHub: {e}")))?;
    let status = response.status();
    if status.as_u16() == 429 || status.is_server_error() {
      return Err(SetupError::runtime(format!(
        "GitHub answered {status} while checking organization membership — try again in a moment"
      )));
    }
    if status.as_u16() != 404 && !status.is_success() {
      return Err(SetupError::runtime(format!(
        "GitHub answered {status} while checking organization membership — try again in a moment"
      )));
    }
    let body: serde_json::Value = response
      .json()
      .await
      .map_err(|e| SetupError::runtime(format!("GitHub's membership reply did not parse: {e}")))?;
    let admin = status.is_success()
      && body.get("state").and_then(serde_json::Value::as_str) == Some("active")
      && body.get("role").and_then(serde_json::Value::as_str) == Some("admin");
    if !admin {
      return Err(SetupError::usage(format!(
        "the token belongs to {login}, who is not an admin of {org}: org runners need an organization admin's token (or a GitHub App) — re-run with --auth app or an admin's --token"
      )));
    }
    return Ok(login);
  }
  for repo in repos {
    let response = client
      .get(format!("{api_base}/repos/{repo}"))
      .timeout(READ_TIMEOUT)
      .bearer_auth(token)
      .header("Accept", "application/vnd.github+json")
      .send()
      .await
      .map_err(|e| SetupError::runtime(format!("could not reach GitHub: {e}")))?;
    let status = response.status();
    if status.as_u16() == 429 || status.is_server_error() {
      return Err(SetupError::runtime(format!(
        "GitHub answered {status} while checking {repo} — try again in a moment"
      )));
    }
    if !status.is_success() {
      return Err(SetupError::usage(format!(
        "GitHub answered {status} for {repo}: the token cannot see it, or it does not exist"
      )));
    }
    let body: serde_json::Value = response
      .json()
      .await
      .map_err(|e| SetupError::runtime(format!("GitHub's reply for {repo} did not parse: {e}")))?;
    let admin = body
      .get("permissions")
      .and_then(|p| p.get("admin"))
      .and_then(serde_json::Value::as_bool)
      .unwrap_or(false);
    if !admin {
      return Err(SetupError::usage(format!(
        "the token belongs to {login}, who is not an admin of {repo}: repo runners need admin on the repository — re-run with an admin's --token or --auth app"
      )));
    }
  }
  Ok(login)
}

/// The runner registered under `name` for `url`, if any (collision and
/// online checks share this lookup).
pub(crate) async fn find_runner(
  client: &reqwest::Client,
  url: &str,
  bearer: &str,
  name: &str,
) -> Result<Option<wire::net::register::RunnerSummary>, RunnerError> {
  wire::net::register::find_runner(client, url, bearer, name, READ_TIMEOUT).await
}

/// Poll until GitHub reports the runner `online`, or the deadline
/// passes. Transient answers (429/5xx/transport, not yet listed) retry
/// until the deadline; an auth answer fails at once.
///
/// # Errors
///
/// [`SetupError`] code 1 naming `doctor` when the deadline passes, or
/// code 2 when GitHub refuses the credential.
pub(crate) async fn poll_online(
  client: &reqwest::Client,
  url: &str,
  bearer: &str,
  name: &str,
  timeout: Duration,
) -> Result<wire::net::register::RunnerSummary, SetupError> {
  let started = std::time::Instant::now();
  loop {
    match find_runner(client, url, bearer, name).await {
      Ok(Some(runner)) if runner.status == "online" => return Ok(runner),
      Ok(_) | Err(RunnerError::Network(_)) => {},
      Err(e) => {
        return Err(SetupError::usage(format!(
          "could not check whether '{name}' is online: {e}"
        )));
      },
    }
    if started.elapsed() >= timeout {
      return Err(SetupError::runtime(format!(
        "the runner '{name}' did not come online within {}s — run `toolu-runner doctor` to see what is wrong, then re-run `toolu-runner setup`",
        timeout.as_secs()
      )));
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
  }
}

#[cfg(test)]
#[path = "tests/github.rs"]
mod tests;

//! Organization runner groups (#201): resolve a `--runner-group` name to the
//! numeric id `generate-jitconfig` takes.
//!
//! `GET <api>/orgs/{org}/actions/runner-groups` needs org self-hosted-runner
//! read access (an App installation token or an `admin:org` PAT). Failures
//! map like the App calls ([`super::github_app`]).

use std::time::Duration;

use serde::Deserialize;
use shared::RunnerError;

use super::github_app::{decode, github_request, send};

/// Groups requested per list call (GitHub's maximum page size); a longer
/// list is not paged through.
pub const PAGE_SIZE: usize = 100;

/// One organization runner group.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct RunnerGroup {
  /// The id `generate-jitconfig`'s `runner_group_id` takes.
  pub id: i64,
  /// The group name shown in the org settings.
  pub name: String,
  /// GitHub's `default` flag: the org's Default group.
  #[serde(default)]
  pub default: bool,
  /// Whether the group allows public repositories (GitHub's
  /// `allows_public_repositories`); the Default group does not.
  #[serde(default)]
  pub allows_public_repositories: bool,
}

/// The list reply envelope.
#[derive(Deserialize)]
struct RunnerGroupsPage {
  runner_groups: Vec<RunnerGroup>,
}

/// The org's runner groups (the first [`PAGE_SIZE`]).
///
/// # Errors
///
/// `RunnerError::Auth` for a permanent non-2xx (401, 403 — the bearer cannot
/// read runner groups —, 404), `RunnerError::Network` for transport / 429 /
/// 5xx, `RunnerError::Protocol` for an unparseable 200.
pub async fn list_runner_groups(
  client: &reqwest::Client,
  api_base: &str,
  org: &str,
  token: &str,
  timeout: Duration,
) -> Result<Vec<RunnerGroup>, RunnerError> {
  let url = format!("{api_base}/orgs/{org}/actions/runner-groups");
  let request = client
    .get(&url)
    .query(&[("per_page", PAGE_SIZE.to_string())]);
  let what = "runner group list";
  let (status, body) = send(github_request(request, token, timeout), what).await?;
  let page: RunnerGroupsPage = decode(what, status, &body)?;
  Ok(page.runner_groups)
}

/// The id of the group called `name`: an exact match, else the one group
/// whose name matches ignoring ASCII case. `None` when absent or ambiguous.
pub fn pick_group(groups: &[RunnerGroup], name: &str) -> Option<i64> {
  if let Some(group) = groups.iter().find(|group| group.name == name) {
    return Some(group.id);
  }
  let mut folded = groups
    .iter()
    .filter(|group| group.name.eq_ignore_ascii_case(name));
  match (folded.next(), folded.next()) {
    (Some(group), None) => Some(group.id),
    _ => None,
  }
}

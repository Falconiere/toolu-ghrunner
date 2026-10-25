//! Live JIT registration: the github.com `generate-jitconfig` call.
//!
//! POSTs the REST bearer to GitHub's
//! `…/actions/runners/generate-jitconfig` — under `repos/{owner}/{repo}` for
//! a repository URL, `orgs/{org}` for an organization URL ([`RunnerScope`]) —
//! which returns the runner `id`
//! plus an `encoded_jit_config` (the base64 3-blob envelope that
//! [`protocol::JitConfig`] parses at run time). The RSA → JWT → `OAuth2`
//! exchange happens at run time from that config, not here.
//!
//! Split for token-free testing: [`build_request`] / [`parse_response`]
//! are pure; [`register_jit`] is the async send that glues them.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use shared::RunnerError;

/// Successful `generate-jitconfig` result, ready to persist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JitRegistration {
  /// Runner ID GitHub assigned to this registration.
  pub runner_id: i64,
  /// Base64 3-blob JIT config envelope (`.runner` / `.credentials` /
  /// `.credentials_rsaparams`). Parsed by [`protocol::JitConfig`] at run time.
  pub encoded_jit_config: String,
  /// The runner name GitHub recorded (echoed back from the request).
  pub runner_name: String,
}

/// A built, un-sent registration request: target URL + JSON body.
///
/// Pure value so tests can assert the wire contract (URL, body shape)
/// without any network or token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterRequest {
  /// Fully-qualified `generate-jitconfig` endpoint URL.
  pub url: String,
  /// JSON request body per the GitHub REST contract.
  pub body: GenerateJitConfigBody,
}

/// Request body for `POST …/actions/runners/generate-jitconfig`.
///
/// Field names match GitHub's REST contract exactly (`runner_group_id`,
/// `work_folder`). `serde` serializes this verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GenerateJitConfigBody {
  /// Runner display name.
  pub name: String,
  /// Runner group ID (1 = the repo/org `Default` group).
  pub runner_group_id: i64,
  /// Labels the runner advertises.
  pub labels: Vec<String>,
  /// Work folder name (relative); GitHub stores it in `.runner`.
  pub work_folder: String,
}

/// GitHub's `generate-jitconfig` response envelope.
#[derive(Debug, Clone, Deserialize)]
struct GenerateJitConfigResponse {
  runner: RunnerObject,
  encoded_jit_config: String,
}

/// The `runner` sub-object GitHub returns (only `id` is load-bearing here).
#[derive(Debug, Clone, Deserialize)]
struct RunnerObject {
  id: i64,
}

/// Build the `generate-jitconfig` request from inputs.
///
/// Parses the [`RunnerScope`] from `url` (`owner/repo` or an org); targets
/// `api.github.com` for github.com and `<host>/api/v3` for GHES.
/// `runner_group_id` defaults to `1` (the `Default` group) when `None`.
///
/// # Errors
///
/// `RunnerError::Config` when `url` has no host or no owner/org path.
pub fn build_request(
  url: &str,
  name: &str,
  labels: &[String],
  runner_group_id: Option<i64>,
  work_folder: &str,
) -> Result<RegisterRequest, RunnerError> {
  let url = resolve_endpoint(url)?;
  let body = GenerateJitConfigBody {
    name: name.to_owned(),
    runner_group_id: runner_group_id.unwrap_or(1),
    labels: labels.to_vec(),
    work_folder: work_folder.to_owned(),
  };
  Ok(RegisterRequest { url, body })
}

/// Resolve the `generate-jitconfig` endpoint from a registration URL.
///
/// # Errors
///
/// `RunnerError::Config` when `url` lacks a host or an owner/org path.
fn resolve_endpoint(url: &str) -> Result<String, RunnerError> {
  Ok(format!("{}/generate-jitconfig", resolve_runners_base(url)?))
}

/// Which runners API a registration URL addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerScope {
  /// A repository runner: `https://<host>/<owner>/<repo>`.
  Repo {
    /// The repository owner (user or organization).
    owner: String,
    /// The repository name, `.git` stripped.
    repo: String,
  },
  /// An organization runner: `https://<host>/<org>`.
  Org {
    /// The organization login.
    org: String,
  },
}

impl RunnerScope {
  /// The scope's REST path under the API base: `repos/{owner}/{repo}` or
  /// `orgs/{org}`.
  pub fn api_path(&self) -> String {
    match self {
      RunnerScope::Repo { owner, repo } => format!("repos/{owner}/{repo}"),
      RunnerScope::Org { org } => format!("orgs/{org}"),
    }
  }

  /// `owner/repo` or the org login, for messages.
  pub fn display_name(&self) -> String {
    match self {
      RunnerScope::Repo { owner, repo } => format!("{owner}/{repo}"),
      RunnerScope::Org { org } => org.clone(),
    }
  }
}

/// Resolve a registration URL into its REST API base ([`api_base`]) and
/// [`RunnerScope`]: one path segment is an organization, two a repository
/// (a trailing `.git` stripped; later segments ignored).
///
/// # Errors
///
/// `RunnerError::Config` when `url` does not parse, has no host, or has no
/// path segment.
pub fn resolve_target(url: &str) -> Result<(String, RunnerScope), RunnerError> {
  let parsed =
    url::Url::parse(url).map_err(|e| RunnerError::Config(format!("invalid --url: {e}")))?;
  let host = parsed
    .host_str()
    .ok_or_else(|| RunnerError::Config("URL missing host".to_owned()))?;
  let mut segments = parsed
    .path_segments()
    .ok_or_else(|| {
      RunnerError::Config(format!(
        "URL '{url}' has no path — expected owner/repo or an organization"
      ))
    })?
    .filter(|s| !s.is_empty());
  let owner = segments.next().ok_or_else(|| {
    RunnerError::Config(format!("URL '{url}' names no owner/repo or organization"))
  })?;
  let scope = match segments.next() {
    Some(repo_raw) => RunnerScope::Repo {
      owner: owner.to_owned(),
      repo: repo_raw.strip_suffix(".git").unwrap_or(repo_raw).to_owned(),
    },
    None => RunnerScope::Org {
      org: owner.to_owned(),
    },
  };
  Ok((api_base(&parsed, host), scope))
}

/// Resolve the `…/actions/runners` API base from a registration URL:
/// `<api>/repos/{owner}/{repo}/actions/runners` or
/// `<api>/orgs/{org}/actions/runners` ([`resolve_target`]).
///
/// # Errors
///
/// `RunnerError::Config` when `url` lacks a host or an owner/org path.
fn resolve_runners_base(url: &str) -> Result<String, RunnerError> {
  let (base, scope) = resolve_target(url)?;
  Ok(format!("{base}/{}/actions/runners", scope.api_path()))
}

/// The REST API base for a GitHub URL whose host is `host`: github.com
/// routes through `https://api.github.com`; any other host keeps the URL's
/// scheme and authority and adds `/api/v3` (GHES). `url` is a parsed
/// `http(s)` repository or server URL and `host` its `host_str()`; the result
/// is only as valid as that input.
#[must_use]
pub fn api_base(url: &url::Url, host: &str) -> String {
  if host.eq_ignore_ascii_case("github.com") {
    return "https://api.github.com".to_owned();
  }
  let authority = match url.port() {
    Some(port) => format!("{host}:{port}"),
    None => host.to_owned(),
  };
  format!("{}://{authority}/api/v3", url.scheme())
}

/// Parse GitHub's `generate-jitconfig` JSON response body.
///
/// Pure: feed it the raw response body and the runner name from the
/// request, and it yields the persistable [`JitRegistration`].
///
/// # Errors
///
/// Returns `RunnerError::Protocol` when the body is not the expected
/// JSON shape (missing `runner.id` or `encoded_jit_config`).
pub fn parse_response(body: &str, runner_name: &str) -> Result<JitRegistration, RunnerError> {
  let resp: GenerateJitConfigResponse = serde_json::from_str(body)
    .map_err(|e| RunnerError::Protocol(format!("generate-jitconfig response parse failed: {e}")))?;
  if resp.encoded_jit_config.is_empty() {
    return Err(RunnerError::Protocol(
      "generate-jitconfig response had an empty encoded_jit_config".to_owned(),
    ));
  }
  Ok(JitRegistration {
    runner_id: resp.runner.id,
    encoded_jit_config: resp.encoded_jit_config,
    runner_name: runner_name.to_owned(),
  })
}

/// Registration inputs for [`register_jit`] (and [`build_request`]).
#[derive(Debug, Clone)]
pub struct RegisterParams<'a> {
  /// Repo or org URL (`https://<host>/<owner>/<repo>`, `https://<host>/<org>`).
  pub url: &'a str,
  /// API token with `administration:write` on the repo, or org self-hosted
  /// runner admin rights (PAT or App installation token), sent as
  /// `Authorization: Bearer …`. NOT the
  /// classic runner registration token — `generate-jitconfig` is a
  /// plain REST endpoint and rejects registration tokens with 401.
  pub runner_token: &'a str,
  /// Runner display name.
  pub name: &'a str,
  /// Labels the runner advertises.
  pub labels: &'a [String],
  /// Runner group ID; `None` defaults to `1` (the `Default` group).
  pub runner_group_id: Option<i64>,
  /// Work folder name GitHub records in `.runner`.
  pub work_folder: &'a str,
  /// On 409 (name taken), delete the same-name runner and retry once.
  pub replace: bool,
  /// Per-request timeout, applied inside [`send_generate`],
  /// [`find_runner_id_by_name`], and [`delete_runner`] — NOT at the `client`
  /// level (`replace_existing` only threads it through to the latter two).
  /// A mint can issue up to three requests (generate → replace → generate);
  /// an explicit per-request timeout holds regardless of whatever default
  /// the caller's shared `client` carries (the always-online loop reuses one
  /// client across the listener and every re-mint).
  pub timeout: Duration,
  /// Add a hint to a 404 for a one-segment (organization) URL that it may be
  /// a mistyped repository URL — for a first `register`, not for a re-mint of
  /// an existing org registration.
  pub hint_org_404: bool,
}

/// POST the built `generate-jitconfig` request; return status + body.
/// `timeout` bounds this one request — see [`RegisterParams::timeout`].
async fn send_generate(
  client: &reqwest::Client,
  request: &RegisterRequest,
  token: &str,
  timeout: Duration,
) -> Result<(reqwest::StatusCode, String), RunnerError> {
  let response = client
    .post(&request.url)
    .timeout(timeout)
    .bearer_auth(token)
    .header("Accept", "application/vnd.github+json")
    .header("X-GitHub-Api-Version", "2022-11-28")
    // GitHub's REST API rejects requests without a User-Agent (403).
    .header(
      "User-Agent",
      concat!("toolu-runner/", env!("CARGO_PKG_VERSION")),
    )
    .json(&request.body)
    .send()
    .await
    .map_err(|e| RunnerError::Network(format!("generate-jitconfig request failed: {e}")))?;

  let status = response.status();
  let text = response
    .text()
    .await
    .map_err(|e| RunnerError::Network(format!("reading generate-jitconfig body failed: {e}")))?;
  Ok((status, text))
}

/// Look up the id of the runner registered under `name`, if any.
///
/// Uses the `?name=` filter on the list-runners endpoint and still
/// re-checks the echoed name for an exact match.
async fn find_runner_id_by_name(
  client: &reqwest::Client,
  runners_base: &str,
  token: &str,
  name: &str,
  timeout: Duration,
) -> Result<Option<i64>, RunnerError> {
  let response = client
    .get(runners_base)
    .timeout(timeout)
    .query(&[("name", name)])
    .bearer_auth(token)
    .header("Accept", "application/vnd.github+json")
    .header("X-GitHub-Api-Version", "2022-11-28")
    .header(
      "User-Agent",
      concat!("toolu-runner/", env!("CARGO_PKG_VERSION")),
    )
    .send()
    .await
    .map_err(|e| RunnerError::Network(format!("list-runners request failed: {e}")))?;
  let status = response.status();
  let body: serde_json::Value = response
    .json()
    .await
    .map_err(|e| RunnerError::Network(format!("reading list-runners body failed: {e}")))?;
  if !status.is_success() {
    return Err(RunnerError::Auth(format!(
      "list-runners failed with status {status}: {body}"
    )));
  }
  Ok(
    body
      .get("runners")
      .and_then(serde_json::Value::as_array)
      .into_iter()
      .flatten()
      .find(|r| r.get("name").and_then(serde_json::Value::as_str) == Some(name))
      .and_then(|r| r.get("id"))
      .and_then(serde_json::Value::as_i64),
  )
}

/// One runner from the list-runners reply, as `setup` verifies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerSummary {
  /// The runner id GitHub assigned.
  pub id: i64,
  /// GitHub's status string (`online` / `offline`).
  pub status: String,
  /// Whether the runner is currently running a job.
  pub busy: bool,
  /// The runner's label names (GitHub sends `[{id, name, type}]`).
  pub labels: Vec<String>,
}

/// The runner registered under `name` for `url`'s repo/org, if any:
/// the list-runners endpoint with the `?name=` filter, paged to
/// exhaustion (`per_page=100`), still re-checking the echoed name.
///
/// # Errors
///
/// `RunnerError::Network` for transport / 429 / 5xx answers,
/// `RunnerError::Auth` for other non-2xx, `RunnerError::Protocol`
/// for an unparseable reply, `RunnerError::Config` for a bad `url`.
pub async fn find_runner(
  client: &reqwest::Client,
  url: &str,
  token: &str,
  name: &str,
  timeout: Duration,
) -> Result<Option<RunnerSummary>, RunnerError> {
  let base = resolve_runners_base(url)?;
  let mut page = 1_u32;
  loop {
    let response = client
      .get(&base)
      .timeout(timeout)
      .query(&[
        ("name", name.to_owned()),
        ("per_page", "100".to_owned()),
        ("page", page.to_string()),
      ])
      .bearer_auth(token)
      .header("Accept", "application/vnd.github+json")
      .header("X-GitHub-Api-Version", "2022-11-28")
      .header(
        "User-Agent",
        concat!("toolu-runner/", env!("CARGO_PKG_VERSION")),
      )
      .send()
      .await
      .map_err(|e| RunnerError::Network(format!("list-runners request failed: {e}")))?;
    // Status first: a non-JSON error body must not become a
    // (retryable) Network error and hide an auth refusal.
    let status = response.status();
    let text = response
      .text()
      .await
      .map_err(|e| RunnerError::Network(format!("reading list-runners body failed: {e}")))?;
    if status.as_u16() == 429 || status.is_server_error() {
      return Err(RunnerError::Network(format!(
        "list-runners failed with status {status}: {text}"
      )));
    }
    if !status.is_success() {
      return Err(RunnerError::Auth(format!(
        "list-runners failed with status {status}: {text}"
      )));
    }
    let body: serde_json::Value = serde_json::from_str(&text)
      .map_err(|e| RunnerError::Protocol(format!("list-runners reply did not parse: {e}")))?;
    let runners = body
      .get("runners")
      .and_then(serde_json::Value::as_array)
      .cloned()
      .ok_or_else(|| {
        RunnerError::Protocol("list-runners reply missing 'runners' array".to_owned())
      })?;
    let count = runners.len();
    for runner in &runners {
      if runner.get("name").and_then(serde_json::Value::as_str) == Some(name) {
        let labels = runner
          .get("labels")
          .and_then(serde_json::Value::as_array)
          .map(|labels| {
            labels
              .iter()
              .filter_map(|label| {
                label
                  .get("name")
                  .and_then(serde_json::Value::as_str)
                  .map(str::to_owned)
              })
              .collect()
          })
          .unwrap_or_default();
        return Ok(Some(RunnerSummary {
          id: runner
            .get("id")
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| {
              RunnerError::Protocol("list-runners reply missing runner id".to_owned())
            })?,
          status: runner
            .get("status")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
              RunnerError::Protocol("list-runners reply missing runner status".to_owned())
            })?
            .to_owned(),
          busy: runner
            .get("busy")
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| {
              RunnerError::Protocol("list-runners reply missing runner busy flag".to_owned())
            })?,
          labels,
        }));
      }
    }
    if count < 100 {
      return Ok(None);
    }
    page = page.saturating_add(1);
  }
}

/// Outcome of a DELETE against a specific runner id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeleteOutcome {
  /// GitHub accepted the delete.
  Deleted,
  /// GitHub answered 404. AMBIGUOUS on its own: the runner may be gone, or
  /// the token may simply not be allowed to see it — GitHub hides
  /// inaccessible resources behind 404 rather than 403. Callers that treat
  /// this as success must confirm absence some other way.
  NotFound,
}

/// DELETE the runner with `id` from the repo or org.
///
/// A 404 is returned as [`DeleteOutcome::NotFound`] rather than folded into
/// either success or failure, because the caller's correct reading differs
/// and neither can be inferred here.
async fn delete_runner(
  client: &reqwest::Client,
  runners_base: &str,
  token: &str,
  id: i64,
  timeout: Duration,
) -> Result<DeleteOutcome, RunnerError> {
  let response = client
    .delete(format!("{runners_base}/{id}"))
    .timeout(timeout)
    .bearer_auth(token)
    .header("Accept", "application/vnd.github+json")
    .header("X-GitHub-Api-Version", "2022-11-28")
    .header(
      "User-Agent",
      concat!("toolu-runner/", env!("CARGO_PKG_VERSION")),
    )
    .send()
    .await
    .map_err(|e| RunnerError::Network(format!("delete-runner request failed: {e}")))?;
  let status = response.status();
  if status == reqwest::StatusCode::NOT_FOUND {
    return Ok(DeleteOutcome::NotFound);
  }
  if !status.is_success() {
    // Not `unwrap_or_default()`: an empty body and an unreadable body would
    // then look identical, and this text IS the whole diagnostic a human
    // gets for a failed delete.
    let text = response
      .text()
      .await
      .unwrap_or_else(|e| format!("<error body unreadable: {e}>"));
    return Err(RunnerError::Auth(format!(
      "delete-runner {id} failed with status {status}: {text}"
    )));
  }
  Ok(DeleteOutcome::Deleted)
}

/// Unregister a runner from its repo or org: `DELETE …/actions/runners/{id}`.
///
/// Idempotent: an already-absent runner — 404, or no same-name match — is
/// `Ok(())`, so a repeated `remove` is safe. `runner_id` comes from the
/// persisted registration; a non-positive one means none was recorded, so the
/// runner is looked up by `name` instead.
///
/// # Errors
///
/// `RunnerError::Config` (URL lacks a host or an owner/org path),
/// `RunnerError::Network` (transport), `RunnerError::Auth` (non-2xx other
/// than 404, GitHub's body included).
pub async fn unregister_runner(
  client: &reqwest::Client,
  url: &str,
  token: &str,
  runner_id: i64,
  name: &str,
  timeout: Duration,
) -> Result<(), RunnerError> {
  let runners_base = resolve_runners_base(url)?;
  let id = if runner_id > 0 {
    runner_id
  } else {
    let Some(found) = find_runner_id_by_name(client, &runners_base, token, name, timeout).await?
    else {
      tracing::info!(
        name,
        "no runner registered under this name — nothing to unregister"
      );
      return Ok(());
    };
    found
  };
  match delete_runner(client, &runners_base, token, id, timeout).await? {
    DeleteOutcome::Deleted => Ok(()),
    // 404 alone cannot distinguish "already gone" from "this token may not
    // see this runner". Confirm with a lookup the token must be able to
    // perform: if that succeeds and finds nothing, the runner really is
    // absent; if it fails, propagate rather than report a false success —
    // the caller deletes the only local handle on the strength of this.
    DeleteOutcome::NotFound => {
      match find_runner_id_by_name(client, &runners_base, token, name, timeout).await? {
        None => {
          tracing::debug!(runner_id = id, "runner absent, confirmed by lookup");
          Ok(())
        },
        Some(other) => Err(RunnerError::Auth(format!(
          "delete of runner {id} returned 404, but a runner named '{name}' still exists (id \
         {other}) — refusing to report it unregistered"
        ))),
      }
    },
  }
}

/// Delete the same-name runner blocking a 409, so the retry can mint.
///
/// # Errors
///
/// Surfaces the lookup / delete error, or `RunnerError::Auth` when no
/// same-name runner exists (the 409 must then have another cause).
async fn replace_existing(
  client: &reqwest::Client,
  params: &RegisterParams<'_>,
) -> Result<(), RunnerError> {
  let runners_base = resolve_runners_base(params.url)?;
  let id = find_runner_id_by_name(
    client,
    &runners_base,
    params.runner_token,
    params.name,
    params.timeout,
  )
  .await?
  .ok_or_else(|| {
    RunnerError::Auth(format!(
      "generate-jitconfig returned 409 but no runner named '{}' was found to replace",
      params.name
    ))
  })?;
  tracing::info!(
    runner_id = id,
    name = params.name,
    "replacing existing runner registration"
  );
  match delete_runner(
    client,
    &runners_base,
    params.runner_token,
    id,
    params.timeout,
  )
  .await?
  {
    DeleteOutcome::Deleted => Ok(()),
    // The id came from a lookup moments ago, so a 404 is a real anomaly —
    // often a token that cannot administer runners. Surface it verbatim.
    DeleteOutcome::NotFound => Err(RunnerError::Auth(format!(
      "delete-runner {id} failed with status 404 Not Found: the runner vanished between the \
       lookup and the delete, or this token may not administer runners on this repo"
    ))),
  }
}

/// Mint a JIT runner config via `POST …/generate-jitconfig`.
///
/// All-or-nothing: any failure surfaces GitHub's response body as an
/// `Err` and the caller writes no partial config. With `replace` set,
/// a 409 (runner name taken) deletes the existing same-name runner and
/// retries the mint once.
///
/// # Errors
///
/// `RunnerError::Config` (bad URL), `RunnerError::Network` (transport),
/// `RunnerError::Auth` (non-2xx, body included), `RunnerError::Protocol`
/// (malformed success body).
pub async fn register_jit(
  client: &reqwest::Client,
  params: &RegisterParams<'_>,
) -> Result<JitRegistration, RunnerError> {
  let request = build_request(
    params.url,
    params.name,
    params.labels,
    params.runner_group_id,
    params.work_folder,
  )?;

  let (status, text) = send_generate(client, &request, params.runner_token, params.timeout).await?;
  if status.is_success() {
    return parse_response(&text, params.name);
  }

  if status == reqwest::StatusCode::CONFLICT && params.replace {
    replace_existing(client, params).await?;
    let (status, text) =
      send_generate(client, &request, params.runner_token, params.timeout).await?;
    if status.is_success() {
      return parse_response(&text, params.name);
    }
    return Err(RunnerError::Auth(format!(
      "generate-jitconfig failed after --replace retry with status {status}: {text}"
    )));
  }

  let failure = mint_failure(status, &text);
  if params.hint_org_404 && status == reqwest::StatusCode::NOT_FOUND {
    return Err(org_hint(failure, params.url));
  }
  Err(failure)
}

/// A 404 for a one-segment URL may be a mistyped repository URL read as an
/// organization (#201): say so, since the URL decides the endpoint.
fn org_hint(failure: RunnerError, url: &str) -> RunnerError {
  let Ok((_, RunnerScope::Org { org })) = resolve_target(url) else {
    return failure;
  };
  if let RunnerError::Auth(msg) = failure {
    return RunnerError::Auth(format!(
      "{msg}\n'{org}' was read as an organization because the URL has one path segment; for a \
       repository runner pass --url https://<host>/<owner>/<repo>"
    ));
  }
  failure
}

/// Map a non-2xx `generate-jitconfig` status to the loop-critical error
/// kind: the run loop treats `Auth` as fatal and `Network` as transient
/// (backoff). 401 gets its own login-specific message; 429 and 5xx are the
/// only genuinely transient statuses; every remaining non-2xx is permanent
/// and must be fatal — mapping it to `Network` would make the loop retry a
/// hopeless mint forever. That includes 403 (a valid bearer with
/// insufficient scope): it lands in `Auth` even though the bearer itself
/// is fine, because only a new token can fix it.
fn mint_failure(status: reqwest::StatusCode, text: &str) -> RunnerError {
  if status == reqwest::StatusCode::UNAUTHORIZED {
    return RunnerError::Auth(
      "GitHub rejected the bearer as invalid or expired (401) — for a personal token, run \
       'toolu-runner login'"
        .into(),
    );
  }
  if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
    return RunnerError::Network(format!(
      "generate-jitconfig failed with status {status} (transient — the run loop will back \
       off and retry): {text}"
    ));
  }
  RunnerError::Auth(format!(
    "generate-jitconfig failed with status {status} (permanent — retrying cannot succeed; \
     check the registration URL and runner parameters, or, for a personal token, re-run \
     'toolu-runner login' if the token lacks access to this repository or organization): {text}"
  ))
}

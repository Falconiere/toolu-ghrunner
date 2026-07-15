//! Live JIT registration: the github.com `generate-jitconfig` call.
//!
//! POSTs the short-lived registration token to GitHub's
//! `…/actions/runners/generate-jitconfig`, which returns the runner `id`
//! plus an `encoded_jit_config` (the base64 3-blob envelope that
//! [`protocol::JitConfig`] parses at run time). The RSA → JWT → OAuth2
//! exchange happens at run time from that config, not here.
//!
//! Split for token-free testing: [`build_request`] / [`parse_response`]
//! are pure; [`register_jit`] is the async send that glues them.

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

/// Build the repo-scoped `generate-jitconfig` request from inputs.
///
/// Parses `owner/repo` from `url`; targets `api.github.com` for
/// github.com and `<host>/api/v3` for GHES. `runner_group_id` defaults
/// to `1` (the `Default` group) when `None`.
///
/// # Errors
///
/// `RunnerError::Config` when `url` is not a `<host>/<owner>/<repo>` URL.
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

/// Resolve the repo-scoped `generate-jitconfig` endpoint from a repo URL.
///
/// # Errors
///
/// `RunnerError::Config` when `url` lacks a host or `owner/repo` path.
fn resolve_endpoint(url: &str) -> Result<String, RunnerError> {
  Ok(format!("{}/generate-jitconfig", resolve_runners_base(url)?))
}

/// Resolve the repo-scoped `…/actions/runners` API base from a repo URL.
///
/// github.com routes through `api.github.com`; other hosts keep the input
/// scheme + authority and add `/api/v3`. A trailing `.git` is stripped.
///
/// # Errors
///
/// `RunnerError::Config` when `url` lacks a host or `owner/repo` path.
fn resolve_runners_base(url: &str) -> Result<String, RunnerError> {
  let parsed =
    url::Url::parse(url).map_err(|e| RunnerError::Config(format!("invalid --url: {e}")))?;
  let host = parsed
    .host_str()
    .ok_or_else(|| RunnerError::Config("URL missing host".to_owned()))?;

  let mut segments = parsed
    .path_segments()
    .ok_or_else(|| RunnerError::Config(format!("URL '{url}' has no path — expected owner/repo")))?
    .filter(|s| !s.is_empty());
  let owner = segments
    .next()
    .ok_or_else(|| RunnerError::Config(format!("URL '{url}' missing owner segment")))?;
  let repo_raw = segments
    .next()
    .ok_or_else(|| RunnerError::Config(format!("URL '{url}' missing repo segment")))?;
  let repo = repo_raw.strip_suffix(".git").unwrap_or(repo_raw);

  let api_base = if host.eq_ignore_ascii_case("github.com") {
    "https://api.github.com".to_owned()
  } else {
    let authority = match parsed.port() {
      Some(port) => format!("{host}:{port}"),
      None => host.to_owned(),
    };
    format!("{}://{authority}/api/v3", parsed.scheme())
  };
  Ok(format!("{api_base}/repos/{owner}/{repo}/actions/runners"))
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
  /// Repo URL (`https://<host>/<owner>/<repo>`).
  pub url: &'a str,
  /// API token with `administration:write` on the repo (PAT or App
  /// installation token), sent as `Authorization: Bearer …`. NOT the
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
}

/// POST the built `generate-jitconfig` request; return status + body.
async fn send_generate(
  client: &reqwest::Client,
  request: &RegisterRequest,
  token: &str,
) -> Result<(reqwest::StatusCode, String), RunnerError> {
  let response = client
    .post(&request.url)
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
) -> Result<Option<i64>, RunnerError> {
  let response = client
    .get(runners_base)
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

/// DELETE the runner with `id` from the repo.
async fn delete_runner(
  client: &reqwest::Client,
  runners_base: &str,
  token: &str,
  id: i64,
) -> Result<(), RunnerError> {
  let response = client
    .delete(format!("{runners_base}/{id}"))
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
  if !status.is_success() {
    let text = response.text().await.unwrap_or_default();
    return Err(RunnerError::Auth(format!(
      "delete-runner {id} failed with status {status}: {text}"
    )));
  }
  Ok(())
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
  let id = find_runner_id_by_name(client, &runners_base, params.runner_token, params.name)
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
  delete_runner(client, &runners_base, params.runner_token, id).await
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

  let (status, text) = send_generate(client, &request, params.runner_token).await?;
  if status.is_success() {
    return parse_response(&text, params.name);
  }

  if status == reqwest::StatusCode::CONFLICT && params.replace {
    replace_existing(client, params).await?;
    let (status, text) = send_generate(client, &request, params.runner_token).await?;
    if status.is_success() {
      return parse_response(&text, params.name);
    }
    return Err(RunnerError::Auth(format!(
      "generate-jitconfig failed after --replace retry with status {status}: {text}"
    )));
  }

  Err(mint_failure(status, &text))
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
      "stored GitHub token invalid or expired — run 'toolu-runner login'".into(),
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
     check the registration URL and runner parameters, or re-run 'toolu-runner login' if \
     the token lacks access to this repository): {text}"
  ))
}

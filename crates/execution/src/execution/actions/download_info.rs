//! Action download service location and request data from an acquired job.

use base64::Engine;
use serde_json::{Value, json};
use shared::{AgentJobRequestMessage, RunnerError, SecretMasker};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

use super::download_info_v1::{LegacyContext, legacy_info};
use super::resolver::ActionRef;

/// Per-job location of the server API that resolves action download details.
#[derive(Clone)]
pub struct ActionDownloadContext {
  launch_url: Option<String>,
  api_url: String,
  service_token: Option<String>,
  job_token: Option<String>,
  legacy: Option<LegacyContext>,
}

/// Validated archive metadata for one resolved remote action revision.
pub struct ActionDownloadInfo {
  /// Server-selected archive URL.
  pub tarball_url: String,
  /// Server-resolved commit revision.
  pub resolved_sha: String,
  /// Scoped archive credential, when the server supplied one.
  pub token: Option<String>,
  /// Cache identity qualified by API host and resolved revision.
  pub cache_key: String,
}

impl ActionDownloadContext {
  /// Read the action resolver location from a real acquired job message.
  ///
  /// # Errors
  ///
  /// Returns `ActionResolution` when a reported location is malformed.
  pub fn from_message(msg: &AgentJobRequestMessage) -> Result<Self, RunnerError> {
    let api_url = api_url_from_message(msg)?;
    let launch_url = launch_url_from_message(msg)?;
    Ok(Self {
      launch_url,
      api_url,
      legacy: enterprise_job(msg)
        .then(|| LegacyContext::from_message(msg))
        .flatten(),
      service_token: msg
        .resources
        .endpoints
        .iter()
        .find(|endpoint| endpoint.name == "SystemVssConnection")
        .and_then(|endpoint| endpoint.authorization.as_ref())
        .and_then(|auth| auth.parameters.get("AccessToken"))
        .filter(|value| !value.is_empty())
        .cloned(),
      job_token: msg
        .variables
        .get("system.github.token")
        .map(|variable| variable.value.as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_owned),
    })
  }

  /// Launch API URL, if this acquired job advertises one.
  pub fn launch_url(&self) -> Option<String> {
    self.launch_url.clone()
  }

  /// REST API base selected from the acquired `github` context.
  pub fn api_url(&self) -> &str {
    &self.api_url
  }

  /// JSON request body sent to the Launch resolver for remote action refs.
  pub fn launch_request(&self, refs: &[ActionRef]) -> Value {
    let actions: Vec<Value> = refs
      .iter()
      .map(|action| match &action.subpath {
        Some(path) => json!({
          "action": format!("{}/{}", action.owner, action.repo),
          "version": action.git_ref,
          "path": path
        }),
        None => json!({
          "action": format!("{}/{}", action.owner, action.repo),
          "version": action.git_ref
        }),
      })
      .collect();
    json!({ "actions": actions })
  }

  /// Resolve the action through the acquired Run Service launch endpoint.
  ///
  /// # Errors
  ///
  /// Returns `ActionDownload` on authorization, transport, status, or decode
  /// failure. Response bodies and credentials are omitted from errors.
  pub async fn resolve(
    &self,
    client: &reqwest::Client,
    action: &ActionRef,
    masker: &Arc<Mutex<SecretMasker>>,
    cancel: &CancellationToken,
  ) -> Result<Value, RunnerError> {
    if self.launch_url.is_none() {
      if let Some(legacy) = &self.legacy {
        let token = self.service_token.as_deref().ok_or_else(|| {
          RunnerError::ActionDownload("legacy action service credential unavailable".to_owned())
        })?;
        match masker.lock() {
          Ok(mut guard) => guard.add_secret(token),
          Err(poisoned) => poisoned.into_inner().add_secret(token),
        }
        if let Some(body) = legacy.resolve(client, action, token, cancel).await? {
          return Ok(body);
        }
      }
      return self.resolve_fallback(client, action, masker, cancel).await;
    }
    self.resolve_launch(client, action, masker, cancel).await
  }

  /// Resolve and validate the archive location for a remote action.
  ///
  /// # Errors
  ///
  /// Fails on missing or malformed server metadata without disclosing tokens.
  pub async fn resolve_info(
    &self,
    client: &reqwest::Client,
    action: &ActionRef,
    masker: &Arc<Mutex<SecretMasker>>,
    cancel: &CancellationToken,
  ) -> Result<ActionDownloadInfo, RunnerError> {
    let body = self.resolve(client, action, masker, cancel).await?;
    let (sha, url, token) = if body.get("Actions").is_some() {
      legacy_info(&body, action, self.job_token.as_deref())?
    } else if self.launch_url.is_some() {
      launch_info(&body, action, self.job_token.as_deref())?
    } else {
      fallback_info(&body, action, &self.api_url, self.job_token.as_deref())?
    };
    if !valid_sha(&sha) {
      return Err(RunnerError::ActionDownload(
        "action resolver returned invalid revision".to_owned(),
      ));
    }
    validate_archive_url(&url)?;
    if let Some(value) = token.as_deref() {
      let encoded =
        base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{value}"));
      match masker.lock() {
        Ok(mut guard) => guard.add_secrets([value, encoded.as_str()]),
        Err(poisoned) => poisoned.into_inner().add_secrets([value, encoded.as_str()]),
      }
    }
    let host = blake3::hash(self.api_url.as_bytes()).to_hex();
    let cache_key = format!("{}/{}/{}/{}", &host[..16], action.owner, action.repo, sha);
    Ok(ActionDownloadInfo {
      tarball_url: url,
      resolved_sha: sha,
      token,
      cache_key,
    })
  }
}

impl ActionDownloadContext {
  async fn resolve_launch(
    &self,
    client: &reqwest::Client,
    action: &ActionRef,
    masker: &Arc<Mutex<SecretMasker>>,
    cancel: &CancellationToken,
  ) -> Result<Value, RunnerError> {
    let url = self.launch_url.as_deref().ok_or_else(|| {
      RunnerError::ActionDownload("action download-info endpoint unavailable".to_owned())
    })?;
    let token = self.service_token.as_deref().ok_or_else(|| {
      RunnerError::ActionDownload("action download-info credential unavailable".to_owned())
    })?;
    match masker.lock() {
      Ok(mut guard) => guard.add_secret(token),
      Err(poisoned) => poisoned.into_inner().add_secret(token),
    }
    let request = client
      .post(url)
      .bearer_auth(token)
      .json(&self.launch_request(std::slice::from_ref(action)));
    let response = tokio::select! {
      () = cancel.cancelled() => return Err(RunnerError::ActionDownload("action resolution cancelled".to_owned())),
      result = request.send() => result.map_err(|error| {
        RunnerError::ActionDownload(format!("action download-info request failed: timeout={}", error.is_timeout()))
      })?,
    };
    if !response.status().is_success() {
      return Err(RunnerError::ActionDownload(format!(
        "action download-info status {}",
        response.status()
      )));
    }
    tokio::select! {
      () = cancel.cancelled() => Err(RunnerError::ActionDownload("action resolution cancelled".to_owned())),
      result = response.json::<Value>() => result.map_err(|_error| {
        RunnerError::ActionDownload("action download-info JSON invalid".to_owned())
      }),
    }
  }

  async fn resolve_fallback(
    &self,
    client: &reqwest::Client,
    action: &ActionRef,
    masker: &Arc<Mutex<SecretMasker>>,
    cancel: &CancellationToken,
  ) -> Result<Value, RunnerError> {
    let token = self.job_token.as_deref().ok_or_else(|| {
      RunnerError::ActionDownload("action fallback credential unavailable".to_owned())
    })?;
    match masker.lock() {
      Ok(mut guard) => guard.add_secret(token),
      Err(poisoned) => poisoned.into_inner().add_secret(token),
    }
    let mut url = reqwest::Url::parse(&self.api_url).map_err(|_error| {
      RunnerError::ActionDownload("action fallback API URL invalid".to_owned())
    })?;
    url
      .path_segments_mut()
      .map_err(|()| RunnerError::ActionDownload("action fallback API URL invalid".to_owned()))?
      .extend([
        "repos",
        &action.owner,
        &action.repo,
        "commits",
        &action.git_ref,
      ]);
    let request = client
      .get(url)
      .bearer_auth(token)
      .header(reqwest::header::USER_AGENT, "toolu-runner");
    let response = tokio::select! {
      () = cancel.cancelled() => return Err(RunnerError::ActionDownload("action resolution cancelled".to_owned())),
      result = request.send() => result.map_err(|error| {
        RunnerError::ActionDownload(format!("action fallback request failed: timeout={}", error.is_timeout()))
      })?,
    };
    if !response.status().is_success() {
      return Err(RunnerError::ActionDownload(format!(
        "action fallback status {}",
        response.status()
      )));
    }
    tokio::select! {
      () = cancel.cancelled() => Err(RunnerError::ActionDownload("action resolution cancelled".to_owned())),
      result = response.json::<Value>() => result.map_err(|_error| {
        RunnerError::ActionDownload("action fallback revision JSON invalid".to_owned())
      }),
    }
  }
}

fn enterprise_job(msg: &AgentJobRequestMessage) -> bool {
  github_value(msg, "server_url")
    .and_then(|server| reqwest::Url::parse(server).ok())
    .and_then(|url| url.host_str().map(str::to_owned))
    .is_some_and(|host| !host.eq_ignore_ascii_case("github.com"))
}

fn launch_info(
  body: &Value,
  action: &ActionRef,
  job_token: Option<&str>,
) -> Result<(String, String, Option<String>), RunnerError> {
  let key = format!("{}/{}@{}", action.owner, action.repo, action.git_ref);
  let entry = body
    .get("actions")
    .and_then(|actions| actions.get(&key))
    .ok_or_else(|| {
      RunnerError::ActionDownload("action resolver omitted requested action".to_owned())
    })?;
  let resolved = entry
    .get("resolved_name")
    .and_then(Value::as_str)
    .ok_or_else(|| RunnerError::ActionDownload("action resolver omitted repository".to_owned()))?;
  if resolved != format!("{}/{}", action.owner, action.repo) {
    return Err(RunnerError::ActionDownload(
      "action resolver changed repository".to_owned(),
    ));
  }
  let sha = required_string(entry, "resolved_sha")?;
  let url = required_string(entry, "tar_url")?;
  let token = entry
    .get("authentication")
    .and_then(|auth| auth.get("token"))
    .and_then(Value::as_str)
    .filter(|value| !value.is_empty())
    .or(job_token)
    .map(str::to_owned);
  Ok((sha, url, token))
}

fn fallback_info(
  body: &Value,
  action: &ActionRef,
  api_url: &str,
  job_token: Option<&str>,
) -> Result<(String, String, Option<String>), RunnerError> {
  let sha = required_string(body, "sha")?;
  let mut url = reqwest::Url::parse(api_url)
    .map_err(|_error| RunnerError::ActionDownload("action fallback API URL invalid".to_owned()))?;
  url
    .path_segments_mut()
    .map_err(|()| RunnerError::ActionDownload("action fallback API URL invalid".to_owned()))?
    .extend(["repos", &action.owner, &action.repo, "tarball", &sha]);
  Ok((sha, url.to_string(), job_token.map(str::to_owned)))
}

fn required_string(entry: &Value, key: &str) -> Result<String, RunnerError> {
  entry
    .get(key)
    .and_then(Value::as_str)
    .filter(|value| !value.is_empty())
    .map(str::to_owned)
    .ok_or_else(|| RunnerError::ActionDownload(format!("action resolver omitted {key}")))
}

fn valid_sha(sha: &str) -> bool {
  matches!(sha.len(), 40 | 64) && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_archive_url(raw: &str) -> Result<(), RunnerError> {
  let url = reqwest::Url::parse(raw)
    .map_err(|_error| RunnerError::ActionDownload("action archive URL invalid".to_owned()))?;
  let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
  if (url.scheme() != "https" && !(url.scheme() == "http" && loopback))
    || url.host_str().is_none()
    || !url.username().is_empty()
    || url.password().is_some()
  {
    return Err(RunnerError::ActionDownload(
      "action archive URL invalid".to_owned(),
    ));
  }
  Ok(())
}

fn github_value<'a>(msg: &'a AgentJobRequestMessage, name: &str) -> Option<&'a str> {
  msg
    .context_data
    .get("github")
    .and_then(|value| value.d.as_ref())
    .and_then(|entries| {
      entries
        .iter()
        .find(|entry| entry.key.s.as_deref() == Some(name))
    })
    .and_then(|entry| entry.value.s.as_deref())
    .filter(|value| !value.is_empty())
}

fn api_url_from_message(msg: &AgentJobRequestMessage) -> Result<String, RunnerError> {
  let api_url = if let Some(url) = github_value(msg, "api_url") {
    url.to_owned()
  } else {
    let server = github_value(msg, "server_url").ok_or_else(|| {
      RunnerError::ActionResolution("acquired job has no GitHub API host".to_owned())
    })?;
    if server.trim_end_matches('/') == "https://github.com" {
      "https://api.github.com".to_owned()
    } else {
      format!("{}/api/v3", server.trim_end_matches('/'))
    }
  };
  validate_api_url(&api_url)?;
  Ok(api_url)
}

fn launch_url_from_message(msg: &AgentJobRequestMessage) -> Result<Option<String>, RunnerError> {
  msg
    .variables
    .get("system.github.launch_endpoint")
    .map(|variable| variable.value.trim())
    .filter(|value| !value.is_empty())
    .map(|base| {
      let url = reqwest::Url::parse(base).map_err(|e| {
        RunnerError::ActionResolution(format!("invalid launch endpoint in acquired job: {e}"))
      })?;
      let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
      if (url.scheme() != "https" && !(url.scheme() == "http" && loopback))
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
      {
        return Err(RunnerError::ActionResolution(
          "invalid launch endpoint in acquired job".to_owned(),
        ));
      }
      let plan = uuid::Uuid::parse_str(&msg.plan.plan_id).map_err(|e| {
        RunnerError::ActionResolution(format!("invalid plan id in acquired job: {e}"))
      })?;
      let job = uuid::Uuid::parse_str(&msg.job_id).map_err(|e| {
        RunnerError::ActionResolution(format!("invalid job id in acquired job: {e}"))
      })?;
      Ok(format!(
        "{}/actions/build/{plan}/jobs/{job}/runnerresolve/actions",
        url.origin().ascii_serialization()
      ))
    })
    .transpose()
}

/// Validate the host before using it for a credentialed REST request.
fn validate_api_url(raw: &str) -> Result<(), RunnerError> {
  let url = reqwest::Url::parse(raw).map_err(|e| {
    RunnerError::ActionResolution(format!("invalid GitHub API URL in acquired job: {e}"))
  })?;
  let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
  if (url.scheme() != "https" && !(url.scheme() == "http" && loopback))
    || url.host_str().is_none()
    || !url.username().is_empty()
    || url.password().is_some()
    || url.query().is_some()
    || url.fragment().is_some()
  {
    return Err(RunnerError::ActionResolution(
      "invalid GitHub API URL in acquired job".to_owned(),
    ));
  }
  Ok(())
}

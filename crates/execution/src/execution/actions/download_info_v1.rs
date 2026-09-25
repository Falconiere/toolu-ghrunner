//! Legacy GHES action download-info discovery and response decoding.
//!
//! Older servers advertise the download-info resource through connection
//! data. This module keeps the discovery URL on the acquired service origin
//! and decodes its `Actions` reply for the requested repository.

use reqwest::{Client, Url};
use serde_json::{Value, json};
use shared::{AgentJobRequestMessage, RunnerError};
use tokio_util::sync::CancellationToken;

use super::resolver::ActionRef;

// Resource identifier matched against the GHES connectionData advertisement.
const RESOURCE_ID: &str = "27d7f831-88c1-4719-8ca1-6a061dad90eb";

/// Acquired legacy job fields needed to resolve the server's V1 resource.
#[derive(Clone)]
pub(super) struct LegacyContext {
  base_url: String,
  scope: String,
  hub: String,
  plan: String,
  job: String,
}

impl LegacyContext {
  /// Retain only the service endpoint and plan identifiers from the job.
  pub(super) fn from_message(msg: &AgentJobRequestMessage) -> Option<Self> {
    let base_url = msg
      .resources
      .endpoints
      .iter()
      .find(|endpoint| endpoint.name == "SystemVssConnection")?
      .url
      .as_ref()?
      .clone();
    let scope = msg.plan.scope_identifier.clone()?;
    let hub = msg.plan.plan_type.clone()?;
    Some(Self {
      base_url,
      scope,
      hub,
      plan: msg.plan.plan_id.clone(),
      job: msg.job_id.clone(),
    })
  }

  /// Discover the V1 resource and POST one action. `None` means the resource
  /// was not advertised by this older server.
  pub(super) async fn resolve(
    &self,
    client: &Client,
    action: &ActionRef,
    token: &str,
    cancel: &CancellationToken,
  ) -> Result<Option<Value>, RunnerError> {
    let discovered = self.discover(client, token, cancel).await?;
    let Some(url) = discovered else {
      return Ok(None);
    };
    let body = json!({"Actions":[{
      "NameWithOwner":format!("{}/{}", action.owner, action.repo),
      "Ref":action.git_ref,
      "Path":action.subpath,
    }]});
    let request = client.post(url).bearer_auth(token).json(&body);
    let response = tokio::select! {
      () = cancel.cancelled() => return Err(cancelled()),
      result = request.send() => result.map_err(|error| transport_error(&error))?,
    };
    if response.status() == reqwest::StatusCode::NOT_IMPLEMENTED
      || response.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED
    {
      return Ok(None);
    }
    if !response.status().is_success() {
      return Err(RunnerError::ActionDownload(format!(
        "legacy action resolution status {}",
        response.status()
      )));
    }
    tokio::select! {
      () = cancel.cancelled() => Err(cancelled()),
      result = response.json::<Value>() => result.map(Some).map_err(|_error| {
        RunnerError::ActionDownload("legacy action resolution JSON invalid".to_owned())
      }),
    }
  }

  async fn discover(
    &self,
    client: &Client,
    token: &str,
    cancel: &CancellationToken,
  ) -> Result<Option<Url>, RunnerError> {
    let base = Url::parse(&self.base_url)
      .map_err(|_error| RunnerError::ActionDownload("legacy service URL invalid".to_owned()))?;
    let loopback = matches!(base.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if (base.scheme() != "https" && !(base.scheme() == "http" && loopback))
      || base.host_str().is_none()
      || !base.username().is_empty()
      || base.password().is_some()
      || base.query().is_some()
      || base.fragment().is_some()
    {
      return Err(RunnerError::ActionDownload(
        "legacy service URL invalid".to_owned(),
      ));
    }
    let discovery_url = format!(
      "{}/_apis/connectionData",
      self.base_url.trim_end_matches('/')
    );
    let request = client.get(discovery_url).bearer_auth(token);
    let response = tokio::select! {
      () = cancel.cancelled() => return Err(cancelled()),
      result = request.send() => result.map_err(|error| transport_error(&error))?,
    };
    if !response.status().is_success() {
      return Err(RunnerError::ActionDownload(format!(
        "legacy service discovery status {}",
        response.status()
      )));
    }
    let body: Value = response.json().await.map_err(|_error| {
      RunnerError::ActionDownload("legacy service discovery JSON invalid".to_owned())
    })?;
    let service = resource_entry(&body);
    service
      .map(|entry| self.resource_url(entry, &base))
      .transpose()
  }

  fn resource_url(&self, entry: &Value, base: &Url) -> Result<Url, RunnerError> {
    let relative = entry
      .get("relativePath")
      .and_then(Value::as_str)
      .ok_or_else(|| {
        RunnerError::ActionDownload("legacy action resource omitted path".to_owned())
      })?;
    if !valid_relative_resource_path(relative) {
      return Err(RunnerError::ActionDownload(
        "legacy action resource path invalid".to_owned(),
      ));
    }
    let path = relative
      .replace("{scopeIdentifier}", &self.scope)
      .replace("{hubName}", &self.hub)
      .replace("{planId}", &self.plan);
    if path.contains('{') || path.contains('}') {
      return Err(RunnerError::ActionDownload(
        "legacy action resource path unknown".to_owned(),
      ));
    }
    let raw = format!("{}{}", self.base_url.trim_end_matches('/'), path);
    let mut url = Url::parse(&raw).map_err(|_error| {
      RunnerError::ActionDownload("legacy action resource URL invalid".to_owned())
    })?;
    if url.origin() != base.origin() || !url.username().is_empty() || url.password().is_some() {
      return Err(RunnerError::ActionDownload(
        "legacy action resource host invalid".to_owned(),
      ));
    }
    url
      .query_pairs_mut()
      .append_pair("jobId", &self.job)
      .append_pair("api-version", "6.0-preview.1");
    Ok(url)
  }
}

fn valid_relative_resource_path(relative: &str) -> bool {
  let lower = relative.to_ascii_lowercase();
  relative.starts_with('/')
    && !relative.contains("..")
    && !relative.contains("://")
    && !lower.contains("%2e")
    && !lower.contains("%2f")
    && !lower.contains("%5c")
}

fn resource_entry(body: &Value) -> Option<&Value> {
  body
    .pointer("/locationServiceData/serviceDefinitions")
    .and_then(Value::as_array)
    .and_then(|entries| {
      entries.iter().find(|entry| {
        entry
          .get("identifier")
          .and_then(Value::as_str)
          .is_some_and(|value| value.eq_ignore_ascii_case(RESOURCE_ID))
      })
    })
}

/// Decode the V1 `Actions` map for the requested action.
pub(super) fn legacy_info(
  body: &Value,
  action: &ActionRef,
  job_token: Option<&str>,
) -> Result<(String, String, Option<String>), RunnerError> {
  let key = format!("{}/{}@{}", action.owner, action.repo, action.git_ref);
  let entry = body
    .get("Actions")
    .and_then(|actions| actions.get(&key))
    .ok_or_else(|| {
      RunnerError::ActionDownload("legacy resolver omitted requested action".to_owned())
    })?;
  let resolved = entry
    .get("ResolvedNameWithOwner")
    .and_then(Value::as_str)
    .ok_or_else(|| RunnerError::ActionDownload("legacy resolver omitted repository".to_owned()))?;
  if resolved != format!("{}/{}", action.owner, action.repo) {
    return Err(RunnerError::ActionDownload(
      "legacy resolver changed repository".to_owned(),
    ));
  }
  let sha = required(entry, "ResolvedSha")?;
  let url = required(entry, "TarballUrl")?;
  let token = entry
    .get("Authentication")
    .and_then(|auth| auth.get("Token"))
    .and_then(Value::as_str)
    .filter(|value| !value.is_empty())
    .or(job_token)
    .map(str::to_owned);
  Ok((sha, url, token))
}

fn required(value: &Value, key: &str) -> Result<String, RunnerError> {
  value
    .get(key)
    .and_then(Value::as_str)
    .filter(|text| !text.is_empty())
    .map(str::to_owned)
    .ok_or_else(|| RunnerError::ActionDownload(format!("legacy resolver omitted {key}")))
}

fn cancelled() -> RunnerError {
  RunnerError::ActionDownload("action resolution cancelled".to_owned())
}

fn transport_error(error: &reqwest::Error) -> RunnerError {
  RunnerError::ActionDownload(format!(
    "legacy action service request failed: timeout={}",
    error.is_timeout()
  ))
}

#[cfg(test)]
#[path = "tests/download_info_v1.rs"]
mod tests;

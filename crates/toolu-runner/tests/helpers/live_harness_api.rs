//! GitHub REST helpers for [`LiveHarness`] — workflow-file push/delete,
//! dispatch + run polling, and GH-side cancellation. Child module of
//! `live_harness.rs`; split out to keep each helper file within the
//! repo's file-size budget.

use std::time::{Duration, Instant};

use base64::Engine;

use super::LiveHarness;

/// Workflow-file operations against the GitHub contents API.
impl LiveHarness {
  /// GET the blob `sha` of a repo file, or `None` if it doesn't exist.
  async fn fetch_file_sha(&self, client: &reqwest::Client, url: &str) -> Option<String> {
    client
      .get(url)
      .bearer_auth(&self.token)
      .header("Accept", "application/vnd.github+json")
      .send()
      .await
      .ok()?
      .json::<serde_json::Value>()
      .await
      .ok()
      .and_then(|v| {
        v.get("sha")
          .and_then(serde_json::Value::as_str)
          .map(str::to_owned)
      })
  }

  /// PUT a workflow YAML to `.github/workflows/{name}` in the test
  /// repo. If the file already exists (re-run), the existing blob's
  /// `sha` is included in the PUT body so GH treats it as an update
  /// rather than a 422 conflict.
  pub async fn push_workflow(
    &self,
    name: &str,
    content: &str,
  ) -> Result<(), Box<dyn std::error::Error>> {
    let url = format!(
      "{}/repos/{}/contents/.github/workflows/{}",
      self.api_base(),
      self.repo,
      name,
    );
    let client = self.http()?;
    let existing_sha = self.fetch_file_sha(&client, &url).await;

    let encoded = base64::engine::general_purpose::STANDARD.encode(content.as_bytes());
    let mut body = serde_json::json!({
      "message": format!("test: push {name}"),
      "content": encoded,
    });
    if let Some(sha) = existing_sha {
      let map = body.as_object_mut().ok_or("body is not an object")?;
      map.insert("sha".to_owned(), serde_json::Value::String(sha));
    }

    let resp = client
      .put(&url)
      .bearer_auth(&self.token)
      .header("Accept", "application/vnd.github+json")
      .json(&body)
      .send()
      .await?;
    let status = resp.status();
    if !status.is_success() {
      let body: serde_json::Value = resp.json().await?;
      return Err(format!("workflow PUT failed: {status} {body}").into());
    }
    Ok(())
  }

  /// Delete a workflow file from the test repo. Best-effort —
  /// returns `Ok` on 404 (file already gone).
  pub async fn delete_workflow(&self, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let url = format!(
      "{}/repos/{}/contents/.github/workflows/{}",
      self.api_base(),
      self.repo,
      name,
    );
    let client = self.http()?;
    let resp = client
      .get(&url)
      .bearer_auth(&self.token)
      .header("Accept", "application/vnd.github+json")
      .send()
      .await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
      return Ok(());
    }
    let body: serde_json::Value = resp.json().await?;
    let Some(sha) = body
      .get("sha")
      .and_then(serde_json::Value::as_str)
      .map(str::to_owned)
    else {
      return Ok(());
    };
    let resp = client
      .delete(&url)
      .bearer_auth(&self.token)
      .header("Accept", "application/vnd.github+json")
      .json(&serde_json::json!({
        "message": format!("test: delete {name}"),
        "sha": sha,
      }))
      .send()
      .await?;
    let status = resp.status();
    if !status.is_success() {
      let body: serde_json::Value = resp.json().await?;
      return Err(format!("workflow DELETE failed: {status} {body}").into());
    }
    Ok(())
  }
}

/// Run-level operations: dispatch, poll, cancel.
impl LiveHarness {
  /// Poll the workflow's run list until a run id shows up (dispatch
  /// returns 204 with no id; the run appears asynchronously).
  async fn latest_run_id(
    &self,
    client: &reqwest::Client,
    name: &str,
  ) -> Result<u64, Box<dyn std::error::Error>> {
    let list_url = format!(
      "{}/repos/{}/actions/workflows/{}/runs?per_page=1",
      self.api_base(),
      self.repo,
      name,
    );
    for _ in 0..20 {
      tokio::time::sleep(Duration::from_secs(2)).await;
      let runs: serde_json::Value = client
        .get(&list_url)
        .bearer_auth(&self.token)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .json()
        .await?;
      let id = runs
        .get("workflow_runs")
        .and_then(serde_json::Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(|v| v.get("id"))
        .and_then(serde_json::Value::as_u64);
      if let Some(id) = id {
        return Ok(id);
      }
    }
    Err(
      format!(
        "could not find a run for {name} after dispatching on {}",
        self.branch
      )
      .into(),
    )
  }

  /// POST `/repos/{owner}/{repo}/actions/workflows/{name}/dispatches`
  /// to trigger `name` on the default branch. Returns the run id of
  /// the newly created run (GH returns 204 from dispatch; we list
  /// recent runs to pick the latest one).
  ///
  /// A freshly PUT workflow file is not dispatchable until GitHub's
  /// Actions indexer picks it up, and until then the dispatch endpoint
  /// returns 404 — so 404s are retried for up to a minute; any other
  /// non-2xx fails immediately.
  pub async fn trigger_workflow(&self, name: &str) -> Result<u64, Box<dyn std::error::Error>> {
    let dispatch_url = format!(
      "{}/repos/{}/actions/workflows/{}/dispatches",
      self.api_base(),
      self.repo,
      name,
    );
    let client = self.http()?;
    let mut attempts_left = 20u8;
    loop {
      let resp = client
        .post(&dispatch_url)
        .bearer_auth(&self.token)
        .header("Accept", "application/vnd.github+json")
        .json(&serde_json::json!({"ref": self.branch}))
        .send()
        .await?;
      let status = resp.status();
      if status.is_success() {
        return self.latest_run_id(&client, name).await;
      }
      let body: serde_json::Value = resp.json().await.unwrap_or_default();
      attempts_left -= 1;
      if status != reqwest::StatusCode::NOT_FOUND || attempts_left == 0 {
        return Err(format!("workflow dispatch failed: {status} {body}").into());
      }
      tokio::time::sleep(Duration::from_secs(3)).await;
    }
  }

  /// Poll `GET /repos/{owner}/{repo}/actions/runs/{run_id}` until the
  /// run's `status` flips to `completed`. Returns the `conclusion`
  /// field — `success`, `failure`, `cancelled`, or `skipped`.
  pub async fn wait_for_run(
    &self,
    run_id: u64,
    timeout: Duration,
  ) -> Result<String, Box<dyn std::error::Error>> {
    let url = format!(
      "{}/repos/{}/actions/runs/{}",
      self.api_base(),
      self.repo,
      run_id,
    );
    let client = self.http()?;
    let deadline = Instant::now() + timeout;
    loop {
      let run: serde_json::Value = client
        .get(&url)
        .bearer_auth(&self.token)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .json()
        .await?;
      let status = run
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
      if status == "completed" {
        let conclusion = run
          .get("conclusion")
          .and_then(serde_json::Value::as_str)
          .unwrap_or("unknown")
          .to_owned();
        return Ok(conclusion);
      }
      if Instant::now() >= deadline {
        return Err(format!("run {run_id} did not complete within {timeout:?}").into());
      }
      tokio::time::sleep(Duration::from_secs(5)).await;
    }
  }

  /// Cancel a run via
  /// `POST /repos/{owner}/{repo}/actions/runs/{run_id}/cancel`.
  /// Used by the AC #14 test to verify the runner reacts to GH-side
  /// cancellation.
  pub async fn cancel_run(&self, run_id: u64) -> Result<(), Box<dyn std::error::Error>> {
    let url = format!(
      "{}/repos/{}/actions/runs/{}/cancel",
      self.api_base(),
      self.repo,
      run_id,
    );
    let client = self.http()?;
    let resp = client
      .post(&url)
      .bearer_auth(&self.token)
      .header("Accept", "application/vnd.github+json")
      .send()
      .await?;
    let status = resp.status();
    if !status.is_success() {
      let body: serde_json::Value = resp.json().await.unwrap_or_default();
      return Err(format!("run cancel POST failed: {status} {body}").into());
    }
    Ok(())
  }
}

//! Live E2E for accelerated-mode cache (S17, no Docker).
//!
//! Each test registers a real runner against the test repo, flips its
//! persisted config into `[services] mode = "accelerated"`, pushes a
//! workflow that drives `actions/cache@v4` (and, for AC-13,
//! `actions/upload-artifact@v4`), spawns `toolu-runner run --once`, triggers
//! the workflow, and asserts on the resulting run's `conclusion`. The
//! workflows self-assert (a failed cache hit / integrity check fails the
//! job), so a `success` conclusion is proof the round-trip went through the
//! local content-addressed cache.
//!
//! Every test is `#[ignore]`'d so the harness compiles under
//! `cargo test --features live` without a real PAT. Run with
//! `cargo test -p toolu-runner --features live --test cache_live_e2e --
//! --ignored`, with `TOOLU_RUNNER_LIVE_TOKEN` and `TOOLU_RUNNER_LIVE_REPO`
//! set. When those are absent each test prints a SKIP line and returns Ok.

#![cfg(feature = "live")]

use std::time::Duration;

use config::config::{load_config, save_config};

#[path = "helpers/live_harness.rs"]
mod harness;

use harness::LiveHarness;

/// Skip (returning `Ok`) with a loud SKIP line when the live env is absent.
macro_rules! require_live_env {
  () => {
    if std::env::var("TOOLU_RUNNER_LIVE_TOKEN").is_err()
      || std::env::var("TOOLU_RUNNER_LIVE_REPO").is_err()
    {
      eprintln!(
        "SKIP cache_live_e2e: set TOOLU_RUNNER_LIVE_TOKEN and TOOLU_RUNNER_LIVE_REPO to run the accelerated-cache live tests"
      );
      return Ok(());
    }
  };
}

/// AC-1 — a single-run `actions/cache@v4` save then restore of the same key.
/// The save uploads through the local Twirp `CreateCacheEntry` +
/// `FinalizeCacheEntryUpload` + Azure-blob PUT; the restore resolves through
/// `GetCacheEntryDownloadURL` + blob GET. Asserting `cache-hit == 'true'` and
/// that the bytes survive proves the content-addressed round-trip in
/// accelerated mode.
const CACHE_V4_YAML: &str = r#"
name: cache-v4-accel
on:
  workflow_dispatch:
jobs:
  cache:
    runs-on: [self-hosted, toolu-runner-v1]
    steps:
      - name: seed a cache payload
        run: |
          mkdir -p cache-payload
          echo "toolu-cache-$GITHUB_RUN_ID" > cache-payload/marker.txt
      - name: save through actions/cache/save@v4
        uses: actions/cache/save@v4
        with:
          path: cache-payload
          key: toolu-accel-${{ github.run_id }}
      - name: wipe the payload so the restore must repopulate it
        run: rm -rf cache-payload
      - name: restore through actions/cache/restore@v4
        id: restore
        uses: actions/cache/restore@v4
        with:
          path: cache-payload
          key: toolu-accel-${{ github.run_id }}
      - name: assert the restore hit and the bytes survived
        run: |
          test "${{ steps.restore.outputs.cache-hit }}" = "true"
          grep -q "toolu-cache-$GITHUB_RUN_ID" cache-payload/marker.txt
"#;

/// AC-6 — the same round-trip with `ACTIONS_CACHE_SERVICE_V2` forced off, which
/// drives `actions/cache` down its legacy v1 REST path (the `@v4.1` / no-v2
/// shape). The v1 path talks to `ACTIONS_CACHE_URL`; accelerated mode must
/// point that at the LOCAL server, or the action silently round-trips to Azure
/// and no-ops. A `cache-hit == 'true'` here is proof the v1 REST hole is
/// closed.
const CACHE_V1_REST_YAML: &str = r#"
name: cache-v1-rest-accel
on:
  workflow_dispatch:
jobs:
  cache:
    runs-on: [self-hosted, toolu-runner-v1]
    env:
      ACTIONS_CACHE_SERVICE_V2: 'false'
    steps:
      - name: seed a cache payload
        run: |
          mkdir -p v1-payload
          echo "toolu-v1-$GITHUB_RUN_ID" > v1-payload/marker.txt
      - name: save through actions/cache/save@v4 (v1 REST)
        uses: actions/cache/save@v4
        with:
          path: v1-payload
          key: toolu-v1-${{ github.run_id }}
      - name: wipe the payload so the restore must repopulate it
        run: rm -rf v1-payload
      - name: restore through actions/cache/restore@v4 (v1 REST)
        id: restore
        uses: actions/cache/restore@v4
        with:
          path: v1-payload
          key: toolu-v1-${{ github.run_id }}
      - name: assert the v1 REST restore hit locally
        run: |
          test "${{ steps.restore.outputs.cache-hit }}" = "true"
          grep -q "toolu-v1-$GITHUB_RUN_ID" v1-payload/marker.txt
"#;

/// AC-13 — cache is served locally while `actions/upload-artifact@v4` still
/// reaches real GitHub through the reverse proxy. The cache save round-trips
/// the local CAS; the artifact upload only succeeds if the proxy forwarded it
/// to the real results service, so a `success` conclusion proves the split.
const CACHE_ARTIFACT_SPLIT_YAML: &str = r#"
name: cache-artifact-split-accel
on:
  workflow_dispatch:
jobs:
  split:
    runs-on: [self-hosted, toolu-runner-v1]
    steps:
      - name: produce a payload for both cache and artifact
        run: |
          mkdir -p out
          echo "toolu-split-$GITHUB_RUN_ID" > out/payload.txt
      - name: save a cache entry through the LOCAL accelerated server
        uses: actions/cache/save@v4
        with:
          path: out
          key: toolu-split-${{ github.run_id }}
      - name: upload-artifact must still reach real GitHub through the proxy
        uses: actions/upload-artifact@v4
        with:
          name: toolu-split-${{ github.run_id }}
          path: out/payload.txt
"#;

/// Flip the persisted `config.toml` into accelerated services mode. Loaded and
/// re-saved with the same lib types `register` wrote it with, so the runner
/// picks up `[services] mode = "accelerated"` on the next `run --once`.
fn enable_accelerated(harness: &LiveHarness) -> Result<(), Box<dyn std::error::Error>> {
  let path = harness.config_path();
  let mut cfg = load_config(&path)?;
  cfg.services.mode = "accelerated".to_owned();
  save_config(&path, &cfg)?;
  Ok(())
}

/// Wait for the `run --once` child to exit so it does not outlive the test,
/// returning its exit status (`None` if the child was already reaped).
async fn wait_child(mut child: tokio::process::Child) -> Option<std::process::ExitStatus> {
  child.wait().await.ok()
}

/// Register + accelerate + push + run + trigger + wait, asserting `success`.
/// Shared by every accelerated round-trip test — the workflow itself carries
/// the cache-hit / integrity assertions.
async fn run_accelerated_workflow(
  workflow: &str,
  yaml: &str,
) -> Result<(), Box<dyn std::error::Error>> {
  let harness = LiveHarness::new().await?;
  let _ = harness.cleanup(&[workflow]).await;

  harness.register().await?;
  enable_accelerated(&harness)?;
  harness.push_workflow(workflow, yaml).await?;

  let child = harness.run_once().await?;
  let run_id = harness.trigger_workflow(workflow).await?;
  let conclusion = harness
    .wait_for_run(run_id, Duration::from_secs(600))
    .await?;
  let status = wait_child(child).await;
  let _ = harness.cleanup(&[workflow]).await;

  // The run's conclusion is the source of truth for "did the job pass"; the
  // runner's exit code is a secondary signal (0 on success, 2 on error).
  assert_eq!(
    conclusion, "success",
    "{workflow} should conclude with success in accelerated mode; got {conclusion} (runner exit: {status:?})"
  );
  if let Some(s) = status {
    assert!(s.success(), "runner should exit 0 on success; got {s}");
  }
  Ok(())
}

/// AC-1: `actions/cache@v4` saves and restores a real directory through a
/// runner in accelerated mode; the second (restore) leg reports
/// `cache-hit == 'true'` and the bytes survive.
#[tokio::test]
#[ignore = "live test — requires TOOLU_RUNNER_LIVE_TOKEN + accelerated runner"]
async fn actions_cache_v4_round_trips_through_accelerated_mode()
-> Result<(), Box<dyn std::error::Error>> {
  require_live_env!();
  run_accelerated_workflow("cache-v4-accel-live.yml", CACHE_V4_YAML).await
}

/// AC-6: `actions/cache` with `ACTIONS_CACHE_SERVICE_V2` off (the v4.1 / v1
/// REST shape) must hit the LOCAL service via `ACTIONS_CACHE_URL` and
/// round-trip — the guard against the silent-no-op-to-Azure hole.
#[tokio::test]
#[ignore = "live test — requires TOOLU_RUNNER_LIVE_TOKEN + accelerated runner"]
async fn actions_cache_v4_1_uses_local_v1_rest() -> Result<(), Box<dyn std::error::Error>> {
  require_live_env!();
  run_accelerated_workflow("cache-v1-rest-accel-live.yml", CACHE_V1_REST_YAML).await
}

/// AC-13: `upload-artifact@v4` still reaches real GitHub through the proxy
/// while the cache is served locally. Success proves the artifact upload was
/// forwarded upstream while the cache save stayed on the local CAS.
#[tokio::test]
#[ignore = "live test — requires TOOLU_RUNNER_LIVE_TOKEN + accelerated runner"]
async fn upload_artifact_still_reaches_github() -> Result<(), Box<dyn std::error::Error>> {
  require_live_env!();
  run_accelerated_workflow(
    "cache-artifact-split-accel-live.yml",
    CACHE_ARTIFACT_SPLIT_YAML,
  )
  .await
}

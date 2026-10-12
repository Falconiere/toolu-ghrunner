//! `register` label resolution and `--ephemeral` persistence (#92) through
//! the real `toolu-runner` binary.
//!
//! The binary POSTs `generate-jitconfig` to a local HTTP endpoint at the
//! GHES `api/v3` path (the `--url` host is the endpoint), which answers with
//! the committed real-envelope fixture. Each test reads the request body the
//! binary actually sent and the `config.toml` it persisted. What GitHub
//! itself stores for a label list is pinned separately by the live capture
//! in `crates/config/tests/fixtures/jit_labels_probe.json`.

use std::path::Path;
use std::process::{Command, Output, Stdio};

use config::config::load_config;
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Boxed error alias so helpers can use `?`.
type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

/// Committed `generate-jitconfig` 201 body with a genuine 3-blob envelope.
const RESPONSE_FIXTURE: &str = include_str!("fixtures/generate_jitconfig_response.json");
/// The JIT endpoint for `octo/demo` on the local host.
const JIT_PATH: &str = "/api/v3/repos/octo/demo/actions/runners/generate-jitconfig";
/// Upstream's message for `--no-default-labels` without `--labels`.
const NO_LABELS_MESSAGE: &str =
  "Disabling default labels via --no-default-labels without specifying --labels is not supported";

/// A local endpoint answering `generate-jitconfig` with the fixture.
async fn jit_server() -> TestResult<MockServer> {
  let server = MockServer::start().await;
  let body: Value = serde_json::from_str(RESPONSE_FIXTURE)?;
  Mock::given(method("POST"))
    .and(path(JIT_PATH))
    .respond_with(ResponseTemplate::new(201).set_body_json(body))
    .mount(&server)
    .await;
  Ok(server)
}

/// Run `register --url <server>/octo/demo <extra…>` in an isolated home.
fn register(home: &Path, server: &MockServer, extra: &[&str]) -> TestResult<Output> {
  let url = format!("{}/octo/demo", server.uri());
  let output = Command::new(env!("CARGO_BIN_EXE_toolu-runner"))
    .env("TOOLU_RUNNER_HOME", home)
    .env("HOME", home)
    .env("TOOLU_RUNNER_TOKEN", "register-labels-token")
    .stdin(Stdio::null())
    .args(["register", "--url", &url, "--name", "labels-92"])
    .args(extra)
    .output()?;
  Ok(output)
}

/// The `labels` array of every `generate-jitconfig` request received.
async fn sent_labels(server: &MockServer) -> TestResult<Vec<Vec<String>>> {
  let requests = server
    .received_requests()
    .await
    .ok_or("request recording disabled")?;
  requests
    .iter()
    .map(|req| {
      let body: Value = serde_json::from_slice(&req.body)?;
      let labels = body
        .get("labels")
        .and_then(Value::as_array)
        .ok_or("labels array")?;
      labels
        .iter()
        .map(|l| {
          l.as_str()
            .map(str::to_owned)
            .ok_or_else(|| "label string".into())
        })
        .collect()
    })
    .collect()
}

fn config_path(home: &Path) -> std::path::PathBuf {
  home.join("runners/octo/demo/config.toml")
}

fn owned(items: &[&str]) -> Vec<String> {
  items.iter().map(|s| (*s).to_owned()).collect()
}

/// `self-hosted`, then GitHub's `RUNNER_OS` / `RUNNER_ARCH` spelling.
fn defaults() -> Vec<String> {
  vec![
    "self-hosted".to_owned(),
    shared::platform::runner_os().to_owned(),
    shared::platform::runner_arch().to_owned(),
  ]
}

/// Assert a successful register sent and persisted exactly `expected`.
async fn assert_registered(
  output: &Output,
  server: &MockServer,
  home: &Path,
  expected: &[String],
) -> TestResult<()> {
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(output.status.success(), "register failed: {stderr}");
  assert_eq!(sent_labels(server).await?, vec![expected.to_vec()]);
  assert_eq!(load_config(&config_path(home))?.labels, expected);
  Ok(())
}

/// AC-1: `--labels` adds to the defaults (upstream `CreateNewAgent`).
#[tokio::test]
async fn labels_are_added_to_the_defaults() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  let server = jit_server().await?;
  let output = register(home.path(), &server, &["--labels", "gpu"])?;
  let mut expected = defaults();
  expected.push("gpu".to_owned());
  assert_registered(&output, &server, home.path(), &expected).await?;
  assert!(!load_config(&config_path(home.path()))?.ephemeral);
  Ok(())
}

/// AC-1: `--no-default-labels` registers only the explicit labels; empty
/// entries are dropped and a case-duplicate keeps its first spelling.
#[tokio::test]
async fn no_default_labels_registers_only_explicit_labels() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  let server = jit_server().await?;
  let output = register(
    home.path(),
    &server,
    &["--no-default-labels", "--labels", "gpu,GPU,,tpu"],
  )?;
  assert_registered(&output, &server, home.path(), &owned(&["gpu", "tpu"])).await
}

/// AC-1: an empty `--labels` value leaves exactly the defaults.
#[tokio::test]
async fn empty_labels_value_keeps_exactly_the_defaults() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  let server = jit_server().await?;
  let output = register(home.path(), &server, &["--labels", ""])?;
  assert_registered(&output, &server, home.path(), &defaults()).await
}

/// Assert a register refused before any request and wrote nothing.
async fn assert_refused(
  output: &Output,
  server: &MockServer,
  home: &Path,
  needle: &str,
) -> TestResult<()> {
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert_eq!(output.status.code(), Some(2), "stderr: {stderr}");
  assert!(stderr.contains(needle), "stderr: {stderr}");
  assert!(
    sent_labels(server).await?.is_empty(),
    "no request may be sent"
  );
  assert!(!config_path(home).exists(), "no config may be written");
  Ok(())
}

/// AC-2: `--no-default-labels` with no label (none, or only empty entries)
/// fails with upstream's message before any request.
#[tokio::test]
async fn no_default_labels_without_labels_is_refused() -> TestResult<()> {
  for extra in [
    vec!["--no-default-labels"],
    vec!["--no-default-labels", "--labels", ",,"],
  ] {
    let home = tempfile::tempdir()?;
    let server = jit_server().await?;
    let output = register(home.path(), &server, &extra)?;
    assert_refused(&output, &server, home.path(), NO_LABELS_MESSAGE).await?;
  }
  Ok(())
}

/// AC-2: a label over 256 characters is refused before any request.
#[tokio::test]
async fn overlong_label_is_refused() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  let server = jit_server().await?;
  let long = "x".repeat(257);
  let output = register(home.path(), &server, &["--labels", &long])?;
  assert_refused(&output, &server, home.path(), "256").await
}

/// AC-4: `--ephemeral` is persisted in `config.toml` and reported.
#[tokio::test]
async fn ephemeral_flag_is_persisted() -> TestResult<()> {
  let home = tempfile::tempdir()?;
  let server = jit_server().await?;
  let output = register(home.path(), &server, &["--ephemeral"])?;
  assert_registered(&output, &server, home.path(), &defaults()).await?;
  let text = std::fs::read_to_string(config_path(home.path()))?;
  assert!(text.contains("ephemeral = true"), "{text}");
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(stdout.contains("ephemeral"), "stdout: {stdout}");
  Ok(())
}

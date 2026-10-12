//! `register --replace` keeps the replaced registration's `[network]` (#94):
//! the mint goes through it, and the rewritten `config.toml` still carries
//! it. Real `toolu-runner` binary against a local `generate-jitconfig`
//! endpoint answering with the committed real-envelope fixture.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use config::config::{load_config, save_config};
use config::network::NetworkSection;
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

const RESPONSE_FIXTURE: &str = include_str!("fixtures/generate_jitconfig_response.json");
const JIT_PATH: &str = "/api/v3/repos/octo/demo/actions/runners/generate-jitconfig";

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

fn register(home: &Path, server: &MockServer, extra: &[&str]) -> TestResult<Output> {
  let url = format!("{}/octo/demo", server.uri());
  Ok(
    Command::new(env!("CARGO_BIN_EXE_toolu-runner"))
      .env("TOOLU_RUNNER_HOME", home)
      .env("HOME", home)
      .env("TOOLU_RUNNER_TOKEN", "register-replace-token")
      .env_remove("HTTPS_PROXY")
      .env_remove("https_proxy")
      .env_remove("HTTP_PROXY")
      .env_remove("http_proxy")
      .env_remove("ALL_PROXY")
      .env_remove("all_proxy")
      .stdin(Stdio::null())
      .args(["register", "--url", &url, "--name", "replace-94"])
      .args(extra)
      .output()?,
  )
}

fn config_path(home: &Path) -> PathBuf {
  home.join("runners/octo/demo/config.toml")
}

async fn requests(server: &MockServer) -> TestResult<usize> {
  Ok(
    server
      .received_requests()
      .await
      .ok_or("request recording disabled")?
      .len(),
  )
}

/// Register once, then give the registration `network`.
async fn registered_with(network: NetworkSection) -> TestResult<(tempfile::TempDir, MockServer)> {
  let home = tempfile::tempdir()?;
  let server = jit_server().await?;
  let first = register(home.path(), &server, &[])?;
  assert!(
    first.status.success(),
    "first register: {}",
    String::from_utf8_lossy(&first.stderr)
  );
  let mut cfg = load_config(&config_path(home.path()))?;
  cfg.network = network;
  save_config(&config_path(home.path()), &cfg)?;
  Ok((home, server))
}

#[tokio::test(flavor = "multi_thread")]
async fn replace_mints_through_the_existing_network_section() -> TestResult<()> {
  let missing = std::env::temp_dir().join("toolu-94-replace-no-such-ca.pem");
  let network = NetworkSection {
    ca_bundle: Some(missing.display().to_string()),
    ..NetworkSection::default()
  };
  let (home, server) = registered_with(network.clone()).await?;
  let output = register(home.path(), &server, &["--replace"])?;
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert_eq!(output.status.code(), Some(2), "{stderr}");
  assert!(
    stderr.contains(&format!(
      "[network] ca_bundle {}: cannot access",
      missing.display()
    )),
    "{stderr}"
  );
  assert_eq!(
    requests(&server).await?,
    1,
    "the replacement mint was never sent"
  );
  assert_eq!(load_config(&config_path(home.path()))?.network, network);
  Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn replace_keeps_the_network_section_in_the_rewritten_config() -> TestResult<()> {
  // Config mode; the loopback mint endpoint is always bypassed.
  let network = NetworkSection {
    proxy: Some("http://proxy.corp.test:3128".to_owned()),
    no_proxy: vec!["internal.corp".to_owned()],
    ca_bundle: None,
  };
  let (home, server) = registered_with(network.clone()).await?;
  let output = register(home.path(), &server, &["--replace"])?;
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert_eq!(requests(&server).await?, 2);
  assert_eq!(load_config(&config_path(home.path()))?.network, network);
  Ok(())
}

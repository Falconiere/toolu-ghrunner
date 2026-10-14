//! #94 AC-10 / S1–S3 through the real binary: `check --config` probes through
//! the registration's `[network]` proxy (env `HTTPS_PROXY` names a dead port
//! and is ignored), reports the redacted settings, and `check` / `run` refuse
//! an unusable `ca_bundle` with exit 2 before any network call.

use crate::{proxy_fixtures, tls_fixtures};

use std::path::{Path, PathBuf};
use std::process::Command;

use config::config::{
  CacheSection, CredentialsFile, RunnerRegistrationConfig, RuntimeConfig, ServicesSection,
  ShadowSection, WorkspaceSection, save_config, save_credentials,
};
use config::network::NetworkSection;
use proxy_fixtures::RecordingProxy;
use tls_fixtures::{FixtureResult, Leaf, Pki, TlsTarget};

/// A loopback port nothing listens on.
async fn closed_port() -> FixtureResult<u16> {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
  Ok(listener.local_addr()?.port())
}

const GHES_JIT: &str = include_str!("fixtures/jit_config_ghes.json");
const ROUTING_VARS: [&str; 12] = [
  "HTTPS_PROXY",
  "https_proxy",
  "HTTP_PROXY",
  "http_proxy",
  "ALL_PROXY",
  "all_proxy",
  "NO_PROXY",
  "no_proxy",
  "REQUEST_METHOD",
  "SSL_CERT_FILE",
  "SSL_CERT_DIR",
  "NODE_EXTRA_CA_CERTS",
];

/// Persist a registration for `runner_url` with `network` under
/// `<home>/runners/o/r/`, the way `register` does.
fn write_registration(
  home: &Path,
  runner_url: &str,
  network: NetworkSection,
) -> FixtureResult<PathBuf> {
  let fixture: serde_json::Value = serde_json::from_str(GHES_JIT)?;
  let jit = fixture
    .get("encoded")
    .and_then(serde_json::Value::as_str)
    .ok_or("fixture has no encoded field")?;
  let dir = home.join("runners").join("o").join("r");
  std::fs::create_dir_all(&dir)?;
  let config_path = dir.join("config.toml");
  save_config(
    &config_path,
    &RunnerRegistrationConfig {
      runner_url: runner_url.to_owned(),
      runner_name: "net94".to_owned(),
      runner_id: 94,
      auth_token: "client-id".to_owned(),
      labels: vec!["self-hosted".to_owned()],
      runner_group: "Default".to_owned(),
      ephemeral: false,
      runtime: RuntimeConfig {
        jit_config: jit.to_owned(),
        work_dir: dir.join("_work").to_string_lossy().into_owned(),
        data_dir: dir.to_string_lossy().into_owned(),
        protocol_version: "v1".to_owned(),
      },
      services: ServicesSection::default(),
      cache: CacheSection::default(),
      workspace: WorkspaceSection::default(),
      shadow: ShadowSection::default(),
      network,
    },
  )?;
  save_credentials(
    &dir.join("credentials.json"),
    &CredentialsFile {
      access_token: "client-id".to_owned(),
      issued_at: "2026-10-12T00:00:00Z".to_owned(),
      expires_at: None,
    },
  )?;
  Ok(config_path)
}

/// The binary pinned to `home`, every routing variable cleared, then
/// `HTTPS_PROXY` set to a dead port (config mode must ignore it).
fn runner(home: &Path, dead_port: u16) -> Command {
  let mut cmd = Command::new(env!("CARGO_BIN_EXE_toolu-runner"));
  cmd
    .current_dir(home)
    .env("HOME", home)
    .env("TOOLU_RUNNER_HOME", home)
    .env("TOOLU_RUNNER_NO_KEYRING", "1");
  for var in ROUTING_VARS {
    cmd.env_remove(var);
  }
  cmd.env("HTTPS_PROXY", format!("http://127.0.0.1:{dead_port}"));
  cmd
}

async fn output(mut cmd: Command) -> FixtureResult<(Option<i32>, String, String)> {
  let out = tokio::task::spawn_blocking(move || cmd.output()).await??;
  Ok((
    out.status.code(),
    String::from_utf8_lossy(&out.stdout).into_owned(),
    String::from_utf8_lossy(&out.stderr).into_owned(),
  ))
}

#[tokio::test(flavor = "multi_thread")]
async fn check_probes_through_the_configured_proxy_and_reports_it_redacted() -> FixtureResult<()> {
  let home = tempfile::tempdir()?;
  let pki = Pki::new()?;
  let ghes = TlsTarget::serve(
    pki.server_config(&["ghes.test", "codeload.ghes.test"], Leaf::Valid)?,
    b"ok".to_vec(),
  )
  .await?;
  let proxy = RecordingProxy::start(Some(("runner", "pw94secret"))).await?;
  let bundle = pki.write_bundle(home.path())?;
  let config = write_registration(
    home.path(),
    &format!("https://ghes.test:{}/my-org", ghes.port()),
    NetworkSection {
      proxy: Some(proxy.url(Some("runner:pw94secret"))),
      no_proxy: vec!["internal.corp".to_owned()],
      ca_bundle: Some(bundle.to_string_lossy().into_owned()),
    },
  )?;
  let mut cmd = runner(home.path(), closed_port().await?);
  cmd
    .args(["check", "--config"])
    .arg(&config)
    .args(["--timeout", "5"]);
  let (code, stdout, stderr) = output(cmd).await?;
  // Deterministic exit 1: `api` / `codeload` pass through the tunnel, but the
  // fixture JIT's `pipelines` and `token` endpoints sit on `ghes.example.com:443`,
  // which the recording proxy cannot reach (it tunnels to the fixture's
  // loopback port only), so exactly those two required probes fail.
  assert_eq!(code, Some(1), "stdout:\n{stdout}\nstderr:\n{stderr}");
  assert!(
    stdout.contains("result: 2 required checks failed (exit 1)")
      && stdout.contains("FAIL  pipelines")
      && stdout.contains("FAIL  token"),
    "{stdout}"
  );

  assert!(
    stdout.contains(&format!(
      "PASS  api        https://ghes.test:{}/api/v3",
      ghes.port()
    )),
    "stdout:\n{stdout}\nstderr:\n{stderr}"
  );
  assert!(
    ghes
      .requests()
      .iter()
      .any(|line| line == "GET /api/v3 HTTP/1.1"),
    "the api probe must reach GHES through the tunnel: {:?}",
    ghes.requests()
  );
  let hits = proxy.hits();
  assert!(
    hits
      .iter()
      .any(|hit| hit.line == format!("CONNECT ghes.test:{} HTTP/1.1", ghes.port())),
    "{hits:?}"
  );
  assert!(
    stdout.contains(&format!(
      "[network]    proxy {} (config), no_proxy 1 entries, ca_bundle {} (1 certificates)",
      proxy.url(Some("***")),
      bundle.display()
    )),
    "{stdout}"
  );
  assert!(
    stdout.contains("[network] proxy is set: the proxy variables below are ignored"),
    "{stdout}"
  );
  assert!(!stdout.contains("pw94secret") && !stderr.contains("pw94secret"));
  Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn check_and_run_refuse_an_unusable_ca_bundle_with_exit_2() -> FixtureResult<()> {
  let home = tempfile::tempdir()?;
  let missing = home.path().join("no-such-ca.pem");
  let config = write_registration(
    home.path(),
    "https://ghes.test/my-org",
    NetworkSection {
      proxy: None,
      no_proxy: Vec::new(),
      ca_bundle: Some(missing.to_string_lossy().into_owned()),
    },
  )?;
  let dead = closed_port().await?;

  let mut check = runner(home.path(), dead);
  check.args(["check", "--config"]).arg(&config);
  let (code, stdout, stderr) = output(check).await?;
  assert_eq!(code, Some(2), "stdout:\n{stdout}\nstderr:\n{stderr}");
  assert!(stderr.contains(&missing.display().to_string()), "{stderr}");
  assert!(stdout.is_empty(), "nothing is probed: {stdout}");

  let mut run = runner(home.path(), dead);
  run.args(["run", "--once", "--config"]).arg(&config);
  let (code, _stdout, stderr) = output(run).await?;
  assert_eq!(code, Some(2), "{stderr}");
  assert!(stderr.contains(&missing.display().to_string()), "{stderr}");
  assert!(stderr.contains("[network] ca_bundle"), "{stderr}");
  Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn check_refuses_an_unusable_proxy_url_with_exit_2() -> FixtureResult<()> {
  let home = tempfile::tempdir()?;
  let config = write_registration(
    home.path(),
    "https://ghes.test/my-org",
    NetworkSection {
      proxy: Some("ftp://u:pw94secret@x".to_owned()),
      no_proxy: Vec::new(),
      ca_bundle: None,
    },
  )?;
  let mut check = runner(home.path(), closed_port().await?);
  check.args(["check", "--config"]).arg(&config);
  let (code, stdout, stderr) = output(check).await?;
  assert_eq!(code, Some(2), "stdout:\n{stdout}\nstderr:\n{stderr}");
  assert!(
    stderr.contains("[network] proxy ftp://***@x: scheme must be http or https"),
    "{stderr}"
  );
  assert!(!stderr.contains("pw94secret"), "{stderr}");
  assert!(stdout.is_empty(), "nothing is probed: {stdout}");
  Ok(())
}

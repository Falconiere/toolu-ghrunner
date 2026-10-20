//! Shared fixtures for the `doctor` shell-out tests: a hermetic `toolu-runner
//! doctor` command and real registrations persisted the way `register`
//! writes them, built from the committed GHES JIT fixture.
//!
//! Hermetic means no test reaches the internet or the developer's own
//! credentials: `HOME` / `TOOLU_RUNNER_HOME` point at the test's tempdir, the
//! token / keyring / proxy / trust / Docker variables are removed, `https://`
//! traffic goes to a closed loopback proxy, and the release check targets a
//! closed loopback port unless a test supplies a peer.

use std::path::{Path, PathBuf};
use std::process::Command;

use config::config::{
  CacheSection, CredentialsFile, RunnerRegistrationConfig, RuntimeConfig, ServicesSection,
  ShadowSection, WorkspaceSection, save_config, save_credentials,
};

use crate::check_servers::{closed_port, jit_pointing_at};

/// Variables that would leak a developer's token, keyring choice, proxy,
/// trust store or Docker endpoint into a test.
const ISOLATED_VARS: [&str; 17] = [
  "TOOLU_RUNNER_TOKEN",
  "TOOLU_RUNNER_KEYRING",
  "TOOLU_RUNNER_NO_KEYRING",
  "TOOLU_RUNNER_RELEASES_URL",
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
  "DOCKER_HOST",
];

/// `toolu-runner doctor` pinned to an isolated home and cwd.
pub fn doctor_cmd(home: &Path) -> std::io::Result<Command> {
  let mut cmd = Command::new(env!("CARGO_BIN_EXE_toolu-runner"));
  cmd
    .arg("doctor")
    .current_dir(home)
    .env("HOME", home)
    .env("TOOLU_RUNNER_HOME", home);
  for var in ISOLATED_VARS {
    cmd.env_remove(var);
  }
  cmd
    .env(
      "HTTPS_PROXY",
      format!("http://127.0.0.1:{}", closed_port()?),
    )
    .env("NO_PROXY", "127.0.0.1,localhost")
    .env(
      "TOOLU_RUNNER_RELEASES_URL",
      format!("http://127.0.0.1:{}/releases/latest", closed_port()?),
    );
  Ok(cmd)
}

/// Persist a registration for `owner/repo` under `<home>/runners/` the way
/// `register` does: `runner_url` = `<base>/<owner>/<repo>` and the GHES JIT
/// fixture's service URLs pointed at `base`. Returns its `config.toml` path.
pub fn write_registration(
  home: &Path,
  owner: &str,
  repo: &str,
  base: &str,
  ephemeral: bool,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
  let dir = home.join("runners").join(owner).join(repo);
  std::fs::create_dir_all(&dir)?;
  let config_path = dir.join("config.toml");
  save_config(
    &config_path,
    &RunnerRegistrationConfig {
      runner_url: format!("{base}/{owner}/{repo}"),
      runner_name: "doctor-test".to_owned(),
      runner_id: 7,
      auth_token: "client-id".to_owned(),
      labels: vec!["self-hosted".to_owned()],
      runner_group: "Default".to_owned(),
      ephemeral,
      runtime: RuntimeConfig {
        jit_config: jit_pointing_at(base)?,
        work_dir: dir.join("_work").to_string_lossy().into_owned(),
        data_dir: dir.to_string_lossy().into_owned(),
        protocol_version: "v1".to_owned(),
      },
      services: ServicesSection::default(),
      cache: CacheSection::default(),
      workspace: WorkspaceSection::default(),
      shadow: ShadowSection::default(),
      network: config::network::NetworkSection::default(),
    },
  )?;
  save_credentials(
    &dir.join("credentials.json"),
    &CredentialsFile {
      access_token: "client-id".to_owned(),
      issued_at: "2026-10-19T00:00:00Z".to_owned(),
      expires_at: None,
    },
  )?;
  Ok(config_path)
}

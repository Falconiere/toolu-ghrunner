//! Startup-WARN behavior of the always-online `run` loop (s5).
//!
//! Default `run` (no `--once`) with no stored login token must WARN, before
//! polling, that the runner drops offline after one job — naming
//! `toolu-runner login`. `--once` opts out of that warning. Both invocations
//! error fast at JIT-config parse (the fixture's blob is invalid base64), so
//! neither test touches the network.

use std::path::{Path, PathBuf};
use std::process::Command;

use config::config::{
  CacheSection, CredentialsFile, RunnerRegistrationConfig, RuntimeConfig, ServicesSection,
  ShadowSection, WorkspaceSection, save_config, save_credentials,
};

/// The exact substring the startup WARN must contain.
const LOGIN_HINT: &str = "toolu-runner login";

/// Persist a real registration under `<home>/runners/<owner>/<repo>/` whose
/// JIT blob is invalid base64, so `run` errors at parse right after the
/// startup WARN. Returns the config path. A helper (not a `#[test]` fn), so
/// it must avoid `expect`/`unwrap` — it threads errors out via `?`.
fn write_fixture(
  home: &Path,
  owner: &str,
  repo: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
  let reg_dir = home.join("runners").join(owner).join(repo);
  std::fs::create_dir_all(&reg_dir)?;
  let config_path = reg_dir.join("config.toml");
  let cfg = RunnerRegistrationConfig {
    runner_url: format!("https://github.com/{owner}/{repo}"),
    runner_name: "test-runner".to_owned(),
    runner_id: 1,
    auth_token: "client-id".to_owned(),
    labels: vec!["self-hosted".to_owned()],
    runner_group: "Default".to_owned(),
    runtime: RuntimeConfig {
      // `!`/spaces are outside the standard base64 alphabet, so
      // `JitConfig::parse` fails and `run` exits fast with no network.
      jit_config: "!! not base64 !!".to_owned(),
      work_dir: reg_dir.join("_work").to_string_lossy().into_owned(),
      data_dir: reg_dir.to_string_lossy().into_owned(),
      protocol_version: "v2".to_owned(),
    },
    services: ServicesSection::default(),
    cache: CacheSection::default(),
    workspace: WorkspaceSection::default(),
    shadow: ShadowSection::default(),
  };
  save_config(&config_path, &cfg)?;
  save_credentials(
    &reg_dir.join("credentials.json"),
    &CredentialsFile {
      access_token: "client-id".to_owned(),
      issued_at: "2026-07-14T00:00:00Z".to_owned(),
      expires_at: None,
    },
  )?;
  Ok(config_path)
}

/// Build a `run --config <path>` invocation pinned to an isolated home with
/// the keyring forced off and no `TOOLU_RUNNER_TOKEN`, so the only bearer
/// source is the (absent) stored login token.
fn runner_cmd(home: &Path, config_path: &Path, once: bool) -> Command {
  let mut cmd = Command::new(env!("CARGO_BIN_EXE_toolu-runner"));
  cmd
    .env("HOME", home)
    .env("TOOLU_RUNNER_HOME", home)
    .env("TOOLU_RUNNER_NO_KEYRING", "1")
    .env_remove("TOOLU_RUNNER_TOKEN")
    .args(["run", "--config"])
    .arg(config_path);
  if once {
    cmd.arg("--once");
  }
  cmd
}

#[test]
fn default_run_warns_about_missing_login_before_erroring() {
  let home = tempfile::tempdir().expect("tempdir");
  let config_path = write_fixture(home.path(), "octo", "demo").expect("write fixture");

  let output = runner_cmd(home.path(), &config_path, false)
    .output()
    .expect("run toolu-runner run");

  assert!(
    !output.status.success(),
    "run must fail fast at the invalid JIT blob"
  );
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    stderr.contains(LOGIN_HINT),
    "startup WARN must name `{LOGIN_HINT}` even though run later errors; got stderr:\n{stderr}"
  );
}

#[test]
fn once_run_omits_the_login_warning() {
  let home = tempfile::tempdir().expect("tempdir");
  let config_path = write_fixture(home.path(), "octo", "demo").expect("write fixture");

  let output = runner_cmd(home.path(), &config_path, true)
    .output()
    .expect("run toolu-runner run --once");

  assert!(
    !output.status.success(),
    "run --once must also fail at the invalid JIT blob"
  );
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    !stderr.contains(LOGIN_HINT),
    "--once must not emit the login WARN; got stderr:\n{stderr}"
  );
}

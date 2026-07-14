//! `install-service` shell-out tests (s8).
//!
//! Exercises the print / no-activate / remove modes against a real persisted
//! registration under a temp home. The temp dir is BOTH `TOOLU_RUNNER_HOME`
//! (so config resolution finds the registration) and `HOME` (so the unit file
//! lands under an isolated LaunchAgents / systemd-user dir). Assertions branch
//! on the host OS: launchd plist on macOS, systemd unit on Linux. No test
//! exercises the default activate mode — nothing here invokes a live
//! `launchctl` / `systemctl` that would load a real service.

use std::path::{Path, PathBuf};
use std::process::Command;

use config::config::{
  CacheSection, CredentialsFile, RunnerRegistrationConfig, RuntimeConfig, ServicesSection,
  ShadowSection, WorkspaceSection, save_config, save_credentials,
};

/// Persist a real registration under `<home>/runners/<owner>/<repo>/` and
/// return its config path. A helper (not a `#[test]`), so it threads errors
/// out via `?` rather than `expect`/`unwrap`.
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
      jit_config: "unused-by-install-service".to_owned(),
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

/// `install-service <args>` pinned to an isolated home (both env vars point
/// at `home`), so the registration resolves there and any unit file lands
/// under that same tree.
fn install_cmd(home: &Path, config_path: &Path, extra: &[&str]) -> Command {
  let mut cmd = Command::new(env!("CARGO_BIN_EXE_toolu-runner"));
  cmd
    .env("HOME", home)
    .env("TOOLU_RUNNER_HOME", home)
    .env("TOOLU_RUNNER_NO_KEYRING", "1")
    .args(["install-service", "--config"])
    .arg(config_path)
    .args(extra);
  cmd
}

/// The OS-conventional unit destination for `io.toolu.runner.octo.demo` under
/// `home`, matching `service_cmd::dest_path` for the current target.
fn expected_dest(home: &Path) -> PathBuf {
  if cfg!(target_os = "macos") {
    home
      .join("Library/LaunchAgents")
      .join("io.toolu.runner.octo.demo.plist")
  } else {
    home
      .join(".config/systemd/user")
      .join("toolu-runner-octo-demo.service")
  }
}

/// Structural assertions on the printed unit, branching on the host OS:
/// launchd plist on macOS, systemd unit on Linux. `exe` / `config` are the
/// absolute paths the unit must embed.
fn assert_unit_content(stdout: &str, exe: &str, config: &str) {
  assert!(
    stdout.contains("io.toolu.runner.octo.demo"),
    "unit must carry the derived label; got:\n{stdout}"
  );
  if cfg!(target_os = "macos") {
    assert!(
      stdout.contains(&format!("<string>{exe}</string>")),
      "plist must carry the absolute binary path in a <string>; got:\n{stdout}"
    );
    assert!(
      stdout.contains(&format!("<string>{config}</string>")),
      "plist must carry the absolute config path in a <string>; got:\n{stdout}"
    );
    assert!(
      stdout.contains("<string>run</string>") && stdout.contains("<string>--config</string>"),
      "plist ProgramArguments must run `run --config`; got:\n{stdout}"
    );
    assert!(
      stdout.contains("<key>KeepAlive</key>"),
      "plist must set KeepAlive; got:\n{stdout}"
    );
  } else {
    assert!(
      stdout.contains(&format!("ExecStart=\"{exe}\" run --config \"{config}\"")),
      "unit ExecStart must run `run --config <config>` with absolute paths; got:\n{stdout}"
    );
    assert!(
      stdout.contains("Restart=always"),
      "unit must set Restart=always; got:\n{stdout}"
    );
  }
}

#[test]
fn print_emits_the_unit_without_writing_a_file() {
  let home = tempfile::tempdir().expect("tempdir");
  let config_path = write_fixture(home.path(), "octo", "demo").expect("write fixture");

  let output = install_cmd(home.path(), &config_path, &["--print"])
    .output()
    .expect("run install-service --print");

  assert!(
    output.status.success(),
    "--print should exit 0; stderr:\n{}",
    String::from_utf8_lossy(&output.stderr)
  );
  let stdout = String::from_utf8_lossy(&output.stdout);
  // config_path is canonicalized in the unit (symlinks resolved), so the
  // test canonicalizes the same path before asserting containment.
  let config = config_path
    .canonicalize()
    .expect("canonicalize config path");
  assert_unit_content(
    &stdout,
    env!("CARGO_BIN_EXE_toolu-runner"),
    &config.to_string_lossy(),
  );
  assert!(
    !expected_dest(home.path()).exists(),
    "--print must not write a unit file"
  );
}

#[test]
fn no_activate_writes_the_unit_and_prints_the_activation_command() {
  let home = tempfile::tempdir().expect("tempdir");
  let config_path = write_fixture(home.path(), "octo", "demo").expect("write fixture");

  let output = install_cmd(home.path(), &config_path, &["--no-activate"])
    .output()
    .expect("run install-service --no-activate");

  assert!(
    output.status.success(),
    "--no-activate should exit 0; stderr:\n{}",
    String::from_utf8_lossy(&output.stderr)
  );
  let dest = expected_dest(home.path());
  assert!(
    dest.is_file(),
    "--no-activate must write the unit at {}",
    dest.display()
  );
  let stdout = String::from_utf8_lossy(&output.stdout);
  let activator = if cfg!(target_os = "macos") {
    "launchctl"
  } else {
    "systemctl"
  };
  assert!(
    stdout.contains(activator),
    "--no-activate must print the {activator} activation command; got:\n{stdout}"
  );
}

#[test]
fn remove_deletes_the_unit_and_is_idempotent() {
  let home = tempfile::tempdir().expect("tempdir");
  let config_path = write_fixture(home.path(), "octo", "demo").expect("write fixture");

  // Install (write-only) so there is a file to remove — never activated, so
  // `--remove`'s best-effort deactivation is a harmless no-op.
  install_cmd(home.path(), &config_path, &["--no-activate"])
    .output()
    .expect("run install-service --no-activate");
  let dest = expected_dest(home.path());
  assert!(dest.is_file(), "precondition: unit written");

  let first = install_cmd(home.path(), &config_path, &["--remove"])
    .output()
    .expect("run install-service --remove");
  assert!(
    first.status.success(),
    "--remove should exit 0; stderr:\n{}",
    String::from_utf8_lossy(&first.stderr)
  );
  assert!(!dest.exists(), "--remove must delete the unit file");

  let second = install_cmd(home.path(), &config_path, &["--remove"])
    .output()
    .expect("run install-service --remove (again)");
  assert!(
    second.status.success(),
    "second --remove should exit 0 (idempotent); stderr:\n{}",
    String::from_utf8_lossy(&second.stderr)
  );
  let stdout = String::from_utf8_lossy(&second.stdout);
  assert!(
    stdout.contains("nothing to do"),
    "second --remove must report nothing-to-do; got:\n{stdout}"
  );
}

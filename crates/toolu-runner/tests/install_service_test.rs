//! `install-service` shell-out tests (s8).
//!
//! Exercises the print / no-activate / remove modes against a real persisted
//! registration under a temp home. The temp dir is BOTH `TOOLU_RUNNER_HOME`
//! (so config resolution finds the registration) and `HOME` (so the unit file
//! lands under an isolated LaunchAgents / systemd-user dir). Assertions branch
//! on the host OS: launchd plist on macOS, systemd unit on Linux. The default
//! activate mode is exercised through PATH-shim `launchctl` / `systemctl`
//! scripts that log their argv — no real service is ever loaded.

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

/// Write an executable shell script named `name` into `dir` (a PATH shim for
/// `launchctl` / `systemctl`). The script appends its argv to `$SHIM_LOG`.
#[cfg(unix)]
fn write_shim(dir: &Path, name: &str, body: &str) -> Result<(), Box<dyn std::error::Error>> {
  use std::os::unix::fs::PermissionsExt;
  let path = dir.join(name);
  std::fs::write(&path, body)?;
  std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
  Ok(())
}

/// Prepare a shim dir + log and return `(shim_dir, log_path, path_env)` —
/// `path_env` prepends the shim dir to the current `PATH`.
#[cfg(unix)]
fn shim_env(home: &Path) -> Result<(PathBuf, PathBuf, String), Box<dyn std::error::Error>> {
  let bin = home.join("shim-bin");
  std::fs::create_dir_all(&bin)?;
  let log = home.join("shim.log");
  let path_env = format!(
    "{}:{}",
    bin.display(),
    std::env::var("PATH").unwrap_or_default()
  );
  Ok((bin, log, path_env))
}

/// Run the default (write + activate) mode with `shim_body` installed as
/// BOTH the `launchctl` and `systemctl` PATH shim; returns the process
/// output and the shim-call log (empty if no shim was ever invoked).
#[cfg(unix)]
fn run_activate(
  home: &Path,
  config_path: &Path,
  shim_body: &str,
) -> Result<(std::process::Output, String), Box<dyn std::error::Error>> {
  let (bin, log, path_env) = shim_env(home)?;
  write_shim(&bin, "launchctl", shim_body)?;
  write_shim(&bin, "systemctl", shim_body)?;
  let output = install_cmd(home, config_path, &[])
    .env("PATH", &path_env)
    .env("SHIM_LOG", &log)
    .output()?;
  let calls = std::fs::read_to_string(&log).unwrap_or_default();
  Ok((output, calls))
}

#[cfg(unix)]
#[test]
fn default_mode_writes_the_unit_and_invokes_the_supervisor() {
  let home = tempfile::tempdir().expect("tempdir");
  let config_path = write_fixture(home.path(), "octo", "demo").expect("write fixture");
  let record_ok = "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$SHIM_LOG\"\nexit 0\n";

  let (output, calls) =
    run_activate(home.path(), &config_path, record_ok).expect("default activate run");

  assert!(
    output.status.success(),
    "default mode should exit 0; stderr:\n{}",
    String::from_utf8_lossy(&output.stderr)
  );
  let dest = expected_dest(home.path());
  assert!(
    dest.is_file(),
    "default mode must write the unit at {}",
    dest.display()
  );
  if cfg!(target_os = "macos") {
    assert_eq!(
      calls,
      format!(
        "bootstrap gui/{} {}\n",
        real_uid().expect("id -u"),
        dest.display()
      ),
      "modern bootstrap alone must load the unit"
    );
  } else {
    assert_eq!(
      calls, "--user daemon-reload\n--user enable --now toolu-runner-octo-demo.service\n",
      "systemd must daemon-reload then enable --now the unit"
    );
  }
}

#[cfg(target_os = "macos")]
#[test]
fn launchd_bootstrap_failure_falls_back_to_legacy_load() {
  let home = tempfile::tempdir().expect("tempdir");
  let config_path = write_fixture(home.path(), "octo", "demo").expect("write fixture");
  // A failing `bootstrap` (older host) must fall back to legacy `load -w`.
  let bootstrap_fails = "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$SHIM_LOG\"\n\
                         case \"$1\" in bootstrap) exit 1;; esac\nexit 0\n";

  let (output, calls) =
    run_activate(home.path(), &config_path, bootstrap_fails).expect("default activate run");

  assert!(
    output.status.success(),
    "legacy fallback should still exit 0; stderr:\n{}",
    String::from_utf8_lossy(&output.stderr)
  );
  let dest = expected_dest(home.path());
  assert_eq!(
    calls,
    format!(
      "bootstrap gui/{uid} {dest}\nload -w {dest}\n",
      uid = real_uid().expect("id -u"),
      dest = dest.display()
    ),
    "failed bootstrap must fall back to legacy load -w"
  );
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn systemd_activation_failure_is_fatal_and_named() {
  let home = tempfile::tempdir().expect("tempdir");
  let config_path = write_fixture(home.path(), "octo", "demo").expect("write fixture");
  // A failing `daemon-reload` must surface as a fatal, named error.
  let always_fails = "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$SHIM_LOG\"\nexit 1\n";

  let (output, calls) =
    run_activate(home.path(), &config_path, always_fails).expect("default activate run");

  assert!(
    !output.status.success(),
    "a failing systemctl must fail the command"
  );
  assert!(
    String::from_utf8_lossy(&output.stderr).contains("systemctl"),
    "the error must name the failing systemctl invocation; stderr:\n{}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert_eq!(
    calls, "--user daemon-reload\n",
    "activation must stop at the first failing systemctl call"
  );
}

/// The real `id -u` for the current user — the same value the bin embeds in
/// its `gui/<uid>` launchd target (the `id` binary is NOT shimmed).
#[cfg(unix)]
fn real_uid() -> Result<String, Box<dyn std::error::Error>> {
  let out = Command::new("id").arg("-u").output()?;
  Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
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

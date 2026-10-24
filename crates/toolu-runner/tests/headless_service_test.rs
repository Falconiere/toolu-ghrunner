//! Linux headless service tests through the real CLI and isolated unit files.
//! Supervisor and logind commands are recorded because activating a temporary
//! HOME against the host manager would install a real host service.

#![cfg(target_os = "linux")]

use std::io::Write;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use crate::install_service_test::{shim_env, write_fixture, write_shim};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const ID: &str = "#!/bin/sh\ncase \"$1\" in -u) echo \"$TEST_UID\";; -un) echo runner;; esac\n";
const LOGINCTL: &str = "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$SHIM_LOG\"\ncase \"$1\" in\n  show-user) if [ \"$BAD_LINGER\" = 1 ]; then echo unknown; elif [ -f \"$LINGER_MARK\" ]; then echo Linger=yes; else echo Linger=no; fi;;\n  enable-linger) if [ \"$DENY_LINGER\" = 1 ]; then echo 'polkit denied it' >&2; exit 1; fi; if [ -e \"$EXPECTED_UNIT\" ]; then echo 'unit existed before linger' >&2; exit 10; fi; touch \"$LINGER_MARK\";;\n  *) echo \"unexpected loginctl command: $*\" >&2; exit 2;;\nesac\n";
const SYSTEMCTL: &str = "#!/bin/sh\nprintf 'systemctl %s\\n' \"$*\" >> \"$SHIM_LOG\"\nexit 0\n";

struct Harness {
  home: tempfile::TempDir,
  config: PathBuf,
  path_env: String,
  log: PathBuf,
  mark: PathBuf,
  dest: PathBuf,
}

impl Harness {
  fn new() -> Result<Self, Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    let config = write_fixture(home.path(), "octo", "demo")?;
    let (bin, log, path_env) = shim_env(home.path())?;
    write_shim(&bin, "id", ID)?;
    write_shim(&bin, "loginctl", LOGINCTL)?;
    write_shim(&bin, "systemctl", SYSTEMCTL)?;
    let dest = home
      .path()
      .join(".config/systemd/user/toolu-runner.service");
    let mark = home.path().join("linger-enabled");
    Ok(Self {
      home,
      config,
      path_env,
      log,
      mark,
      dest,
    })
  }

  fn command(&self, args: &[&str], uid: &str) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_toolu-runner"));
    cmd
      .args(args)
      .env("HOME", self.home.path())
      .env("TOOLU_RUNNER_HOME", self.home.path())
      .env("PATH", &self.path_env)
      .env("TEST_UID", uid)
      .env("SHIM_LOG", &self.log)
      .env("LINGER_MARK", &self.mark)
      .env("EXPECTED_UNIT", &self.dest);
    cmd
  }

  fn calls(&self) -> std::io::Result<String> {
    match std::fs::read_to_string(&self.log) {
      Ok(calls) => Ok(calls),
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
      Err(error) => Err(error),
    }
  }
}

fn said(output: &Output) -> String {
  format!(
    "stdout: {}\nstderr: {}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  )
}

#[test]
fn affirmative_linger_finishes_before_a_user_unit_is_written() -> TestResult {
  let h = Harness::new()?;
  let output = h
    .command(
      &["install-service", "--enable-linger", "--no-activate"],
      "1000",
    )
    .output()?;
  assert!(output.status.success(), "{}", said(&output));
  assert!(h.dest.is_file(), "unit was not written");
  assert!(h.mark.is_file(), "linger was not enabled");
  assert_eq!(
    h.calls()?,
    "show-user runner --property=Linger\nenable-linger runner\nshow-user runner --property=Linger\n"
  );
  Ok(())
}

#[test]
fn yes_flag_enables_linger_without_a_terminal() -> TestResult {
  let h = Harness::new()?;
  let output = h
    .command(&["install-service", "--yes", "--no-activate"], "1000")
    .output()?;
  assert!(output.status.success(), "{}", said(&output));
  assert!(h.mark.is_file());
  assert!(h.dest.is_file());
  Ok(())
}

#[test]
fn denied_linger_keeps_the_old_unit_and_writes_no_machine_unit() -> TestResult {
  let h = Harness::new()?;
  let old = h
    .home
    .path()
    .join(".config/systemd/user/toolu-runner-old.service");
  std::fs::create_dir_all(old.parent().ok_or("old unit has no parent")?)?;
  let old_text =
    "[Service]\nExecStart=/usr/local/bin/toolu-runner run --config /tmp/gone/config.toml\n";
  std::fs::write(&old, old_text)?;
  let preview = h
    .command(&["install-service", "--print"], "1000")
    .output()?;
  assert!(preview.status.success(), "{}", said(&preview));
  assert!(
    said(&preview).contains(&format!("would replace {}", old.display())),
    "{}",
    said(&preview)
  );
  let output = h
    .command(&["install-service", "--enable-linger"], "1000")
    .env("DENY_LINGER", "1")
    .output()?;
  assert_eq!(output.status.code(), Some(2), "{}", said(&output));
  assert!(
    said(&output).contains("Couldn't enable linger: polkit denied it. Nothing was installed.")
  );
  assert!(said(&output).contains("sudo loginctl enable-linger runner"));
  assert!(!h.dest.exists());
  assert_eq!(std::fs::read_to_string(old)?, old_text);
  assert!(!h.calls()?.contains("systemctl"));
  Ok(())
}

#[test]
fn unknown_linger_state_aborts_before_writing() -> TestResult {
  let h = Harness::new()?;
  let output = h
    .command(&["install-service", "--yes"], "1000")
    .env("BAD_LINGER", "1")
    .output()?;
  assert_eq!(output.status.code(), Some(2), "{}", said(&output));
  assert!(said(&output).contains("unexpected `loginctl` output"));
  assert!(!h.dest.exists());
  assert!(!h.calls()?.contains("enable-linger"));
  Ok(())
}

#[test]
fn no_terminal_names_the_unattended_flags_and_writes_nothing() -> TestResult {
  let h = Harness::new()?;
  let output = h.command(&["install-service"], "1000").output()?;
  assert_eq!(output.status.code(), Some(2), "{}", said(&output));
  assert!(said(&output).contains("--enable-linger or --yes"));
  assert!(!h.dest.exists());
  Ok(())
}

#[test]
fn deliberate_no_on_a_terminal_installs_a_short_lived_user_unit() -> TestResult {
  let h = Harness::new()?;
  Command::new("script").arg("-V").output().map_err(|error| {
    format!("util-linux script is required for the pseudo-terminal test: {error}")
  })?;
  let mut child = Command::new("timeout")
    .args([
      "10",
      "script",
      "-q",
      "-e",
      "-c",
      "\"$TOOLU_TEST_BINARY\" install-service --no-activate",
      "/dev/null",
    ])
    .env("TOOLU_TEST_BINARY", env!("CARGO_BIN_EXE_toolu-runner"))
    .env("HOME", h.home.path())
    .env("TOOLU_RUNNER_HOME", h.home.path())
    .env("PATH", &h.path_env)
    .env("TEST_UID", "1000")
    .env("SHIM_LOG", &h.log)
    .env("LINGER_MARK", &h.mark)
    .env("EXPECTED_UNIT", &h.dest)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()?;
  child
    .stdin
    .take()
    .ok_or("no script stdin")?
    .write_all(b"n\n")?;
  let output = child.wait_with_output()?;
  let said = String::from_utf8_lossy(&output.stdout);
  assert!(output.status.success(), "{said}");
  assert!(said.contains("Keep running after you log out?"));
  assert!(said.contains("warning: linger is disabled; this user service stops when you log out"));
  assert!(h.dest.is_file());
  assert!(!h.mark.exists());
  Ok(())
}

#[test]
fn root_print_pins_home_without_querying_linger_or_writing() -> TestResult {
  let h = Harness::new()?;
  let system_path = Path::new("/etc/systemd/system/toolu-runner.service");
  let before = std::fs::read(system_path).ok();
  let output = h.command(&["install-service", "--print"], "0").output()?;
  assert!(output.status.success(), "{}", said(&output));
  let unit = String::from_utf8_lossy(&output.stdout);
  assert!(unit.contains("WantedBy=multi-user.target"));
  assert!(unit.contains(&format!("Environment=\"HOME={}\"", h.home.path().display())));
  assert!(unit.contains(&format!(
    "Environment=\"TOOLU_RUNNER_HOME={}\"",
    h.home.path().display()
  )));
  assert!(unit.contains("Environment=\"PATH="));
  assert!(
    h.calls()?.is_empty(),
    "root must not query linger: {}",
    h.calls()?
  );
  assert_eq!(std::fs::read(system_path).ok(), before);
  Ok(())
}

#[test]
fn root_print_rejects_a_non_utf8_home_before_rendering() -> TestResult {
  let h = Harness::new()?;
  let mut raw_home = h.home.path().as_os_str().as_encoded_bytes().to_vec();
  raw_home.push(0xff);
  let output = h
    .command(&["install-service", "--print"], "0")
    .env("HOME", std::ffi::OsString::from_vec(raw_home))
    .output()?;
  assert_eq!(output.status.code(), Some(2), "{}", said(&output));
  assert!(said(&output).contains("HOME contains non-UTF-8 bytes"));
  assert!(!h.dest.exists());
  Ok(())
}

#[test]
fn nonroot_print_does_not_query_or_enable_linger() -> TestResult {
  let h = Harness::new()?;
  let output = h
    .command(&["install-service", "--print"], "1000")
    .output()?;
  assert!(output.status.success(), "{}", said(&output));
  assert!(String::from_utf8_lossy(&output.stdout).contains("WantedBy=default.target"));
  assert!(h.calls()?.is_empty());
  assert!(!h.dest.exists());
  Ok(())
}

#[test]
fn root_registration_print_uses_system_unit_and_keeps_state_unchanged() -> TestResult {
  let h = Harness::new()?;
  let output = h
    .command(
      &[
        "install-service",
        "--config",
        &h.config.to_string_lossy(),
        "--print",
      ],
      "0",
    )
    .output()?;
  assert!(output.status.success(), "{}", said(&output));
  let unit = String::from_utf8_lossy(&output.stdout);
  assert!(unit.contains("WantedBy=multi-user.target"));
  assert!(unit.contains("Environment=\"HOME="));
  assert!(unit.contains("Environment=\"TOOLU_RUNNER_HOME="));
  assert!(h.calls()?.is_empty());
  Ok(())
}

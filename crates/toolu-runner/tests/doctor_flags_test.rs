//! Shell-out tests for the flags of `toolu-runner doctor`: `--config` (must
//! exist, selects one registration, a relative path is made absolute) and
//! `--offline` / `--timeout` on a home with no `.lock`, where the command must
//! not create anything.

use crate::check_servers::{Recorder, run};
use crate::doctor_env::{
  TEST_BEARER, block, doctor_cmd, fix, heading, healthy_reply, install_fix, jit_ok, report, row,
  section, service_install_fix, snapshot, write_registration,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn read_only_without_a_lock() -> TestResult {
  let home = tempfile::tempdir()?;
  let peer = Recorder::spawn(|head| Some(healthy_reply(head)))?;
  let base = format!("http://127.0.0.1:{}", peer.port());
  let config = write_registration(home.path(), "o", "r", &base, false)?;
  let reg_dir = config.parent().ok_or("no parent")?;
  assert!(!reg_dir.join(".lock").exists());
  let before = snapshot(home.path())?;

  for args in [&["--offline"][..], &["--timeout", "5"][..]] {
    let (_, stdout, stderr, _) = run(
      doctor_cmd(home.path())?
        .args(args)
        .env("TOOLU_RUNNER_TOKEN", TEST_BEARER),
    )?;
    assert!(
      block(&stdout, "o/r").contains(&row("FAIL", "run", "not running").as_str()),
      "{}",
      report(&stdout, &stderr)
    );
    assert!(
      !reg_dir.join(".lock").exists(),
      "doctor created a .lock ({args:?})"
    );
    for created in ["_diag", "_work", ".pending_remove"] {
      assert!(
        !reg_dir.join(created).exists(),
        "doctor created {created} ({args:?})"
      );
    }
    assert_eq!(
      snapshot(home.path())?,
      before,
      "doctor changed the home ({args:?})"
    );
  }
  Ok(())
}

#[test]
fn config_flag_must_exist() -> TestResult {
  let home = tempfile::tempdir()?;
  let missing = home.path().join("nowhere").join("config.toml");
  let (code, stdout, stderr, _) = run(doctor_cmd(home.path())?.arg("--config").arg(&missing))?;
  assert_eq!(code, Some(2), "{}", report(&stdout, &stderr));
  assert_eq!(stdout, "", "a usage error prints no report");
  assert_eq!(
    stderr,
    format!("toolu-runner: config not found at {}\n", missing.display())
  );

  // A relative path is reported as given.
  let (code, stdout, stderr, _) = run(doctor_cmd(home.path())?.args(["--config", "nope.toml"]))?;
  assert_eq!(code, Some(2), "{}", report(&stdout, &stderr));
  assert_eq!(stdout, "");
  assert_eq!(stderr, "toolu-runner: config not found at nope.toml\n");
  Ok(())
}

#[test]
fn config_flag_selects_one_registration() -> TestResult {
  let home = tempfile::tempdir()?;
  let base = "http://127.0.0.1:9";
  let wanted = write_registration(home.path(), "o", "wanted", base, true)?;
  write_registration(home.path(), "o", "other", base, true)?;

  // Without the flag both are reported.
  let (_, both, _, _) = run(doctor_cmd(home.path())?.arg("--offline"))?;
  assert!(!block(&both, "o/wanted").is_empty(), "{both}");
  assert!(!block(&both, "o/other").is_empty(), "{both}");

  let (code, stdout, stderr, _) = run(
    doctor_cmd(home.path())?
      .arg("--offline")
      .arg("--config")
      .arg(&wanted),
  )?;
  assert_eq!(code, Some(0), "{}", report(&stdout, &stderr));
  assert_eq!(
    section(&stdout, "registrations"),
    [
      heading("o/wanted", "ephemeral"),
      row("OK", "config", &wanted.display().to_string()),
      row("OK", "creds", "present (0600)"),
      jit_ok(&wanted)?,
      row("WARN", "run", "not running"),
      fix(&format!("toolu-runner run --config {}", wanted.display())),
      row("INFO", "last job", "no jobs yet"),
    ],
    "{stdout}"
  );
  // The other registration is nowhere: not a heading, not a disk row.
  assert!(!stdout.contains("other"), "{stdout}");
  let disks = section(&stdout, "host")
    .into_iter()
    .filter(|line| line.contains("  disk "))
    .count();
  assert_eq!(disks, 2, "the home and the one work dir:\n{stdout}");
  Ok(())
}

#[test]
fn relative_config_flag_is_made_absolute() -> TestResult {
  let home = tempfile::tempdir()?;
  let config = write_registration(home.path(), "o", "r", "http://127.0.0.1:9", false)?;
  let reg_dir = config.parent().ok_or("no parent")?;
  // The child's working directory is what `std::path::absolute` joins, and
  // the OS reports it with symlinks resolved (/var -> /private/var).
  let absolute = std::fs::canonicalize(&config)?;

  let (code, stdout, stderr, _) = run(doctor_cmd(home.path())?.current_dir(reg_dir).args([
    "--offline",
    "--config",
    "config.toml",
  ]))?;
  assert_eq!(code, Some(1), "{}", report(&stdout, &stderr));
  assert_eq!(
    section(&stdout, "registrations"),
    [
      heading("o/r", "persistent"),
      row("OK", "config", &absolute.display().to_string()),
      row("OK", "creds", "present (0600)"),
      jit_ok(&config)?,
      row("FAIL", "run", "not running"),
      install_fix(&absolute),
      row("WARN", "service", "not installed"),
      service_install_fix(),
      row("INFO", "last job", "no jobs yet"),
    ],
    "{stdout}"
  );
  assert!(!stdout.contains("  config.toml:"), "{stdout}");

  // From a subdirectory, `../config.toml` folds to the same identity: an
  // unfolded `_diag/..` would hide `runners/o/r` from the service lookup.
  let diag = reg_dir.join("_diag");
  std::fs::create_dir_all(&diag)?;
  let (code, from_diag, stderr, _) = run(doctor_cmd(home.path())?.current_dir(&diag).args([
    "--offline",
    "--config",
    "../config.toml",
  ]))?;
  assert_eq!(code, Some(1), "{}", report(&from_diag, &stderr));
  assert_eq!(
    section(&from_diag, "registrations"),
    section(&stdout, "registrations"),
    "{from_diag}"
  );
  Ok(())
}
